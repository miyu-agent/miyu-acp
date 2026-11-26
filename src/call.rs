//! 一次 `driver.call`（设计 73 第四节第 4～8 条，照 `miyu-antigravity` 的 `call` 搬）：
//!
//! 1. 认是哪段对话（[`crate::sessions::next_turn`]）：表里有闲着的、接得上的，就是**新的一轮**——只把新的几条 user 消息作为
//!    一次 `session/prompt` 交给那个活着的会话（`call/resume.rs`）；挂着的（上一次以交回一件工具调用、或一次问主人结束）带着
//!    结果来，就把结果回给 agent、接着听同一个 `session/prompt`。
//! 2. 认不上的**新开**（`call/open.rs`）：找到、装好 agent，起进程，`initialize`，开会话（系统提示词照能力交，Miyu 的工具经
//!    `mcpServers` 交），整份历史转写成第一次提问。
//! 3. 出了任何错（含 `driver.cancel`）：这段会话作废——收进程组（丢掉 [`Live`] 就是收掉）。
//!
//! `driver.cancel` 经 [`Signal::Cancel`] 送进同一个通道。

mod approve;
mod open;
mod resume;
mod talk;

use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::agents::{self, Family};
use crate::bridge::Tool;
use crate::config::Config;
use crate::failure::Class;
use crate::hub::Tools;
use crate::live::Live;
use crate::names::Names;
use crate::pool::Registry;
use crate::rpc::Out;
use crate::sessions::{Identity, Incoming, Meta};
pub use crate::signal::Signal;
use crate::turn;
use crate::{schema, transcript, workspace};
pub use approve::{approve_id, approve_of};
use resume::{resume, resume_pending};

/// 这一次怎么回：照常的 `result`，或者 JSON-RPC 的出错（参数不对）。
#[derive(Debug, PartialEq)]
pub enum Answer {
    /// `result`：`{"usage"?, "error"?, "sent"?}`。
    Result(Value),
    /// JSON-RPC 出错：参数读不成（`bad_params`）。
    BadParams(String),
}

/// 这一次要用的。
struct Job<'a> {
    out: &'a Out,
    config: &'a Config,
    registry: &'a Registry,
    tools: &'a Tools,
    call: &'a str,
    params: &'a Value,
    request: &'a Value,
    /// agent 的编号和要它用的模型（`None` 是它默认的）。
    agent: String,
    model: Option<String>,
    family: Family,
    /// 请求里的工具（照这个 agent 整过形的）。
    declared: Vec<Tool>,
    names: Names,
    signals: &'a Receiver<Signal>,
    sender: &'a Sender<Signal>,
}

/// 跑一次 `driver.call`：参数 `params`，增量经 `out` 交，`signals` 收 agent 的输出、MCP 转来的工具调用和取消（`sender` 是它的
/// 发送头）；活着的会话在 `registry` 里，工具这一头是 `tools`。
pub fn run(
    out: &Out,
    config: &Config,
    registry: &Registry,
    tools: &Tools,
    params: &Value,
    signals: &Receiver<Signal>,
    sender: &Sender<Signal>,
) -> Answer {
    let Some(call) = params.get("call").and_then(Value::as_str) else {
        return Answer::BadParams("params.call is missing".into());
    };
    let Some(model) = params.pointer("/params/model").and_then(Value::as_str) else {
        return Answer::BadParams("params.params.model is missing".into());
    };
    let Some(request) = params.get("request") else {
        return Answer::BadParams("params.request is missing".into());
    };
    if request.get("system").and_then(Value::as_str).is_none() {
        return Answer::BadParams("params.request.system is not text".into());
    }
    let driver = params.get("driver").and_then(Value::as_str);
    let Some((agent, agent_model)) = agents::split(driver, model) else {
        return Answer::BadParams(format!(
            "cannot tell the ACP agent from the model {model:?}"
        ));
    };
    // 请求的形状不对的当场拒。
    if let Err(error) = transcript::transcribe(request, transcript::BUDGET) {
        return Answer::BadParams(error.to_string());
    }
    let family = Family::of(&agent);
    let declared = match declared_tools(request, family) {
        Ok(declared) => declared,
        Err(message) => return Answer::BadParams(message),
    };
    let names = Names::new(declared.iter().map(|tool| tool.name.clone()));
    let incoming = incoming(params, model, request);
    if incoming.is_none() {
        eprintln!("call {call}: no usable fingerprint; this agent session will not be kept");
    }
    let job = Job {
        out,
        config,
        registry,
        tools,
        call,
        params,
        request,
        agent,
        model: agent_model,
        family,
        declared,
        names,
        signals,
        sender,
    };
    if let Some(incoming) = &incoming
        && let Some(claimed) = registry.claim(incoming)
    {
        let resumed = if claimed.pending {
            resume_pending(&job, claimed, incoming)
        } else {
            resume(&job, claimed, incoming)
        };
        match resumed {
            Ok(answer) => return Answer::Result(answer),
            Err(gone) => eprintln!("call {call}: {gone}; starting a new agent session"),
        }
    }
    Answer::Result(open::fresh(&job, incoming))
}

