//! 本包在磁盘上用的几个地方：状态目录（装好的 agent、学到的模型）、缓存目录（登记表、npx 和 uvx 的包缓存）、给 ACP 会话当
//! 工作目录的空目录。一律照核心给的环境变量找（核心 `docs/zh/dev/extensions.md`「被拉起」），不自己拼 `MIYU_HOME` 下的路径。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 一个这个进程里不重、跨进程也几乎不撞的名字：进程号、纳秒、计数。
#[must_use]
pub fn unique() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// 字节写成小写十六进制。
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// 变量 `name` 的值，空的当没有。
fn dir_var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// 扩展的状态目录：`MIYU_STATE_DIR`，没设的用当前目录（核心拉起时两个是同一处）。
///
/// # Errors
///
/// 都没有：读当前目录的错原样交。
pub fn state_dir() -> std::io::Result<PathBuf> {
    match dir_var("MIYU_STATE_DIR") {
        Some(dir) => Ok(dir),
        None => std::env::current_dir(),
    }
}

/// 扩展的缓存目录：`MIYU_CACHE_DIR`，没设的用状态目录下的 `cache`。
///
/// # Errors
///
/// 同 [`state_dir`]。
pub fn cache_dir() -> std::io::Result<PathBuf> {
    match dir_var("MIYU_CACHE_DIR") {
        Some(dir) => Ok(dir),
        None => state_dir().map(|state| state.join("cache")),
    }
}

/// 给 ACP 会话当工作目录（`cwd`）的空目录：状态目录下的 `work`，建好了交回。所有会话用同一个：`session/resume` 要求 `cwd`
/// 和开会话时一样。
///
/// # Errors
///
/// 建不成：原样交。
pub fn work_dir() -> std::io::Result<PathBuf> {
    let dir = state_dir()?.join("work");
    std::fs::create_dir_all(&dir)?;
    // agent 要的是绝对路径（ACP「Working directory」：MUST be an absolute path）。
    if dir.is_absolute() {
        Ok(dir)
    } else {
        std::env::current_dir().map(|here| here.join(dir))
    }
}

/// 删掉 `path`（文件或目录）；已经没了不算错。
///
/// # Errors
///
/// 删不掉：原样交。
pub fn remove(path: &Path) -> std::io::Result<()> {
    let removed = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names_differ_and_hex_is_lowercase() {
        assert_ne!(unique(), unique());
        assert_eq!(hex(&[0x00, 0xab, 0x7f]), "00ab7f");
    }

    #[test]
    fn removing_what_is_not_there_is_fine() {
        let dir = std::env::temp_dir().join(format!("miyu-acp-ws-{}", unique()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::write(dir.join("a/b"), "x").unwrap();
        remove(&dir.join("a")).unwrap();
        assert!(!dir.join("a").exists());
        remove(&dir.join("a")).unwrap();
        std::fs::write(dir.join("f"), "x").unwrap();
        remove(&dir.join("f")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
