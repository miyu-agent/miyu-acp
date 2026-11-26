use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::client::exchange;
use super::*;

const TOKEN: &str = "good-token";

/// 一次交到处理者手里的 `call`。
struct Call {
    name: String,
    arguments: Value,
    reply: Reply,
}

struct Fake {
    tools: Vec<Tool>,
    calls: Mutex<Sender<Call>>,
}

impl Handler for Fake {
    fn list(&self, token: &str) -> Option<Vec<Tool>> {
        (token == TOKEN).then(|| self.tools.clone())
    }

    fn call(&self, token: &str, name: &str, arguments: Value, reply: Reply) {
        if token != TOKEN {
            reply.refuse("unknown token").unwrap();
            return;
        }
        let call = Call {
            name: name.to_string(),
            arguments,
            reply,
        };
        self.calls.lock().unwrap().send(call).unwrap();
    }
}

fn tool(name: &str) -> Tool {
    Tool {
        name: name.into(),
        description: format!("does {name}"),
        input_schema: json!({"type": "object", "properties": {"x": {"type": "string"}}}),
    }
}

/// 短的临时目录（Unix 套接字路径有长度上限，不放在很深的 target 目录里）。
fn socket_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acp-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Rig {
    server: Server,
    calls: Receiver<Call>,
    dir: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let dir = socket_dir(name);
        let (sender, calls) = channel();
        let handler = Arc::new(Fake {
            tools: vec![tool("bash"), tool("read")],
            calls: Mutex::new(sender),
        });
        let server = Server::start(&dir.join("a.sock"), handler).unwrap();
        Rig { server, calls, dir }
    }

    fn path(&self) -> &Path {
        self.server.path()
    }

    fn next_call(&self) -> Call {
        self.calls.recv_timeout(Duration::from_secs(10)).unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "等了 10 秒还没成");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn list_returns_the_tools_in_the_wire_shape() {
    let rig = Rig::new("list");
    let reply = exchange(rig.path(), &list_request(TOKEN)).unwrap();
    assert_eq!(
        reply,
        json!({"tools": [
            {"name": "bash", "description": "does bash", "inputSchema": tool("bash").input_schema},
            {"name": "read", "description": "does read", "inputSchema": tool("read").input_schema},
        ]})
    );
}

#[test]
fn call_passes_name_and_arguments_and_returns_the_result() {
    let rig = Rig::new("call");
    let path = rig.path().to_path_buf();
    let client = std::thread::spawn(move || {
        exchange(
            &path,
            &call_request(TOKEN, "bash", &json!({"command": "ls", "n": 1.50})),
        )
        .unwrap()
    });
    let call = rig.next_call();
    assert_eq!(call.name, "bash");
    assert_eq!(call.arguments, json!({"command": "ls", "n": 1.50}));
    call.reply
        .send(&[json!({"type": "text", "text": "ok"})], true)
        .unwrap();
    assert_eq!(
        client.join().unwrap(),
        json!({"content": [{"type": "text", "text": "ok"}], "isError": true})
    );
}

#[test]
fn a_call_without_arguments_gets_an_empty_object() {
    let rig = Rig::new("noargs");
    let mut stream = UnixStream::connect(rig.path()).unwrap();
    writeln!(
        stream,
        r#"{{"token": "{TOKEN}", "op": "call", "name": "t"}}"#
    )
    .unwrap();
    let call = rig.next_call();
    assert_eq!(call.arguments, json!({}));
    call.reply.send(&[], false).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    assert_eq!(line.trim_end(), r#"{"content":[],"isError":false}"#);
}

#[test]
fn an_unknown_token_gets_an_error_and_the_connection_closes() {
    let rig = Rig::new("token");
    let error = exchange(rig.path(), &list_request("bad")).unwrap_err();
    assert!(error.contains("unknown token"), "{error}");
    let error = exchange(rig.path(), &call_request("bad", "bash", &json!({}))).unwrap_err();
    assert!(error.contains("unknown token"), "{error}");
    assert!(rig.calls.try_recv().is_err(), "口令不对的调用没交到别处");

    let mut stream = UnixStream::connect(rig.path()).unwrap();
    writeln!(stream, "{}", list_request("bad")).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim_end(), r#"{"error":"unknown token"}"#);
    line.clear();
    assert_eq!(reader.read_line(&mut line).unwrap(), 0, "回完关连接");
}

#[test]
fn malformed_requests_get_an_error_line() {
    let rig = Rig::new("malformed");
    for bad in [
        "not json",
        r#"{"op": "list"}"#,
        r#"{"token": "t", "op": "launch"}"#,
        r#"{"token": "t", "op": "call"}"#,
        "[1]",
    ] {
        let mut stream = UnixStream::connect(rig.path()).unwrap();
        writeln!(stream, "{bad}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert!(
            reply["error"].as_str().unwrap().starts_with("bad request"),
            "{bad} -> {line}"
        );
    }
}

#[test]
fn the_reply_may_come_much_later_from_another_thread() {
    let rig = Rig::new("late");
    let path = rig.path().to_path_buf();
    let client = std::thread::spawn(move || {
        exchange(&path, &call_request(TOKEN, "slow", &json!({}))).unwrap()
    });
    let call = rig.next_call();
    // 接连接的线程不被占着：这时别的操作照常答。
    let listed = exchange(rig.path(), &list_request(TOKEN)).unwrap();
    assert_eq!(listed["tools"].as_array().unwrap().len(), 2);
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        call.reply
            .send(&[json!({"type": "text", "text": "late"})], false)
            .unwrap();
    });
    let result = client.join().unwrap();
    assert_eq!(result["content"][0]["text"], "late");
    assert_eq!(result["isError"], false);
    late.join().unwrap();
}

