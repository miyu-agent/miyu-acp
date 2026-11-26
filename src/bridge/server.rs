//! 套接字的服务端（扩展进程这一头）：一个接连接的线程，每条连接一个线程读第一行、验写法、交给处理者（[`Handler`]）。
//! 协议见 [`crate::bridge`]。
//!
//! `list` 在连接自己的线程里答完。`call` 把连接包成 [`Reply`] 交给处理者就算交接完（处理者不许在 `call` 里等结果）：
//! `Reply` 可以存起来、几十分钟以后在别的线程上回话；这条连接的线程留下来继续读，读到对面关了就记下（[`Reply::is_closed`]）。

use std::io::{BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use super::{MAX_PATH_BYTES, Tool};
use crate::process::{LINE_LIMIT, Line, next_line};

/// 连上以后等第一行至多多久。
pub const FIRST_LINE_WAIT: Duration = Duration::from_secs(10);

/// 口令不认识时回的话。
const UNKNOWN_TOKEN: &str = "unknown token";

/// 谁来办连接上的请求。几条连接各在自己的线程里调它，所以要 `Sync`。
pub trait Handler: Send + Sync + 'static {
    /// 口令对应的那段对话的工具表；口令不认识是 `None`（服务端回 `{"error"}` 关连接）。
    fn list(&self, token: &str) -> Option<Vec<Tool>>;

    /// 一次工具调用。**马上返回**：`reply` 存起来，结果好了再 [`Reply::send`]；口令不认识调 [`Reply::refuse`]。
    /// 丢掉 `reply` 而没回话，连接直接关掉，客户端读到头。
    fn call(&self, token: &str, name: &str, arguments: Value, reply: Reply);
}

/// 一条等着回话的 `call` 连接。可以存起来、移到别的线程上回。
#[derive(Debug)]
pub struct Reply {
    stream: UnixStream,
    closed: Arc<AtomicBool>,
}

impl Reply {
    /// 回话：`content` 是 MCP 的内容块，`is_error` 是工具失败。回完关连接。
    ///
    /// # Errors
    ///
    /// 对面已经关了（[`Reply::is_closed`]）或者写不出去：交 `BrokenPipe` 或写的错。
    pub fn send(self, content: &[Value], is_error: bool) -> std::io::Result<()> {
        self.write(&json!({"content": content, "isError": is_error}))
    }

    /// 拒绝：回 `{"error": message}`，关连接。
    ///
    /// # Errors
    ///
    /// 同 [`Reply::send`]。
    pub fn refuse(self, message: &str) -> std::io::Result<()> {
        self.write(&json!({"error": message}))
    }

    /// 对面（客户端）已经关了连接。不实时通知：服务端读到头才记下，持有的一方在回话之前或者隔一会儿问一下。
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn write(mut self, message: &Value) -> std::io::Result<()> {
        if self.is_closed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the client closed the connection",
            ));
        }
        let mut line = message.to_string();
        line.push('\n');
        self.stream.write_all(line.as_bytes())?;
        self.stream.flush()
    }
}

impl Drop for Reply {
    /// 回完或者丢掉都关连接（连接线程的读也随之返回）；对面早已断开的不算错。
    fn drop(&mut self) {
        if let Err(error) = self.stream.shutdown(std::net::Shutdown::Both)
            && error.kind() != std::io::ErrorKind::NotConnected
        {
            eprintln!("closing a bridge connection failed: {error}");
        }
    }
}

/// 起着的套接字服务端。丢掉它：不再接新连接，删套接字文件（已经交出去的 [`Reply`] 照常能回）。
#[derive(Debug)]
pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    accepting: Option<JoinHandle<()>>,
}

