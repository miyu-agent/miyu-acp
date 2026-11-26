use super::*;

#[test]
fn the_request_id_names_the_call_and_the_question() {
    let id = approve_id("d12", 3);
    assert_eq!(id, "approve:d12:3");
    assert_eq!(approve_of(&id), Some(("d12", 3)));
    assert_eq!(
        approve_of("approve:a:b:7"),
        Some(("a:b", 7)),
        "调用编号里有冒号的照最后一个分"
    );
    assert_eq!(approve_of("hello"), None);
    assert_eq!(approve_of("approve:d1:x"), None);
}
