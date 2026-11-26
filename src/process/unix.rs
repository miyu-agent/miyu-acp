//! Unix 上起、收 agent：`process_group(0)` 让它自成进程组（它起的 MCP 服务、npx 底下的 node 这些子进程也在组里），收的时候对整组发信号。
//!
//! 收的先后：领头的进程收尸以前，它的进程号（也就是组号）不会被别的进程拿去，对组发信号是稳的。所以先对组发
//! `SIGTERM`，只「看」领头的退没退、不收尸（Linux 用 `waitid` 的 `WNOWAIT`）；到点或者它退了，对组发 `SIGKILL`；这时才收尸；
//! 最后等到整组都没了（组里别的进程由 init 收尸）。

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// `SIGTERM` 以后等多久再 `SIGKILL`。
pub const GRACE: Duration = Duration::from_secs(5);

/// `SIGKILL` 以后等整组没了至多多久（收不掉的记一行日志）。
const GONE: Duration = Duration::from_secs(5);

/// 起 `command`，让它自成进程组。标准输入、输出、错误怎么接由调用的人在 `command` 上设好。
///
/// # Errors
///
/// 起不来（找不到程序、没有执行权限）：原样交。
pub fn spawn(mut command: Command) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    command.process_group(0).spawn()
}

/// 收掉 `child` 和它的整个进程组：对整组 `SIGTERM`，等至多 `grace`（领头的退了就不再等），对整组 `SIGKILL`，收尸，再等到
/// 整组都没了。已经退了的照样走一遍，组里剩下的照样收。
pub fn terminate(child: &mut Child, grace: Duration) {
    let Ok(raw) = i32::try_from(child.id()) else {
        eprintln!("agent process id {} does not fit a pid", child.id());
        return;
    };
    let group = Pid::from_raw(raw);
    signal(group, Signal::SIGTERM);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline && !leader_exited(child) {
        std::thread::sleep(Duration::from_millis(20));
    }
    signal(group, Signal::SIGKILL);
    if let Err(error) = child.wait() {
        eprintln!("reaping the agent (pid {raw}) failed: {error}");
    }
    // macOS 上组里只剩已经死了、还没被收尸的进程时，`killpg` 回 `EPERM` 而不是成功（macOS CI 上见过）：和「还在」一样接着等，
    // 等 init 收完尸变成 `ESRCH`。到点还是 `EPERM` 的，组里已经没有在跑的进程，不记日志。
    let deadline = Instant::now() + GONE;
    loop {
        let seen = killpg(group, None);
        if matches!(seen, Ok(()) | Err(Errno::EPERM)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        match seen {
            Err(Errno::ESRCH | Errno::EPERM) => {}
            Ok(()) => eprintln!("the agent's process group {group} is still there after SIGKILL"),
            Err(error) => eprintln!("checking the agent's process group {group} failed: {error}"),
        }
        return;
    }
}

/// 领头的退了没有，不收尸（收了尸，进程号就可能被别的进程拿去，之后对组发的信号可能打错人）。
#[cfg(target_os = "linux")]
fn leader_exited(child: &Child) -> bool {
    use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid};
    let Ok(raw) = i32::try_from(child.id()) else {
        return true;
    };
    let flags = WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT;
    match waitid(Id::Pid(Pid::from_raw(raw)), flags) {
        Ok(WaitStatus::StillAlive) => false,
        Ok(_) => true,
        Err(error) => {
            eprintln!("watching the agent (pid {raw}) failed: {error}");
            true
        }
    }
}

/// 领头的退了没有。macOS 没有 `waitid` 可用（`nix` 不给）：照收尸来看，收了以后到发 `SIGKILL` 之间有一个很小的窗口。
#[cfg(not(target_os = "linux"))]
fn leader_exited(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(None) => false,
        Ok(Some(_)) => true,
        Err(error) => {
            eprintln!("waiting for the agent (pid {}) failed: {error}", child.id());
            true
        }
    }
}

/// 对进程组发信号；组已经没了（`ESRCH`）不算错，别的错记进日志。
fn signal(group: Pid, signal: Signal) {
    match killpg(group, signal) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => {
            eprintln!("sending {signal} to the agent's process group {group} failed: {error}");
        }
    }
}
