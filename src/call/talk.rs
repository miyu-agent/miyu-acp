//! 一轮的读写：先做「开头」（发 `session/prompt`；或者把工具结果回给 agent），然后读 agent 的 `session/update`，
//! 照 [`Turn`] 交增量，读到 `session/prompt` 的回应、或者交回一件工具调用为止。
//!
//! - agent 要权限（`session/request_permission`）：照 [`permission::judge`] 当场答；要问主人的请核心问（`approve.rs`，核心的
//!   `driver.approve`），接着听，核心回了再回 agent。别的 agent 请求（`fs/*`、`terminal/*`，本包都没声明）回「没有这个方法」。
//! - Miyu 的工具调用有两边：流里的 `tool_call` 和 MCP 服务转来的 `tools/call`，谁先到都可能。第一边到了就开始掐表（[`pair_wait`]），
//!   到点还没等到另一边就照 [`Turn::timeout`]（照 `miyu-antigravity`）。
//! - `driver.cancel`：发 `session/cancel`，回 `retryable`；这段会话随后作废。

use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::approve::Asking;
use super::{Job, Signal, cancelled};
use crate::acp::METHOD_NOT_FOUND;
use crate::bridge::Reply;
use crate::hub::McpCall;
use crate::live::{Live, Pending};
use crate::permission::{self, Verdict};
use crate::process;
use crate::turn::{Fed, Hand, Turn};

/// 工具调用只到了一边时等另一边多久。
pub const PAIR_WAIT: Duration = Duration::from_secs(10);

/// 改 [`PAIR_WAIT`] 的环境变量（毫秒）：只给测试用，不是配置项。
pub const PAIR_WAIT_ENV: &str = "MIYU_ACP_PAIR_WAIT_MS";

/// 等另一边多久。
#[must_use]
pub fn pair_wait() -> Duration {
    std::env::var(PAIR_WAIT_ENV)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(PAIR_WAIT, Duration::from_millis)
}

/// 一轮怎么结束的。
#[derive(Debug)]
pub enum Ended {
    /// 有了回答：回应（成功的、出错的）、被取消、流里出了错。
    Answer(Value),
    /// agent 一行都没写就没了：照没交增量归类的出错。接在复用的进程上的，可以改成新开。
    Silent(Value),
    /// 交回了一件工具调用：回答和挂着的那件。
    Handed(Value, Pending),
}

impl Ended {
    /// 回答本身（挂着的随之丢掉）。
    pub fn into_answer(self) -> Value {
        match self {
            Ended::Answer(answer) | Ended::Silent(answer) | Ended::Handed(answer, _) => answer,
        }
    }
}

/// 在 `live` 上说一轮：`start` 是这一轮的开头，**接上路由以后**才做；它交 `Err(原因)` 的，这一轮是 [`Ended::Silent`]。
pub fn talk(
    job: &Job<'_>,
    live: &mut Live,
    turn: &mut Turn,
    start: impl FnOnce(&mut Live) -> Result<(), String>,
) -> Ended {
    live.conn().route(job.sender.clone());
    // 「看过它还活着」和「接上路由」之间退了的：最后一条送给了没人听的路由，自己补一条。
    if live.closed() {
        job.sender.send(Signal::Closed).ok();
    }
    let ended = match start(live) {
        Ok(()) => listen(job, live, turn),
        Err(why) => Ended::Silent(turn.broken(&why)),
    };
    live.conn().unroute();
    live.memory = turn.memory();
    if matches!(ended, Ended::Handed(..)) {
        // 交回以后 agent 不该再说话：路由撤掉之前就送进来、还没收的，说明它已经不等了。
        while let Ok(signal) = job.signals.try_recv() {
            if signal.is_speech() {
                live.conn().mark_stray();
            }
        }
    }
    ended
}

/// 手里挂着的：MCP 那边到了、还没交回的那件。
type Held = Option<(u64, Reply)>;

