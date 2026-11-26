use super::*;

fn options() -> Vec<Value> {
    vec![
        json!({"optionId": "always", "name": "Always allow", "kind": "allow_always"}),
        json!({"optionId": "once", "name": "Allow", "kind": "allow_once"}),
        json!({"optionId": "no", "name": "Reject", "kind": "reject_once"}),
    ]
}

#[test]
fn miyu_tools_and_readonly_kinds_are_allowed_in_both_settings() {
    let names = Names::new(["read".to_string()]);
    let seen: HashSet<String> = ["call_7".to_string()].into();
    for own in [OwnTools::MiyuOnly, OwnTools::Allow] {
        for call in [
            json!({"toolCallId": "c1", "name": "mcp__miyu__read", "kind": "read"}),
            json!({"toolCallId": "call_7", "title": "whatever", "kind": "execute"}),
            json!({"toolCallId": "c2", "title": "Searching the web", "kind": "search"}),
            json!({"toolCallId": "c3", "title": "Fetch page", "kind": "fetch"}),
            json!({"toolCallId": "c4", "title": "web_search for rust"}),
        ] {
            assert_eq!(judge(own, &call, &names, &seen), Verdict::Allow, "{call}");
        }
    }
}

#[test]
fn other_tools_are_refused_or_asked_depending_on_the_setting() {
    let names = Names::default();
    let none = HashSet::new();
    for kind in ["edit", "delete", "move", "execute", "other"] {
        let call = json!({"toolCallId": "c", "title": "Run it", "kind": kind});
        assert_eq!(
            judge(OwnTools::MiyuOnly, &call, &names, &none),
            Verdict::Reject
        );
        assert_eq!(
            judge(OwnTools::Allow, &call, &names, &none),
            Verdict::Ask,
            "问不问得了由核心判"
        );
    }
}

#[test]
fn the_once_options_are_preferred_and_a_missing_one_means_cancelled() {
    assert_eq!(
        choose(&options(), true),
        json!({"outcome": {"outcome": "selected", "optionId": "once"}})
    );
    assert_eq!(
        choose(&options(), false),
        json!({"outcome": {"outcome": "selected", "optionId": "no"}})
    );
    let only_always = [json!({"optionId": "a", "name": "A", "kind": "allow_always"})];
    assert_eq!(choose(&only_always, true)["outcome"]["optionId"], "a");
    assert_eq!(
        choose(&only_always, false),
        json!({"outcome": {"outcome": "cancelled"}})
    );
}

#[test]
fn a_command_is_asked_with_its_text_and_the_options_renamed() {
    let call = json!({"toolCallId": "t", "title": "rm -rf build", "kind": "execute",
        "rawInput": {"command": "rm -rf build", "description": "clean"}});
    let params = approve("d3", 2, &call, &options()).unwrap();
    assert_eq!(
        params,
        json!({"call": "d3", "index": 2, "access": "execute", "title": "rm -rf build",
            "command": "rm -rf build", "options": [
                {"id": "always", "name": "Always allow", "kind": "allow_always"},
                {"id": "once", "name": "Allow", "kind": "allow_once"},
                {"id": "no", "name": "Reject", "kind": "reject_once"}]})
    );
}

#[test]
fn edits_are_asked_with_their_paths_and_others_with_their_input() {
    let edit = json!({"title": "Edit main.rs", "kind": "edit",
        "locations": [{"path": "/w/src/main.rs"}, {"path": "/w/a.rs"}], "rawInput": {"x": 1}});
    let params = approve("d3", 0, &edit, &options()).unwrap();
    assert_eq!(params["access"], "write");
    assert_eq!(params["text"], "/w/src/main.rs\n/w/a.rs");
    assert!(params.get("command").is_none());
    let other = json!({"title": "Do it", "kind": "other", "rawInput": {"x": 1}});
    let params = approve("d3", 0, &other, &options()).unwrap();
    assert_eq!(
        (&params["access"], &params["text"]),
        (&json!("execute"), &json!(r#"{"x":1}"#))
    );
    let fetch = json!({"title": "Get", "kind": "fetch"});
    let params = approve("d3", 0, &fetch, &options()).unwrap();
    assert_eq!(params["access"], "network");
    assert!(params.get("text").is_none(), "没有可说的不写");
    let odd = [json!({"optionId": "x", "kind": "maybe"})];
    assert_eq!(approve("d3", 0, &fetch, &odd), None, "一项对得上的都没有");
}

#[test]
fn the_cores_answer_picks_what_to_reply() {
    let reply = |result: Value| approved(Ok(&result), &options());
    assert_eq!(
        reply(json!({"decision": "once", "option": "once"})),
        (selected(&json!("once")), true)
    );
    assert_eq!(
        reply(json!({"decision": "deny", "option": "no", "reason": "不要"})),
        (selected(&json!("no")), false)
    );
    assert_eq!(
        reply(json!({"decision": "once", "option": "made-up"})),
        (selected(&json!("once")), true),
        "核心挑的不是 agent 给的照决定挑"
    );
    assert_eq!(
        reply(json!({"decision": "cancelled"})),
        (json!({"outcome": {"outcome": "cancelled"}}), false)
    );
    let missing = json!({"code": -32601, "message": "unknown method"});
    assert_eq!(
        approved(Err(&missing), &options()),
        (selected(&json!("no")), false),
        "旧核心照拒绝"
    );
}
