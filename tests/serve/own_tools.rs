//! 对方自带的工具两档（设计 73 第四节第 7 条）：只读的搜索放行、画成供应商侧工具；别的「只用 Miyu 的工具」回拒绝，「也放行
//! 对方的工具」请核心问主人（核心施工 ACP-8 的 `driver.approve`），照核心回的选项回 agent；旧核心没有这个方法的照拒绝；
//! 没要权限就跑了的照样画出来。

use serde_json::{Value, json};

use crate::support::{Core, request, tool};

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
    let got = core.call_and_collect("claude-acp", &request("S", "EDIT", &json!([tool("read")])));
    assert!(got.text().ends_with("edit: no"), "{}", got.text());
    assert!(
        got.starts("tool_call").is_empty() && got.asked.is_empty(),
        "不问主人"
    );
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
fn also_allowing_asks_the_core_and_follows_its_answer() {
    for (answer, expected) in [
        (json!({"decision": "once", "option": "yes"}), "edit: yes"),
        (
            json!({"decision": "deny", "option": "no", "reason": "不要"}),
            "edit: no",
        ),
        (
            json!({"decision": "deny", "option": "no", "auto": "unattended"}),
            "edit: no",
        ),
        (json!({"decision": "cancelled"}), "edit: cancelled"),
    ] {
        let mut core = Core::start("ask", "fake", "", &allow(), &[]);
        core.call_with(
            "c1",
            "c1",
            &request("S", "EDIT", &json!([tool("read")])),
            "fake",
            &json!({}),
        );
        let result = json!({"result": answer.clone()});
        let got = core.collect_answering("c1", "c1", &|_| result.clone());
        assert!(got.text().ends_with(expected), "{answer}: {}", got.text());
        assert!(
            got.starts("tool_call").is_empty(),
            "不交回 ask_user：{:?}",
            got.deltas
        );
        let hosted = got.starts("hosted_tool");
        assert_eq!(hosted.len(), 1, "{:?}", got.deltas);
        assert_eq!(got.asked.len(), 1, "问一次");
        let asked = &got.asked[0];
        assert_eq!(asked["index"], hosted[0]["index"], "画在那一块上");
        assert_eq!(
            (&asked["access"], &asked["title"], &asked["text"]),
            (
                &json!("write"),
                &json!("Edit notes.txt"),
                &json!(r#"{"path":"notes.txt"}"#)
            )
        );
        assert_eq!(
            asked["options"],
            json!([
                {"id": "yes", "name": "Allow", "kind": "allow_once"},
                {"id": "always", "name": "Always allow", "kind": "allow_always"},
                {"id": "no", "name": "Reject", "kind": "reject_once"}
            ])
        );
        let start = got.deltas.iter().position(|delta| {
            delta
                .get("start")
                .is_some_and(|start| start["kind"].get("hosted_tool").is_some())
        });
        assert!(start.is_some(), "先开块再问");
    }
}

#[test]
fn an_older_core_without_the_method_refuses_like_only_miyu_tools() {
    let mut core = Core::start("old", "fake", "", &allow(), &[]);
    core.call_with(
        "c1",
        "c1",
        &request("S", "EDIT", &json!([tool("read")])),
        "fake",
        &json!({}),
    );
    let missing = json!({"error": {"code": -32601, "message": "unknown method", "data": {"reason": "unknown_method"}}});
    let got = core.collect_answering("c1", "c1", &|_| missing.clone());
    assert!(got.text().ends_with("edit: no"), "{}", got.text());
    let outcome = got
        .deltas
        .iter()
        .find_map(|delta| delta.get("outcome"))
        .unwrap();
    assert_eq!(outcome["failed"], true, "写明被拒：{outcome}");
    assert!(outcome["text"].as_str().unwrap().contains("refused"));
}

#[test]
fn a_tool_run_without_asking_is_drawn_and_marked_for_people() {
    let mut core = Core::start("silent", "fake", "", &json!({}), &[]);
    let got = core.ask("c1", &request("S", "SILENT", &json!([])));
    let hosted = got.starts("hosted_tool");
    assert_eq!(hosted[0]["kind"]["hosted_tool"]["name"], "execute");
    let outcome = got
        .deltas
        .iter()
        .find_map(|delta| delta.get("outcome"))
        .unwrap();
    assert_eq!(
        outcome["text"],
        format!("{}\na b c", miyu_acp::turn::unasked_note(false)),
        "不静默：写明未经确认已执行"
    );
}

/// 施工 ACP-10：对方起在不问的模式（Claude 适配器没有设置时是 `auto`，主人本机设了 `bypassPermissions` 的照它）。开会话以后
/// 切到 `default`，改文件之前就来要权限；不切的话它直接改了（修以前的真人实测）。
#[test]
fn the_agent_is_switched_to_asking_before_its_own_tools_run() {
    let mut core = Core::start("modes", "fake", "modes", &allow(), &[]);
    core.call_with(
        "c1",
        "c1",
        &request("S", "EDIT", &json!([tool("read")])),
        "fake",
        &json!({}),
    );
    let once = json!({"result": {"decision": "once", "option": "yes"}});
    let got = core.collect_answering("c1", "c1", &|_| once.clone());
    assert!(got.text().ends_with("edit: yes"), "{}", got.text());
    assert_eq!(got.asked.len(), 1, "经核心问主人");
    let switched = core.got("session/set_mode");
    assert_eq!(switched.len(), 1, "{switched:?}");
    assert_eq!(switched[0]["modeId"], "default");

    let mut core = Core::start("modes-only", "fake", "modes", &json!({}), &[]);
    let got = core.ask("c1", &request("S", "EDIT", &json!([tool("read")])));
    assert!(
        got.text().ends_with("edit: no"),
        "只用 Miyu 的工具那一档照样先切、再拒：{}",
        got.text()
    );
    assert_eq!(core.noted("mode"), vec![json!("default")]);
}
