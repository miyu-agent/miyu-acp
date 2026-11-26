//! 一个活着的 ACP 会话：一个 agent 进程（[`Conn`]）和它上面开好的那一个会话，连着收好几轮，几次 `driver.call` 之间留着
//! （设计 73 第四节第 4 条：进程按 Miyu 会话复用）。
//!
//! - 带工具的（[`Wiring`]）：路由和「乱说话了」的标记用总台那一份，MCP 服务转来的调用和 agent 的输出进同一条信号通道。
//! - 一轮以交回结束时，挂着的那件（MCP 调用的 [`Reply`]，或者 agent 等着回话的权限请求）跟着 [`Live`]（[`Live::set_pending`]），
//!   等下一次请求带结果来；这一轮的 `session/prompt` 还没回，编号记在 [`Live::prompt`]。
//! - 收掉（丢掉这个值时）：挂着的权限请求回 `cancelled`、从总台拿掉口令、收进程组。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;

use crate::acp::Conn;
use crate::acp::handshake::Caps;
use crate::bridge::Reply;
use crate::hub::Wiring;
use crate::signal::Route;
use crate::turn::Memory;

/// 挂着的：agent 那头等着的东西。丢掉它，MCP 的连接就关了。
#[derive(Debug)]
pub enum Pending {
    /// 交回核心的一件 Miyu 工具调用：编号和等着回话的连接。
    Mcp {
        /// 编号（总台给的，从 1 起）。
        n: u64,
        /// 等着回话的连接。
        reply: Reply,
    },
    /// 交回核心问主人的一次权限请求：问话编号、agent 那个请求的编号、它给的选项。
    Permission {
        /// 问话编号（这个会话里从 1 起）。
        n: u64,
        /// agent 的请求编号，回话时原样带上。
        id: Value,
        /// agent 给的选项。
        options: Vec<Value>,
    },
}

/// 一个活着的 ACP 会话。
pub struct Live {
    conn: Conn,
    /// ACP 的会话编号。
    pub session: String,
    /// agent 的编号。
    pub agent: String,
    /// agent 给人看的名字。
    pub title: String,
    /// agent 的能力。
    pub caps: Caps,
    /// 要登录时给人看的一句。
    pub login: String,
    wiring: Option<Wiring>,
    pending: Option<Pending>,
    /// 这一轮还没回的 `session/prompt` 的编号。
    pub prompt: Option<u64>,
    /// 这一个 `session/prompt` 跨调用记着的（[`Memory`]）。
    pub memory: Memory,
    asks: u64,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("pid", &self.conn.pid())
            .field("agent", &self.agent)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// 开会话之前、起进程时要的路由和标记：带工具的用总台那一份，不带的新建。
#[must_use]
pub fn plumbing(wiring: Option<&Wiring>) -> (Route, Arc<AtomicBool>) {
    match wiring {
        Some(wiring) => (wiring.route(), wiring.stray()),
        None => (Route::default(), Arc::default()),
    }
}

/// 开好的会话的几样，[`Live::new`] 用。
pub struct Opened {
    /// ACP 的会话编号。
    pub session: String,
    /// agent 的编号。
    pub agent: String,
    /// agent 给人看的名字。
    pub title: String,
    /// agent 的能力。
    pub caps: Caps,
    /// 要登录时给人看的一句。
    pub login: String,
}

impl Live {
    /// 把起好的进程、开好的会话、总台的登记包起来。
    #[must_use]
    pub fn new(conn: Conn, opened: Opened, wiring: Option<Wiring>) -> Live {
        Live {
            conn,
            session: opened.session,
            agent: opened.agent,
            title: opened.title,
            caps: opened.caps,
            login: opened.login,
            wiring,
            pending: None,
            prompt: None,
            memory: Memory::default(),
            asks: 0,
        }
    }

    /// 那条连接。
    #[must_use]
    pub fn conn(&self) -> &Conn {
        &self.conn
    }

    /// 进程号。
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.conn.pid()
    }

    /// 进程已经不能用了。
    #[must_use]
    pub fn closed(&self) -> bool {
        self.conn.closed()
    }

    /// 没人听的时候 agent 说过话。
    #[must_use]
    pub fn stray(&self) -> bool {
        self.conn.stray()
    }

    /// 带着工具（总台里有口令）。
    #[must_use]
    pub fn wired(&self) -> bool {
        self.wiring.is_some()
    }

    /// 下一个问话编号。
    pub fn next_ask(&mut self) -> u64 {
        self.asks += 1;
        self.asks
    }

    /// 记下挂着的那件。
    pub fn set_pending(&mut self, pending: Pending) {
        self.pending = Some(pending);
    }

    /// 取走挂着的那件。
    pub fn take_pending(&mut self) -> Option<Pending> {
        self.pending.take()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(Pending::Permission { id, .. }) = self.pending.take() {
            self.conn.reply(
                &id,
                &serde_json::json!({"outcome": {"outcome": "cancelled"}}),
            );
        }
        drop(self.wiring.take());
    }
}
