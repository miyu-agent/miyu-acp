//! `serve`：和核心之间标准输入输出上的 JSON-RPC 2.0，一行一条（核心 `docs/zh/dev/extensions.md`「握手和配置」「退出、崩了」，
//! `docs/zh/reference/extensions.md` 的 `driver.*`、`extension.config`）。照 `miyu-antigravity` 的 `rpc` 搬。
//!
//! 先发 `hello`、读回应里的 `config`、`language`；之后读核心的请求和通知：`driver.call`（一个调用一个线程，可以几个同时跑）、
//! `driver.cancel`、`driver.models`、`extension.config`；不认识的请求回 `-32601`。写出去的每一行加锁、整行写、刷新。标准错误只写
//! 日志。握手被拒、配置读不懂的退出码 1（核心不再拉起）；标准输入读到头是请退出：收掉还在跑的和留着的 agent，退出码 0。
//!
//! 活着的 ACP 会话记在一张表里（[`Registry`]），一个巡检线程按 `acp.keep_seconds`、`acp.tool_wait_seconds` 收闲置到点的
//! （[`Sweeper`]）。握手以后起给 MCP 服务用的套接字（[`Tools`]），退出时删掉；起之前先删上一个被硬杀的扩展进程留下的套接字文件。

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::JoinHandle;

use serde_json::{Map, Value, json};

use crate::call::{self, Answer, Signal};
use crate::config::Config;
use crate::hub::Tools;
use crate::pool::{self, Registry, Sweeper};
use crate::{bridge, catalog, models, workspace};

/// 往标准输出写的一头：几个线程共用，一行一锁。
#[derive(Clone)]
pub struct Out {
    sink: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Out {
    /// 写到标准输出。
    #[must_use]
    pub fn stdout() -> Out {
        Out {
            sink: Arc::new(Mutex::new(Box::new(std::io::stdout()))),
        }
    }

    /// 写一条：整行写完、刷新。写不出去（核心没了）记一行日志，读那头会读到头、照常退出。
    pub fn send(&self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = sink.write_all(line.as_bytes()).and_then(|()| sink.flush()) {
            eprintln!("writing to the core failed: {error}");
        }
    }

    /// 回答请求 `id`。
    pub fn reply(&self, id: &Value, result: &Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    /// 拒绝请求 `id`：`code` 是 JSON-RPC 的码，`reason` 是 `data.reason`。
    pub fn refuse(&self, id: &Value, code: i64, reason: &str, message: &str) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message, "data": {"reason": reason}}
        }));
    }

    /// 发通知。
    pub fn notify(&self, method: &str, params: &Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

/// 握手请求的编号。
const HELLO_ID: &str = "hello";

/// 跑 `serve`：读 `input`、写 `out`，直到读到头或者配置读不懂。
#[must_use]
pub fn serve(input: impl BufRead, out: &Out) -> ExitCode {
    let mut lines = input.lines();
    out.send(&json!({
        "jsonrpc": "2.0",
        "id": HELLO_ID,
        "method": "hello",
        "params": {"protocol": [1, 1], "head": {"kind": "miyu-acp", "version": env!("CARGO_PKG_VERSION")}}
    }));
    let mut config = Config::default();
    loop {
        let Some(line) = next(&mut lines) else {
            return ExitCode::SUCCESS;
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            eprintln!("the core sent a line that is not JSON before the handshake finished");
            continue;
        };
        if message.get("id").and_then(Value::as_str) != Some(HELLO_ID)
            || message.get("method").is_some()
        {
            eprintln!("ignoring a message that came before the handshake answer: {line}");
            continue;
        }
        if let Some(error) = message.get("error") {
            eprintln!("the core refused the handshake: {error}");
            return ExitCode::from(1);
        }
        let keys = message
            .pointer("/result/config")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Err(error) = config.apply(&keys) {
            eprintln!("bad configuration: {error}");
            return ExitCode::from(1);
        }
        if let Some(language) = message.pointer("/result/language").and_then(Value::as_str) {
            config.language = language.to_string();
        }
        break;
    }

    sweep_sockets();
    let config = Arc::new(RwLock::new(config));
    let registry = Arc::new(Registry::default());
    let tools = Arc::new(Tools::start());
    let sweeper = Sweeper::start(
        Arc::clone(&registry),
        Arc::clone(&config),
        pool::sweep_every(),
    );
    let mut server = Server {
        out: out.clone(),
        config,
        registry,
        tools,
        sweeper,
        calls: Arc::new(Mutex::new(HashMap::new())),
        threads: Vec::new(),
    };
    let code = loop {
        let Some(line) = next(&mut lines) else {
            break ExitCode::SUCCESS;
        };
        if let Err(error) = server.handle(&line) {
            eprintln!("bad configuration: {error}");
            break ExitCode::from(1);
        }
    };
    server.shut_down();
    code
}

/// 删掉上一个被硬杀的扩展进程留下的套接字文件（`acp-<进程号>.sock`，那个进程已经不在了的）。出的错只记日志。
fn sweep_sockets() {
    let Some(dir) = bridge::socket_path()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| {
            name.strip_prefix("acp-")?
                .strip_suffix(".sock")?
                .parse::<i32>()
                .ok()
        }) else {
            continue;
        };
        let gone = pid != i32::try_from(std::process::id()).unwrap_or(-1)
            && nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)
                == Err(nix::errno::Errno::ESRCH);
        if gone {
            eprintln!("cleaning up: removing {}", entry.path().display());
            if let Err(error) = workspace::remove(&entry.path()) {
                eprintln!("cleaning up: {error}");
            }
        }
    }
}