/// 读到这一轮有回答、或者交回了为止；还在请核心问着的权限请求回 `cancelled`。
fn listen(job: &Job<'_>, live: &mut Live, turn: &mut Turn) -> Ended {
    let mut asking = Asking::default();
    let ended = hear(job, live, turn, &mut asking);
    asking.abandon(live);
    ended
}

/// [`listen`] 的读。
fn hear(job: &Job<'_>, live: &mut Live, turn: &mut Turn, asking: &mut Asking) -> Ended {
    let mut heard = false;
    let mut held: Held = None;
    let mut deadline: Option<Instant> = None;
    loop {
        let signal = match deadline {
            None => job.signals.recv().unwrap_or(Signal::Closed),
            Some(at) => match job
                .signals
                .recv_timeout(at.saturating_duration_since(Instant::now()))
            {
                Ok(signal) => signal,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(ended) = settle(job, turn.timeout(), &mut held) {
                        return ended;
                    }
                    deadline = None;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => Signal::Closed,
            },
        };
        let why = match signal {
            Signal::Cancel => {
                // ACP「Cancellation」：还没回的权限请求先回 `cancelled`。
                asking.abandon(live);
                wind_down(job, live);
                return Ended::Answer(cancelled());
            }
            Signal::Approved { n, outcome } => {
                asking.answered(job, (live, turn), n, &outcome);
                continue;
            }
            Signal::ToolCall(McpCall {
                n,
                name,
                arguments,
                reply,
            }) => {
                heard = true;
                eprintln!(
                    "call {}: the agent called tool {name} (#{n}) through the tool server",
                    job.call
                );
                if held.is_some() {
                    eprintln!(
                        "call {}: a second tool call arrived while one was waiting; refusing it",
                        job.call
                    );
                    continue;
                }
                held = Some((n, reply));
                let fed = turn.tool_call(n, &name, &arguments);
                if let Some(ended) = settle(job, fed, &mut held) {
                    return ended;
                }
                arm(turn, &mut deadline);
                continue;
            }
            Signal::Update(params) => {
                heard = true;
                let fed = turn.update(params.get("update").unwrap_or(&Value::Null));
                if let Some(ended) = settle(job, fed, &mut held) {
                    return ended;
                }
                arm(turn, &mut deadline);
                continue;
            }
            Signal::Request { id, method, params } => {
                heard = true;
                request(job, (live, turn), asking, (&id, &method, &params));
                continue;
            }
            Signal::Response { id, outcome } => {
                heard = true;
                if live.prompt != Some(id) {
                    eprintln!("call {}: ignoring an answer to request {id}", job.call);
                    continue;
                }
                live.prompt = None;
                let fed = match outcome {
                    Ok(result) => turn.finish(&result),
                    Err(error) => turn.rejected(&error),
                };
                deliver(job, fed.deltas);
                return Ended::Answer(fed.answer.unwrap_or_else(|| json!({})));
            }
            Signal::TooLong => format!(
                "the agent wrote a line longer than {} bytes",
                process::LINE_LIMIT
            ),
            Signal::ReadFailed(error) => format!("reading the agent's output failed: {error}"),
            Signal::Closed => "the agent exited before it answered".to_string(),
        };
        let answer = turn.broken(&why);
        return if heard {
            Ended::Answer(answer)
        } else {
            Ended::Silent(answer)
        };
    }
}

