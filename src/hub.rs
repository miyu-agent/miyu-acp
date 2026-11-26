//! 总台：扩展进程里接 MCP 服务（`miyu-acp mcp`）经套接字转来的 `tools/list`、`tools/call` 的地方。实现 [`bridge::Handler`]。
//! 照 `miyu-antigravity` 的 `hub` 搬。
//!
//! 一张表：口令 → 这个 ACP 会话的 { 工具表、路由（读 agent 输出的线程用的那一份，[`Route`]）、「乱说话了」的标记、下一个调用
//! 编号、要去掉的参数 }。
//!
//! - `list`：交那张工具表；口令不认识是 `None`。
//! - `call`：编号加一，往路由现在指着的那一次 `driver.call` 送 [`Signal::ToolCall`]（连接 [`Reply`] 跟着去，由那一次调用存起来）。
//!   路由是空的（没有哪一次调用在听）：记下「乱说话了」、丢掉 `reply`（连接关掉）。**不在 `call` 里等**。
//!
//! 拿锁的先后：先拿表的锁、拿到这一项（[`Arc`]）就放开；再单独拿路由的锁。两把锁不嵌套，读输出的线程只拿路由的锁，所以不会成环。
//!
//! 一个会话登记以后得到一份 [`Wiring`]，交给 [`crate::live::Live`]；`Live` 丢掉时 `Wiring` 跟着丢，口令就从表里拿掉。
//! [`Tools`] 是扩展进程里这一头的全部：总台加套接字服务端；服务端起不来（路径太长之类）不让扩展退出，带工具的调用回出错。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

use crate::bridge::{self, Handler, Reply, Server, Tool};
use crate::schema;
use crate::signal::{Route, Signal};

/// MCP 服务转来的一件工具调用：编号（这个会话里从 1 起）、工具名和 `arguments`（照 MCP 收到的原样）、等着回话的连接。
#[derive(Debug)]
pub struct McpCall {
    /// 调用编号。
    pub n: u64,
    /// 工具名，MCP 收到的原样（不带 agent 加的前缀）。
    pub name: String,
    /// `arguments`（已经去掉了为这个 agent 多声明的参数，[`schema::without_extras`]）。
    pub arguments: Value,
    /// 等着回话的连接；丢掉就关。
    pub reply: Reply,
}

/// 表里的一项。
struct Entry {
    tools: Vec<Tool>,
    route: Route,
    stray: Arc<AtomicBool>,
    /// 给这个 agent 的工具多声明了 [`schema::AGY_EXTRAS`]：转给 Miyu 之前去掉。
    extras: bool,
    next: AtomicU64,
}

/// 总台。
#[derive(Default)]
pub struct Hub {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}

impl Hub {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<Entry>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn entry(&self, token: &str) -> Option<Arc<Entry>> {
        self.lock().get(token).cloned()
    }

    /// 登记一个就要开的 ACP 会话：新口令、它的工具表 `tools`、新的路由和标记；`socket` 是套接字的路径，原样记在 [`Wiring`] 里。
    /// `extras` 是工具表里多声明了 [`schema::AGY_EXTRAS`]。
    ///
    /// # Errors
    ///
    /// 口令生成不了（读不了 `/dev/urandom`）：原样交。
    pub fn register(
        self: &Arc<Hub>,
        tools: Vec<Tool>,
        extras: bool,
        socket: &Path,
    ) -> std::io::Result<Wiring> {
        let token = bridge::new_token()?;
        let route = Route::default();
        let stray = Arc::new(AtomicBool::new(false));
        let entry = Arc::new(Entry {
            tools,
            route: Arc::clone(&route),
            stray: Arc::clone(&stray),
            extras,
            next: AtomicU64::new(0),
        });
        self.lock().insert(token.clone(), entry);
        Ok(Wiring {
            hub: Arc::clone(self),
            token,
            socket: socket.to_path_buf(),
            route,
            stray,
        })
    }

    /// 表里几项。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 表是空的。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

impl Handler for Hub {
    fn list(&self, token: &str) -> Option<Vec<Tool>> {
        self.entry(token).map(|entry| entry.tools.clone())
    }