/// 下一行；读到头、读不下去是 `None`。
fn next(lines: &mut impl Iterator<Item = std::io::Result<String>>) -> Option<String> {
    match lines.next()? {
        Ok(line) => Some(line),
        Err(error) => {
            eprintln!("reading from the core failed: {error}");
            None
        }
    }
}

/// 握手以后的状态。
struct Server {
    out: Out,
    config: Arc<RwLock<Config>>,
    /// 活着的 ACP 会话。
    registry: Arc<Registry>,
    /// 工具这一头：总台和给 MCP 服务用的套接字（起不来的，带工具的调用回出错）。
    tools: Arc<Tools>,
    /// 收闲置到点的 agent 的巡检线程。
    sweeper: Sweeper,
    /// 在跑的 `driver.call`：调用编号 → 送信号的一头。
    calls: Arc<Mutex<HashMap<String, Sender<Signal>>>>,
    threads: Vec<JoinHandle<()>>,
}

impl Server {
    /// 处理一行。
    ///
    /// # Errors
    ///
    /// `extension.config` 推来的配置读不懂：原因。
    fn handle(&mut self, line: &str) -> Result<(), String> {
        self.threads.retain(|thread| !thread.is_finished());
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                self.out.refuse(
                    &Value::Null,
                    -32700,
                    "parse_error",
                    &format!("not JSON: {error}"),
                );
                return Ok(());
            }
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // 回应：扩展只向核心发 `driver.approve`（核心施工 ACP-8），照编号交给那一次调用；别的不用对。
            self.approved(&message);
            return Ok(());
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method, message.get("id")) {
            ("driver.call", Some(id)) => self.start_call(id.clone(), params),
            ("driver.models", Some(id)) => self.list_models(id.clone()),
            ("driver.cancel", _) => self.cancel(&params),
            ("extension.config", _) => {
                let keys = params
                    .get("keys")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_else(Map::new);
                self.config
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .apply(&keys)?;
            }
            (_, Some(id)) => self.out.refuse(
                id,
                -32601,
                "unknown_method",
                &format!("no such method: {method}"),
            ),
            (_, None) => {}
        }
        Ok(())
    }

    fn start_call(&mut self, id: Value, params: Value) {
        let Some(call) = params
            .get("call")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            self.out
                .refuse(&id, -32602, "bad_params", "params.call is missing");
            return;
        };
        let (sender, receiver) = mpsc::channel();
        {
            let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
            if calls.contains_key(&call) {
                self.out.refuse(
                    &id,
                    -32602,
                    "bad_params",
                    &format!("call {call} is already running"),
                );
                return;
            }
            calls.insert(call.clone(), sender.clone());
        }
        let config = self
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (out, calls) = (self.out.clone(), Arc::clone(&self.calls));
        let registry = Arc::clone(&self.registry);
        let tools = Arc::clone(&self.tools);
        self.threads.push(std::thread::spawn(move || {
            let answer = call::run(
                &out, &config, &registry, &tools, &params, &receiver, &sender,
            );
            calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&call);
            match answer {
                Answer::Result(result) => out.reply(&id, &result),
                Answer::BadParams(message) => out.refuse(&id, -32602, "bad_params", &message),
            }
        }));
    }

    /// 核心回了 `driver.approve`（编号 `approve:<调用编号>:<问话编号>`，[`call::approve_id`]）：交给那一次调用；已经结束了的不理。
    fn approved(&self, message: &Value) {
        let Some((call, n)) = message
            .get("id")
            .and_then(Value::as_str)
            .and_then(call::approve_of)
        else {
            return;
        };
        let outcome = match message.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        if let Some(sender) = self
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(call)
        {
            sender.send(Signal::Approved { n, outcome }).ok();
        }
    }

    fn cancel(&self, params: &Value) {
        let Some(call) = params.get("call").and_then(Value::as_str) else {
            return;
        };
        // 不认识的、已经结束的调用不理（参考文档：核心之后不理这一次的任何东西）。
        if let Some(sender) = self
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(call)
        {
            sender.send(Signal::Cancel).ok();
        }
    }

    fn list_models(&mut self, id: Value) {
        let config = self
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let out = self.out.clone();
        self.threads.push(std::thread::spawn(move || {
            let listed = workspace::state_dir()
                .and_then(|state| workspace::cache_dir().map(|cache| (state, cache)))
                .map_err(|error| format!("no state folder: {error}"))
                .and_then(|(state, cache)| {
                    models::list(&config, &state, || catalog::load_now(&cache))
                });
            match listed {
                Ok(answer) => out.reply(&id, &answer),
                Err(message) => out.refuse(&id, -32603, "internal_error", &message),
            }
        }));
    }

    /// 退出前：让在跑的调用都收掉自己的 agent，等它们结束；停下巡检；收掉留着的 agent。
    fn shut_down(self) {
        for sender in self
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
        {
            sender.send(Signal::Cancel).ok();
        }
        for thread in self.threads {
            if thread.join().is_err() {
                eprintln!("a call thread panicked while shutting down");
            }
        }
        self.sweeper.stop();
        self.registry.drain();
        // 收完 agent 以后才关套接字（删文件）。
        drop(self.tools);
    }
}
