//! agent 要权限时（`session/request_permission`，ACP「Tool Calls」一页）怎么答（设计 73 第四节第 7 条）。纯函数。
//!
//! - Miyu 的工具（[`Names`] 认得出、或这一轮里认出过的那件调用）：一律放行，权限、沙盒、确认由核心在执行时管。
//! - 只读的（`kind` 是 `search`、`fetch`、`think`，或者不带 `kind`、名字和标题里有 search）：放行，照供应商侧工具画。
//! - 别的：「只用 Miyu 的工具」回拒绝；「也放行对方的工具」问主人（[`Verdict::Ask`]）。
//!
//! 问主人：扩展发不了核心的确认框（`session.answer` 只给人用、`tool.guard` 只管核心执行的工具），所以本包把这一问交回核心，
//! 当成一次 `ask_user` 工具调用（[`question`]），核心照常弹提问、主人选；下一次请求带着回答来，照选的那一项回 agent（[`picked`]）。
//! 这一次请求的工具里没有 `ask_user` 的（没人看着的会话、群聊）问不了，按拒绝答。
//!
//! 选项：放行挑 `allow_once`（没有才用 `allow_always`），拒绝挑 `reject_once`（没有才用 `reject_always`）；一项都对不上的
//! 回 `cancelled`（「Agents SHOULD NOT treat an outcome they don't recognize as approval」）。

use std::collections::HashSet;
use std::fmt::Write;
use std::hash::BuildHasher;

use serde_json::{Value, json};

use crate::config::OwnTools;
use crate::names::Names;

/// 核心问人的那件工具。
pub const ASK_TOOL: &str = "ask_user";

/// 怎么答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 放行。
    Allow,
    /// 拒绝。
    Reject,
    /// 交回核心问主人。
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

/// 照设置 `own` 定怎么答这一次要权限（`call` 是请求里的 `toolCall`）。`miyu` 是这一轮里认出过的 Miyu 工具调用的编号；
/// `can_ask` 是这一次请求的工具里有 [`ASK_TOOL`]。
#[must_use]
pub fn judge<S: BuildHasher>(
    own: OwnTools,
    call: &Value,
    names: &Names,
    miyu: &HashSet<String, S>,
    can_ask: bool,
) -> Verdict {
    let known = call
        .get("toolCallId")
        .and_then(Value::as_str)
        .is_some_and(|id| miyu.contains(id));
    if known || names.of_call(call).is_some() || readonly(call) {
        return Verdict::Allow;
    }
    if own == OwnTools::Allow && can_ask {
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

/// 选中了编号 `id` 的那一项的回应。
#[must_use]
pub fn selected(id: &Value) -> Value {
    json!({"outcome": {"outcome": "selected", "optionId": id}})
}

/// 交回核心问主人的 `ask_user` 参数：哪个 agent 要跑什么（标题、类别、参数原文截到 600 个字）、不在 Miyu 的沙盒里；选项照
/// agent 给的名字。`language` 是 `zh` 的写中文，别的写英文（给人看的字）。
#[must_use]
pub fn question(agent: &str, call: &Value, options: &[Value], language: &str) -> Value {
    let title = call
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("a tool");
    let kind = call.get("kind").and_then(Value::as_str).unwrap_or("other");
    let input = call
        .get("rawInput")
        .filter(|input| !input.is_null())
        .map(|input| input.to_string().chars().take(600).collect::<String>());
    let (text, header) = if language == "zh" {
        let mut text = format!("{agent} 想用它自带的工具：{title}（{kind}）。");
        if let Some(input) = &input {
            let _ = write!(text, "参数：{input}。");
        }
        text.push_str("这个工具由对方直接运行，不在 Miyu 的沙盒和权限里。要放行吗？");
        (text, "对方的工具")
    } else {
        let mut text = format!("{agent} wants to use its own tool: {title} ({kind}).");
        if let Some(input) = &input {
            let _ = write!(text, " Input: {input}.");
        }
        text.push_str(
            " The agent runs it directly, outside Miyu's sandbox and permissions. Allow it?",
        );
        (text, "Agent tool")
    };
    let labels: Vec<Value> = options
        .iter()
        .filter_map(|option| option.get("name").and_then(Value::as_str))
        .map(|name| json!({"label": name}))
        .collect();
    json!({"questions": [{"question": text, "header": header, "options": labels}]})
}

/// 核心交回的 `ask_user` 结果 `answer`（`"<问题>" = <选的>` 那样的字）里主人选了哪一项：交那一项（`optionId` 和是不是放行）。
/// 选项名字在字里出现成 `= <名字>` 的算选了它，几项都对得上的取名字最长的；自己写了别的、没答的是 `None`（按拒绝）。
#[must_use]
pub fn picked(answer: &str, options: &[Value]) -> Option<(Value, bool)> {
    options
        .iter()
        .filter_map(|option| {
            let name = option.get("name")?.as_str()?;
            let id = option.get("optionId")?.clone();
            let allow = option
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("allow"));
            answer
                .lines()
                .any(|line| line.trim_end().ends_with(&format!("= {name}")))
                .then_some((name.len(), id, allow))
        })
        .max_by_key(|(length, _, _)| *length)
        .map(|(_, id, allow)| (id, allow))
}

#[cfg(test)]
mod tests;