#[test]
fn dropping_the_reply_closes_the_connection() {
    let rig = Rig::new("drop");
    let path = rig.path().to_path_buf();
    let client = std::thread::spawn(move || exchange(&path, &call_request(TOKEN, "x", &json!({}))));
    drop(rig.next_call().reply);
    let error = client.join().unwrap().unwrap_err();
    assert!(error.contains("closed the connection"), "{error}");
}

#[test]
fn the_holder_can_see_that_the_client_went_away() {
    let rig = Rig::new("gone");
    let stream = UnixStream::connect(rig.path()).unwrap();
    let mut writer = stream.try_clone().unwrap();
    writeln!(writer, "{}", call_request(TOKEN, "x", &json!({}))).unwrap();
    let call = rig.next_call();
    assert!(!call.reply.is_closed());
    drop(writer);
    drop(stream);
    wait_until(|| call.reply.is_closed());
    let error = call.reply.send(&[], false).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
}

#[test]
fn the_socket_is_private_replaces_a_stale_file_and_goes_away_with_the_server() {
    let dir = socket_dir("file");
    let path = dir.join("a.sock");
    std::fs::write(&path, "stale").unwrap();
    let (sender, _calls) = channel();
    let handler = Arc::new(Fake {
        tools: vec![],
        calls: Mutex::new(sender),
    });
    let server = Server::start(&path, handler).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        exchange(&path, &list_request(TOKEN)).unwrap(),
        json!({"tools": []})
    );
    drop(server);
    assert!(!path.exists(), "服务端丢掉后文件删了");
    assert!(exchange(&path, &list_request(TOKEN)).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_held_reply_still_works_after_the_server_is_dropped() {
    let dir = socket_dir("outlive");
    let path = dir.join("a.sock");
    let (sender, calls) = channel();
    let handler = Arc::new(Fake {
        tools: vec![],
        calls: Mutex::new(sender),
    });
    let server = Server::start(&path, handler).unwrap();
    let client_path = path.clone();
    let client = std::thread::spawn(move || {
        exchange(&client_path, &call_request(TOKEN, "x", &json!({}))).unwrap()
    });
    let call = calls.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(server);
    assert!(!path.exists());
    call.reply.send(&[], false).unwrap();
    assert_eq!(client.join().unwrap()["isError"], false);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_path_that_is_too_long_is_an_error_that_says_so() {
    let dir = socket_dir("long");
    let path = dir.join("x".repeat(120)).join("a.sock");
    let (sender, _calls) = channel();
    let handler = Arc::new(Fake {
        tools: vec![],
        calls: Mutex::new(sender),
    });
    let error = Server::start(&path, handler).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    let message = error.to_string();
    assert!(
        message.contains("bytes") && message.contains("limit"),
        "{message}"
    );
    assert!(message.contains(&path.display().to_string()), "{message}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_one_mebibyte_argument_goes_through_both_ways() {
    let rig = Rig::new("big");
    let text = "a\"b\\c ✓ ".repeat(140_000);
    assert!(text.len() > 1 << 20);
    let arguments = json!({"text": text});
    let path = rig.path().to_path_buf();
    let sent = arguments.clone();
    let client =
        std::thread::spawn(move || exchange(&path, &call_request(TOKEN, "big", &sent)).unwrap());
    let call = rig.next_call();
    assert_eq!(call.arguments, arguments, "arguments 原文到了");
    let echoed = call.arguments["text"].as_str().unwrap().to_string();
    call.reply
        .send(&[json!({"type": "text", "text": echoed})], false)
        .unwrap();
    let result = client.join().unwrap();
    assert_eq!(result["content"][0]["text"], arguments["text"]);
}

#[test]
fn an_idle_connection_does_not_block_the_others() {
    // 连上去什么都不发的连接占着自己的线程（10 秒后被关），别的操作照常答。
    let rig = Rig::new("silent");
    let _idle = UnixStream::connect(rig.path()).unwrap();
    assert_eq!(
        exchange(rig.path(), &list_request(TOKEN)).unwrap()["tools"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn tokens_are_128_bits_of_lowercase_hex_and_differ() {
    let a = new_token().unwrap();
    let b = new_token().unwrap();
    assert_eq!(a.len(), 32);
    assert!(a.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "{a}");
    assert_ne!(a, b);
}

#[test]
fn the_socket_directory_prefers_runtime_over_state() {
    use std::ffi::OsString;
    let os = |text: &str| Some(OsString::from(text));
    assert_eq!(
        socket_path_in(os("/run/x"), os("/state"), 7),
        Some(PathBuf::from("/run/x/acp-7.sock"))
    );
    assert_eq!(
        socket_path_in(None, os("/state"), 7),
        Some(PathBuf::from("/state/acp-7.sock"))
    );
    assert_eq!(
        socket_path_in(os(""), os("/state"), 7),
        Some(PathBuf::from("/state/acp-7.sock")),
        "空的当没设"
    );
    assert_eq!(socket_path_in(None, os(""), 7), None);
}
