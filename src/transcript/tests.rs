use serde_json::{Value, json};

use std::collections::BTreeSet;

use super::*;

fn user(text: &str) -> Value {
    json!({"role": "user", "blocks": [{"type": "text", "text": text}]})
}

fn assistant(text: &str) -> Value {
    json!({"role": "assistant", "blocks": [{"type": "text", "text": text}]})
}

fn request(messages: &[Value]) -> Value {
    json!({"tools": [], "system": "s", "messages": messages, "stable": 0})
}

#[test]
fn a_single_user_message_goes_as_its_own_text() {
    let got = transcribe(&request(&[user("hello there")]), BUDGET).unwrap();
    assert_eq!(got, "hello there");
}

#[test]
fn history_is_wrapped_and_the_last_turn_follows_it() {
    let got = transcribe(
        &request(&[
            user("q1"),
            assistant("a1"),
            user("q2"),
            user("runtime note"),
        ]),
        BUDGET,
    )
    .unwrap();
    assert_eq!(
        got,
        format!(
            "{}User:\nq1\n\nAssistant:\na1\n\n</conversation-history>\n\nq2\n\nruntime note",
            words::HISTORY_HEAD
        )
    );
}

#[test]
fn tool_calls_results_and_reasoning() {
    let messages = vec![
        user("list files"),
        json!({"role": "assistant", "blocks": [
            {"type": "reasoning", "text": "secret thoughts"},
            {"type": "text", "text": "Let me look."},
            {"type": "tool_call", "call_id": "c_1", "name": "ls", "args": "{\"path\":\".\"}"}
        ]}),
        json!({"role": "tool", "call_id": "c_1", "error": false, "blocks": [{"type": "text", "text": "a.txt"}]}),
        json!({"role": "tool", "call_id": "c_2", "error": true, "blocks": [{"type": "text", "text": "denied"}]}),
        json!({"role": "assistant", "blocks": [
            {"type": "hosted_tool", "call_id": "c_3", "name": "search_web", "args": "{\"query\":\"rust\"}",
             "outcome": {"text": "1. Rust https://www.rust-lang.org"}},
            {"type": "hosted_tool", "call_id": "c_4", "name": "search_web", "args": "{}",
             "outcome": {"text": "boom", "failed": true}},
            {"type": "hosted_tool", "call_id": "c_5", "name": "search_web", "args": "{}"}
        ]}),
        user("thanks"),
    ];
    let got = transcribe(&request(&messages), BUDGET).unwrap();
    assert!(!got.contains("secret thoughts"), "思考不写");
    let body = got
        .strip_prefix(words::HISTORY_HEAD)
        .unwrap()
        .strip_suffix("</conversation-history>\n\nthanks")
        .unwrap();
    assert_eq!(
        body,
        "User:\nlist files\n\n\
         Assistant:\nLet me look.\n[called tool ls with {\"path\":\".\"}]\n\n\
         [tool result]\na.txt\n\n\
         [tool result, failed]\ndenied\n\n\
         Assistant:\n[provider tool search_web with {\"query\":\"rust\"}]\n[provider tool result]\n1. Rust https://www.rust-lang.org\n\
         [provider tool search_web with {}]\n[provider tool failed]\nboom\n\
         [provider tool search_web with {}]\n\n"
    );
}

#[test]
fn images_files_and_paths_become_placeholders() {
    let blob = format!("sha256:{}", "a".repeat(64));
    let other = format!("sha256:{}", "b".repeat(64));
    let mut req = request(&[json!({"role": "user", "blocks": [
        {"type": "text", "text": "look"},
        {"type": "image", "blob": blob, "name": "sunset.png", "media_type": "image/png", "width": 1, "height": 1},
        {"type": "image", "blob": other, "media_type": "image/png", "width": 1, "height": 1},
        {"type": "file", "blob": other, "name": "report.pdf", "media_type": "application/pdf"},
        {"type": "path", "name": "disk.iso", "path": "/home/a/disk.iso", "size": 9},
        {"type": "future_kind", "x": 1}
    ]})]);
    req["described"] = json!({ blob.clone(): "A red sky over the sea." });
    let got = transcribe(&req, BUDGET).unwrap();
    assert_eq!(
        got,
        "look\n\
         [image \"sunset.png\", described: A red sky over the sea.]\n\
         [image omitted: this model reads text only]\n\
         [file \"report.pdf\" omitted: this model reads text only]\n\
         [file \"disk.iso\" attached by path /home/a/disk.iso; its content is not included]"
    );
}

#[test]
fn tail_goes_last() {
    let mut req = request(&[user("hi")]);
    req["tail"] = json!("<reminder/>");
    assert_eq!(transcribe(&req, BUDGET).unwrap(), "hi\n\n<reminder/>");
    let mut req = request(&[user("q"), assistant("a"), user("q2")]);
    req["tail"] = json!("<reminder/>");
    assert!(
        transcribe(&req, BUDGET)
            .unwrap()
            .ends_with("</conversation-history>\n\nq2\n\n<reminder/>")
    );
}

