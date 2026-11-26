//! 说 ACP（Agent Client Protocol，<https://agentclientprotocol.com/protocol/v1/overview>）：本包是客户端，agent 是子进程，标准
//! 输入输出上一行一条 JSON-RPC 2.0（「Transports」：stdio）。
//!
//! - [`Conn`]：一个 agent 进程和它那一条连接：发请求（等回应的、回应送进这一轮信号的）、发通知、回 agent 的请求。
//! - [`handshake`]：`initialize`、`authenticate`、`session/new`（`session/resume`）怎么写、怎么读。
//! - [`modes`]：开会话以后把对方的权限模式切到「每次都问」。

mod conn;
pub mod handshake;
pub mod modes;

pub use conn::{Conn, Failure};

use serde_json::Value;

/// ACP 的「要先登录」（`-32000`，「Errors」一页）。
pub const AUTH_REQUIRED: i64 = -32000;

/// 客户端没实现的方法（`-32601`）。
pub const METHOD_NOT_FOUND: i64 = -32601;

/// agent 回的 JSON-RPC 出错。
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    /// 码。
    pub code: i64,
    /// 原话。
    pub message: String,
    /// `data`（形状看码）。
    pub data: Option<Value>,
}

impl RpcError {
    /// 读回应里的 `error`；不是对象的当一句「出错了」。
    #[must_use]
    pub fn read(error: &Value) -> RpcError {
        RpcError {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the agent returned an error")
                .to_string(),
            data: error.get("data").cloned(),
        }
    }

    /// 这是「要先登录」。
    #[must_use]
    pub fn needs_login(&self) -> bool {
        self.code == AUTH_REQUIRED
    }

    /// 原话连同 `data`（有的话）写成一句，好归类、给人看。
    #[must_use]
    pub fn said(&self) -> String {
        match &self.data {
            Some(Value::String(text)) => format!("{} ({text})", self.message),
            Some(data) if !data.is_null() => format!("{} ({data})", self.message),
            _ => self.message.clone(),
        }
    }
}
