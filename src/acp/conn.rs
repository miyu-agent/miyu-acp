//! 一个 agent 进程和它那一条 ACP 连接。
//!
//! - 标准输入：一个写的线程，按先后写进来的每一行；agent 没读就退的，写不进去、记一行日志，线程结束，不卡调用的人。
//! - 标准输出：一个读的线程，一行一条：
//!   - 回应：找发请求时登记的等候（[`Conn::request`] 的是单独一条通道；[`Conn::request_routed`] 的送进这一轮的信号，排在它前面
//!     的 `session/update` 后头——agent 照协议先发完更新再回 `session/prompt`，同一条通道保住了先后）。
//!   - agent 的请求、`session/update` 通知：送给这一轮在听的那一次调用（[`Conn::route`]）。没人听的时候：要权限的回
//!     `cancelled`、别的请求回 `-32601`；「说话」的更新（[`crate::signal::is_speech`]）记下「乱说话了」（[`Conn::stray`]），
//!     状态类的更新不理。
//!   - 读到头、读不下去、一行太长：先记下 `closed`，再送最后一条，线程结束；等着回应的都收到「连接没了」。
//! - 收掉（丢掉这个值时）：收进程组（[`crate::process::terminate`]）、关标准输入、等写的线程。
//!
//! 标准错误接到扩展自己的标准错误（核心记进包的日志）。

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{METHOD_NOT_FOUND, RpcError};
use crate::install::Launch;
use crate::process::{self, Line};
use crate::signal::{Route, Signal, is_speech};

/// 一次请求没成的原因。
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// agent 回了出错。
    Rpc(RpcError),
    /// 连接没了（agent 退了、读不下去）。
    Closed,
    /// 到点没回。
    Timeout,
    /// 核心取消了这一次。
    Cancelled,
}

/// 等一个回应的。
enum Waiter {
    /// 单独一条通道。
    Direct(Sender<Result<Value, RpcError>>),
    /// 送进这一轮的信号。
    Routed,
}

type Waiters = Arc<Mutex<HashMap<u64, Waiter>>>;