impl Server {
    /// 在 `path` 建套接字（同名的旧文件先删，权限只给本人）、起接连接的线程。
    ///
    /// # Errors
    ///
    /// 路径超过 [`MAX_PATH_BYTES`]（说明长度和路径）、旧文件删不掉、建不成、改不了权限、起不了线程：原样交。
    pub fn start(path: &Path, handler: Arc<impl Handler>) -> std::io::Result<Server> {
        let length = path.as_os_str().as_bytes().len();
        if length > MAX_PATH_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "the socket path is {length} bytes, over the {MAX_PATH_BYTES}-byte limit of Unix sockets: {}",
                    path.display()
                ),
            ));
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(path)?;
        let server_path = path.to_path_buf();
        let started = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .and_then(|()| {
                let stop = Arc::new(AtomicBool::new(false));
                let flag = Arc::clone(&stop);
                std::thread::Builder::new()
                    .name("bridge-accept".into())
                    .spawn(move || accept_loop(&listener, &flag, &handler))
                    .map(|thread| (stop, thread))
            });
        match started {
            Ok((stop, thread)) => Ok(Server {
                path: server_path,
                stop,
                accepting: Some(thread),
            }),
            Err(error) => {
                if let Err(removing) = std::fs::remove_file(path) {
                    eprintln!("removing {} failed: {removing}", path.display());
                }
                Err(error)
            }
        }
    }

    /// 套接字文件的路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // 接连接的线程停在 accept 上：自己连一下把它叫醒。
        match (UnixStream::connect(&self.path), self.accepting.take()) {
            (Ok(_), Some(thread)) => {
                if thread.join().is_err() {
                    eprintln!("the bridge accept thread panicked");
                }
            }
            (Err(error), _) => eprintln!("waking the bridge accept thread failed: {error}"),
            (Ok(_), None) => {}
        }
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("removing {} failed: {error}", self.path.display());
        }
    }
}

fn accept_loop(listener: &UnixListener, stop: &AtomicBool, handler: &Arc<impl Handler>) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("accepting a bridge connection failed: {error}");
                // 出错多半是句柄用光了：别空转。
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let handler = Arc::clone(handler);
        let spawned = std::thread::Builder::new()
            .name("bridge-connection".into())
            .spawn(move || {
                if let Err(error) = serve_connection(stream, &*handler) {
                    eprintln!("a bridge connection failed: {error}");
                }
            });
        if let Err(error) = spawned {
            eprintln!("starting a bridge connection thread failed: {error}");
        }
    }
}

/// 一条请求的写法。
#[derive(Debug, PartialEq)]
enum Request {
    List {
        token: String,
    },
    Call {
        token: String,
        name: String,
        arguments: Value,
    },
}

/// 验第一行的写法：对象、`token` 是字、`op` 是 `list` 或 `call`、`call` 有字的 `name`（没有 `arguments` 当 `{}`）。
fn parse(line: &str) -> Result<Request, String> {
    let value: Value = serde_json::from_str(line).map_err(|error| format!("not JSON: {error}"))?;
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .ok_or("missing token")?
        .to_string();
    match value.get("op").and_then(Value::as_str) {
        Some("list") => Ok(Request::List { token }),
        Some("call") => Ok(Request::Call {
            token,
            name: value
                .get("name")
                .and_then(Value::as_str)
                .ok_or("missing name")?
                .to_string(),
            arguments: value.get("arguments").cloned().unwrap_or_else(|| json!({})),
        }),
        _ => Err("op must be \"list\" or \"call\"".into()),
    }
}

fn serve_connection(stream: UnixStream, handler: &dyn Handler) -> std::io::Result<()> {
    stream.set_read_timeout(Some(FIRST_LINE_WAIT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let reply = Reply {
        stream,
        closed: Arc::new(AtomicBool::new(false)),
    };
    let line = match next_line(&mut reader, LINE_LIMIT)? {
        Some(Line::Text(line)) => line,
        Some(Line::TooLong) => return reply.refuse("the request line is too long"),
        None => return Ok(()),
    };
    reply.stream.set_read_timeout(None)?;
    match parse(&line) {
        Err(message) => reply.refuse(&format!("bad request: {message}")),
        Ok(Request::List { token }) => match handler.list(&token) {
            Some(tools) => {
                reply.write(&json!({"tools": tools.iter().map(Tool::to_json).collect::<Vec<_>>()}))
            }
            None => reply.refuse(UNKNOWN_TOKEN),
        },
        Ok(Request::Call {
            token,
            name,
            arguments,
        }) => {
            let closed = Arc::clone(&reply.closed);
            handler.call(&token, &name, arguments, reply);
            watch(&mut reader, &closed);
            Ok(())
        }
    }
}

/// 读到头（对面关了、或者我们自己关了）就记下；读出错（对面重置）也是断了。客户端回话前不会再发什么，来了的丢掉。
fn watch(reader: &mut impl Read, closed: &AtomicBool) {
    let mut sink = [0_u8; 256];
    loop {
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    closed.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests;
