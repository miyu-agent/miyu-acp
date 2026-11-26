//! Miyu 的工具：经 `mcpServers` 交给 agent（名字 `miyu`），agent 一调就交回核心，下一次请求带着结果来再回给 agent、接着说。

use serde_json::json;

use crate::support::{Core, handed_blocks, request, then, tool, tool_result};

#[test]
fn a_miyu_tool_goes_back_to_the_core_and_its_result_reaches_the_agent() {
    let mut core = Core::start("tool", "fake", "", &json!({}), &[]);
    let first = request(
        "S",
        r#"TOOL:read {"path": "a.txt"}"#,
        &json!([tool("read"), tool("shell")]),
    );
    let handed = core.ask("c1", &first);
    assert!(
        handed.result().get("error").is_none(),
        "{}",
        handed.response
    );
    assert_eq!(
        handed.text().split("calling").count(),
        2,
        "交回之前的正文先交了"
    );
    let calls = handed.starts("tool_call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["kind"]["tool_call"]["name"], "read");
    let blocks = handed_blocks(&handed, "call_1");
    assert_eq!(blocks[0]["args"], r#"{"path":"a.txt"}"#);
    assert_eq!(blocks[0]["private"]["data"], json!({"mcp": 1}));
    // 交给 agent 的工具表、mcpServers 那一条。
    let new = &core.got("session/new")[0];
    assert_eq!(new["mcpServers"][0]["name"], "miyu");
    assert_eq!(new["mcpServers"][0]["args"], json!(["mcp"]));
    let names: Vec<String> = core.noted("tools")[0]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["read", "shell"]);

    let second = then(&first, &blocks, tool_result("call_1", "file contents"));
    let done = core.ask("c2", &second);
    assert_eq!(
        done.text(),
        "got(yes): file contents",
        "Miyu 的工具要权限时放行"
    );
    assert!(
        done.starts("hosted_tool").is_empty(),
        "交回过的那件的进度不当成对方的工具画：{:?}",
        done.deltas
    );
    assert!(
        done.result()["usage"].is_object(),
        "这一轮的用量记在最后一次"
    );
    assert_eq!(core.got("session/new").len(), 1, "接着同一个会话");
    assert_eq!(
        core.got("session/prompt").len(),
        1,
        "回的是工具结果，不是新的提问"
    );
}

#[test]
fn a_result_that_does_not_match_the_pending_call_starts_over() {
    let mut core = Core::start("mismatch", "fake", "", &json!({}), &[]);
    let first = request(
        "S",
        r#"TOOL:read {"path": "a.txt"}"#,
        &json!([tool("read")]),
    );
    let handed = core.ask("c1", &first);
    let mut blocks = handed_blocks(&handed, "call_1");
    blocks[0]["private"]["data"] = json!({"mcp": 99});
    let second = then(&first, &blocks, tool_result("call_1", "x"));
    core.ask("c2", &second);
    assert_eq!(core.got("session/new").len(), 2, "对不上的作废、新开");
    let text = core.got("session/prompt")[1]["prompt"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("[called tool read with"), "{text}");
}

#[test]
fn gemini_family_agents_get_shaped_schemas_and_antigravity_gets_tool_action() {
    let mut core = Core::start(
        "gemini",
        "antigravity-acp",
        "sys=antigravity",
        &json!({}),
        &[],
    );
    let mut odd = tool("pick");
    odd["parameters"] = json!({"type": "object", "properties": {"site": {"type": "string", "enum": ["a", ""]}, "extra": {"anyOf": []}}});
    let first = request(
        "S",
        r#"TOOL:pick {"site": "a", "toolAction": "pick a site"}"#,
        &json!([odd]),
    );
    let handed = core.call_and_collect("antigravity-acp", &first);
    let args = handed_blocks(&handed, "c")[0]["args"].clone();
    assert_eq!(
        args, r#"{"site":"a"}"#,
        "多声明的 toolAction 交回核心之前去掉"
    );
    let schema = &core.noted("tools")[0][0]["inputSchema"];
    assert_eq!(
        schema["properties"]["site"]["enum"],
        json!(["a"]),
        "{schema}"
    );
    assert!(schema["properties"]["toolAction"].is_object(), "{schema}");
    assert!(
        schema["properties"]["extra"].get("anyOf").is_none(),
        "{schema}"
    );
}
