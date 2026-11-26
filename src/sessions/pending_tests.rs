//! 挂着的项（这一轮以交回一件工具调用结束，agy 还等着结果）：两种下一步的形状、被顶掉、挤掉、巡检的两种上限。

use super::*;

fn identity(model: &str) -> Identity {
    Identity {
        model: model.into(),
        system: "s0".into(),
        tools: "t0".into(),
    }
}

fn meta(messages: &[&str], used: Instant, pending: bool) -> Meta {
    Meta {
        identity: identity("m"),
        messages: messages.iter().map(|text| (*text).to_string()).collect(),
        agent: "miyu-a".into(),
        used,
        pending,
    }
}

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

/// 挂着的项记着 `u1`；带结果来的这一次比它多恰好两条。
const WITH_RESULT: [(&str, &str); 3] = [("u1", "user"), ("a1", "assistant"), ("t1", "tool")];

#[test]
fn a_pending_entry_takes_exactly_one_assistant_and_one_tool() {
    let now = Instant::now();
    let pending = meta(&["u1"], now, true);
    assert_eq!(next_turn(&pending, &incoming(&WITH_RESULT)), Some(1));
    for shape in [
        // 多了一条 user（人在她干活时排进来的话）。
        vec![
            ("u1", "user"),
            ("a1", "assistant"),
            ("t1", "tool"),
            ("u2", "user"),
        ],
        // 两条 tool。
        vec![
            ("u1", "user"),
            ("a1", "assistant"),
            ("t1", "tool"),
            ("t2", "tool"),
        ],
        // assistant 加 user：这是闲着的那种接法。
        vec![("u1", "user"), ("a1", "assistant"), ("u2", "user")],
        // 只有 assistant。
        vec![("u1", "user"), ("a1", "assistant")],
        // 一模一样。
        vec![("u1", "user")],
        // tool 在前。
        vec![("u1", "user"), ("t1", "tool"), ("a1", "assistant")],
    ] {
        assert_eq!(next_turn(&pending, &incoming(&shape)), None, "{shape:?}");
    }
    // 指纹、模型不一样的也不是。
    let mut other = incoming(&WITH_RESULT);
    other.identity.tools = "t1".into();
    assert_eq!(next_turn(&pending, &other), None);
    assert_eq!(
        next_turn(
            &pending,
            &incoming(&[("u9", "user"), ("a1", "assistant"), ("t1", "tool")])
        ),
        None
    );
}

#[test]
fn an_idle_entry_does_not_take_an_assistant_and_a_tool() {
    let idle = meta(&["u1"], Instant::now(), false);
    assert_eq!(next_turn(&idle, &incoming(&WITH_RESULT)), None);
}

#[test]
fn claim_finds_pending_entries_and_says_so() {
    let now = Instant::now();
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["u1"], now, true), "hung");
    let claimed = table.claim(&incoming(&WITH_RESULT)).unwrap();
    assert_eq!(
        (claimed.live, claimed.known, claimed.pending),
        ("hung", 1, true)
    );
    assert!(
        table.claim(&incoming(&WITH_RESULT)).is_none(),
        "正忙的不再认"
    );
    // 这一轮又以交回工具调用结束：记成挂着，记着这一次的。
    let messages: Vec<String> = ["u1", "a1", "t1"].map(String::from).to_vec();
    assert!(
        table
            .release_pending(claimed.id, claimed.live, messages, now)
            .is_empty()
    );
    let next = incoming(&[
        ("u1", "user"),
        ("a1", "assistant"),
        ("t1", "tool"),
        ("a2", "assistant"),
        ("t2", "tool"),
    ]);
    let again = table.claim(&next).unwrap();
    assert_eq!((again.known, again.pending), (3, true));
    // 说完了交回的：闲着的，认 assistant + user。
    let messages: Vec<String> = ["u1", "a1", "t1", "a2", "t2"].map(String::from).to_vec();
    table.release(again.id, again.live, messages, now);
    let idle = incoming(&[
        ("u1", "user"),
        ("a1", "assistant"),
        ("t1", "tool"),
        ("a2", "assistant"),
        ("t2", "tool"),
        ("a3", "assistant"),
        ("u3", "user"),
    ]);
    assert!(!table.claim(&idle).unwrap().pending);
}

#[test]
fn a_request_with_the_pending_prefix_but_another_shape_displaces_it() {
    let now = Instant::now();
    for shape in [
        // 一模一样（核心把同一次请求重发了一遍）。
        vec![("u1", "user")],
        // 工具结果后面又跟了人的话。
        vec![
            ("u1", "user"),
            ("a1", "assistant"),
            ("t1", "tool"),
            ("u2", "user"),
        ],
        // 人打断了工具、直接说了话。
        vec![("u1", "user"), ("a1", "assistant"), ("u2", "user")],
    ] {
        let mut table: Table<&str> = Table::new(LIMIT);
        table.insert(meta(&["u1"], now, true), "hung");
        let incoming = incoming(&shape);
        assert!(table.claim(&incoming).is_none(), "{shape:?}");
        assert_eq!(table.displace(&incoming), ["hung"], "{shape:?}");
        assert!(table.is_empty());
    }
}

