//! agent 要权限时（`session/request_permission`，ACP「Tool Calls」一页）怎么答（设计 73 第四节第 7 条）。纯函数。
//!
//! - Miyu 的工具（[`Names`] 认得出、或这一轮里认出过的那件调用）：一律放行，权限、沙盒、确认由核心在执行时管。
//! - 只读的（`kind` 是 `search`、`fetch`、`think`，或者不带 `kind`、名字和标题里有 search）：放行，照供应商侧工具画。
//! - 别的：「只用 Miyu 的工具」回拒绝；「也放行对方的工具」请核心问主人（[`Verdict::Ask`]）。
//!
//! 问主人（核心施工 ACP-8 的 `driver.approve`）：先把这件画成一块供应商侧工具，再请核心弹出 Miyu 自己的确认框（[`approve`]），
//! 照核心回的选项回 agent（[`approved`]）。没人能确认的会话、只读的会话由核心当场拒；旧核心没有这个方法（回 `-32601`）的照
//! 「只用 Miyu 的工具」那一档拒绝。
//!
//! 选项：放行挑 `allow_once`（没有才用 `allow_always`），拒绝挑 `reject_once`（没有才用 `reject_always`）；一项都对不上的
//! 回 `cancelled`（「Agents SHOULD NOT treat an outcome they don't recognize as approval」）。

use std::collections::HashSet;
use std::hash::BuildHasher;

use serde_json::{Value, json};

use crate::config::OwnTools;
use crate::names::Names;

/// 怎么答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 放行。
    Allow,
    /// 拒绝。
    Reject,
    /// 请核心问主人。
    Ask,
}

/// 只读、不碰本机的那几类（ACP 的 `kind`）。
const READONLY_KINDS: &[&str] = &["search", "fetch", "think"];

/// 一件工具调用是不是只读的搜索、抓网页、思考类。
#[must_use]
pub fn readonly(call: &Value) -> bool {
    match call.get("kind").and_then(Value::as_str) {
        Some(kind) => READONLY_KINDS.contains(&kind),
        None => ["name", "title"].iter().any(|key| {
            call.get(*key)
                .and_then(Value::as_str)
                .is_some_and(|text| text.to_ascii_lowercase().contains("search"))
        }),
    }
}

/// 照设置 `own` 定怎么答这一次要权限（`call` 是请求里的 `toolCall`）。`miyu` 是这一轮里认出过的 Miyu 工具调用的编号。
#[must_use]
pub fn judge<S: BuildHasher>(
    own: OwnTools,
    call: &Value,
    names: &Names,
    miyu: &HashSet<String, S>,
) -> Verdict {
    let known = call
        .get("toolCallId")
        .and_then(Value::as_str)
        .is_some_and(|id| miyu.contains(id));
    if known || names.of_call(call).is_some() || readonly(call) {
        return Verdict::Allow;
    }
    if own == OwnTools::Allow {
        Verdict::Ask
    } else {
        Verdict::Reject
    }
}

/// 照放不放行挑一项，写成回应的 `result`。
#[must_use]
pub fn choose(options: &[Value], allow: bool) -> Value {
    let kinds: &[&str] = if allow {
        &["allow_once", "allow_always"]
    } else {
        &["reject_once", "reject_always"]
    };
    kinds
        .iter()
        .find_map(|kind| {
            options
                .iter()
                .find(|option| option.get("kind").and_then(Value::as_str) == Some(kind))
        })
        .and_then(|option| option.get("optionId"))
        .map_or_else(
            || json!({"outcome": {"outcome": "cancelled"}}),
            |id| json!({"outcome": {"outcome": "selected", "optionId": id}}),
        )
}

/// 核心的 `driver.approve` 认的选项种类。
const KINDS: &[&str] = &["allow_once", "allow_always", "reject_once", "reject_always"];

/// 请核心问主人的 `driver.approve` 参数：这一次调用 `call`、画在第 `index` 块，agent 的 `toolCall` 是 `tool`、选项是 `options`。
/// 跑命令的交命令原文，别的交路径（没有的交参数原文）；核心照它画成跑命令、或者包问人的样子。对得上的选项一项都没有的交 `None`。
#[must_use]
pub fn approve(call: &str, index: u64, tool: &Value, options: &[Value]) -> Option<Value> {
    let choices: Vec<Value> = options
        .iter()
        .filter_map(|option| {
            let id = option.get("optionId")?.as_str()?;
            let kind = option
                .get("kind")?
                .as_str()
                .filter(|kind| KINDS.contains(kind))?;
            let name = option.get("name").and_then(Value::as_str).unwrap_or(id);
            Some(json!({"id": id, "name": name, "kind": kind}))
        })
        .take(16)
        .collect();
    if choices.is_empty() {
        return None;
    }
    let kind = tool.get("kind").and_then(Value::as_str);
    let access = match kind {
        Some("read" | "search" | "think") => "read",
        Some("fetch") => "network",
        Some("edit" | "delete" | "move") => "write",
        _ => "execute",
    };
    let title = tool
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("a tool");
    let mut params =
        json!({"call": call, "index": index, "access": access, "title": title, "options": choices});
    let command = tool.pointer("/rawInput/command").and_then(Value::as_str);
    if let Some(command) = command.filter(|_| kind.is_none_or(|kind| kind == "execute")) {
        params["command"] = json!(command);
        return Some(params);
    }
    let paths: Vec<&str> = tool
        .get("locations")
        .and_then(Value::as_array)
        .map(|all| {
            all.iter()
                .filter_map(|at| at.get("path")?.as_str())
                .collect()
        })
        .unwrap_or_default();
    let text = if paths.is_empty() {
        tool.get("rawInput")
            .filter(|input| !input.is_null() && *input != &json!({}))
            .map(Value::to_string)
            .unwrap_or_default()
    } else {
        paths.join("\n")
    };
    if !text.is_empty() {
        params["text"] = json!(text);
    }
    Some(params)
}

/// 核心回的 `driver.approve`（`result`，或者 JSON-RPC 的 `error`）怎么回 agent：交回给 agent 的 `result`、放没放行。核心挑好的
/// 选项照它；没挑的照决定挑；`cancelled` 的回 `cancelled`。出错的（旧核心没有这个方法、参数不对）照拒绝。
#[must_use]
pub fn approved(answer: Result<&Value, &Value>, options: &[Value]) -> (Value, bool) {
    let Ok(result) = answer else {
        return (choose(options, false), false);
    };
    let allow = match result.get("decision").and_then(Value::as_str) {
        Some("once") => true,
        Some("deny") => false,
        _ => return (json!({"outcome": {"outcome": "cancelled"}}), false),
    };
    let picked = result.get("option").and_then(Value::as_str).filter(|id| {
        options
            .iter()
            .any(|option| option.get("optionId").and_then(Value::as_str) == Some(*id))
    });
    match picked {
        Some(id) => (selected(&json!(id)), allow),
        None => (choose(options, allow), allow),
    }
}

/// 选中了编号 `id` 的那一项的回应。
#[must_use]
pub fn selected(id: &Value) -> Value {
    json!({"outcome": {"outcome": "selected", "optionId": id}})
}

#[cfg(test)]
mod tests;
