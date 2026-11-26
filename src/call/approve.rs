//! 「也放行对方的工具」时请核心问主人（设计 73 第四节第 7 条，核心施工 ACP-8 的 `driver.approve`）：agent 要权限的那件先画成一块
//! 供应商侧工具，再发一条 `driver.approve`（编号 `approve:<调用编号>:<问话编号>`），接着听流；核心回了照选项回 agent 的权限请求。
//!
//! - 核心回的照 [`permission::approved`] 回 agent；没人能确认的会话、只读的会话核心当场回拒绝（`auto`），日志里写一句。
//! - 旧核心没有这个方法（`-32601`）：照「只用 Miyu 的工具」那一档拒绝，这一块写上被拒了，日志里写一句。别的出错同样拒绝。
//! - 问着的时候这一轮结束了（交回工具调用、回应来了、取消、agent 退了）：还没回的权限请求回 `cancelled`，之后核心回的不理。

use std::collections::HashMap;

use serde_json::{Value, json};

use super::Job;
use super::talk::deliver;
use crate::live::Live;
use crate::permission;
use crate::turn::Turn;

/// JSON-RPC 的「没有这个方法」。
const METHOD_NOT_FOUND: i64 = -32601;

/// 第 `n` 问请核心问人的请求编号。
#[must_use]
pub fn approve_id(call: &str, n: u64) -> String {
    format!("approve:{call}:{n}")
}

/// 从请求编号读出是哪一次调用的第几问；不是本包发的没有。
#[must_use]
pub fn approve_of(id: &str) -> Option<(&str, u64)> {
    let (call, n) = id.strip_prefix("approve:")?.rsplit_once(':')?;
    Some((call, n.parse().ok()?))
}

/// 请核心问着、agent 那头等着回话的一问。
#[derive(Debug)]
struct Waiting {
    /// agent 的请求编号。
    id: Value,
    /// agent 给的选项。
    options: Vec<Value>,
    /// agent 的工具调用编号。
    tool: String,
}

/// 这一轮里请核心问着的几问：问话编号到等着的那一问。
#[derive(Debug, Default)]
pub(super) struct Asking(HashMap<u64, Waiting>);

impl Asking {
    /// agent 要权限的 `call`（选项 `options`、请求编号 `id`）请核心问主人：画好那一块、发 `driver.approve`。画不了的（这件已经收了）、
    /// 选项一项都对不上的当场拒绝。
    pub(super) fn ask(
        &mut self,
        job: &Job<'_>,
        (live, turn): (&mut Live, &mut Turn),
        id: &Value,
        call: &Value,
        options: Vec<Value>,
    ) {
        let tool = call
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (deltas, index) = turn.hosted_block(call);
        deliver(job, deltas);
        let params = index.and_then(|index| permission::approve(job.call, index, call, &options));
        let Some(params) = params else {
            eprintln!(
                "call {}: cannot ask the owner about tool call {tool}; refusing it",
                job.call
            );
            live.conn().reply(id, &permission::choose(&options, false));
            deliver(job, turn.refused(&tool));
            return;
        };
        let n = live.next_ask();
        job.out.send(&json!({
            "jsonrpc": "2.0",
            "id": approve_id(job.call, n),
            "method": "driver.approve",
            "params": params,
        }));
        self.0.insert(
            n,
            Waiting {
                id: id.clone(),
                options,
                tool,
            },
        );
    }

    /// 核心回了第 `n` 问：照它回 agent。不是这一轮在问的不理。
    pub(super) fn answered(
        &mut self,
        job: &Job<'_>,
        (live, turn): (&mut Live, &mut Turn),
        n: u64,
        outcome: &Result<Value, Value>,
    ) {
        let Some(Waiting { id, options, tool }) = self.0.remove(&n) else {
            return;
        };
        if let Err(error) = outcome {
            let code = error.get("code").and_then(Value::as_i64);
            match code {
                Some(METHOD_NOT_FOUND) => eprintln!(
                    "call {}: this Miyu core is too old to ask the owner (it has no driver.approve), \
                     so the agent's own tools are refused as if only Miyu's tools were allowed",
                    job.call
                ),
                _ => eprintln!(
                    "call {}: the core refused to ask the owner about tool call {tool} ({error}); \
                     refusing it",
                    job.call
                ),
            }
        }
        let (reply, allow) = permission::approved(outcome.as_ref(), &options);
        if let Ok(result) = outcome {
            eprintln!(
                "call {}: the owner's answer about tool call {tool}: {}{}",
                job.call,
                result
                    .get("decision")
                    .and_then(Value::as_str)
                    .unwrap_or("none"),
                result
                    .get("auto")
                    .and_then(Value::as_str)
                    .map(|auto| format!(" ({auto}, decided by Miyu without asking)"))
                    .unwrap_or_default(),
            );
        }
        live.conn().reply(&id, &reply);
        if outcome.is_err() && !allow {
            deliver(job, turn.refused(&tool));
        }
    }

    /// 这一轮要结束了：还在等的权限请求回 `cancelled`。
    pub(super) fn abandon(&mut self, live: &mut Live) {
        for (_, waiting) in self.0.drain() {
            live.conn()
                .reply(&waiting.id, &json!({"outcome": {"outcome": "cancelled"}}));
        }
    }
}

#[cfg(test)]
mod tests;
