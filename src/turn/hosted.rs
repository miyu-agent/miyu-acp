//! 供应商侧工具：agent 报的不是 Miyu 的工具调用，都是它自己跑的（设计 73 第四节第 7 条、核心设计 71），照实交成 `hosted_tool` 块。
//!
//! - 头一次见到这件调用（`tool_call`；没见过 `tool_call` 直接来了 `tool_call_update` 的也算）：先收掉开着的正文块，
//!   `start {kind: {"hosted_tool": {"name"}}}`（名字用 `name`，没有的用 `kind`），有 `rawInput` 的交成 `text`。
//! - 参数后到的（Claude 适配器先报空的、输入写完再补 `rawInput`）：块还开着、还没交过参数的补交一次。
//! - `completed`：有结果的交 `outcome`（`content` 里的字，没有的用 `rawOutput`），再 `end`；`failed`：`outcome` 带 `failed: true`。
//! - 结果先截到 [`OUTCOME_LIMIT`]。不带 `private`。这件没收的时候来了别的（正文、下一件工具、回应、交回的调用）：收掉它，不交结果。
//! - 会动本机的（不是只读的搜索、抓网页、思考，也不是 `read`）又没要过权限就跑了（`in_progress`、`completed`）的，记一行警告
//!   （设计第四节第 7 条「照样画成供应商侧工具，并在包的日志里记一条警告」），结果前面再写一句给人看的「未经确认已执行」
//!   （[`unasked_note`]，施工 ACP-10：不静默）。没有结果的也交这一句。

use std::collections::HashSet;

use serde_json::{Value, json};

use super::{Turn, call_id, push_text};
use crate::permission;

/// 本包拒了对方的工具时这一块的结果（模型和人都看）。
pub const REFUSED: &str = "Miyu refused this tool: only Miyu's tools may run in this session.";

/// 一件供应商侧工具的结果至多多少字节（核心也照这个数截）。
pub const OUTCOME_LIMIT: usize = 64 * 1024;

/// 供应商侧工具块的状态。
#[derive(Debug, Default)]
pub(super) struct Hosting {
    /// 开着的块：哪件调用、第几块、交过参数没有、它是不是只读类。
    open: Option<OpenTool>,
    /// 开过块的调用（跨几次调用记着，见 [`super::Memory`]）。
    pub(super) seen: HashSet<String>,
    /// 记过警告的调用。
    warned: HashSet<String>,
    /// 给人看的字写中文。
    pub(super) zh: bool,
}

#[derive(Debug)]
struct OpenTool {
    id: String,
    index: u64,
    has_args: bool,
    /// 跑之前该要权限：不是只读类、也不是 `read`。
    consent: bool,
}

impl Turn {
    /// `tool_call`：开块（同一件已经开过的照更新处理）。
    pub(super) fn hosted_start(&mut self, update: &Value, deltas: &mut Vec<Value>) {
        let id = call_id(update);
        if self.hosting.seen.contains(&id) {
            self.hosted_update(update, deltas);
            return;
        }
        self.close_open(deltas);
        let index = self.next_index();
        let name = update
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .or_else(|| update.get("kind").and_then(Value::as_str))
            .unwrap_or("tool");
        deltas.push(json!({"start": {"index": index, "kind": {"hosted_tool": {"name": name}}}}));
        self.hosting.seen.insert(id.clone());
        self.hosting.open = Some(OpenTool {
            id,
            index,
            has_args: false,
            consent: needs_consent(update),
        });
        self.hosted_update(update, deltas);
    }

    /// `tool_call_update`（或开块时的同一条）：补参数、交结果、收块。
    pub(super) fn hosted_update(&mut self, update: &Value, deltas: &mut Vec<Value>) {
        let id = call_id(update);
        if self.hosting.open.as_ref().is_none_or(|open| open.id != id) {
            if !self.hosting.seen.contains(&id) {
                self.hosted_start(update, deltas);
            }
            return;
        }
        let status = update.get("status").and_then(Value::as_str);
        self.warn_unasked(&id, status);
        let Some(open) = self.hosting.open.as_mut() else {
            return;
        };
        if !open.has_args
            && let Some(input) = update.get("rawInput").filter(|input| !input.is_null())
            && input != &json!({})
        {
            open.has_args = true;
            push_text(deltas, open.index, &input.to_string());
        }
        let index = open.index;
        let note = self
            .hosting
            .warned
            .contains(&id)
            .then(|| unasked_note(self.hosting.zh));
        let text = outcome_text(update).filter(|text| !text.is_empty());
        match status {
            Some("completed") => {
                if let Some(text) = noted(note, text) {
                    deltas.push(json!({"outcome": {"index": index, "text": cut(&text)}}));
                }
            }
            Some("failed") => {
                let text = noted(note, Some(text.unwrap_or_else(|| "failed".into())));
                let said = text.unwrap_or_default();
                deltas
                    .push(json!({"outcome": {"index": index, "text": cut(&said), "failed": true}}));
            }
            _ => return,
        }
        self.close_hosted(deltas);
    }

