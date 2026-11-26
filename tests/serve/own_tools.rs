//! 对方自带的工具两档（设计 73 第四节第 7 条）：只读的搜索放行、画成供应商侧工具；别的「只用 Miyu 的工具」回拒绝，「也放行
//! 对方的工具」交回核心问主人（`ask_user`），照主人选的回 agent；没要权限就跑了的照样画出来。

use serde_json::{Value, json};

use crate::support::{Core, handed_blocks, request, then, tool, tool_result};

fn allow() -> Value {
    json!({"acp.own_tools": "allow"})
}

#[test]
fn a_search_is_allowed_in_both_settings_and_drawn_as_a_hosted_tool() {
    for config in [json!({}), allow()] {
        let mut core = Core::start("search", "fake", "", &config, &[]);
        let got = core.ask("c1", &request("S", "SEARCH", &json!([])));
        let hosted = got.starts("hosted_tool");
        assert_eq!(hosted.len(), 1, "{:?}", got.deltas);
        assert_eq!(hosted[0]["kind"]["hosted_tool"]["name"], "WebSearch");
        let outcome = got
            .deltas
            .iter()
            .find_map(|delta| delta.get("outcome"))
            .unwrap();
        assert_eq!(outcome["text"], "1. Rust https://rust-lang.org");
        assert!(got.text().contains(r#"{"query":"rust"}"#), "参数原文");
        assert!(got.text().ends_with("searched: yes"), "{}", got.text());
    }
}

#[test]
fn only_miyu_tools_refuses_an_edit_and_the_claude_adapter_gets_its_tools_switched_off() {
    let mut core = Core::start("refuse", "claude-acp", "", &json!({}), &[]);
    let got = core.call_and_collect(
        "claude-acp",
        &request("S", "EDIT", &json!([tool("ask_user")])),
    );
    assert!(got.text().ends_with("edit: no"), "{}", got.text());
    assert!(got.starts("tool_call").is_empty(), "不问主人");
    let outcome = got
        .deltas
        .iter()
        .find_map(|delta| delta.get("outcome"))
        .unwrap();
    assert_eq!(outcome["failed"], true, "拒了的那件写明被拒：{outcome}");
    assert!(outcome["text"].as_str().unwrap().contains("refused"));
    let new = &core.got("session/new")[0];
    assert_eq!(
        new["_meta"]["claudeCode"]["options"]["tools"],
        json!(["WebSearch", "WebFetch"])
    );
}

#[test]
fn also_allowing_asks_the_owner_through_ask_user_and_follows_the_answer() {
    for (answer, expected) in [
        ("Allow", "edit: yes"),
        ("Reject", "edit: no"),
        ("something else", "edit: no"),
    ] {
        let mut core = Core::start("ask", "fake", "", &allow(), &[]);
        let first = request("S", "EDIT", &json!([tool("ask_user")]));
        let handed = core.ask("c1", &first);
        let calls = handed.starts("tool_call");
        assert_eq!(calls.len(), 1, "{:?}", handed.deltas);
        assert_eq!(calls[0]["kind"]["tool_call"]["name"], "ask_user");
        let blocks = handed_blocks(&handed, "call_9");
        let question: Value = serde_json::from_str(blocks[0]["args"].as_str().unwrap()).unwrap();
        let text = question["questions"][0]["question"].as_str().unwrap();
        assert!(
            text.contains("Fake Agent")
                && text.contains("Edit notes.txt")
                && text.contains("sandbox"),
            "{text}"
        );
        assert_eq!(
            question["questions"][0]["options"][0],
            json!({"label": "Allow"})
        );
        let result = format!("\"{text}\" = {answer}\n\nGo on with these answers in mind.");
        let done = core.ask("c2", &then(&first, &blocks, tool_result("call_9", &result)));
        assert!(done.text().ends_with(expected), "{answer}: {}", done.text());
    }
}

#[test]
fn also_allowing_without_anyone_to_ask_refuses() {
    let mut core = Core::start("noask", "fake", "", &allow(), &[]);
    let got = core.ask("c1", &request("S", "EDIT", &json!([tool("read")])));
    assert!(got.text().ends_with("edit: no"), "{}", got.text());
}

#[test]
fn a_tool_run_without_asking_is_still_drawn() {
    let mut core = Core::start("silent", "fake", "", &json!({}), &[]);
    let got = core.ask("c1", &request("S", "SILENT", &json!([])));
    let hosted = got.starts("hosted_tool");
    assert_eq!(hosted[0]["kind"]["hosted_tool"]["name"], "execute");
    let outcome = got
        .deltas
        .iter()
        .find_map(|delta| delta.get("outcome"))
        .unwrap();
    assert_eq!(outcome["text"], "a b c");
}
