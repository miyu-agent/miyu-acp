//! 把统一请求的 `messages` 整份转写成发给 agent 的一段文字（设计 73 第四节第 4 条：对不上的新开一个 ACP 会话，历史写成一段文字
//! 放在第一次提问前面）。纯函数。照 `miyu-antigravity` 的 `transcript` 搬。
//!
//! 格式照旧版 Miyu 的 `cli_relay/payload.rs`：一段说明加 `<conversation-history>`，里面照先后写 `User:`、`Assistant:`、
//! `[called tool NAME with ARGS]`、`[tool result]`；最后一轮人说的话（结尾连着的几条 user 消息）单独成块跟在后面。只有这
//! 一轮、没有历史的，直接发那几条的字。`request.tail` 有的接在最后。
//!
//! 接在活着的 ACP 会话上的新的一轮只发新的那几条（[`new_turn`]），一条一段。
//!
//! 图、文件：作为 ACP 的内容块一起交过去的（[`with_attached`] 记下的那几份，[`crate::prompt`] 定哪些交得过去），这里只写一句
//! 「附在后面」；交不过去的照核心给的转述，没有转述的写看不到。
//!
//! 整条不超过 [`BUDGET`] 字节：超了从最老的回合丢起、写一句省略；最后一轮不砍。给模型看的字在 [`crate::words`]。
//!
//! 没走完的那一轮（施工 ACP-10）：核心把「这一轮出错了、被打断了」那一句（`<turn-ended reason=…>` 一块字，内核
//! `resources/core/turn-ended/`）攒在人这一边，和下一句人话在同一条 user 消息里。这里照实分开：那一句和它前面的人话归进历史
//! （`User:` 那一段以那一句结尾，看得出那一轮没有回答），后面的才是这一轮的话。不分的话两次的话连成一段当这一轮交过去，agent
//! 把两句都答一遍、还当成自己上一次的回复被打断了在重试（真人实测：登录失败那一轮之后自我介绍了两遍）。字一个不加。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::words;

/// 一条输入至多多少字节。ACP 的 agent 没有 agy 那种悄悄截尾的上限，超过模型窗口的会报错（核心照 `context_too_long` 先压缩），
/// 所以这里只挡离谱的大小：两百万字节，远大于常见窗口能装下的字。
pub const BUDGET: usize = 2_000_000;

/// 请求的形状不对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRequest(pub String);

impl std::fmt::Display for BadRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad request: {}", self.0)
    }
}

impl std::error::Error for BadRequest {}

/// 转写 `request`（统一请求的 JSON），整条至多 `budget` 字节（最后一轮自己就超了的照发、历史全丢）。
///
/// # Errors
///
/// `messages` 不是数组、一条消息不是对象或 `role` 不是 `user`、`assistant`、`tool`、`blocks` 不是数组：[`BadRequest`]。
pub fn transcribe(request: &Value, budget: usize) -> Result<String, BadRequest> {
    let parsed = parse(request)?;
    let live_from = split(&parsed);
    let (history, live) = parsed.split_at(live_from);
    let mut lines: Vec<(bool, String)> = history
        .iter()
        .map(|message| (message.role == Role::User, message.history_line()))
        .collect();
    let mut current: Vec<&str> = live.iter().map(|message| message.body.as_str()).collect();
    if let Some(at) = live.first().and_then(|first| first.ended) {
        let (done, rest) = live[0].body.split_at(at);
        lines.push((true, format!("{}{done}\n\n", words::USER)));
        current[0] = rest.trim_start_matches('\n');
    }

    let mut last = current
        .into_iter()
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if last.is_empty() && (!lines.is_empty() || request.get("tail").is_none()) {
        last = words::CONTINUE.to_string();
    }
    if let Some(tail) = request.get("tail").and_then(Value::as_str) {
        if !last.is_empty() {
            last.push_str("\n\n");
        }
        last.push_str(tail);
    }
    if lines.is_empty() {
        return Ok(last);
    }

    let fixed = words::HISTORY_HEAD.len() + words::HISTORY_FOOT.len() + 2 + last.len();
    let whole: usize = lines.iter().map(|(_, line)| line.len()).sum();
    let keep_from = if fixed + whole <= budget {
        0
    } else {
        oldest_kept(&lines, budget.saturating_sub(fixed + words::OMITTED.len()))
    };

    let mut text = String::from(words::HISTORY_HEAD);
    if keep_from > 0 {
        text.push_str(words::OMITTED);
    }
    for (_, line) in &lines[keep_from..] {
        text.push_str(line);
    }
    text.push_str(words::HISTORY_FOOT);
    text.push_str("\n\n");
    text.push_str(&last);
    Ok(text)
}