#[test]
fn displace_leaves_the_right_next_step_other_conversations_idle_entries_and_busy_ones() {
    let now = Instant::now();
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["u1"], now, true), "hung");
    table.insert(meta(&["z1"], now, true), "other");
    table.insert(meta(&["u1", "a1", "u2"], now, false), "idle");
    // 对的接法：不顶。
    assert!(table.displace(&incoming(&WITH_RESULT)).is_empty());
    // 别的对话的、前缀不对的不顶；闲着的不这样收。
    let fork = incoming(&[
        ("u1", "user"),
        ("a1", "assistant"),
        ("u2", "user"),
        ("a9", "assistant"),
    ]);
    assert_eq!(
        table.displace(&fork),
        ["hung"],
        "挂着的 u1 以它开头、形状不对"
    );
    assert_eq!(table.len(), 2);
    let mut other_model = incoming(&[("z1", "user")]);
    other_model.identity.model = "another".into();
    assert!(table.displace(&other_model).is_empty(), "模型不一样的不顶");
    // 正忙的（被认走了的）不顶。
    let claimed = table.claim(&incoming(&[
        ("z1", "user"),
        ("a", "assistant"),
        ("t", "tool"),
    ]));
    assert_eq!(claimed.unwrap().live, "other");
    assert!(table.displace(&incoming(&[("z1", "user")])).is_empty());
    assert_eq!(table.len(), 2);
}

#[test]
fn the_same_request_again_is_displaced_but_claimed_entries_are_never_both() {
    // 认上的那一项不会同时被顶掉：认走以后它正忙，顶掉的只看闲着（含挂着）的。
    let now = Instant::now();
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["u1"], now, true), "hung");
    let incoming = incoming(&WITH_RESULT);
    let claimed = table.claim(&incoming).unwrap();
    assert_eq!(claimed.live, "hung");
    assert!(table.displace(&incoming).is_empty());
    assert_eq!(table.len(), 1);
}

#[test]
fn pending_entries_count_toward_the_limit_like_idle_ones() {
    let start = Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    let mut table: Table<&str> = Table::new(2);
    table.insert(meta(&["a"], at(1), true), "pending-old");
    table.insert(meta(&["b"], at(2), false), "idle");
    assert_eq!(
        table.insert(meta(&["c"], at(3), false), "new"),
        ["pending-old"]
    );
    assert_eq!(table.len(), 2);
    // 退出时挂着的和闲着的一起交出。
    table.insert(meta(&["d"], at(4), true), "pending-new");
    let mut drained = table.drain();
    drained.sort_unstable();
    assert_eq!(drained, ["new", "pending-new"]);
}

#[test]
fn sweep_uses_the_tool_wait_for_pending_entries_and_keep_for_idle_ones() {
    let start = Instant::now();
    let keep = Duration::from_mins(10);
    let wait = Duration::from_hours(1);
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["p-old"], start, true), "p-old");
    table.insert(
        meta(&["p-new"], start + Duration::from_mins(30), true),
        "p-new",
    );
    table.insert(meta(&["i-old"], start, false), "i-old");
    table.insert(
        meta(&["p-dead"], start + Duration::from_mins(30), true),
        "p-dead",
    );
    // 一小时零一秒：挂着的 p-old 到点，闲着的 i-old 早到点；p-new 才挂了半小时，留着；进程退了的 p-dead 照收。
    let mut removed = table.sweep_with(start + wait + Duration::from_secs(1), keep, wait, |live| {
        *live == "p-dead"
    });
    removed.sort_unstable();
    assert_eq!(removed, ["i-old", "p-dead", "p-old"]);
    assert_eq!(table.len(), 1);

    // 挂着的在闲着的上限（keep）以后还留着；到了 tool_wait 才收。
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["p"], start, true), "p");
    table.insert(meta(&["i"], start, false), "i");
    let removed = table.sweep_with(start + keep, keep, wait, |_| false);
    assert_eq!(removed, ["i"]);
    assert_eq!(table.sweep_with(start + wait, keep, wait, |_| false), ["p"]);
}

#[test]
fn plain_sweep_treats_pending_entries_like_idle_ones() {
    let start = Instant::now();
    let keep = Duration::from_mins(10);
    let mut table: Table<&str> = Table::new(LIMIT);
    table.insert(meta(&["p"], start, true), "p");
    assert_eq!(table.sweep(start + keep, keep, |_| false), ["p"]);
}
