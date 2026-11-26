//! `mcp`：agent 照 `session/new` 的 `mcpServers` 拉起的 MCP 服务（设计 73 第四节第 6 条，名字 `miyu`），标准输入输出上一行一条
//! JSON-RPC。照 `miyu-antigravity` 的 `mcp` 搬。很薄：握手自己答，`tools/list`、`tools/call` 经本机套接字（[`crate::bridge`]）转给
//! 扩展进程。靠 `mcpServers` 那一条给的环境变量 [`bridge::ENV_BRIDGE`]、[`bridge::ENV_TOKEN`] 认出自己属于哪个 ACP 会话；缺一个的
//! `tools/list` 是空表，`tools/call` 回一句说明。
//!
//! 一个 `tools/call` 可能挂几十分钟，挂着的时候别的请求（`tools/list`、再一条 `tools/call`）也要能答：
//! 要等套接字的请求各在自己的线程里办，写标准输出照 [`Out`] 加锁、整行写、刷新。标准输入读到头就退出，挂着的调用不等。
//! 工具失败回 `isError: true` 加文字，不回 JSON-RPC 错误；JSON-RPC 错误只给不认识的方法（`-32601`）和写坏了的参数（`-32602`）。

use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::bridge::{self, client};
use crate::process::{LINE_LIMIT, Line, next_line};
use crate::rpc::Out;
use crate::words;

/// 客户端没给版本时答的协议版本。
const DEFAULT_PROTOCOL: &str = "2025-11-25";

/// 套接字的位置和口令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    path: PathBuf,
    token: String,
}

impl Bridge {
    /// 照环境变量：两个都有（非空）才有，缺一个是 `None`。
    #[must_use]
    pub fn from_env() -> Option<Bridge> {
        Bridge::from_values(
            std::env::var_os(bridge::ENV_BRIDGE),
            std::env::var(bridge::ENV_TOKEN).ok(),
        )
    }

    fn from_values(path: Option<std::ffi::OsString>, token: Option<String>) -> Option<Bridge> {
        let path = path.filter(|path| !path.is_empty())?;
        let token = token.filter(|token| !token.is_empty())?;
        Some(Bridge {
            path: PathBuf::from(path),
            token,
        })
    }
}

/// 跑 `mcp`：读 `input`、写 `out`，读到头退出（码 0）；读出错、一行超长（没法对齐下一行了）退出码 1。
#[must_use]
pub fn run(mut input: impl BufRead, out: &Out, bridge: Option<Bridge>) -> ExitCode {
    let bridge = Arc::new(bridge);
    loop {
        match next_line(&mut input, LINE_LIMIT) {
            Ok(Some(Line::Text(line))) => handle(&line, out, &bridge),
            Ok(None) => return ExitCode::SUCCESS,
            Ok(Some(Line::TooLong)) => {
                eprintln!("a request line from the agent is over {LINE_LIMIT} bytes; stopping");
                return ExitCode::from(1);
            }
            Err(error) => {
                eprintln!("reading from the agent failed: {error}");
                return ExitCode::from(1);
            }
        }
    }
}

/// 办一行。
fn handle(line: &str, out: &Out, bridge: &Arc<Option<Bridge>>) {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        eprintln!("skipping a line from the agent that is not JSON: {line:.200}");
        return;
    };
    // 没有 `id` 的是通知（`notifications/*`），不理；没有 `method` 的是别人的回应，也不理。
    let (Some(id), Some(method)) = (
        message.get("id").filter(|id| !id.is_null()),
        message.get("method").and_then(Value::as_str),
    ) else {
        return;
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => out.reply(id, &initialize_result(&params)),
        "tools/list" => spawn(out, id, move |bridge| list_result(bridge.as_ref()), bridge),
        "tools/call" => {
            let Some(name) = params
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                error(out, id, -32602, "tools/call needs a tool name");
                return;
            };
            let arguments = params
                .get("arguments")
                .filter(|arguments| !arguments.is_null())
                .cloned()
                .unwrap_or_else(|| json!({}));
            spawn(
                out,
                id,
                move |bridge| call_result(bridge.as_ref(), &name, &arguments),
                bridge,
            );
        }
        other => error(out, id, -32601, &format!("method not found: {other}")),
    }
}

/// 在新线程里办会等套接字的请求，办完回话；线程起不来回 JSON-RPC 内部错误。
fn spawn(
    out: &Out,
    id: &Value,
    work: impl FnOnce(&Option<Bridge>) -> Value + Send + 'static,
    bridge: &Arc<Option<Bridge>>,
) {
    let (out_for_thread, id_for_thread, bridge) = (out.clone(), id.clone(), Arc::clone(bridge));
    let spawned = std::thread::Builder::new()
        .name("mcp-request".into())
        .spawn(move || out_for_thread.reply(&id_for_thread, &work(&bridge)));
    if let Err(failure) = spawned {
        eprintln!("starting a request thread failed: {failure}");
        error(out, id, -32603, "cannot start a thread for this request");
    }
}

fn error(out: &Out, id: &Value, code: i64, message: &str) {
    out.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
}

/// `initialize` 的回应：版本照客户端给的写回，没给回 [`DEFAULT_PROTOCOL`]。
fn initialize_result(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "miyu-acp", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// `tools/list` 的结果：没有套接字、连不上、对面拒绝、回话没有 `tools` 数组，都是空表（记一行到标准错误）。
fn list_result(bridge: Option<&Bridge>) -> Value {
    let Some(bridge) = bridge else {
        return json!({"tools": []});
    };
    let request = bridge::list_request(&bridge.token);
    match client::exchange(&bridge.path, &request) {
        Ok(reply) => match reply.get("tools") {
            Some(tools) if tools.is_array() => json!({"tools": tools}),
            _ => {
                eprintln!("the bridge's list reply has no tools array; answering an empty list");
                json!({"tools": []})
            }
        },
        Err(failure) => {
            eprintln!("asking Miyu for the tool list failed: {failure}; answering an empty list");
            json!({"tools": []})
        }
    }
}

/// `tools/call` 的结果：一直等到扩展回话，原样交（`content`、`isError`）；别的情况都是 `isError` 加一句说明。
fn call_result(bridge: Option<&Bridge>, name: &str, arguments: &Value) -> Value {
    let Some(bridge) = bridge else {
        return tool_failure(words::MCP_NO_BRIDGE);
    };
    let request = bridge::call_request(&bridge.token, name, arguments);
    match client::exchange(&bridge.path, &request) {
        Ok(reply) => match reply.get("content") {
            Some(content) if content.is_array() => json!({
                "content": content,
                "isError": reply.get("isError").and_then(Value::as_bool).unwrap_or(false),
            }),
            _ => {
                eprintln!("the bridge's call reply has no content array");
                tool_failure(words::MCP_BRIDGE_GONE)
            }
        },
        Err(failure) => {
            eprintln!("calling {name} through Miyu failed: {failure}");
            tool_failure(words::MCP_BRIDGE_GONE)
        }
    }
}

fn tool_failure(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

#[cfg(test)]
mod tests;
