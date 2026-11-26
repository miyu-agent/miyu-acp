use serde_json::json;

use super::*;

#[test]
fn a_request_is_list_or_call_and_anything_else_is_refused() {
    assert_eq!(
        parse(&json!({"token": "t", "op": "list"}).to_string()),
        Ok(Request::List { token: "t".into() })
    );
    assert_eq!(
        parse(
            &json!({"token": "t", "op": "call", "name": "bash", "arguments": {"a": 1}}).to_string()
        ),
        Ok(Request::Call {
            token: "t".into(),
            name: "bash".into(),
            arguments: json!({"a": 1}),
        })
    );
    assert_eq!(
        parse(&json!({"token": "t", "op": "call", "name": "bash"}).to_string()),
        Ok(Request::Call {
            token: "t".into(),
            name: "bash".into(),
            arguments: json!({}),
        }),
        "没有 arguments 的当空对象"
    );
    for bad in [
        json!({"op": "list"}),
        json!({"token": "t", "op": "drop"}),
        json!({"token": "t", "op": "call"}),
        json!({"token": 1, "op": "list"}),
    ] {
        assert!(parse(&bad.to_string()).is_err(), "{bad}");
    }
    assert!(parse("not json").is_err());
}
