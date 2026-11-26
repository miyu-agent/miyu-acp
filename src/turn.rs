//! 一轮 ACP 的 `session/update` → `driver.delta` 的增量和 `driver.call` 的回答（设计 73 第四节第 8 条）。纯的状态机，不碰 I/O。
//!
//! - `agent_message_chunk` → 正文块，`agent_thought_chunk` → 思考块：同一种、同一条消息（`messageId`）接着写，换了另开一块。
//! - `tool_call`：Miyu 的工具（[`Names`] 认得出）记「流这边到了」；MCP 服务转来的那一件调用（[`Turn::tool_call`]）记「MCP
//!   那边到了」。两边都到了才交一块工具调用并结束这一次（先后以流为准，内容以 MCP 那边为准）；只到了一边的由调用的人掐表
//!   （[`Turn::waiting`]、[`Turn::timeout`]），照 `miyu-antigravity` 的做法。别的工具调用照实交成供应商侧工具块（`turn/hosted.rs`）。
//! - `usage_update`：记下最后一次的 `used`（回答的 `usage.context`）、`size`（模型窗口）。`plan` 等别的不交。
//! - `session/prompt` 的回应（[`Turn::finish`]）：`end_turn` 正常结束；`max_tokens`、`max_turn_requests` 交一条 `truncated`；
//!   `refusal` → `content_policy`；`cancelled` → `retryable`。回应带 `usage`（ACP 草案「End-Turn Token Usage」、Claude 适配器
//!   0.112.0 照它报）的照它记账；没有的不报用量（核心照「没有报告」记，和 `miyu-antigravity` 一样）。
//! - 要主人确认时（[`Turn::ask`]）交一块 `ask_user` 工具调用，私有数据 `{"permission": n}`，这一次到此结束。

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::acp::RpcError;
use crate::failure::{self, Class};
use crate::names::Names;

mod hosted;

pub use hosted::{OUTCOME_LIMIT, cut};

/// 一段字至多多少字节：交给核心的一行不能超过 1 MiB，字里的控制字符转义后至多六倍长。
pub const TEXT_PIECE: usize = 128 * 1024;

/// 开着的正文或思考块。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Open {
    reasoning: bool,
    index: u64,
    message: Option<String>,
}

/// MCP 那边到了的一件调用：编号、名字、`arguments` 的 JSON 原文。
#[derive(Debug)]
struct Held {
    n: u64,
    name: String,
    arguments: String,
}

/// 这一次以交回什么结束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    /// 一件 Miyu 的工具调用（MCP 那边的编号）。
    Mcp(u64),
    /// 一次问主人（这个会话里的问话编号）。
    Permission(u64),
}

/// 只到了一边，在等另一边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    /// 流这边到了，等 MCP 那边的调用。
    ForMcp,
    /// MCP 那边到了，等流这边的 `tool_call`。
    ForStream,
}

/// 读一条以后：要交的增量，和读完了的话这一次的回答。
#[derive(Debug, Default, PartialEq)]
pub struct Fed {
    /// 照先后要交的增量。
    pub deltas: Vec<Value>,
    /// 这一次结束了：回答。
    pub answer: Option<Value>,
    /// 这一次是交回什么结束的（回答是用量）。
    pub handed: Option<Hand>,
}

/// 同一个 `session/prompt` 跨几次 `driver.call`（中间交回过工具调用、问过主人）要记住的：哪几件是 Miyu 的工具调用、哪几件要过
/// 权限、哪几件对方的工具已经画过。下一次接着听的时候交给 [`Turn::resume`]，不然交回过的那件的进度会被当成对方的工具再画一遍。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Memory {
    miyu: HashSet<String>,
    asked: HashSet<String>,
    seen: HashSet<String>,
}

/// 一轮的状态。
#[derive(Debug)]
pub struct Turn {
    next_block: u64,
    open: Option<Open>,
    delivered: bool,
    names: Names,
    /// 这一轮认出来的 Miyu 工具调用（ACP 的 `toolCallId`）：它们的进度不画。
    miyu: HashSet<String>,
    stream_tool: bool,
    mcp: Option<Held>,
    handed: bool,
    context: Option<u64>,
    window: Option<u64>,
    /// 要过权限的工具调用：自己跑了却没要权限的记一条警告。
    pub(crate) asked: HashSet<String>,
    hosting: hosted::Hosting,
    /// 要登录时给人看的一句（[`Turn::rejected`]）。
    login: String,
}

impl Turn {
    /// 新的一轮：Miyu 的工具名 `names`；`login` 是 agent 要登录时给人看的一句。
    #[must_use]
    pub fn new(names: Names, login: &str) -> Turn {
        Turn::resume(names, login, Memory::default())
    }

    /// 接着听同一个 `session/prompt`：带上之前记住的 `memory`。
    #[must_use]
    pub fn resume(names: Names, login: &str, memory: Memory) -> Turn {
        let Memory { miyu, asked, seen } = memory;
        let mut turn = Turn {
            next_block: 0,
            open: None,
            delivered: false,
            names,
            miyu,
            stream_tool: false,
            mcp: None,
            handed: false,
            context: None,
            window: None,
            asked,
            hosting: hosted::Hosting::default(),
            login: login.to_string(),
        };
        turn.hosting.seen = seen;
        turn
    }

