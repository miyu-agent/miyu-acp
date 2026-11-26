use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::bridge::client::exchange;

const WAIT: Duration = Duration::from_secs(30);

/// 总台加真的套接字服务端（扮 MCP 服务的客户端用 [`exchange`] 连）。临时目录用短路径：套接字路径有长度上限。
struct Rig {
    hub: Arc<Hub>,
    server: Option<Server>,
    dir: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("acp-hub-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = Arc::new(Hub::default());
        let server = Server::start(&dir.join("a.sock"), Arc::clone(&hub)).unwrap();
        Rig {
            hub,
            server: Some(server),
            dir,
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("a.sock")
    }

    fn register(&self) -> Wiring {
        self.hub
            .register(vec![tool("bash"), tool("read")], false, &self.socket())
            .unwrap()
    }

    /// 在另一个线程里发一次 `call`，交线程（它的结果是 [`exchange`] 的）。
    fn call(
        &self,
        token: &str,
        name: &str,
        arguments: &Value,
    ) -> JoinHandle<Result<Value, String>> {
        let (socket, request) = (self.socket(), bridge::call_request(token, name, arguments));
        std::thread::spawn(move || exchange(&socket, &request))
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server = None;
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn tool(name: &str) -> Tool {
    Tool {
        name: name.into(),
        description: format!("does {name}"),
        input_schema: json!({"type": "object"}),
    }
}

/// 把 `wiring` 的路由指到一条新的通道。
fn listen(wiring: &Wiring) -> Receiver<Signal> {
    let (sender, receiver) = channel();
    *wiring.route().lock().unwrap() = Some(sender);
    receiver
}

fn next_call(receiver: &Receiver<Signal>) -> McpCall {
    match receiver.recv_timeout(WAIT).unwrap() {
        Signal::ToolCall(call) => call,
        other => panic!("想要一件工具调用，来的是 {other:?}"),
    }
}

#[test]
fn list_gives_the_registered_table_and_unknown_tokens_get_nothing() {
    let rig = Rig::new("list");
    let wiring = rig.register();
    assert_eq!(
        rig.hub.list(wiring.token()),
        Some(vec![tool("bash"), tool("read")])
    );
    assert_eq!(rig.hub.list("nope"), None);
    let reply = exchange(&rig.socket(), &bridge::list_request(wiring.token())).unwrap();
    assert_eq!(reply["tools"][0]["name"], "bash");
    let error = exchange(&rig.socket(), &bridge::list_request("nope")).unwrap_err();
    assert!(error.contains("unknown token"), "{error}");
}

#[test]
fn tokens_are_distinct_and_the_socket_path_is_kept() {
    let rig = Rig::new("tokens");
    let (one, two) = (rig.register(), rig.register());
    assert_ne!(one.token(), two.token());
    assert_eq!(one.socket(), rig.socket());
    assert_eq!(rig.hub.len(), 2);
}

#[test]
fn a_call_goes_to_the_listening_route_numbered_from_one_and_the_reply_comes_back() {
    let rig = Rig::new("call");
    let wiring = rig.register();
    let receiver = listen(&wiring);

    let first = rig.call(wiring.token(), "bash", &json!({"command": "ls", "n": 1}));
    let call = next_call(&receiver);
    assert_eq!((call.n, call.name.as_str()), (1, "bash"));
    assert_eq!(call.arguments, json!({"command": "ls", "n": 1}));
    call.reply
        .send(&[json!({"type": "text", "text": "one"})], false)
        .unwrap();
    assert_eq!(
        first.join().unwrap().unwrap(),
        json!({"content": [{"type": "text", "text": "one"}], "isError": false})
    );

    let second = rig.call(wiring.token(), "read", &json!({}));
    let call = next_call(&receiver);
    assert_eq!((call.n, call.name.as_str()), (2, "read"));
    call.reply.send(&[], true).unwrap();
    assert_eq!(second.join().unwrap().unwrap()["isError"], true);
    assert!(!wiring.stray().load(Ordering::SeqCst));
}

#[test]
fn a_call_with_nobody_listening_marks_the_process_stray_and_closes_the_connection() {
    let rig = Rig::new("stray");
    let wiring = rig.register();
    let result = rig.call(wiring.token(), "bash", &json!({})).join().unwrap();
    assert!(result.unwrap_err().contains("closed the connection"));
    assert!(wiring.stray().load(Ordering::SeqCst));

    // 听的那一头已经没了（接收端丢了、路由还指着）：同样算乱说话。
    let other = rig.register();
    drop(listen(&other));
    let result = rig.call(other.token(), "bash", &json!({})).join().unwrap();
    assert!(result.is_err());
    assert!(other.stray().load(Ordering::SeqCst));
}

#[test]
fn a_call_with_an_unknown_token_is_refused() {
    let rig = Rig::new("refuse");
    let wiring = rig.register();
    let receiver = listen(&wiring);
    let error = rig
        .call("wrong", "bash", &json!({}))
        .join()
        .unwrap()
        .unwrap_err();
    assert!(error.contains("unknown token"), "{error}");
    assert!(receiver.try_recv().is_err(), "没有送给别人的路由");
}

#[test]
fn numbers_are_per_process() {
    let rig = Rig::new("numbers");
    let (one, two) = (rig.register(), rig.register());
    let (first, second) = (listen(&one), listen(&two));
    let calls = [
        rig.call(one.token(), "bash", &json!({})),
        rig.call(two.token(), "bash", &json!({})),
    ];
    assert_eq!(next_call(&first).n, 1);
    assert_eq!(next_call(&second).n, 1);
    drop(calls);
}

#[test]
fn dropping_the_wiring_takes_the_token_out() {
    let rig = Rig::new("drop");
    let wiring = rig.register();
    let token = wiring.token().to_string();
    assert_eq!(rig.hub.len(), 1);
    drop(wiring);
    assert!(rig.hub.is_empty());
    assert_eq!(rig.hub.list(&token), None);
}

#[test]
fn a_call_to_a_tool_outside_the_table_is_answered_as_failed_without_reaching_the_core() {
    let rig = Rig::new("unknown");
    let wiring = rig.register();
    let receiver = listen(&wiring);
    let reply = rig
        .call(wiring.token(), "rm", &json!({}))
        .join()
        .unwrap()
        .unwrap();
    assert_eq!(reply["isError"], true);
    assert!(reply["content"][0]["text"].as_str().unwrap().contains("rm"));
    assert!(receiver.try_recv().is_err(), "不交回核心");
}

#[test]
fn extras_declared_for_the_agent_are_taken_out_before_the_core_sees_them() {
    let rig = Rig::new("extras");
    let wiring = rig
        .hub
        .register(vec![tool("bash")], true, &rig.socket())
        .unwrap();
    let receiver = listen(&wiring);
    let pending = rig.call(
        wiring.token(),
        "bash",
        &json!({"command": "ls", "toolAction": "list"}),
    );
    let call = next_call(&receiver);
    assert_eq!(call.arguments, json!({"command": "ls"}));
    call.reply.send(&[], false).unwrap();
    pending.join().unwrap().unwrap();
}