/// 一个 agent 进程。
pub struct Conn {
    child: Child,
    input: Option<Sender<String>>,
    writer: Option<JoinHandle<()>>,
    route: Route,
    closed: Arc<AtomicBool>,
    stray: Arc<AtomicBool>,
    waiters: Waiters,
    next_id: AtomicU64,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl Conn {
    /// 照 `launch` 起 agent（工作目录 `cwd`，自成进程组）。`route`、`stray` 和总台共用（带工具的会话）或者新的。
    ///
    /// # Errors
    ///
    /// 起不来：原样交。
    pub fn start(
        launch: &Launch,
        cwd: &Path,
        route: Route,
        stray: Arc<AtomicBool>,
    ) -> std::io::Result<Conn> {
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .envs(launch.env.iter().map(|(key, value)| (key, value)))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = process::spawn(command)?;
        let pid = child.id();
        let closed = Arc::new(AtomicBool::new(false));
        let waiters = Waiters::default();
        let (input, lines) = mpsc::channel();
        let writer = child
            .stdin
            .take()
            .map(|stdin| write_input(stdin, lines, pid));
        match child.stdout.take() {
            Some(stdout) => {
                let reader = Reader {
                    route: Arc::clone(&route),
                    closed: Arc::clone(&closed),
                    stray: Arc::clone(&stray),
                    waiters: Arc::clone(&waiters),
                    input: input.clone(),
                    pid,
                };
                std::thread::spawn(move || reader.run(stdout));
            }
            None => closed.store(true, Ordering::SeqCst),
        }
        Ok(Conn {
            child,
            input: Some(input),
            writer,
            route,
            closed,
            stray,
            waiters,
            next_id: AtomicU64::new(0),
        })
    }

    /// 进程号。
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// 写一条（加换行）。写的线程已经不在了的交 `false`。
    fn send(&self, message: &Value) -> bool {
        let mut line = message.to_string();
        line.push('\n');
        self.input
            .as_ref()
            .is_some_and(|input| input.send(line).is_ok())
    }

    /// 发一个通知。
    pub fn notify(&self, method: &str, params: &Value) -> bool {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// 回 agent 的请求 `id`。
    pub fn reply(&self, id: &Value, result: &Value) -> bool {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    /// 拒绝 agent 的请求 `id`。
    pub fn refuse(&self, id: &Value, code: i64, message: &str) -> bool {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
    }

    /// 发请求，登记 `waiter`，交编号；写不进去的交 `None`（登记撤掉）。
    fn start_request(&self, method: &str, params: &Value, waiter: Waiter) -> Option<u64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, waiter);
        let sent = !self.closed()
            && self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        if !sent {
            self.waiters
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
        }
        sent.then_some(id)
    }

    /// 发请求、等回应，至多 `wait`；等的时候 `signals` 里来了取消就不等了（别的信号这时不该有，丢掉）。
    ///
    /// # Errors
    ///
    /// agent 回了出错、连接没了、到点、被取消：[`Failure`]。
    pub fn request(
        &self,
        method: &str,
        params: &Value,
        wait: Duration,
        signals: &Receiver<Signal>,
    ) -> Result<Value, Failure> {
        let (sender, receiver) = mpsc::channel();
        let id = self
            .start_request(method, params, Waiter::Direct(sender))
            .ok_or(Failure::Closed)?;
        let deadline = Instant::now() + wait;
        let forget = || {
            self.waiters
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
        };
        loop {
            while let Ok(signal) = signals.try_recv() {
                if matches!(signal, Signal::Cancel) {
                    forget();
                    return Err(Failure::Cancelled);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                forget();
                return Err(Failure::Timeout);
            }
            match receiver.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(Ok(result)) => return Ok(result),
                Ok(Err(error)) => return Err(Failure::Rpc(error)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(Failure::Closed),
            }
        }
    }

    /// 发请求，回应作为 [`Signal::Response`] 送进这一轮的信号（先接上路由再调）。交编号；写不进去的交 `None`。
    pub fn request_routed(&self, method: &str, params: &Value) -> Option<u64> {
        self.start_request(method, params, Waiter::Routed)
    }

    /// 之后的信号送给 `to`（这一轮在听的那一次调用）。
    pub fn route(&self, to: Sender<Signal>) {
        *self.route.lock().unwrap_or_else(PoisonError::into_inner) = Some(to);
    }

    /// 这一轮完了，没人听了。
    pub fn unroute(&self) {
        *self.route.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// agent 的输出读到头了（进程退了）或者读不下去了：这个进程不能再用。
    #[must_use]
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// 没人听的时候 agent 说过话（[`Signal::is_speech`]）。
    #[must_use]
    pub fn stray(&self) -> bool {
        self.stray.load(Ordering::SeqCst)
    }

    /// 记下「乱说话了」。
    pub fn mark_stray(&self) {
        self.stray.store(true, Ordering::SeqCst);
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let pid = self.child.id();
        process::terminate(&mut self.child, process::GRACE);
        drop(self.input.take());
        if let Some(writer) = self.writer.take()
            && writer.join().is_err()
        {
            eprintln!("agent (pid {pid}): the thread writing its input panicked");
        }
    }
}

/// 在单独的线程里把 `lines` 按先后写进 agent 的标准输入；`lines` 关了就关上标准输入。
fn write_input(mut stdin: ChildStdin, lines: Receiver<String>, pid: u32) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in lines {
            if let Err(error) = stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush())
            {
                eprintln!("agent (pid {pid}) did not take its input: {error}");
                return;
            }
        }
    })
}

/// 读的线程要的。
struct Reader {
    route: Route,
    closed: Arc<AtomicBool>,
    stray: Arc<AtomicBool>,
    waiters: Waiters,
    input: Sender<String>,
    pid: u32,
}