/// 这一轮从第几条起：结尾连着的 user 消息里最后一条带 `<turn-ended>` 的那一条（它的前半截归历史）；没有的照结尾连着的 user
/// 消息的头一条，没有历史的是 0。
fn split(parsed: &[Message]) -> usize {
    let trailing = parsed
        .iter()
        .rposition(|message| message.role != Role::User)
        .map_or(0, |index| index + 1);
    parsed[trailing..]
        .iter()
        .rposition(|message| message.ended.is_some())
        .map_or(trailing, |offset| trailing + offset)
}

/// 接在活着的 ACP 会话上的新的一轮：`request.messages` 第 `from` 条起每条一段字，块转成字照
/// [`transcribe`] 的规矩，没有字的不要；`request.tail` 有的接在最后一段后面；一段都没有的是 [`words::CONTINUE`]。
///
/// # Errors
///
/// 同 [`transcribe`]。
pub fn new_turn(request: &Value, from: usize) -> Result<Vec<String>, BadRequest> {
    let parsed = parse(request)?;
    let mut texts: Vec<String> = parsed
        .into_iter()
        .skip(from)
        .map(|message| message.body)
        .filter(|body| !body.is_empty())
        .collect();
    if texts.is_empty() {
        texts.push(words::CONTINUE.to_string());
    }
    if let Some(tail) = request.get("tail").and_then(Value::as_str)
        && let Some(last) = texts.last_mut()
    {
        last.push_str("\n\n");
        last.push_str(tail);
    }
    Ok(texts)
}

/// 第 `index` 条消息的字（块转成字，块之间换行，规矩同 [`transcribe`]）；没有这一条的是 `None`。给回工具结果用
/// （图片、文件换成占位）。
///
/// # Errors
///
/// 同 [`transcribe`]。
pub fn message_text(request: &Value, index: usize) -> Result<Option<String>, BadRequest> {
    Ok(parse(request)?
        .into_iter()
        .nth(index)
        .map(|message| message.body))
}

/// 读出 `request.messages` 的每一条。
fn parse(request: &Value) -> Result<Vec<Message>, BadRequest> {
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| BadRequest("request.messages is not an array".into()))?;
    let texts = |key: &str| -> BTreeMap<&str, &str> {
        request
            .get(key)
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(blob, text)| Some((blob.as_str(), text.as_str()?)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let known = Known {
        described: texts("described"),
        attached: request
            .get(ATTACHED)
            .and_then(Value::as_array)
            .map(|hashes| hashes.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default(),
    };
    messages
        .iter()
        .map(|message| Message::read(message, &known))
        .collect()
}

/// 请求里「作为内容块交过去了」的图、文件那一格：`["<blob 哈希>", ..]`。核心不写这一格，是本包定好交哪几份以后加进来的
/// （[`with_attached`]）。
pub const ATTACHED: &str = "miyu_acp_attached";

/// 照 `attached`（作为内容块交过去的 blob 哈希）给 `request` 加上 [`ATTACHED`] 那一格；一份都没有的原样交回。
#[must_use]
pub fn with_attached<'a>(
    request: &'a Value,
    attached: &BTreeSet<String>,
) -> std::borrow::Cow<'a, Value> {
    if attached.is_empty() {
        return std::borrow::Cow::Borrowed(request);
    }
    let mut request = request.clone();
    if let Some(object) = request.as_object_mut() {
        let hashes = attached.iter().cloned().map(Value::from).collect();
        object.insert(ATTACHED.to_string(), Value::Array(hashes));
    }
    std::borrow::Cow::Owned(request)
}

/// 这一轮从第几条起：结尾连着的 user 消息的头一条，带着没走完的那一轮的从那一条起；没有历史的是 0。
///
/// # Errors
///
/// 同 [`transcribe`]。
pub fn live_from(request: &Value) -> Result<usize, BadRequest> {
    Ok(split(&parse(request)?))
}

