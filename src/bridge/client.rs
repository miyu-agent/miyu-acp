//! 套接字的客户端（MCP 服务这一头）：连上、发一行、**一直等**到对面回一行。协议见 [`crate::bridge`]。

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde_json::Value;

use crate::process::{LINE_LIMIT, Line, next_line};

/// 往 `path` 的套接字发 `request` 这一行，等回话（没有时限：一个工具调用可能挂几十分钟），交回话那一行的 JSON。
///
/// # Errors
///
/// 连不上、写不出去、等的时候连接断了（读到头没有回话）、回话太长或者不是 JSON、对面回了 `{"error"}`：交一句英文说明，
/// 给日志用。
pub fn exchange(path: &Path, request: &Value) -> Result<Value, String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|error| format!("cannot connect to {}: {error}", path.display()))?;
    let mut line = request.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|error| format!("cannot write to the bridge: {error}"))?;
    let mut reader = BufReader::new(stream);
    let reply = match next_line(&mut reader, LINE_LIMIT) {
        Ok(Some(Line::Text(text))) => text,
        Ok(Some(Line::TooLong)) => return Err("the bridge's reply line is too long".into()),
        Ok(None) => return Err("the bridge closed the connection without a reply".into()),
        Err(error) => return Err(format!("cannot read from the bridge: {error}")),
    };
    let value: Value = serde_json::from_str(&reply)
        .map_err(|error| format!("the bridge's reply is not JSON: {error}"))?;
    match value.get("error") {
        Some(error) => Err(format!("the bridge refused the request: {error}")),
        None => Ok(value),
    }
}
