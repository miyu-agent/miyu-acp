use std::io::Cursor;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

fn lines(input: &str, limit: usize) -> Vec<Line> {
    let mut reader = Cursor::new(input.as_bytes().to_vec());
    let mut got = Vec::new();
    while let Some(line) = next_line(&mut reader, limit).unwrap() {
        let stop = line == Line::TooLong;
        got.push(line);
        if stop {
            break;
        }
    }
    got
}

#[test]
fn lines_split_on_newlines_and_keep_the_last_unterminated_one() {
    assert_eq!(
        lines("a\r\nbb\n\nccc", 100),
        vec![
            Line::Text("a".into()),
            Line::Text("bb".into()),
            Line::Text(String::new()),
            Line::Text("ccc".into())
        ]
    );
    assert!(lines("", 100).is_empty());
}

#[test]
fn a_line_over_the_limit_is_too_long() {
    assert_eq!(lines("12345\n", 5), vec![Line::Text("12345".into())]);
    assert_eq!(lines("123456\nx\n", 5), vec![Line::TooLong]);
    assert_eq!(lines("123456", 5), vec![Line::TooLong]);
}

#[test]
fn terminate_stops_the_whole_group_even_when_term_is_ignored() {
    // sh 忽略 SIGTERM，后台再起一个也忽略的：只有对整组的 SIGKILL 收得掉两个。
    let dir = std::env::temp_dir().join(format!("miyu-antigravity-term-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pids = dir.join("pids");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(r#"trap '' TERM; sh -c "trap '' TERM; while :; do sleep 1; done" & echo $$ $! > "$1"; while :; do sleep 1; done"#)
        .arg("sh")
        .arg(&pids)
        .stdin(Stdio::null());
    let mut child = spawn(command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(&pids).map_or(true, |text| text.split_whitespace().count() < 2) {
        assert!(Instant::now() < deadline, "子进程没写进程号");
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(&pids).unwrap();
    let grandchild: i32 = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let started = Instant::now();
    terminate(&mut child, Duration::from_millis(300));
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "等过了宽限"
    );
    assert!(child.try_wait().unwrap().is_some(), "领头的收了");
    let gone = Instant::now() + Duration::from_secs(5);
    loop {
        let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(grandchild), None).is_ok();
        if !alive {
            break;
        }
        assert!(Instant::now() < gone, "孙进程 {grandchild} 还活着");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn terminate_returns_only_when_the_whole_group_is_gone() {
    // 领头的收到 SIGTERM 就退，孙进程忽略 SIGTERM：领头的一退就该对整组 SIGKILL，回来时整组都没了（不用再等）。
    let dir = std::env::temp_dir().join(format!("miyu-antigravity-gone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ready = dir.join("ready");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(r#"sh -c "trap '' TERM; while :; do sleep 1; done" & echo $! > "$1"; while :; do sleep 1; done"#)
        .arg("sh")
        .arg(&ready)
        .stdin(Stdio::null());
    let mut child = spawn(command).unwrap();
    let group = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(&ready).map_or(true, |text| text.trim().is_empty()) {
        assert!(Instant::now() < deadline, "子进程没写进程号");
        std::thread::sleep(Duration::from_millis(20));
    }
    let started = Instant::now();
    terminate(&mut child, Duration::from_secs(10));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "领头的退了就不再等宽限：{:?}",
        started.elapsed()
    );
    assert_eq!(
        nix::sys::signal::killpg(group, None),
        Err(nix::errno::Errno::ESRCH),
        "回来时整组都没了"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
