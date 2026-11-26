use super::*;

fn turn() -> Turn {
    Turn::new(
        Names::new(["read".to_string()]),
        "Agent needs you to log in first.",
    )
}

fn chunk(kind: &str, text: &str, message: Option<&str>) -> Value {
    let mut update = json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}});
    if let Some(message) = message {
        update["messageId"] = json!(message);
    }
    update
}

fn all(turn: &mut Turn, updates: &[Value]) -> Vec<Value> {
    updates
        .iter()
        .flat_map(|update| turn.update(update).deltas)
        .collect()
}

#[test]
fn text_and_thoughts_become_blocks_split_by_kind_and_message() {
    let mut turn = turn();
    let deltas = all(
        &mut turn,
        &[
            chunk("agent_thought_chunk", "hmm", None),
            chunk("agent_message_chunk", "Hel", Some("m1")),
            chunk("agent_message_chunk", "lo", Some("m1")),
            chunk("agent_message_chunk", "", Some("m1")),
            chunk("agent_message_chunk", "Again", Some("m2")),
        ],
    );
    assert_eq!(
        deltas,
        [
            json!({"start": {"index": 0, "kind": "reasoning"}}),
            json!({"text": {"index": 0, "text": "hmm"}}),
            json!({"end": {"index": 0}}),
            json!({"start": {"index": 1, "kind": "text"}}),
            json!({"text": {"index": 1, "text": "Hel"}}),
            json!({"text": {"index": 1, "text": "lo"}}),
            json!({"end": {"index": 1}}),
            json!({"start": {"index": 2, "kind": "text"}}),
            json!({"text": {"index": 2, "text": "Again"}}),
        ]
    );
    let fed = turn.finish(&json!({"stopReason": "end_turn"}));
    assert_eq!(fed.deltas, [json!({"end": {"index": 2}})]);
    assert_eq!(fed.answer, Some(json!({})), "没有记账的数不报用量");
}

#[test]
fn usage_comes_from_the_answer_and_context_from_the_last_usage_update() {
    let mut turn = turn();
    turn.update(&json!({"sessionUpdate": "usage_update", "used": 1000, "size": 200_000}));
    turn.update(&json!({"sessionUpdate": "usage_update", "used": 5300, "size": 200_000, "cost": {"amount": 0.1, "currency": "USD"}}));
    assert_eq!(turn.window(), Some(200_000));
    let fed = turn.finish(&json!({"stopReason": "end_turn", "usage": {
        "inputTokens": 10, "outputTokens": 20, "cachedReadTokens": 5000, "cachedWriteTokens": 3, "thoughtTokens": 7, "totalTokens": 5040
    }}));
    assert_eq!(
        fed.answer,
        Some(json!({"usage": {
            "uncached": 10, "cache_read": 5000, "cache_write": 3, "output": 20, "reasoning": 7, "context": 5300
        }}))
    );
}

#[test]
fn stop_reasons_are_classified() {
    for (reason, check) in [
        ("max_tokens", "truncated"),
        ("max_turn_requests", "truncated"),
        ("refusal", "content_policy"),
        ("cancelled", "retryable"),
    ] {
        let mut turn = turn();
        let fed = turn.finish(&json!({"stopReason": reason}));
        match check {
            "truncated" => {
                assert_eq!(fed.deltas, [json!("truncated")], "{reason}");
                assert!(fed.answer.unwrap().get("error").is_none());
            }
            class => assert_eq!(fed.answer.unwrap()["error"]["class"], class, "{reason}"),
        }
    }
}

#[test]
fn a_rejected_prompt_is_auth_when_login_is_required_and_classified_otherwise() {
    let mut turn = turn();
    let login = RpcError {
        code: -32000,
        message: "Authentication required".into(),
        data: None,
    };
    let answer = turn.rejected(&login).answer.unwrap();
    assert_eq!(answer["error"]["class"], "auth");
    assert_eq!(
        answer["error"]["message"],
        "Agent needs you to log in first."
    );
    let quota = RpcError {
        code: -32603,
        message: "Quota exceeded".into(),
        data: Some(json!("RESOURCE_EXHAUSTED")),
    };
    assert_eq!(
        turn.rejected(&quota).answer.unwrap()["error"]["class"],
        "quota"
    );
}