/// 认对话要的：模型、`fingerprint` 的三样、每条消息的 `role`。缺了、写法不对、消息指纹和消息数对不上的是 `None`。
fn incoming(params: &Value, model: &str, request: &Value) -> Option<Incoming> {
    let fingerprint = params.get("fingerprint")?;
    let text = |key: &str| fingerprint.get(key)?.as_str().map(str::to_string);
    let messages: Vec<String> = fingerprint
        .get("messages")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<_>>()?;
    let roles: Vec<String> = request
        .get("messages")?
        .as_array()?
        .iter()
        .map(|message| message.get("role")?.as_str().map(str::to_string))
        .collect::<Option<_>>()?;
    if messages.len() != roles.len() {
        return None;
    }
    let driver = params
        .get("driver")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(Incoming {
        identity: Identity {
            model: format!("{driver} {model}"),
            system: text("system")?,
            tools: text("tools")?,
        },
        messages,
        roles,
    })
}

/// 请求里声明的工具：名字、说明、参数格式（Gemini 系照 [`schema::shape`] 整形；Antigravity 另声明 [`schema::AGY_EXTRAS`]）。
fn declared_tools(request: &Value, family: Family) -> Result<Vec<Tool>, String> {
    let Some(declared) = request.get("tools").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    declared
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("params.request.tools[{index}].name is not text"))?;
            let mut schema = tool.get("parameters").cloned().unwrap_or_else(|| json!({}));
            if family.gemini_schemas() {
                schema = schema::shape(schema);
            }
            if family == Family::Antigravity {
                schema = schema::with_extras(schema);
            }
            Ok(Tool {
                name: name.to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input_schema: schema,
            })
        })
        .collect()
}

/// 成功的回答带上 `sent`：这一轮交给 agent 的东西（提问的内容块、回的工具结果）的 SHA-256。
fn with_sent(mut answer: Value, sent: &Value) -> Value {
    if let Some(object) = answer.as_object_mut()
        && !object.contains_key("error")
    {
        let digest = Sha256::digest(sent.to_string().as_bytes());
        object.insert(
            "sent".into(),
            Value::String(format!("sha256:{}", workspace::hex(&digest))),
        );
    }
    answer
}

/// 回答成功了没有（没有 `error`）。
fn succeeded(answer: &Value) -> bool {
    answer.get("error").is_none()
}

/// 被取消的这一次的回答：核心已经不理了，照样回一个，请求有始有终。
fn cancelled() -> Value {
    turn::failed(Class::Retryable, "the call was cancelled")
}

/// 登记一段说完了（或挂着的）新会话。
fn keep(job: &Job<'_>, incoming: Option<Incoming>, live: Live, pending: bool) {
    match incoming {
        Some(incoming) => job.registry.insert(
            Meta {
                identity: incoming.identity,
                messages: incoming.messages,
                agent: job.agent.clone(),
                used: Instant::now(),
                pending,
            },
            live,
        ),
        None => drop(live),
    }
}