    /// 请核心问主人之前（核心施工 ACP-8）：这件调用画在哪一块。还开着的照它；没见过的先开块（交回开块的增量）；见过、已经收了的
    /// 没有（问不了，按拒绝）。
    pub fn hosted_block(&mut self, call: &Value) -> (Vec<Value>, Option<u64>) {
        let id = call_id(call);
        let mut deltas = Vec::new();
        if !self.hosting.seen.contains(&id) {
            self.hosted_start(call, &mut deltas);
        }
        let index = self
            .hosting
            .open
            .as_ref()
            .filter(|open| open.id == id)
            .map(|open| open.index);
        (deltas, index)
    }

    /// 本包拒了这件调用的权限请求：块还开着的，交一句「被拒了」（`failed`）、收块。
    pub fn refused(&mut self, id: &str) -> Vec<Value> {
        let mut deltas = Vec::new();
        if let Some(open) = self.hosting.open.as_ref().filter(|open| open.id == id) {
            deltas.push(json!({"outcome": {"index": open.index, "text": REFUSED, "failed": true}}));
            self.close_hosted(&mut deltas);
        }
        deltas
    }

    /// 该要权限、没要就跑了的，记一行警告（一件记一次）。
    fn warn_unasked(&mut self, id: &str, status: Option<&str>) {
        let running = matches!(status, Some("in_progress" | "completed"));
        let consent = self.hosting.open.as_ref().is_some_and(|open| open.consent);
        if running
            && consent
            && !self.asked.contains(id)
            && self.hosting.warned.insert(id.to_string())
        {
            eprintln!(
                "warning: the agent ran its own tool call {id} without asking for permission"
            );
        }
    }

    /// 收掉开着的供应商侧工具块（没有结果的）。
    pub(super) fn close_hosted(&mut self, deltas: &mut Vec<Value>) {
        if let Some(open) = self.hosting.open.take() {
            deltas.push(json!({"end": {"index": open.index}}));
        }
    }
}

/// 跑之前该要权限的：不是只读的搜索、抓网页、思考（[`permission::readonly`]），也不是 `kind: "read"`（读文件不动本机，各家
/// 的「每次都问」那一档也不问它）。
fn needs_consent(call: &Value) -> bool {
    !permission::readonly(call) && call.get("kind").and_then(Value::as_str) != Some("read")
}

/// 没要权限就跑了的那件，结果前面给人看的一句（照界面语言；协议里供应商侧工具的结果模型和人都看，没有只给人的一格）。
#[must_use]
pub fn unasked_note(zh: bool) -> &'static str {
    if zh {
        "未经确认已执行：对方没有请求权限，自己运行了这个工具。"
    } else {
        "Ran without confirmation: the agent ran this tool without asking for permission."
    }
}

/// 有提醒的接在结果前面；都没有的是 `None`。
fn noted(note: Option<&str>, text: Option<String>) -> Option<String> {
    match (note, text) {
        (Some(note), Some(text)) => Some(format!("{note}\n{text}")),
        (Some(note), None) => Some(note.to_string()),
        (None, text) => text,
    }
}

/// 结果的字：`content` 里的文字（差异写成一句改了哪个文件），没有的用 `rawOutput`。
fn outcome_text(update: &Value) -> Option<String> {
    let parts: Vec<String> = update
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                    Some("content") => item
                        .pointer("/content/text")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    Some("diff") => item
                        .get("path")
                        .and_then(Value::as_str)
                        .map(|path| format!("[changed {path}]")),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    if !parts.is_empty() {
        return Some(parts.join("\n"));
    }
    match update.get("rawOutput")? {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// `text` 截到至多 [`OUTCOME_LIMIT`] 字节，不切开一个字。
#[must_use]
pub fn cut(text: &str) -> &str {
    if text.len() <= OUTCOME_LIMIT {
        return text;
    }
    let mut end = OUTCOME_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
