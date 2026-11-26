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
            assert_eq!(
                judge(own, &call, &names, &seen, false),
                Verdict::Allow,
                "{call}"
            );
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
            judge(OwnTools::MiyuOnly, &call, &names, &none, true),
            Verdict::Reject
        );
        assert_eq!(
            judge(OwnTools::Allow, &call, &names, &none, true),
            Verdict::Ask
        );
        assert_eq!(
            judge(OwnTools::Allow, &call, &names, &none, false),
            Verdict::Reject,
            "问不了的按拒绝"
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
fn the_question_names_the_agent_the_tool_and_the_options() {
    let call = json!({"title": "rm -rf build", "kind": "execute", "rawInput": {"command": "rm -rf build"}});
    let english = question("Claude Agent", &call, &options(), "en");
    let text = english["questions"][0]["question"].as_str().unwrap();
    assert!(
        text.contains("Claude Agent") && text.contains("rm -rf build") && text.contains("sandbox"),
        "{text}"
    );
    assert_eq!(
        english["questions"][0]["options"],
        json!([{"label": "Always allow"}, {"label": "Allow"}, {"label": "Reject"}])
    );
    let chinese = question("Claude Agent", &call, &options(), "zh");
    assert!(
        chinese["questions"][0]["question"]
            .as_str()
            .unwrap()
            .contains("沙盒")
    );
}

#[test]
fn the_answer_is_read_back_from_the_tool_result() {
    let answer = "\"Claude Agent wants …\" = Allow\n\nGo on with these answers in mind.";
    assert_eq!(picked(answer, &options()), Some((json!("once"), true)));
    let always = "\"q\" = Always allow";
    assert_eq!(
        picked(always, &options()),
        Some((json!("always"), true)),
        "取最长的那个名字"
    );
    assert_eq!(
        picked("\"q\" = Reject", &options()),
        Some((json!("no"), false))
    );
    assert_eq!(picked("\"q\" = do it later", &options()), None);
    assert_eq!(
        picked("No one can answer questions here.", &options()),
        None
    );
}
