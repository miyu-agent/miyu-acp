//! 集成测试扮核心：起 `miyu-acp serve`，经管道读写一行一条的 JSON-RPC。agent 是设置里手写的一个（`acp.agents`），命令是
//! `python3 tests/fake_acp.py`，演法照 `FAKE_MODE`。扩展的 `HOME`、状态、缓存目录都是临时目录，碰不到真的数据。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 等一行至多多久。
pub const WAIT: Duration = Duration::from_secs(30);

/// 假 agent 的脚本。
pub fn fake() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_acp.py")
}

/// 一个扮核心的测试：起着的扩展和它的几个目录。
pub struct Core {
    child: Child,
    root: PathBuf,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    /// 扩展的状态目录。
    pub state: PathBuf,
    /// 扩展的缓存目录。
    pub cache: PathBuf,
    /// 假 agent 写记录的目录。
    pub record: PathBuf,
    runtime: PathBuf,
}

/// 一次调用收到的：增量（照先后）和回应。
pub struct Collected {
    pub deltas: Vec<Value>,
    pub response: Value,
}

impl Collected {
    /// 正文、思考块的字接起来（不分块）。
    pub fn text(&self) -> String {
        self.deltas
            .iter()
            .filter_map(|delta| delta.pointer("/text/text").and_then(Value::as_str))
            .collect()
    }

    /// 回答（`result`）。
    pub fn result(&self) -> &Value {
        &self.response["result"]
    }

    /// 第 `kind` 种块开头的那几条 `start`（`"text"`、`"reasoning"`、`"tool_call"`、`"hosted_tool"`）。
    pub fn starts(&self, kind: &str) -> Vec<&Value> {
        self.deltas
            .iter()
            .filter_map(|delta| delta.get("start"))
            .filter(|start| start["kind"] == kind || start["kind"].get(kind).is_some())
            .collect()
    }
}

/// 给测试新建一个空目录。
pub fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 一份最小的统一请求：系统提示词 `system`，人说一句 `text`，工具 `tools`。
pub fn request(system: &str, text: &str, tools: &Value) -> Value {
    json!({
        "tools": tools,
        "system": system,
        "messages": [{"role": "user", "blocks": [{"type": "text", "text": text}]}],
        "stable": 0
    })
}

/// 一件工具的规格（统一请求里的写法）。
pub fn tool(name: &str) -> Value {
    json!({"name": name, "description": format!("Does {name}."), "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}})
}

/// 一段内容的指纹：SHA-256 的前 16 位十六进制。
pub fn hash(value: &Value) -> String {
    let digest = Sha256::digest(value.to_string().as_bytes());
    hex(&digest[..8])
}

/// 字节写成小写十六进制。
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        write!(text, "{byte:02x}").unwrap();
        text
    })
}

/// 请求的 `fingerprint`。
pub fn fingerprint(request: &Value) -> Value {
    let messages: Vec<String> = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(hash)
        .collect();
    json!({"tools": hash(&request["tools"]), "system": hash(&request["system"]), "messages": messages})
}

/// 在 `request` 后面接上一条 assistant（`blocks`）和一条消息 `next`。
pub fn then(request: &Value, blocks: &Value, next: Value) -> Value {
    let mut more = request.clone();
    let messages = more["messages"].as_array_mut().unwrap();
    messages.push(json!({"role": "assistant", "blocks": blocks}));
    messages.push(next);
    more
}

/// 人说的一条。
pub fn user(text: &str) -> Value {
    json!({"role": "user", "blocks": [{"type": "text", "text": text}]})
}

/// 回答里交回的那一块工具调用，写成下一次请求 assistant 那条的 `blocks`（核心的写法：名字、参数、私有数据、编号）。
pub fn handed_blocks(collected: &Collected, call_id: &str) -> Value {
    let start = collected.starts("tool_call")[0];
    let index = &start["index"];
    let name = start["kind"]["tool_call"]["name"].clone();
    let args: String = collected
        .deltas
        .iter()
        .filter(|delta| delta.pointer("/text/index") == Some(index))
        .filter_map(|delta| delta.pointer("/text/text").and_then(Value::as_str))
        .collect();
    let private = collected
        .deltas
        .iter()
        .find_map(|delta| delta.get("private").filter(|p| &p["index"] == index))
        .unwrap()["data"]
        .clone();
    json!([{"type": "tool_call", "call_id": call_id, "name": name, "args": args, "private": {"driver": "acp/acp", "data": private}}])
}

/// 工具结果那一条。
pub fn tool_result(call_id: &str, text: &str) -> Value {
    json!({"role": "tool", "call_id": call_id, "blocks": [{"type": "text", "text": text}]})
}

