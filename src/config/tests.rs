use serde_json::json;

use super::*;

fn keys(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn own_tools_takes_the_two_choices_and_null_is_the_default() {
    let mut config = Config::default();
    assert_eq!(config.own_tools, OwnTools::MiyuOnly);
    config.apply(&keys(&json!({OWN_TOOLS: "allow"}))).unwrap();
    assert_eq!(config.own_tools, OwnTools::Allow);
    config.apply(&keys(&json!({"other.key": 1}))).unwrap();
    assert_eq!(config.own_tools, OwnTools::Allow, "只改交了的键");
    config.apply(&keys(&json!({OWN_TOOLS: null}))).unwrap();
    assert_eq!(config.own_tools, OwnTools::MiyuOnly);
    let error = config.apply(&keys(&json!({OWN_TOOLS: "all"}))).unwrap_err();
    assert!(error.contains(OWN_TOOLS), "{error}");
}

#[test]
fn seconds_are_whole_and_tool_wait_stays_in_range() {
    let mut config = Config::default();
    config
        .apply(&keys(&json!({KEEP_SECONDS: 30, TOOL_WAIT_SECONDS: 120})))
        .unwrap();
    assert_eq!(config.keep, Duration::from_secs(30));
    assert_eq!(config.tool_wait, Duration::from_mins(2));
    for bad in [json!(59), json!(86_401), json!(1.5), json!("600")] {
        let error = config
            .apply(&keys(&json!({KEEP_SECONDS: 5, TOOL_WAIT_SECONDS: bad})))
            .unwrap_err();
        assert!(error.contains(TOOL_WAIT_SECONDS), "{error}");
        assert_eq!(config.keep, Duration::from_secs(30), "出错什么都不改");
    }
    config
        .apply(&keys(&json!({KEEP_SECONDS: null, TOOL_WAIT_SECONDS: null})))
        .unwrap();
    assert_eq!(config, Config::default());
}

#[test]
fn custom_agents_take_one_command_line_with_leading_variables() {
    let mut config = Config::default();
    config
        .apply(&keys(&json!({AGENTS: [{
            "name": "my-agent",
            "command": "A=1 B='x y' /opt/agent/bin/agent --acp \"two words\" it's=fine"
        }]})))
        .unwrap_err();
    config
        .apply(&keys(&json!({AGENTS: [{
            "name": "my-agent",
            "command": "A=1 B='x y' /opt/agent/bin/agent --acp \"two \\\"words\\\"\" C=3"
        }]})))
        .unwrap();
    let agent = config.custom("my-agent").unwrap();
    assert_eq!(agent.command, PathBuf::from("/opt/agent/bin/agent"));
    assert_eq!(
        agent.args,
        ["--acp", "two \"words\"", "C=3"],
        "程序后面的 名字=值 是参数"
    );
    assert_eq!(
        agent.env,
        [("A".into(), "1".into()), ("B".into(), "x y".into())]
    );
    assert!(config.custom("other").is_none());
    config.apply(&keys(&json!({AGENTS: null}))).unwrap();
    assert!(config.agents.is_empty());
}

#[test]
fn bad_custom_agents_are_refused_with_the_key() {
    for bad in [
        json!([{"name": "Bad Name", "command": "x"}]),
        json!([{"name": "ok", "command": " "}]),
        json!([{"name": "ok", "command": "A=1"}]),
        json!([{"name": "ok", "command": "agent 'open"}]),
        json!({"name": "ok"}),
    ] {
        let mut config = Config::default();
        let error = config.apply(&keys(&json!({AGENTS: bad}))).unwrap_err();
        assert!(error.contains(AGENTS), "{error}");
        assert_eq!(config, Config::default());
    }
}

#[test]
fn a_leading_tilde_is_the_home_folder() {
    let home = Some(OsStr::new("/home/alice"));
    for (written, expected) in [
        ("~/bin/agent", "/home/alice/bin/agent"),
        ("/usr/bin/agent", "/usr/bin/agent"),
        ("~bob/agent", "~bob/agent"),
        ("agent", "agent"),
    ] {
        assert_eq!(
            expand_home(Path::new(written), home),
            PathBuf::from(expected)
        );
    }
    assert_eq!(
        expand_home(Path::new("~/agent"), None),
        PathBuf::from("~/agent")
    );
}