impl Reader {
    fn run(self, stdout: ChildStdout) {
        let mut reader = BufReader::new(stdout);
        loop {
            let last = match process::next_line(&mut reader, process::LINE_LIMIT) {
                Ok(Some(Line::Text(text))) => {
                    self.line(&text);
                    continue;
                }
                Ok(Some(Line::TooLong)) => Signal::TooLong,
                Ok(None) => Signal::Closed,
                Err(error) => Signal::ReadFailed(error.to_string()),
            };
            self.closed.store(true, Ordering::SeqCst);
            // 等着回应的都不会再等来了：丢掉它们的发送头，那一头读到「断了」。
            self.waiters
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
            self.deliver(last);
            return;
        }
    }

    /// 送给在听的那一次；没人听的交回这一条。
    fn deliver(&self, signal: Signal) -> Option<Signal> {
        let route = self.route.lock().unwrap_or_else(PoisonError::into_inner);
        match route.as_ref() {
            // 那一次已经结束、没人收了：照样往下读。
            Some(to) => {
                to.send(signal).ok();
                None
            }
            None => Some(signal),
        }
    }

    fn reply(&self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.input.send(line).ok();
    }

    fn line(&self, text: &str) {
        let Ok(message) = serde_json::from_str::<Value>(text) else {
            eprintln!(
                "agent (pid {}) wrote a line that is not JSON: {text:.200}",
                self.pid
            );
            return;
        };
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        match (message.get("method").and_then(Value::as_str), id) {
            (Some(method), Some(id)) => {
                let signal = Signal::Request {
                    id: id.clone(),
                    method: method.to_string(),
                    params: message.get("params").cloned().unwrap_or(Value::Null),
                };
                if self.deliver(signal).is_some() {
                    self.idle_request(&id, method);
                }
            }
            (Some("session/update"), None) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                if let Some(Signal::Update(params)) = self.deliver(Signal::Update(params))
                    && is_speech(&params)
                {
                    self.stray.store(true, Ordering::SeqCst);
                    eprintln!("agent (pid {}) spoke while idle: {text:.200}", self.pid);
                }
            }
            (Some(_), None) => {}
            (None, Some(id)) => self.response(&id, &message),
            (None, None) => eprintln!("agent (pid {}) wrote an odd message: {text:.200}", self.pid),
        }
    }

    /// 没人听的时候来的请求：要权限的回 `cancelled`（记下乱说话了），别的回「没有这个方法」。
    fn idle_request(&self, id: &Value, method: &str) {
        if method == "session/request_permission" {
            self.stray.store(true, Ordering::SeqCst);
            eprintln!(
                "agent (pid {}) asked for permission while idle; answering cancelled",
                self.pid
            );
            self.reply(&json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": {"outcome": "cancelled"}}}));
        } else {
            self.reply(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": METHOD_NOT_FOUND, "message": format!("method not found: {method}")}}));
        }
    }

    fn response(&self, id: &Value, message: &Value) {
        let Some(number) = id.as_u64() else {
            eprintln!(
                "agent (pid {}) answered an id we never sent: {id}",
                self.pid
            );
            return;
        };
        let outcome = match message.get("error") {
            Some(error) => Err(RpcError::read(error)),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let waiter = self
            .waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&number);
        match waiter {
            Some(Waiter::Direct(sender)) => {
                sender.send(outcome).ok();
            }
            Some(Waiter::Routed) => {
                if self
                    .deliver(Signal::Response {
                        id: number,
                        outcome,
                    })
                    .is_some()
                {
                    eprintln!(
                        "agent (pid {}) answered request {number} after its call ended",
                        self.pid
                    );
                }
            }
            None => eprintln!(
                "agent (pid {}) answered request {number}, which nobody waits for",
                self.pid
            ),
        }
    }
}
