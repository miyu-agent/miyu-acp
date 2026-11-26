use super::*;

fn identity(model: &str) -> Identity {
    Identity {
        model: model.into(),
        system: "s0".into(),
        tools: "t0".into(),
    }
}

/// 记着 `messages` 的一项（指纹就用消息名）。
fn meta(messages: &[&str], used: Instant) -> Meta {
    Meta {
        identity: identity("m"),
        messages: messages.iter().map(|text| (*text).to_string()).collect(),
        agent: "miyu-a".into(),
        used,
        pending: false,
    }
}

/// 进来的一次：`(指纹, role)`。
fn incoming(messages: &[(&str, &str)]) -> Incoming {
    Incoming {
        identity: identity("m"),
        messages: messages.iter().map(|(fp, _)| (*fp).to_string()).collect(),
        roles: messages
            .iter()
            .map(|(_, role)| (*role).to_string())
            .collect(),
    }
}

const TURN_ONE: [(&str, &str); 1] = [("u1", "user")];
const TURN_TWO: [(&str, &str); 3] = [("u1", "user"), ("a1", "assistant"), ("u2", "user")];

#[test]
fn a_strict_prefix_followed_by_assistant_and_users_is_the_next_turn() {
    let now = Instant::now();
    assert_eq!(
        next_turn(&meta(&["u1"], now), &incoming(&TURN_TWO)),
        Some(1)
    );
    let two_users = incoming(&[
        ("u1", "user"),
        ("a1", "assistant"),
        ("u2", "user"),
        ("u3", "user"),
    ]);
    assert_eq!(next_turn(&meta(&["u1"], now), &two_users), Some(1));
    // 前缀不对（中间一条换了）。
    let edited = incoming(&[("u1x", "user"), ("a1", "assistant"), ("u2", "user")]);
    assert_eq!(next_turn(&meta(&["u1"], now), &edited), None);
    // 记着的比这一次长。
    assert_eq!(
        next_turn(&meta(&["u1", "a1", "u2", "a2"], now), &incoming(&TURN_TWO)),
        None
    );
}

#[test]
fn the_same_request_again_is_not_a_next_turn() {
    let now = Instant::now();
    assert_eq!(next_turn(&meta(&["u1"], now), &incoming(&TURN_ONE)), None);
    assert_eq!(
        next_turn(&meta(&["u1", "a1", "u2"], now), &incoming(&TURN_TWO)),
        None
    );
}

#[test]
fn the_wrong_shape_after_the_prefix_is_not_a_next_turn() {
    let now = Instant::now();
    let known = meta(&["u1"], now);
    for shape in [
        // 只多了一条 assistant。
        vec![("u1", "user"), ("a1", "assistant")],
        // 没有 assistant。
        vec![("u1", "user"), ("u2", "user")],
        // 两条 assistant。
        vec![
            ("u1", "user"),
            ("a1", "assistant"),
            ("a2", "assistant"),
            ("u2", "user"),
        ],
        // 夹着工具结果（AG-3 的事）。
        vec![
            ("u1", "user"),
            ("a1", "assistant"),
            ("t1", "tool"),
            ("u2", "user"),
        ],
        vec![
            ("u1", "user"),
            ("a1", "assistant"),
            ("u2", "user"),
            ("t1", "tool"),
        ],
    ] {
        assert_eq!(next_turn(&known, &incoming(&shape)), None, "{shape:?}");
    }
}

#[test]
fn a_different_model_or_fingerprint_is_not_a_next_turn() {
    let now = Instant::now();
    let known = meta(&["u1"], now);
    let mut other = incoming(&TURN_TWO);
    other.identity.model = "m2".into();
    assert_eq!(next_turn(&known, &other), None);
    let mut other = incoming(&TURN_TWO);
    other.identity.system = "s1".into();
    assert_eq!(next_turn(&known, &other), None);
    let mut other = incoming(&TURN_TWO);
    other.identity.tools = "t1".into();
    assert_eq!(next_turn(&known, &other), None);
    // 指纹和 role 不一样长的不认。
    let mut other = incoming(&TURN_TWO);
    other.roles.push("user".into());
    assert_eq!(next_turn(&known, &other), None);
}

#[test]
fn the_longest_match_wins() {
    let now = Instant::now();
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["u1"], now), "short");
    table.insert(meta(&["u1", "a1", "u2"], now), "long");
    table.insert(meta(&["zz"], now), "unrelated");
    let three = incoming(&[
        ("u1", "user"),
        ("a1", "assistant"),
        ("u2", "user"),
        ("a2", "assistant"),
        ("u3", "user"),
    ]);
    let claimed = table.claim(&three).unwrap();
    assert_eq!((claimed.live, claimed.known), ("long", 3));
    // 长的正忙着：退而接短的那段（记着 1 条，后面的形状不是 assistant + user，接不上）。
    assert!(table.claim(&three).is_none());
}