#[test]
fn a_trailing_tool_result_asks_to_continue() {
    let messages = vec![
        user("q"),
        json!({"role": "assistant", "blocks": [{"type": "tool_call", "call_id": "c_1", "name": "ls", "args": "{}"}]}),
        json!({"role": "tool", "call_id": "c_1", "error": false, "blocks": [{"type": "text", "text": "x"}]}),
    ];
    let got = transcribe(&request(&messages), BUDGET).unwrap();
    assert!(got.ends_with(&format!(
        "[tool result]\nx\n\n</conversation-history>\n\n{}",
        words::CONTINUE
    )));
}

#[test]
fn budget_drops_the_oldest_turns_and_never_the_last_one() {
    let mut messages = Vec::new();
    for index in 0..40 {
        messages.push(user(&format!("q{index} {}", "问".repeat(300))));
        messages.push(assistant(&format!("a{index} {}", "答".repeat(300))));
    }
    messages.push(user("the real question"));
    let req = request(&messages);
    let whole = transcribe(&req, BUDGET).unwrap();
    let budget = 20_000;
    assert!(whole.len() > budget, "夹具要先超预算：{}", whole.len());
    assert!(!whole.contains(words::OMITTED));

    let got = transcribe(&req, budget).unwrap();
    assert!(got.len() <= budget, "{} > {budget}", got.len());
    assert!(got.ends_with("</conversation-history>\n\nthe real question"));
    let body = got
        .strip_prefix(words::HISTORY_HEAD)
        .unwrap()
        .strip_prefix(words::OMITTED)
        .expect("写了省略");
    assert!(body.starts_with("User:\nq"), "从一轮的开头起：{body:.40}");
    assert!(!got.contains("q0 "), "最老的丢了");
    assert!(got.contains("q39 ") && got.contains("a39 "), "最新的在");

    // 最后一轮自己就超了：照发，历史全丢、只留省略。
    let tight = transcribe(&req, 10).unwrap();
    assert_eq!(
        tight,
        format!(
            "{}{}</conversation-history>\n\nthe real question",
            words::HISTORY_HEAD,
            words::OMITTED
        )
    );
}

#[test]
fn budget_exactly_fits_keeps_everything() {
    let req = request(&[user("q"), assistant("a"), user("q2")]);
    let whole = transcribe(&req, BUDGET).unwrap();
    assert_eq!(transcribe(&req, whole.len()).unwrap(), whole);
    let less = transcribe(&req, whole.len() - 1).unwrap();
    assert!(less.contains(words::OMITTED));
    assert!(less.len() <= whole.len() - 1 + words::OMITTED.len());
}

#[test]
fn bad_shapes_are_errors() {
    assert!(transcribe(&json!({}), BUDGET).is_err());
    assert!(transcribe(&request(&[json!({"role": "system", "blocks": []})]), BUDGET).is_err());
    assert!(transcribe(&request(&[json!({"role": "user"})]), BUDGET).is_err());
}

#[test]
fn a_new_turn_is_one_text_per_new_message_with_the_tail_last() {
    let mut req = request(&[
        user("q1"),
        assistant("a1"),
        user("q2"),
        json!({"role": "user", "blocks": [{"type": "text", "text": "line"}, {"type": "path", "name": "a.txt", "path": "/w/a.txt"}]}),
    ]);
    assert_eq!(
        new_turn(&req, 2).unwrap(),
        [
            "q2".to_string(),
            format!("line\n{}", words::path_attached("a.txt", "/w/a.txt"))
        ]
    );
    req["tail"] = json!("<note/>");
    assert_eq!(
        new_turn(&req, 3).unwrap(),
        [format!(
            "line\n{}\n\n<note/>",
            words::path_attached("a.txt", "/w/a.txt")
        )]
    );
}

#[test]
fn a_new_turn_without_text_asks_to_continue() {
    let req = request(&[
        user("q1"),
        assistant("a1"),
        json!({"role": "user", "blocks": [{"type": "text", "text": ""}]}),
    ]);
    assert_eq!(new_turn(&req, 2).unwrap(), [words::CONTINUE]);
    assert!(new_turn(&json!({"messages": 3}), 0).is_err());
}

#[test]
fn attached_media_are_named_and_the_rest_keep_their_placeholders() {
    let blob = format!("sha256:{}", "a".repeat(64));
    let other = format!("sha256:{}", "b".repeat(64));
    let req = request(&[json!({"role": "user", "blocks": [
        {"type": "image", "blob": blob, "name": "sunset.png", "media_type": "image/png", "width": 1, "height": 1},
        {"type": "file", "blob": other, "name": "report.pdf", "media_type": "application/pdf"}
    ]})]);
    let attached: BTreeSet<String> = [blob.clone()].into();
    let req = with_attached(&req, &attached);
    assert_eq!(
        transcribe(&req, BUDGET).unwrap(),
        "[image \"sunset.png\" attached below]\n[file \"report.pdf\" omitted: this model reads text only]"
    );
}

#[test]
fn the_live_turn_starts_after_the_last_message_that_is_not_the_user_s() {
    assert_eq!(live_from(&request(&[user("a")])).unwrap(), 0);
    assert_eq!(
        live_from(&request(&[user("a"), assistant("b"), user("c"), user("d")])).unwrap(),
        2
    );
    assert_eq!(
        live_from(&request(&[user("a"), assistant("b")])).unwrap(),
        2
    );
}
