//! 送给一次 `driver.call` 的事：agent 那边来的（ACP 的通知、请求、交给这一次的回应，读的结局）、MCP 服务转来的工具调用、
//! 核心的取消。读 agent 输出的线程（[`crate::acp::Conn`]）和总台（[`crate::hub`]）照 [`Route`] 现在指着的那一次送。

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::acp::RpcError;
use crate::hub::McpCall;

/// 读的线程、总台把信号送给谁：这一轮在听的那一次调用；没人听是 `None`。
pub type Route = Arc<Mutex<Option<Sender<Signal>>>>;

/// 一件事。
#[derive(Debug)]
pub enum Signal {
    /// agent 发来的 `session/update` 的 `params`。
    Update(Value),
    /// agent 向客户端发的请求（`session/request_permission` 之类）：要回话。
    Request {
        /// 请求的编号，回话时原样带上。
        id: Value,
        /// 方法。
        method: String,
        /// 参数。
        params: Value,
    },
    /// 交给这一轮的请求（`session/prompt`）的回应。
    Response {
        /// 我们发的请求编号。
        id: u64,
        /// 回应：`result` 或 `error`。
        outcome: Result<Value, RpcError>,
    },
    /// agent 写了一行超长的：之后不再读。
    TooLong,
    /// 读 agent 的输出出错了。
    ReadFailed(String),
    /// agent 的输出读到头了（退了）。
    Closed,
    /// agent 经 MCP 服务调了一件 Miyu 的工具。
    ToolCall(McpCall),
    /// 核心回了一条请它问主人的 `driver.approve`（核心施工 ACP-8）：这个会话里的问话编号、`result` 或者 `error`。
    Approved {
        /// 问话编号。
        n: u64,
        /// 回应。
        outcome: Result<Value, Value>,
    },
    /// 核心发了 `driver.cancel`，或者扩展要退出了。
    Cancel,
}

impl Signal {
    /// 这件事说明 agent 在「说话」：正文、思考、开一件新的工具调用、要权限、调 Miyu 的工具。挂着等工具结果的时候来了这种，
    /// 说明它已经不等了。用量、命令表这类状态更新，和已经开着的工具调用的进度不算。
    #[must_use]
    pub fn is_speech(&self) -> bool {
        match self {
            Signal::Update(params) => is_speech(params),
            Signal::Request { .. } | Signal::ToolCall(_) => true,
            _ => false,
        }
    }
}

/// 一条 `session/update` 的 `params` 是不是「说话」（[`Signal::is_speech`]）。
#[must_use]
pub fn is_speech(params: &Value) -> bool {
    matches!(
        params
            .pointer("/update/sessionUpdate")
            .and_then(Value::as_str),
        Some("agent_message_chunk" | "agent_thought_chunk" | "tool_call")
    )
}