#[test]
fn the_same_conversation_registered_again_replaces_the_idle_one() {
    let start = Instant::now();
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["u1"], start), "old");
    let removed = table.insert(meta(&["u1"], start + Duration::from_secs(1)), "new");
    assert_eq!(removed, ["old"], "同一段对话（重新生成）的旧的闲着的被换掉");
    assert_eq!(table.len(), 1);
    assert_eq!(table.claim(&incoming(&TURN_TWO)).unwrap().live, "new");
}

#[test]
fn a_busy_entry_is_not_claimed_again() {
    let now = Instant::now();
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["u1"], now), "agy");
    let claimed = table.claim(&incoming(&TURN_TWO)).unwrap();
    assert_eq!(claimed.live, "agy");
    assert_eq!(table.len(), 1, "正忙的还在表里");
    assert!(table.claim(&incoming(&TURN_TWO)).is_none(), "正忙着");
    // 交回以后记着这一次的，下一轮又能接上。
    let messages: Vec<String> = ["u1", "a1", "u2"].map(String::from).to_vec();
    assert!(
        table
            .release(claimed.id, claimed.live, messages, now)
            .is_empty()
    );
    let three = incoming(&[
        ("u1", "user"),
        ("a1", "assistant"),
        ("u2", "user"),
        ("a2", "assistant"),
        ("u3", "user"),
    ]);
    let again = table.claim(&three).unwrap();
    assert_eq!(again.known, 3);
    table.forget(again.id);
    assert!(table.is_empty(), "出了错的忘掉");
}

#[test]
fn release_of_an_unknown_entry_hands_the_process_back() {
    let mut table: Table<&str> = Table::new(LIMIT);
    assert_eq!(
        table.release(99, "agy", Vec::new(), Instant::now()),
        ["agy"]
    );
    assert!(table.is_empty());
}

#[test]
fn over_the_limit_the_least_recently_used_idle_one_goes() {
    let start = Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    let mut table: Table<&str> = Table::new(LIMIT);
    for (index, name) in ["a", "b", "c", "d"].into_iter().enumerate() {
        let removed = table.insert(meta(&[name], at(index as u64 + 10)), name);
        assert!(removed.is_empty());
    }
    // b 正忙：最久没用的是 a；再让 a 正忙，挤掉的是闲着的里最久的 c。
    let busy_b = table
        .claim(&incoming(&[
            ("b", "user"),
            ("x", "assistant"),
            ("y", "user"),
        ]))
        .unwrap();
    assert_eq!(table.insert(meta(&["e"], at(20)), "e"), ["a"]);
    let busy_c = table
        .claim(&incoming(&[
            ("c", "user"),
            ("x", "assistant"),
            ("y", "user"),
        ]))
        .unwrap();
    assert_eq!(table.insert(meta(&["f"], at(21)), "f"), ["d"]);
    assert_eq!(table.len(), LIMIT);
    let _ = (busy_b, busy_c);

    // 全都正忙：挤不掉，照样登记（超过上限也不收正忙的）。
    let mut full: Table<&str> = Table::new(1);
    full.insert(meta(&["a"], at(1)), "a");
    let _busy = full
        .claim(&incoming(&[
            ("a", "user"),
            ("x", "assistant"),
            ("y", "user"),
        ]))
        .unwrap();
    assert!(full.insert(meta(&["b"], at(2)), "b").is_empty());
    assert_eq!(full.len(), 2);
}

#[test]
fn sweep_takes_expired_and_dead_idle_ones_but_not_busy_ones() {
    let start = Instant::now();
    let keep = Duration::from_mins(10);
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["old"], start), "old");
    table.insert(meta(&["dead"], start + keep), "dead");
    table.insert(meta(&["fresh"], start + keep), "fresh");
    table.insert(meta(&["busy"], start), "busy");
    let _busy = table
        .claim(&incoming(&[
            ("busy", "user"),
            ("x", "assistant"),
            ("y", "user"),
        ]))
        .unwrap();
    let mut removed = table.sweep(start + keep + Duration::from_secs(1), keep, |live| {
        *live == "dead"
    });
    removed.sort_unstable();
    assert_eq!(removed, ["dead", "old"]);
    assert_eq!(table.len(), 2, "新的和正忙的留着");
    assert!(table.agents().contains("miyu-a"));

    let mut drained = table.drain();
    drained.sort_unstable();
    assert_eq!(drained, ["fresh"], "退出时交出闲着的；正忙的由它那一次收");
    assert_eq!(table.len(), 1);
}
