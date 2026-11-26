//! agent 子进程：起在自己的进程组里、按行读输出（有上限）、收掉（对进程组先 `SIGTERM`、
//! 5 秒后 `SIGKILL`）。平台相关的只在 `unix` 里；别的平台编译时就说不支持（第一版只做 Linux、macOS）。照 `miyu-antigravity` 搬。

use std::io::BufRead;

#[cfg(not(unix))]
compile_error!(
    "miyu-acp supports only Unix (Linux, macOS): it stops agents through their process group"
);

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{GRACE, spawn, terminate};

/// 一行至多多少字节：一行是一条 JSON-RPC 消息，正文的增量很短；工具结果、图片可能长，留足。
pub const LINE_LIMIT: usize = 32 * 1024 * 1024;

/// 按行读到的一条。
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// 一行，去掉了行尾的换行；不是 UTF-8 的字节换成替换符。
    Text(String),
    /// 这一行超过了上限：之后不再读。
    TooLong,
}

/// 从 `reader` 读下一行，至多 `limit` 字节（不算换行）。读到头是 `None`；最后一行没有换行的照样交。
///
/// # Errors
///
/// 读的时候出错：原样交。
pub fn next_line(reader: &mut impl BufRead, limit: usize) -> std::io::Result<Option<Line>> {
    let mut buffer = Vec::new();
    let bound = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let read = std::io::Read::take(&mut *reader, bound).read_until(b'\n', &mut buffer)?;
    if read == 0 {
        return Ok(None);
    }
    if buffer.last() == Some(&b'\n') {
        buffer.pop();
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }
    } else if buffer.len() > limit {
        return Ok(Some(Line::TooLong));
    }
    Ok(Some(Line::Text(
        String::from_utf8_lossy(&buffer).into_owned(),
    )))
}

#[cfg(test)]
mod tests;