/// 图、文件块转成字时要查的：核心交的图的转述，作为内容块交过去了的那几份。
struct Known<'a> {
    described: BTreeMap<&'a str, &'a str>,
    attached: BTreeSet<&'a str>,
}

/// 历史从哪一段起留（`lines` 每段带着是不是人说的）：从最新往回加，放不下 `room` 就停；再往后挪到下一段人说的，让转写从一轮的
/// 开头起（截在回复、工具结果中间会让模型把半截旧回复当上文）。一段都放不下的交 `lines.len()`。
fn oldest_kept(lines: &[(bool, String)], room: usize) -> usize {
    let mut used = 0;
    let mut keep_from = lines.len();
    for (index, (_, line)) in lines.iter().enumerate().rev() {
        if used + line.len() > room {
            break;
        }
        used += line.len();
        keep_from = index;
    }
    lines[keep_from..]
        .iter()
        .position(|(user, _)| *user)
        .map_or(lines.len(), |offset| keep_from + offset)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
    Tool { failed: bool },
}

/// 一条消息：谁说的，块转成的字（块之间换行）；人这一边带着「这一轮没走完」那一句（[`TURN_ENDED`]）的，记下最后那一句在
/// 字里结束在哪（字节）。
struct Message {
    role: Role,
    body: String,
    ended: Option<usize>,
}

/// 核心写「这一轮没走完」那一句的外壳（内核 `resources/core/turn-ended/*.txt`）。
const TURN_ENDED: (&str, &str) = ("<turn-ended ", "</turn-ended>");

impl Message {
    fn read(message: &Value, known: &Known<'_>) -> Result<Message, BadRequest> {
        let role = match message.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("tool") => Role::Tool {
                failed: message.get("error").and_then(Value::as_bool) == Some(true),
            },
            other => {
                return Err(BadRequest(format!(
                    "a message has role {other:?}, not user, assistant or tool"
                )));
            }
        };
        let blocks = message
            .get("blocks")
            .and_then(Value::as_array)
            .ok_or_else(|| BadRequest("a message has no blocks array".into()))?;
        let mut body = String::new();
        let mut ended = None;
        for text in blocks.iter().filter_map(|block| block_text(block, known)) {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&text);
            let marker = text.starts_with(TURN_ENDED.0) && text.trim_end().ends_with(TURN_ENDED.1);
            if marker && role == Role::User {
                ended = Some(body.len());
            }
        }
        Ok(Message { role, body, ended })
    }

    fn history_line(&self) -> String {
        let label = match self.role {
            Role::User => words::USER,
            Role::Assistant => words::ASSISTANT,
            Role::Tool { failed: false } => words::TOOL_RESULT,
            Role::Tool { failed: true } => words::TOOL_FAILED,
        };
        format!("{label}{}\n\n", self.body)
    }
}

/// 一块转成的字；不写的（思考、空的字、不认识的块）是 `None`。
fn block_text(block: &Value, known: &Known<'_>) -> Option<String> {
    let field = |key: &str| block.get(key).and_then(Value::as_str);
    let attached = field("blob").is_some_and(|blob| known.attached.contains(blob));
    match field("type")? {
        "text" => field("text")
            .filter(|text| !text.is_empty())
            .map(str::to_string),
        "image" => Some(
            match (
                attached,
                field("blob").and_then(|blob| known.described.get(blob)),
            ) {
                (true, _) => words::image_attached(field("name")),
                (false, Some(description)) => words::image_described(field("name"), description),
                (false, None) => words::image_omitted(field("name")),
            },
        ),
        "file" => {
            let name = field("name").unwrap_or_default();
            Some(if attached {
                words::file_attached(name)
            } else {
                words::file_omitted(name)
            })
        }
        "path" => Some(words::path_attached(
            field("name").unwrap_or_default(),
            field("path").unwrap_or_default(),
        )),
        "tool_call" => Some(words::called_tool(
            field("name").unwrap_or_default(),
            field("args").unwrap_or_default(),
        )),
        "hosted_tool" => {
            let mut text = words::provider_tool(
                field("name").unwrap_or_default(),
                field("args").unwrap_or_default(),
            );
            if let Some(outcome) = block.get("outcome") {
                let failed = outcome.get("failed").and_then(Value::as_bool) == Some(true);
                let said = outcome
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                text.push('\n');
                text.push_str(&words::provider_result(said, failed));
            }
            Some(text)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