    /// 这一次说完，要留给同一个 `session/prompt` 下一次的。
    #[must_use]
    pub fn memory(&self) -> Memory {
        Memory {
            miyu: self.miyu.clone(),
            asked: self.asked.clone(),
            seen: self.hosting.seen.clone(),
        }
    }

    /// 这一轮认出来的 Miyu 工具调用的编号。
    #[must_use]
    pub fn miyu_calls(&self) -> &HashSet<String> {
        &self.miyu
    }

    /// 最后一次 `usage_update` 的 `size`（模型窗口）。
    #[must_use]
    pub fn window(&self) -> Option<u64> {
        self.window
    }

    /// 读一条 `session/update` 的 `update`。
    pub fn update(&mut self, update: &Value) -> Fed {
        let mut fed = Fed::default();
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some(kind @ ("agent_message_chunk" | "agent_thought_chunk")) => {
                let text = update
                    .pointer("/content/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let message = update.get("messageId").and_then(Value::as_str);
                self.chunk(
                    kind == "agent_thought_chunk",
                    message,
                    text,
                    &mut fed.deltas,
                );
            }
            Some("tool_call") => {
                let id = call_id(update);
                if self.names.of_call(update).is_some() {
                    self.miyu.insert(id);
                    self.close_open(&mut fed.deltas);
                    if !self.handed {
                        self.stream_tool = true;
                        self.pair(&mut fed);
                    }
                } else {
                    self.hosted_start(update, &mut fed.deltas);
                }
            }
            Some("tool_call_update") => {
                if !self.miyu.contains(&call_id(update)) {
                    self.hosted_update(update, &mut fed.deltas);
                }
            }
            Some("usage_update") => {
                self.context = update.get("used").and_then(Value::as_u64).or(self.context);
                self.window = update.get("size").and_then(Value::as_u64).or(self.window);
            }
            _ => {}
        }
        fed
    }

    /// 一段正文或思考。
    fn chunk(
        &mut self,
        reasoning: bool,
        message: Option<&str>,
        text: &str,
        deltas: &mut Vec<Value>,
    ) {
        if text.is_empty() {
            return;
        }
        self.close_hosted(deltas);
        let same = self.open.as_ref().is_some_and(|open| {
            open.reasoning == reasoning && (message.is_none() || open.message.as_deref() == message)
        });
        if !same {
            self.close_open(deltas);
            let index = self.next_index();
            let kind = if reasoning { "reasoning" } else { "text" };
            deltas.push(json!({"start": {"index": index, "kind": kind}}));
            self.open = Some(Open {
                reasoning,
                index,
                message: message.map(str::to_string),
            });
        }
        if let Some(open) = &self.open {
            push_text(deltas, open.index, text);
        }
    }

    /// 下一块的编号；交过增量了。
    fn next_index(&mut self) -> u64 {
        let index = self.next_block;
        self.next_block += 1;
        self.delivered = true;
        index
    }

    /// 收掉开着的正文、思考块。
    fn close_text(&mut self, deltas: &mut Vec<Value>) {
        if let Some(open) = self.open.take() {
            deltas.push(json!({"end": {"index": open.index}}));
        }
    }

    /// 收掉开着的块（正文、思考、供应商侧工具）。
    fn close_open(&mut self, deltas: &mut Vec<Value>) {
        self.close_text(deltas);
        self.close_hosted(deltas);
    }

    /// MCP 那边到了：调用 `n`，工具名 `name`，参数 `arguments`。
    pub fn tool_call(&mut self, n: u64, name: &str, arguments: &Value) -> Fed {
        self.mcp = Some(Held {
            n,
            name: name.to_string(),
            arguments: arguments.to_string(),
        });
        let mut fed = Fed::default();
        self.pair(&mut fed);
        fed
    }

    /// 在等哪一边；两边都没到、或者已经交回了的是 `None`。
    #[must_use]
    pub fn waiting(&self) -> Option<Waiting> {
        if self.handed {
            return None;
        }
        match (self.stream_tool, self.mcp.is_some()) {
            (true, false) => Some(Waiting::ForMcp),
            (false, true) => Some(Waiting::ForStream),
            _ => None,
        }
    }

    /// 等另一边等到点了：只有 MCP 那边到了的照交；只有流那边到了的这一次出错（agent 没连上本包的工具服务）。
    pub fn timeout(&mut self) -> Fed {
        let mut fed = Fed::default();
        match self.waiting() {
            Some(Waiting::ForStream) => self.hand_back(&mut fed),
            Some(Waiting::ForMcp) => {
                fed.answer = Some(failed(
                    Class::Unclassified,
                    "the agent called a Miyu tool, but the miyu tool server never reached Miyu",
                ));
            }
            None => {}
        }
        fed
    }

    fn pair(&mut self, fed: &mut Fed) {
        if self.stream_tool && self.mcp.is_some() {
            self.hand_back(fed);
        }
    }