impl Core {
    /// 起扩展、握手：手写的 agent 叫 `agent`，假 agent 照 `mode` 演；`config` 是另加的配置，`env` 是扩展另带的环境变量。
    pub fn start(
        name: &str,
        agent: &str,
        mode: &str,
        config: &Value,
        env: &[(&str, &str)],
    ) -> Core {
        let root = scratch(name);
        let [home, state, cache, record] = ["home", "state", "cache", "record"].map(|part| {
            let dir = root.join(part);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        });
        let serial = root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .rsplit('-')
            .next()
            .unwrap()
            .to_string();
        let runtime = Path::new("/tmp").join(format!("acpt-{}-{serial}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_miyu-acp"))
            .arg("serve")
            .env("HOME", &home)
            .env("MIYU_STATE_DIR", &state)
            .env("MIYU_CACHE_DIR", &cache)
            .env("MIYU_RUNTIME_DIR", &runtime)
            .env("FAKE_RECORD", &record)
            .env(
                "MIYU_ACP_REGISTRY_URL",
                format!("file://{}/registry.json", root.display()),
            )
            .envs(env.iter().copied())
            .current_dir(&state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        let stdin = child.stdin.take();
        let mut core = Core {
            child,
            root,
            stdin,
            lines,
            state,
            cache,
            record,
            runtime,
        };
        let mut keys = json!({"acp.agents": [{
            "name": agent,
            "command": format!("FAKE_MODE='{mode}' python3 '{}'", fake().display())
        }]});
        for (key, value) in config.as_object().unwrap() {
            keys[key] = value.clone();
        }
        let hello = core.recv();
        assert_eq!(hello["method"], "hello", "{hello}");
        let id = hello["id"].clone();
        core.send(&json!({"jsonrpc": "2.0", "id": id, "result": {"protocol": 1, "language": "en", "config": keys}}));
        core
    }

    /// 根目录（登记表 `registry.json` 放这里）。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 写一条。
    pub fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// 读下一条。
    pub fn recv(&self) -> Value {
        match self.lines.recv_timeout(WAIT) {
            Ok(line) => serde_json::from_str(&line).unwrap(),
            Err(RecvTimeoutError::Timeout) => panic!("{} 秒没等来一行", WAIT.as_secs()),
            Err(RecvTimeoutError::Disconnected) => panic!("扩展的标准输出关了"),
        }
    }

    /// 发一次 `driver.call`：模型 `model`，带上 `extra` 里的格（`key`、`blobs`）。
    pub fn call_with(&mut self, id: &str, call: &str, request: &Value, model: &str, extra: &Value) {
        let mut params = json!({
            "call": call,
            "driver": "acp/acp",
            "request": request,
            "fingerprint": fingerprint(request),
            "params": {"model": model}
        });
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "driver.call", "params": params}));
    }

    /// 发一次调用并收齐（模型是 `fake`）。
    pub fn ask(&mut self, id: &str, request: &Value) -> Collected {
        self.call_with(id, id, request, "fake", &json!({}));
        self.collect(id, id)
    }

    /// 一直读到请求 `id` 回应了，交增量和回应。
    pub fn collect(&self, id: &str, call: &str) -> Collected {
        let mut deltas = Vec::new();
        loop {
            let message = self.recv();
            if message["method"] == "driver.delta" {
                assert_eq!(message["params"]["call"], call, "{message}");
                deltas.extend(
                    message["params"]["deltas"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .cloned(),
                );
            } else if message["id"] == id {
                return Collected {
                    deltas,
                    response: message,
                };
            } else {
                panic!("没想到的一条：{message}");
            }
        }
    }

    /// 发 `driver.models`、等回应。
    pub fn models(&mut self) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": "m", "method": "driver.models", "params": {"driver": "acp/acp"}}));
        let message = self.recv();
        assert_eq!(message["id"], "m", "{message}");
        message
    }

    /// 假 agent 的记录：进程号 → 一行一条。
    pub fn records(&self) -> HashMap<i64, Vec<Value>> {
        std::fs::read_dir(&self.record)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .map(|path| {
                let pid = path.file_stem().unwrap().to_string_lossy().parse().unwrap();
                let lines = std::fs::read_to_string(&path)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                (pid, lines)
            })
            .collect()
    }

    /// 所有假 agent 收到的方法是 `method` 的请求的参数（照进程号、先后）。
    pub fn got(&self, method: &str) -> Vec<Value> {
        let mut records: Vec<_> = self.records().into_iter().collect();
        records.sort_by_key(|(pid, _)| *pid);
        records
            .into_iter()
            .flat_map(|(_, lines)| lines)
            .filter_map(|line| {
                line.get("got")
                    .filter(|got| got["method"] == method)
                    .map(|got| got["params"].clone())
            })
            .collect()
    }

    /// 记录里带 `key` 的那几行的值。
    pub fn noted(&self, key: &str) -> Vec<Value> {
        self.records()
            .into_values()
            .flatten()
            .filter_map(|line| line.get(key).cloned())
            .collect()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + WAIT;
        while self.child.try_wait().ok().flatten().is_none() {
            if Instant::now() >= deadline {
                self.child.kill().ok();
                self.child.wait().ok();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(&self.root).ok();
        std::fs::remove_dir_all(&self.runtime).ok();
    }
}