    fn call(&self, token: &str, name: &str, arguments: Value, reply: Reply) {
        let Some(entry) = self.entry(token) else {
            if let Err(error) = reply.refuse("unknown token") {
                eprintln!("refusing a tool call with an unknown token failed: {error}");
            }
            return;
        };
        if !entry.tools.iter().any(|tool| tool.name == name) {
            let text = format!("Tool {name} is not available in this conversation.");
            if let Err(error) = reply.send(&[json!({"type": "text", "text": text})], true) {
                eprintln!("answering a call to the unknown tool {name} failed: {error}");
            }
            return;
        }
        let arguments = if entry.extras {
            schema::without_extras(arguments)
        } else {
            arguments
        };
        let n = entry.next.fetch_add(1, Ordering::SeqCst) + 1;
        let call = McpCall {
            n,
            name: name.to_string(),
            arguments,
            reply,
        };
        let route = entry.route.lock().unwrap_or_else(PoisonError::into_inner);
        let undelivered = match route.as_ref() {
            Some(to) => to.send(Signal::ToolCall(call)).is_err(),
            None => true,
        };
        drop(route);
        if undelivered {
            // 没人听（或者听的那次调用刚结束）：连接随 `call` 丢掉而关掉。
            entry.stray.store(true, Ordering::SeqCst);
            eprintln!("the agent called tool {name} (#{n}) while no driver.call was listening");
        }
    }
}

/// 一个会话在总台里的登记：口令、套接字路径、它的路由和标记。丢掉就把口令从表里拿掉。
pub struct Wiring {
    hub: Arc<Hub>,
    token: String,
    socket: PathBuf,
    route: Route,
    stray: Arc<AtomicBool>,
}

impl std::fmt::Debug for Wiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wiring")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

impl Wiring {
    /// 口令（写进 MCP 服务的环境变量 [`bridge::ENV_TOKEN`]）。
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// 套接字路径（写进 [`bridge::ENV_BRIDGE`]）。
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// 读输出的线程用的路由（和总台那一份是同一个）。
    #[must_use]
    pub fn route(&self) -> Route {
        Arc::clone(&self.route)
    }

    /// 「乱说话了」的标记（和总台那一份是同一个）。
    #[must_use]
    pub fn stray(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stray)
    }
}

impl Drop for Wiring {
    fn drop(&mut self) {
        let removed = self.hub.lock().remove(&self.token);
        drop(removed);
    }
}

/// 扩展进程里工具这一头的全部：总台和套接字服务端。丢掉它就关服务端、删套接字文件。
pub struct Tools {
    hub: Arc<Hub>,
    server: Result<Server, String>,
}

impl Tools {
    /// 起服务端（路径 [`bridge::socket_path`]）。起不来的不退出：记一行，之后 [`Tools::wire`] 都交出错。
    #[must_use]
    pub fn start() -> Tools {
        let hub = Arc::new(Hub::default());
        let server = bridge::socket_path()
            .and_then(|path| Server::start(&path, Arc::clone(&hub)))
            .map_err(|error| error.to_string());
        if let Err(reason) = &server {
            eprintln!(
                "the socket for the agents' tool server could not be set up ({reason}); calls with tools will fail"
            );
        }
        Tools { hub, server }
    }

    /// 登记一个就要开的会话，工具表 `tools`（`extras` 见 [`Hub::register`]）。
    ///
    /// # Errors
    ///
    /// 服务端没起来、口令生成不了：给人看的英文一句（带原因）。
    pub fn wire(&self, tools: Vec<Tool>, extras: bool) -> Result<Wiring, String> {
        let server = self.server.as_ref().map_err(|reason| {
            format!(
                "the socket that connects the agent's tool server to Miyu could not be set up ({reason}), so tools cannot be used; check MIYU_RUNTIME_DIR or MIYU_STATE_DIR"
            )
        })?;
        self.hub
            .register(tools, extras, server.path())
            .map_err(|error| format!("could not make a token for the agent's tool server: {error}"))
    }

    /// 总台里现在登记着几个会话。
    #[must_use]
    pub fn registered(&self) -> usize {
        self.hub.len()
    }
}

#[cfg(test)]
mod tests;
