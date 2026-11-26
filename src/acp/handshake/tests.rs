use std::path::PathBuf;

use super::*;

/// 2026-11-26 实测 Antigravity ACP 服务端 1.5.2 回的 `initialize`（只留这里用得上的几格）。
fn antigravity() -> Value {
    json!({"protocolVersion": 1, "agentCapabilities": {
        "loadSession": true,
        "promptCapabilities": {"image": true, "audio": true, "embeddedContext": true},
        "mcpCapabilities": {"http": true, "sse": true},
        "sessionCapabilities": {"list": {}, "additionalDirectories": {}, "resume": {}},
        "_meta": {"antigravity": {"tier": true, "systemPrompt": {"append": true}}}
    }, "authMethods": [
        {"id": "oauth-personal", "name": "Log in with Google"},
        {"id": "gemini-api-key", "name": "Gemini API key"}
    ], "agentInfo": {"name": "antigravity-acp", "title": "Google Antigravity", "version": "1.5.2"}})
}

/// 同一天实测 Claude 适配器 0.112.0 回的。
fn claude() -> Value {
    json!({"protocolVersion": 1, "agentCapabilities": {
        "promptCapabilities": {"image": true, "embeddedContext": true},
        "sessionCapabilities": {"systemPrompt": {"mode": {"override": {}}}, "fork": {}, "close": {}, "resume": {}}
    }, "authMethods": [], "agentInfo": {"name": "claude-agent-acp"}})
}

#[test]
fn the_client_offers_no_files_and_no_terminal() {
    let params = initialize_params();
    assert_eq!(params["protocolVersion"], 1);
    assert_eq!(params["clientCapabilities"]["fs"]["readTextFile"], false);
    assert_eq!(params["clientCapabilities"]["fs"]["writeTextFile"], false);
    assert_eq!(params["clientCapabilities"]["terminal"], false);
    assert!(params["clientCapabilities"].get("auth").is_none());
}

#[test]
fn capabilities_are_read_from_the_two_real_answers() {
    let caps = read_caps(&antigravity());
    assert_eq!(caps.system, SystemMode::AntigravityMeta);
    assert!(caps.resume && caps.prompt.image && caps.prompt.audio && caps.prompt.embedded);
    assert_eq!(caps.title.as_deref(), Some("Google Antigravity"));
    assert_eq!(api_key_method(&caps).unwrap().id, "gemini-api-key");

    let caps = read_caps(&claude());
    assert_eq!(caps.system, SystemMode::Override);
    assert!(caps.resume && caps.prompt.image && !caps.prompt.audio);
    assert!(caps.auth_methods.is_empty() && api_key_method(&caps).is_none());
    assert_eq!(caps.title.as_deref(), Some("claude-agent-acp"));

    let append =
        read_caps(&json!({"agentCapabilities": {"sessionCapabilities": {"systemPrompt": {}}}}));
    assert_eq!(append.system, SystemMode::Append);
    let none =
        read_caps(&json!({"agentCapabilities": {"sessionCapabilities": {"systemPrompt": null}}}));
    assert_eq!(none.system, SystemMode::InPrompt);
    assert!(!none.resume);
}

#[test]
fn the_system_prompt_goes_where_the_agent_takes_it() {
    let cwd = PathBuf::from("/tmp/work");
    let over = session_params(
        &cwd,
        None,
        &read_caps(&claude()),
        "SYS",
        Family::Claude,
        OwnTools::MiyuOnly,
    );
    assert_eq!(
        over["systemPrompt"],
        json!({"mode": "override", "content": [{"type": "text", "text": "SYS"}]})
    );
    assert_eq!(
        over["_meta"]["claudeCode"]["options"],
        json!({"allowDangerouslySkipPermissions": false, "tools": ["WebSearch", "WebFetch"]})
    );
    assert_eq!(over["mcpServers"], json!([]));
    assert_eq!(over["cwd"], "/tmp/work");

    let agy = session_params(
        &cwd,
        None,
        &read_caps(&antigravity()),
        "SYS",
        Family::Antigravity,
        OwnTools::MiyuOnly,
    );
    assert!(agy.get("systemPrompt").is_none());
    assert_eq!(
        agy["_meta"]["antigravity"],
        json!({"systemPrompt": {"append": "SYS"}, "eagerMcpServers": ["miyu"], "enabledTools": ["search_web"]})
    );

    let mut caps = read_caps(&claude());
    caps.system = SystemMode::Append;
    let other = session_params(&cwd, None, &caps, "SYS", Family::Other, OwnTools::MiyuOnly);
    assert_eq!(other["systemPrompt"]["mode"], "append");
    assert!(other.get("_meta").is_none());
    caps.system = SystemMode::InPrompt;
    assert!(
        session_params(&cwd, None, &caps, "SYS", Family::Other, OwnTools::MiyuOnly)
            .get("systemPrompt")
            .is_none()
    );
}

