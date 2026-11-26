//! 接在活着的会话上说下一步：闲着的（说完了的）是**新的一轮**，挂着的是**工具结果**。照 `miyu-antigravity`
//! 的 `call/resume.rs` 搬。
//!
//! - [`resume`]：新的几条 user 消息作为一次 `session/prompt` 交过去，读流、交增量。
//! - [`resume_pending`]：这段会话正挂着一件。再核三样（assistant 那条里恰好一个 `tool_call` 块、私有数据是交回时写的那个；
//!   tool 那条的 `call_id` 就是这个块的；挂着期间 agent 没说过话），对了才经挂着的连接回给 agent，接着听同一个
//!   `session/prompt`；哪样不对都作废、改走新开。
//!
//! 两种都：那个 agent 没说话就没了的作废，交 `Err`（原因）；出了错的作废；成功的放回表里（以交回结束的记成挂着）。

use serde_json::{Value, json};

use super::open::{learn_window, send_prompt};
use super::talk::{Ended, talk};
use super::{Job, succeeded, with_sent};
use crate::failure::Class;
use crate::live::{Live, Pending};
use crate::prompt::{self, Media};
use crate::sessions::{Claimed, Incoming};
use crate::transcript;
use crate::turn::{self, Turn};

/// 新的一轮：接在 `claimed` 那个活着的会话上。
pub(super) fn resume(
    job: &Job<'_>,
    claimed: Claimed<Live>,
    incoming: &Incoming,
) -> Result<Value, String> {
    let Claimed {
        id,
        mut live,
        known,
        ..
    } = claimed;
    let media = Media::new(job.params, &live.caps);
    let prompt = match prompt::next(job.request, known + 1, &media) {
        Ok(prompt) => prompt,
        Err(error) => {
            job.registry.forget(id);
            return Ok(turn::failed(Class::Unclassified, &error.to_string()));
        }
    };
    let params = json!({"sessionId": live.session, "prompt": prompt});
    let mut turn = Turn::new(job.names.clone(), &live.login).speaking(&job.config.language);
    let ended = talk(job, &mut live, &mut turn, |live| send_prompt(live, &params));
    learn_window(job, &turn);
    conclude(job, id, live, incoming, ended, &params["prompt"])
}

/// 挂着的那件的结果来了：核对、回给 agent、接着读流。
pub(super) fn resume_pending(
    job: &Job<'_>,
    claimed: Claimed<Live>,
    incoming: &Incoming,
) -> Result<Value, String> {
    let Claimed {
        id,
        mut live,
        known,
        ..
    } = claimed;
    let Some(pending) = live.take_pending() else {
        return discard(job, id, live, "the kept session had nothing pending".into());
    };
    let Pending::Mcp { n, reply } = pending;
    let private = json!({"mcp": n});
    let result = match check(job.request, known, &private) {
        Ok(result) => result,
        Err(why) => return discard(job, id, live, why),
    };
    if live.stray() {
        return discard(
            job,
            id,
            live,
            "the agent spoke while something was pending".into(),
        );
    }
    if reply.is_closed() {
        return discard(
            job,
            id,
            live,
            "the tool server closed its connection to the extension".into(),
        );
    }
    let memory = std::mem::take(&mut live.memory);
    let mut turn =
        Turn::resume(job.names.clone(), &live.login, memory).speaking(&job.config.language);
    let sent = json!(result.text);
    let ended = talk(job, &mut live, &mut turn, move |live| {
        if live.stray() {
            return Err("the agent spoke while something was pending".to_string());
        }
        reply
            .send(
                &[json!({"type": "text", "text": result.text})],
                result.error,
            )
            .map_err(|error| format!("the tool server could not take the result: {error}"))
    });
    learn_window(job, &turn);
    conclude(job, id, live, incoming, ended, &sent)
}

/// 这段挂着的对不上：作废，交 `Err`。
fn discard(job: &Job<'_>, id: u64, live: Live, why: String) -> Result<Value, String> {
    job.registry.forget(id);
    drop(live);
    Err(why)
}

/// 一轮结束以后：成功的放回表里，出了错的作废，没说话就没了的作废并交 `Err`。`sent` 是这一轮交给 agent 的东西。
fn conclude(
    job: &Job<'_>,
    id: u64,
    mut live: Live,
    incoming: &Incoming,
    ended: Ended,
    sent: &Value,
) -> Result<Value, String> {
    match ended {
        Ended::Answer(answer) if succeeded(&answer) => {
            job.registry.release(id, live, incoming.messages.clone());
            Ok(with_sent(answer, sent))
        }
        Ended::Handed(answer, pending) => {
            live.set_pending(pending);
            job.registry
                .release_pending(id, live, incoming.messages.clone());
            Ok(with_sent(answer, sent))
        }
        Ended::Answer(answer) => {
            job.registry.forget(id);
            drop(live);
            Ok(answer)
        }
        Ended::Silent(answer) => {
            let pid = live.pid();
            job.registry.forget(id);
            drop(live);
            Err(format!("the kept agent (pid {pid}) was gone ({answer})"))
        }
    }
}

/// 要回给 agent 的结果。
struct ToolResult {
    /// tool 那条的字。
    text: String,
    /// tool 那条的 `error`。
    error: bool,
}

/// 核对请求里第 `known`、`known + 1` 条（assistant 和 tool）和挂着的那件（私有数据 `private`）对得上，交结果。
fn check(request: &Value, known: usize, private: &Value) -> Result<ToolResult, String> {
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("request.messages is not an array")?;
    let assistant = messages
        .get(known)
        .ok_or("the assistant message is missing")?;
    let tool = messages
        .get(known + 1)
        .ok_or("the tool message is missing")?;
    let calls: Vec<&Value> = assistant
        .get("blocks")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_call"))
                .collect()
        })
        .unwrap_or_default();
    let [block] = calls.as_slice() else {
        return Err(format!(
            "the assistant message holds {} tool calls, not exactly one",
            calls.len()
        ));
    };
    if block.pointer("/private/data") != Some(private) {
        return Err(format!("the tool call's private data is not {private}"));
    }
    let call_id = block.get("call_id").and_then(Value::as_str);
    if call_id.is_none() || call_id != tool.get("call_id").and_then(Value::as_str) {
        return Err("the tool result's call_id is not the tool call's".into());
    }
    let text = transcript::message_text(request, known + 1)
        .map_err(|error| error.to_string())?
        .ok_or("the tool message is missing")?;
    Ok(ToolResult {
        text,
        error: tool.get("error").and_then(Value::as_bool) == Some(true),
    })
}
