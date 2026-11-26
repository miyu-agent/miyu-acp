use super::*;

/// 2026-11-26 在空的家目录里起 Claude 适配器 0.112.0、`session/new` 回的（只留模式这两格）：没有设置时起在 `auto`。
fn claude() -> Value {
    let modes = [
        ("default", "Manual", "Always ask before making changes"),
        (
            "acceptEdits",
            "Accept edits",
            "Automatically accept all file edits",
        ),
        ("plan", "Plan", "Create a plan before making changes"),
        ("auto", "Auto", "Claude handles permission decisions"),
        (
            "bypassPermissions",
            "Bypass permissions",
            "Accepts all permissions",
        ),
    ];
    let available: Vec<Value> = modes
        .iter()
        .map(|(id, name, description)| json!({"id": id, "name": name, "description": description}))
        .collect();
    let options: Vec<Value> = modes
        .iter()
        .map(|(id, name, description)| json!({"value": id, "name": name, "description": description}))
        .collect();
    json!({
        "sessionId": "s1",
        "modes": {"currentModeId": "auto", "availableModes": available},
        "configOptions": [
            {"id": "model", "category": "model", "type": "select", "currentValue": "default", "options": []},
            {"id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "auto", "options": options}
        ]
    })
}

#[test]
fn the_claude_adapter_is_switched_from_auto_to_default() {
    let modes = read(&claude()).unwrap();
    assert_eq!(modes.current.as_deref(), Some("auto"));
    assert_eq!(asking(&modes), Some("default"));
    assert_eq!(
        switch("s1", &modes, "default"),
        Some((
            "session/set_mode",
            json!({"sessionId": "s1", "modeId": "default"})
        ))
    );
}

#[test]
fn a_session_already_asking_is_left_alone() {
    let mut result = claude();
    result["modes"]["currentModeId"] = json!("default");
    let modes = read(&result).unwrap();
    assert_eq!(switch("s1", &modes, "default"), None);
}

#[test]
fn gemini_style_modes_without_config_options() {
    // Gemini CLI 0.71.1 的 `buildAvailableModes`：只报老的 `modes`。
    let modes = read(
        &json!({"sessionId": "g", "modes": {"currentModeId": "yolo", "availableModes": [
            {"id": "default", "name": "Default", "description": "Prompts for approval"},
            {"id": "autoEdit", "name": "Auto Edit"},
            {"id": "yolo", "name": "YOLO"}
        ]}}),
    )
    .unwrap();
    assert_eq!(asking(&modes), Some("default"));
    assert_eq!(
        switch("g", &modes, "default").unwrap().0,
        "session/set_mode"
    );
}

#[test]
fn codex_style_config_option_only_picks_read_only() {
    // Codex 适配器 2.12.0 的模式：没有 `default`，最紧的是 `read-only`。
    let modes = read(&json!({"sessionId": "c", "configOptions": [
        {"id": "mode", "category": "mode", "type": "select", "currentValue": "agent", "options": [
            {"value": "read-only"}, {"value": "workspace-write"}, {"value": "agent"}, {"value": "agent-full-access"}
        ]}
    ]}))
    .unwrap();
    assert!(!modes.legacy);
    assert_eq!(asking(&modes), Some("read-only"));
    assert_eq!(
        switch("c", &modes, "read-only"),
        Some((
            "session/set_config_option",
            json!({"sessionId": "c", "configId": "mode", "value": "read-only"})
        ))
    );
}

#[test]
fn unknown_modes_and_no_modes() {
    assert_eq!(read(&json!({"sessionId": "x"})), None);
    let modes = read(
        &json!({"sessionId": "x", "modes": {"currentModeId": "build", "availableModes": [
            {"id": "build"}, {"id": "plan"}
        ]}}),
    )
    .unwrap();
    assert_eq!(asking(&modes), None, "build、plan 都不是每次都问");
}