    /// 交一块工具调用（名字、参数原文、私有数据、收块），这一次以没有用量的回答结束。
    fn hand_back(&mut self, fed: &mut Fed) {
        let Some(held) = self.mcp.take() else {
            return;
        };
        self.handed_block(&held.name, &held.arguments, &json!({"mcp": held.n}), fed);
        fed.handed = Some(Hand::Mcp(held.n));
    }

    /// 交回一次问主人：`ask_user` 的参数 `arguments`，问话编号 `n`。
    pub fn ask(&mut self, n: u64, arguments: &Value) -> Fed {
        let mut fed = Fed::default();
        let name = crate::permission::ASK_TOOL;
        self.handed_block(
            name,
            &arguments.to_string(),
            &json!({"permission": n}),
            &mut fed,
        );
        fed.handed = Some(Hand::Permission(n));
        fed
    }

    fn handed_block(&mut self, name: &str, arguments: &str, private: &Value, fed: &mut Fed) {
        self.close_open(&mut fed.deltas);
        let index = self.next_index();
        self.handed = true;
        fed.deltas
            .push(json!({"start": {"index": index, "kind": {"tool_call": {"name": name}}}}));
        push_text(&mut fed.deltas, index, arguments);
        fed.deltas
            .push(json!({"private": {"index": index, "data": private}}));
        fed.deltas.push(json!({"end": {"index": index}}));
        fed.answer = Some(self.usage_answer(None));
    }

    /// `session/prompt` 回了：照 `stopReason` 收尾。
    pub fn finish(&mut self, result: &Value) -> Fed {
        let mut fed = Fed::default();
        self.close_open(&mut fed.deltas);
        let reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn");
        fed.answer = Some(match reason {
            "refusal" => failed(
                Class::ContentPolicy,
                "the agent refused to continue (stopReason: refusal)",
            ),
            "cancelled" => failed(Class::Retryable, "the agent cancelled the turn"),
            other => {
                if matches!(other, "max_tokens" | "max_turn_requests") {
                    fed.deltas.push(json!("truncated"));
                } else if other != "end_turn" {
                    eprintln!("the agent ended its turn with an unknown stopReason {other:?}");
                }
                self.usage_answer(result.get("usage"))
            }
        });
        fed
    }

    /// `session/prompt` 回了出错：要登录的报 `auth`（说清去哪登录），别的照原话归类。
    pub fn rejected(&mut self, error: &RpcError) -> Fed {
        let mut fed = Fed::default();
        self.close_open(&mut fed.deltas);
        fed.answer = Some(if error.needs_login() {
            failed(Class::Auth, &self.login)
        } else {
            let said = error.said();
            failed(
                failure::classify(&said, self.delivered),
                &format!("the agent failed: {said}"),
            )
        });
        fed
    }

    /// 流断了、没等到回应：`why` 是英文的一句原因。
    #[must_use]
    pub fn broken(&self, why: &str) -> Value {
        failed(failure::otherwise(self.delivered), why)
    }

    /// 回答的用量：`counts` 是回应里的 `usage`（`inputTokens` 这些，照 Anthropic 的算法：输入不含缓存）；没有的不报。有的话
    /// 另写最后一次 `usage_update` 的 `used`（`context`，核心施工 ACP-1）。
    fn usage_answer(&self, counts: Option<&Value>) -> Value {
        let count = |key: &str| {
            counts
                .and_then(|usage| usage.get(key))
                .and_then(Value::as_u64)
        };
        let (Some(input), Some(output)) = (count("inputTokens"), count("outputTokens")) else {
            // 没有记账的数：核心的 `usage` 四项必填，`context` 单独报不了（2026-11-26 核心 main 286ba979f），只好不报。
            return json!({});
        };
        let mut usage = json!({
            "uncached": input,
            "cache_read": count("cachedReadTokens").unwrap_or(0),
            "cache_write": count("cachedWriteTokens").unwrap_or(0),
            "output": output,
        });
        if let Some(thought) = count("thoughtTokens").filter(|thought| *thought > 0) {
            usage["reasoning"] = json!(thought);
        }
        if let Some(context) = self.context {
            usage["context"] = json!(context);
        }
        json!({"usage": usage})
    }
}

/// 一条工具调用的编号。
fn call_id(update: &Value) -> String {
    update
        .get("toolCallId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 回答里只有 `error` 的那种。
#[must_use]
pub fn failed(class: Class, message: &str) -> Value {
    json!({"error": failure::error(class, message)})
}

/// 交一段字，太长的切成几段（不切开一个字）。
fn push_text(deltas: &mut Vec<Value>, index: u64, text: &str) {
    let mut rest = text;
    while rest.len() > TEXT_PIECE {
        let mut cut = TEXT_PIECE;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (piece, tail) = rest.split_at(cut);
        deltas.push(json!({"text": {"index": index, "text": piece}}));
        rest = tail;
    }
    deltas.push(json!({"text": {"index": index, "text": rest}}));
}

#[cfg(test)]
mod tests;