#[test]
fn letting_the_agent_s_own_tools_run_drops_the_restrictions_but_never_bypasses_asking() {
    let cwd = PathBuf::from("/w");
    let open = session_params(
        &cwd,
        None,
        &read_caps(&claude()),
        "S",
        Family::Claude,
        OwnTools::Allow,
    );
    assert_eq!(
        open["_meta"],
        json!({"claudeCode": {"options": {"allowDangerouslySkipPermissions": false}}}),
        "自带的工具不关，但不许照主人本机的设置跳过权限"
    );
    let agy = session_params(
        &cwd,
        None,
        &read_caps(&antigravity()),
        "S",
        Family::Antigravity,
        OwnTools::Allow,
    );
    assert!(agy["_meta"]["antigravity"].get("enabledTools").is_none());
}

#[test]
fn the_mcp_entry_carries_the_socket_and_token() {
    let entry = mcp_server(
        Path::new("/bin/miyu-acp"),
        Path::new("/run/acp-1.sock"),
        "tok",
    );
    assert_eq!(entry["name"], "miyu");
    assert_eq!(entry["args"], json!(["mcp"]));
    assert_eq!(
        entry["env"][0],
        json!({"name": "MIYU_ACP_BRIDGE", "value": "/run/acp-1.sock"})
    );
    assert_eq!(
        entry["env"][1],
        json!({"name": "MIYU_ACP_TOKEN", "value": "tok"})
    );
    let caps = read_caps(&claude());
    let params = session_params(
        Path::new("/w"),
        Some(entry.clone()),
        &caps,
        "S",
        Family::Other,
        OwnTools::Allow,
    );
    assert_eq!(params["mcpServers"], json!([entry]));
}

#[test]
fn models_come_from_config_options_or_the_older_models_field() {
    let found = read_session(&json!({"sessionId": "s1", "configOptions": [
        {"id": "mode", "category": "mode", "type": "select", "currentValue": "a", "options": [{"value": "a", "name": "A"}]},
        {"id": "model", "category": "model", "type": "select", "currentValue": "opus",
         "options": [{"group": "g", "name": "G", "options": [{"value": "opus", "name": "Opus"}, {"value": "sonnet", "name": "Sonnet"}]}]}
    ]}))
    .unwrap();
    assert_eq!(found.models, ["opus", "sonnet"]);
    assert_eq!(found.current.as_deref(), Some("opus"));
    assert_eq!(
        select_model(&found, "sonnet"),
        (
            "session/set_config_option",
            json!({"sessionId": "s1", "configId": "model", "value": "sonnet"})
        )
    );
    let old = read_session(&json!({"sessionId": "s2", "models": {"currentModelId": "m1", "availableModels": [{"modelId": "m1"}, {"modelId": "m2"}]}})).unwrap();
    assert_eq!(old.models, ["m1", "m2"]);
    assert_eq!(select_model(&old, "m2").0, "session/set_model");
    assert!(read_session(&json!({})).is_none());
}

#[test]
fn an_api_key_rides_in_meta() {
    let method = AuthMethod {
        id: "gemini-api-key".into(),
        name: "Gemini API key".into(),
    };
    assert_eq!(
        api_key_params(&method, "k"),
        json!({"methodId": "gemini-api-key", "_meta": {"api-key": "k"}})
    );
}