/// 取消以后等 agent 收尾至多多久。
const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// 取消：发 `session/cancel`，等至多 [`CANCEL_GRACE`] 让 agent 回 `cancelled`（ACP「Cancellation」：客户端要把还没答的权限请求
/// 答成 `cancelled`）。之后这段会话照样作废，等是为了让它体面地停下。
fn wind_down(job: &Job<'_>, live: &mut Live) {
    live.conn()
        .notify("session/cancel", &json!({"sessionId": live.session}));
    let deadline = Instant::now() + CANCEL_GRACE;
    while let Ok(signal) = job
        .signals
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        match signal {
            Signal::Request { id, method, .. } if method == "session/request_permission" => {
                live.conn()
                    .reply(&id, &json!({"outcome": {"outcome": "cancelled"}}));
            }
            Signal::Request { id, method, .. } => {
                live.conn().refuse(
                    &id,
                    METHOD_NOT_FOUND,
                    &format!("method not found: {method}"),
                );
            }
            Signal::Response { id, .. } if live.prompt == Some(id) => return,
            Signal::Closed | Signal::ReadFailed(_) | Signal::TooLong => return,
            _ => {}
        }
    }
}

/// agent 的一个请求：要权限的照设置答（或者请核心问主人）；别的回「没有这个方法」。
fn request(
    job: &Job<'_>,
    (live, turn): (&mut Live, &mut Turn),
    asking: &mut Asking,
    (id, method, params): (&Value, &str, &Value),
) {
    if method != "session/request_permission" {
        live.conn()
            .refuse(id, METHOD_NOT_FOUND, &format!("method not found: {method}"));
        return;
    }
    let call = params.get("toolCall").cloned().unwrap_or(Value::Null);
    let options: Vec<Value> = params
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(tool) = call.get("toolCallId").and_then(Value::as_str) {
        turn.asked.insert(tool.to_string());
    }
    let verdict = permission::judge(job.config.own_tools, &call, &job.names, turn.miyu_calls());
    eprintln!(
        "call {}: the agent asked to run {} ({}); {verdict:?}",
        job.call,
        call.get("title")
            .and_then(Value::as_str)
            .unwrap_or("a tool"),
        call.get("kind")
            .and_then(Value::as_str)
            .unwrap_or("no kind"),
    );
    if verdict == Verdict::Ask {
        asking.ask(job, (live, turn), id, &call, options);
        return;
    }
    live.conn()
        .reply(id, &permission::choose(&options, verdict == Verdict::Allow));
    if verdict == Verdict::Reject
        && let Some(tool) = call.get("toolCallId").and_then(Value::as_str)
    {
        deliver(job, turn.refused(tool));
    }
}

/// 第一边到了就开始掐表；已经在掐的不重新算。
fn arm(turn: &Turn, deadline: &mut Option<Instant>) {
    if deadline.is_none() && turn.waiting().is_some() {
        *deadline = Some(Instant::now() + pair_wait());
    }
}

/// 交出 `fed` 的增量；有了回答的，这一轮就结束了：交回工具调用的带上手里的连接，别的就是回答本身。
fn settle(job: &Job<'_>, fed: Fed, held: &mut Held) -> Option<Ended> {
    let Fed {
        deltas,
        answer,
        handed,
    } = fed;
    deliver(job, deltas);
    let answer = answer?;
    match (handed, held.take()) {
        (None, _) => Some(Ended::Answer(answer)),
        (Some(Hand::Mcp(n)), Some((held_n, reply))) if held_n == n => {
            Some(Ended::Handed(answer, Pending::Mcp { n, reply }))
        }
        _ => Some(Ended::Answer(crate::turn::failed(
            crate::failure::Class::Unclassified,
            "internal error: a tool call was handed back without its connection",
        ))),
    }
}

/// 把增量交给核心：一条 `driver.delta` 至多带一段字，一行不会太长。
pub(super) fn deliver(job: &Job<'_>, deltas: Vec<Value>) {
    let mut batch: Vec<Value> = Vec::new();
    for delta in deltas {
        if delta.get("text").is_some() && batch.iter().any(|queued| queued.get("text").is_some()) {
            job.out.notify(
                "driver.delta",
                &json!({"call": job.call, "deltas": std::mem::take(&mut batch)}),
            );
        }
        batch.push(delta);
    }
    if !batch.is_empty() {
        job.out
            .notify("driver.delta", &json!({"call": job.call, "deltas": batch}));
    }
}