#[test]
fn a_miyu_tool_is_handed_back_once_both_sides_arrive() {
    let mut turn = turn();
    all(&mut turn, &[chunk("agent_message_chunk", "Reading.", None)]);
    let first = turn.update(&json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "name": "mcp__miyu__read", "title": "mcp__miyu__read", "kind": "other", "status": "pending"}));
    assert_eq!(first.deltas, [json!({"end": {"index": 0}})]);
    assert_eq!(turn.waiting(), Some(Waiting::ForMcp));
    assert!(turn.miyu_calls().contains("t1"));
    let fed = turn.tool_call(1, "read", &json!({"path": "a.txt"}));
    assert_eq!(
        fed.deltas,
        [
            json!({"start": {"index": 1, "kind": {"tool_call": {"name": "read"}}}}),
            json!({"text": {"index": 1, "text": "{\"path\":\"a.txt\"}"}}),
            json!({"private": {"index": 1, "data": {"mcp": 1}}}),
            json!({"end": {"index": 1}}),
        ]
    );
    assert_eq!(fed.handed, Some(Hand::Mcp(1)));
    assert_eq!(fed.answer, Some(json!({})));
    assert_eq!(turn.waiting(), None);
}

#[test]
fn only_the_mcp_side_is_handed_back_at_the_timeout_and_only_the_stream_side_fails() {
    let mut mcp_only = turn();
    mcp_only.tool_call(4, "read", &json!({}));
    assert_eq!(mcp_only.waiting(), Some(Waiting::ForStream));
    assert_eq!(mcp_only.timeout().handed, Some(Hand::Mcp(4)));
    let mut stream_only = turn();
    stream_only.update(
        &json!({"sessionUpdate": "tool_call", "toolCallId": "t", "name": "mcp__miyu__read"}),
    );
    let answer = stream_only.timeout().answer.unwrap();
    assert_eq!(answer["error"]["class"], "unclassified");
}

#[test]
fn the_agent_s_own_tools_become_hosted_blocks_with_late_input_and_outcome() {
    let mut turn = turn();
    let deltas = all(
        &mut turn,
        &[
            json!({"sessionUpdate": "tool_call", "toolCallId": "w", "name": "WebSearch", "title": "Search", "kind": "search", "status": "pending", "rawInput": {}}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "w", "rawInput": {"query": "rust"}}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "w", "status": "completed", "content": [{"type": "content", "content": {"type": "text", "text": "1. Rust"}}]}),
            json!({"sessionUpdate": "tool_call", "toolCallId": "x", "kind": "execute", "title": "ls", "status": "failed", "rawOutput": "denied"}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "w", "status": "completed"}),
        ],
    );
    assert_eq!(
        deltas,
        [
            json!({"start": {"index": 0, "kind": {"hosted_tool": {"name": "WebSearch"}}}}),
            json!({"text": {"index": 0, "text": "{\"query\":\"rust\"}"}}),
            json!({"outcome": {"index": 0, "text": "1. Rust"}}),
            json!({"end": {"index": 0}}),
            json!({"start": {"index": 1, "kind": {"hosted_tool": {"name": "execute"}}}}),
            json!({"outcome": {"index": 1, "text": "denied", "failed": true}}),
            json!({"end": {"index": 1}}),
        ]
    );
}

#[test]
fn text_closes_an_open_hosted_block_without_an_outcome() {
    let mut turn = turn();
    let deltas = all(
        &mut turn,
        &[
            json!({"sessionUpdate": "tool_call", "toolCallId": "w", "name": "WebFetch", "kind": "fetch"}),
            chunk("agent_message_chunk", "done", None),
        ],
    );
    assert_eq!(deltas[1], json!({"end": {"index": 0}}));
    assert_eq!(deltas[2], json!({"start": {"index": 1, "kind": "text"}}));
}

#[test]
fn asking_the_owner_hands_back_an_ask_user_call() {
    let mut turn = turn();
    let fed = turn.ask(2, &json!({"questions": []}));
    assert_eq!(fed.handed, Some(Hand::Permission(2)));
    assert_eq!(
        fed.deltas[0],
        json!({"start": {"index": 0, "kind": {"tool_call": {"name": "ask_user"}}}})
    );
    assert_eq!(
        fed.deltas[2],
        json!({"private": {"index": 0, "data": {"permission": 2}}})
    );
}

#[test]
fn a_broken_stream_is_retryable_until_something_was_delivered() {
    let mut turn = turn();
    assert_eq!(turn.broken("gone")["error"]["class"], "retryable");
    all(&mut turn, &[chunk("agent_message_chunk", "x", None)]);
    assert_eq!(turn.broken("gone")["error"]["class"], "unclassified");
}

#[test]
fn what_was_seen_carries_over_to_the_next_call_of_the_same_prompt() {
    let mut first = turn();
    first.update(
        &json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "name": "mcp__miyu__read"}),
    );
    first.tool_call(1, "read", &json!({}));
    first.update(&json!({"sessionUpdate": "tool_call", "toolCallId": "w", "name": "WebSearch", "kind": "search"}));
    let mut next = Turn::resume(Names::new(["read".to_string()]), "", first.memory());
    let deltas = all(
        &mut next,
        &[
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "w", "status": "completed"}),
        ],
    );
    assert!(deltas.is_empty(), "{deltas:?}");
}
