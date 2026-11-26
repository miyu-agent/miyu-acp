use super::*;

#[test]
fn initialize_echoes_the_clients_version_and_defaults_otherwise() {
    let result = initialize_result(&json!({"protocolVersion": "2025-11-25", "capabilities": {}}));
    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert_eq!(result["capabilities"], json!({"tools": {}}));
    assert_eq!(result["serverInfo"]["name"], "miyu-acp");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        initialize_result(&json!({"protocolVersion": "2024-11-05"}))["protocolVersion"],
        "2024-11-05"
    );
    assert_eq!(
        initialize_result(&Value::Null)["protocolVersion"],
        DEFAULT_PROTOCOL
    );
    assert_eq!(
        initialize_result(&json!({"protocolVersion": 5}))["protocolVersion"],
        DEFAULT_PROTOCOL
    );
}

#[test]
fn the_bridge_needs_both_variables_non_empty() {
    use std::ffi::OsString;
    let path = || Some(OsString::from("/run/a.sock"));
    let token = || Some("t".to_string());
    assert_eq!(
        Bridge::from_values(path(), token()),
        Some(Bridge {
            path: PathBuf::from("/run/a.sock"),
            token: "t".into()
        })
    );
    assert_eq!(Bridge::from_values(None, token()), None);
    assert_eq!(Bridge::from_values(path(), None), None);
    assert_eq!(Bridge::from_values(Some(OsString::new()), token()), None);
    assert_eq!(Bridge::from_values(path(), Some(String::new())), None);
}

#[test]
fn without_a_bridge_the_list_is_empty_and_a_call_is_a_tool_failure() {
    assert_eq!(list_result(None), json!({"tools": []}));
    assert_eq!(
        call_result(None, "bash", &json!({})),
        json!({"content": [{"type": "text", "text": words::MCP_NO_BRIDGE}], "isError": true})
    );
}
