//! 会话对应：追加一条的接着用同一个进程、同一个 ACP 会话，只交新的话；改了历史的新开，整份历史转写进第一次提问。

use serde_json::json;

use crate::support::{Core, request, then, user};

#[test]
fn the_next_turn_reuses_the_session_and_sends_only_the_new_message() {
    let mut core = Core::start("next", "fake", "", &json!({}), &[]);
    let first = request("S", "one", &json!([]));
    let got = core.ask("c1", &first);
    let answer = json!([{"type": "text", "text": got.text()}]);
    let second = then(&first, &answer, user("two"));
    let again = core.ask("c2", &second);
    assert_eq!(
        again.text(),
        "thinkingpong 1: two",
        "同一个会话（序号还是 1）"
    );
    assert_eq!(core.got("initialize").len(), 1, "同一个进程");
    assert_eq!(core.got("session/new").len(), 1);
    let prompts = core.got("session/prompt");
    assert_eq!(
        prompts[1]["prompt"],
        json!([{"type": "text", "text": "two"}])
    );
    assert_eq!(prompts[0]["sessionId"], prompts[1]["sessionId"]);
}

#[test]
fn changed_history_opens_a_new_session_with_the_transcript() {
    let mut core = Core::start("changed", "fake", "", &json!({}), &[]);
    let first = request("S", "one", &json!([]));
    core.ask("c1", &first);
    // 改了第一条（撤销、压缩以后就是这样）：记着的那串不再是前缀。
    let edited = then(
        &request("S", "one, edited", &json!([])),
        &json!([{"type": "text", "text": "an answer"}]),
        user("two"),
    );
    let got = core.ask("c2", &edited);
    assert!(got.result().get("error").is_none(), "{}", got.response);
    assert_eq!(core.got("session/new").len(), 2, "对不上的新开");
    let text = core.got("session/prompt")[1]["prompt"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("<conversation-history>"), "{text}");
    assert!(
        text.contains("User:\none, edited") && text.contains("Assistant:\nan answer"),
        "{text}"
    );
    assert!(text.ends_with("</conversation-history>\n\ntwo"), "{text}");
}

#[test]
fn another_system_prompt_is_another_conversation() {
    let mut core = Core::start("system", "fake", "", &json!({}), &[]);
    let first = request("S", "one", &json!([]));
    let got = core.ask("c1", &first);
    let mut second = then(
        &first,
        &json!([{"type": "text", "text": got.text()}]),
        user("two"),
    );
    second["system"] = json!("OTHER");
    core.ask("c2", &second);
    assert_eq!(core.got("session/new").len(), 2);
}

#[test]
fn a_one_off_request_is_not_kept_for_a_next_turn() {
    // 施工 ACP-12：起标题、回顾这类辅助请求不带 system、不带工具、只有一条 user，用完就没人再接着说。以前照样登记、进程留到
    // `acp.keep_seconds`，占着表里的位置，多开几段对话时把真在用的挤掉。
    let mut core = Core::start("oneoff", "fake", "", &json!({}), &[]);
    let first = request("", "title this", &json!([]));
    let got = core.ask("c1", &first);
    assert!(got.result().get("error").is_none(), "{}", got.response);
    let next = then(
        &first,
        &json!([{"type": "text", "text": got.text()}]),
        user("two"),
    );
    core.ask("c2", &next);
    assert_eq!(core.got("session/new").len(), 2, "没登记，接不上");
    assert_eq!(core.got("initialize").len(), 2, "进程没留着");
}
