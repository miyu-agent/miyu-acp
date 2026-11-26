//! 出错、打断：`stopReason` 各种、要登录、密钥登录、agent 报错归类、agent 退了、取消。

use std::time::Duration;

use serde_json::json;

use crate::support::{Core, request};

#[test]
fn stop_reasons_end_the_call_as_the_core_expects() {
    let mut core = Core::start("stops", "fake", "", &json!({}), &[]);
    for (reason, check) in [
        ("max_tokens", "truncated"),
        ("max_turn_requests", "truncated"),
        ("refusal", "content_policy"),
        ("cancelled", "retryable"),
        ("end_turn", "ok"),
    ] {
        let got = core.ask(reason, &request("S", &format!("STOP:{reason}"), &json!([])));
        match check {
            "truncated" => {
                assert!(
                    got.deltas.contains(&json!("truncated")),
                    "{reason}: {:?}",
                    got.deltas
                );
                assert!(got.result().get("error").is_none());
            }
            "ok" => assert!(got.result().get("error").is_none(), "{reason}"),
            class => assert_eq!(got.result()["error"]["class"], class, "{reason}"),
        }
    }
}

#[test]
fn an_agent_that_wants_a_login_gets_a_clear_auth_error() {
    let mut core = Core::start("login", "fake", "auth", &json!({}), &[]);
    let got = core.ask("c1", &request("S", "hi", &json!([])));
    let error = &got.result()["error"];
    assert_eq!(error["class"], "auth", "{}", got.response);
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("Fake Agent needs you to log in first"),
        "{message}"
    );
    assert!(
        message.contains("Log in with Google") && message.contains("API key"),
        "{message}"
    );
    assert!(core.got("authenticate").is_empty(), "不替主人登录");
}

#[test]
fn a_provider_key_logs_in_through_the_api_key_method() {
    let mut core = Core::start("key", "fake", "auth", &json!({}), &[]);
    core.call_with(
        "c1",
        "c1",
        &request("S", "hi", &json!([])),
        "fake",
        &json!({"key": "secret-key"}),
    );
    let got = core.collect("c1", "c1");
    assert!(got.result().get("error").is_none(), "{}", got.response);
    let auth = &core.got("authenticate")[0];
    assert_eq!(auth["methodId"], "gemini-api-key");
    assert_eq!(auth["_meta"]["api-key"], "secret-key");
}

#[test]
fn a_login_required_during_the_turn_is_auth_too() {
    let mut core = Core::start("login2", "fake", "", &json!({}), &[]);
    let got = core.ask("c1", &request("S", "LOGIN", &json!([])));
    assert_eq!(got.result()["error"]["class"], "auth");
}

#[test]
fn agent_errors_are_classified_by_their_words() {
    let mut core = Core::start("errors", "fake", "", &json!({}), &[]);
    for (said, class) in [
        ("Quota exceeded for this account", "quota"),
        (
            "prompt is too long: 300000 tokens > 200000 maximum",
            "context_too_long",
        ),
        ("rate limit reached", "rate_limited"),
        ("something odd", "retryable"),
    ] {
        let got = core.ask(said, &request("S", &format!("FAIL: {said}"), &json!([])));
        assert_eq!(got.result()["error"]["class"], class, "{said}");
        assert!(
            got.result()["error"]["message"]
                .as_str()
                .unwrap()
                .contains(said)
        );
    }
}

#[test]
fn an_agent_that_exits_mid_turn_is_retryable_and_the_next_call_starts_fresh() {
    let mut core = Core::start("exit", "fake", "", &json!({}), &[]);
    let got = core.ask("c1", &request("S", "EXIT", &json!([])));
    assert_eq!(
        got.result()["error"]["class"],
        "retryable",
        "{}",
        got.response
    );
    let again = core.ask("c2", &request("S", "hi", &json!([])));
    assert!(again.result().get("error").is_none());
    assert_eq!(core.got("initialize").len(), 2);
}

#[test]
fn cancelling_sends_session_cancel_and_drops_the_session() {
    let mut core = Core::start("cancel", "fake", "", &json!({}), &[]);
    core.call_with(
        "c1",
        "c1",
        &request("S", "SLOW", &json!([])),
        "fake",
        &json!({}),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while core.got("session/prompt").is_empty() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    core.send(&json!({"jsonrpc": "2.0", "method": "driver.cancel", "params": {"call": "c1"}}));
    let got = core.collect("c1", "c1");
    assert_eq!(got.result()["error"]["class"], "retryable");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while core.got("session/cancel").is_empty() {
        assert!(std::time::Instant::now() < deadline, "没发 session/cancel");
        std::thread::sleep(Duration::from_millis(20));
    }
}
