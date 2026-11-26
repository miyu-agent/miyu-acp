//! 一次 `session/prompt` 的 `prompt`（ACP 的内容块）怎么拼（设计 73 第四节第 4、5 条）。
//!
//! - 新开的会话（[`fresh`]）：一块文字——agent 不收系统提示词的先放 Miyu 的 system（[`words::system_block`]），再是整份历史的
//!   转写和这一轮的话（[`transcript::transcribe`]）；接着是这一轮附的图、文件。
//! - 接着用的会话（[`next`]）：新来的每条消息一块文字（[`transcript::new_turn`]），回合尾巴接在最后；接着是附的图、文件。
//! - 图、文件只附这一轮的（历史里的照转述写字）：图要 agent 收图（`promptCapabilities.image`）、音频要收音频，别的文件要收嵌入的
//!   资源（`embeddedContext`，写成 `resource` 的 `blob`）；交不过去的照 [`transcript`] 写占位。内容是核心交的 `blobs`，原样转交。

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::acp::handshake::Caps;
use crate::transcript::{self, BadRequest};
use crate::words;

/// 核心交的图、文件和 agent 收得下的几种。
#[derive(Debug, Clone, Copy)]
pub struct Media<'a> {
    /// `params.blobs`。
    pub blobs: Option<&'a Map<String, Value>>,
    /// 收图。
    pub image: bool,
    /// 收音频。
    pub audio: bool,
    /// 收嵌入的资源。
    pub embedded: bool,
}

impl<'a> Media<'a> {
    /// 照 `params` 的 `blobs` 和 agent 的能力。
    #[must_use]
    pub fn new(params: &'a Value, caps: &Caps) -> Media<'a> {
        Media {
            blobs: params.get("blobs").and_then(Value::as_object),
            image: caps.prompt.image,
            audio: caps.prompt.audio,
            embedded: caps.prompt.embedded,
        }
    }

    /// 一块图、文件交不交得过去：交得过去的交 ACP 的内容块。
    fn block(&self, block: &Value) -> Option<Value> {
        let hash = block.get("blob")?.as_str()?;
        let blob = self.blobs?.get(hash)?;
        let data = blob.get("data")?.as_str()?;
        let media_type = blob
            .get("media_type")
            .or_else(|| block.get("media_type"))?
            .as_str()?;
        match block.get("type")?.as_str()? {
            "image" if self.image => {
                Some(json!({"type": "image", "mimeType": media_type, "data": data}))
            }
            "file" if media_type.starts_with("audio/") && self.audio => {
                Some(json!({"type": "audio", "mimeType": media_type, "data": data}))
            }
            "file" if self.embedded => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                Some(json!({"type": "resource", "resource": {
                    "uri": format!("attachment:///{}", name.replace(' ', "%20")),
                    "mimeType": media_type,
                    "blob": data
                }}))
            }
            _ => None,
        }
    }
}

/// 第 `from` 条起的消息里交得过去的图、文件：内容块（照先后、同一份只交一次）和它们的 blob 哈希。
fn attachments(request: &Value, from: usize, media: &Media<'_>) -> (Vec<Value>, BTreeSet<String>) {
    let mut blocks = Vec::new();
    let mut attached = BTreeSet::new();
    let messages = request.get("messages").and_then(Value::as_array);
    for message in messages.into_iter().flatten().skip(from) {
        for block in message
            .get("blocks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(hash) = block.get("blob").and_then(Value::as_str) else {
                continue;
            };
            if attached.contains(hash) {
                continue;
            }
            if let Some(content) = media.block(block) {
                attached.insert(hash.to_string());
                blocks.push(content);
            }
        }
    }
    (blocks, attached)
}

fn text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// 新开的会话的第一次提问；`system` 是要放进提问里的系统提示词（agent 不收系统提示词时）。
///
/// # Errors
///
/// 请求的形状不对：[`BadRequest`]。
pub fn fresh(
    request: &Value,
    media: &Media<'_>,
    system: Option<&str>,
) -> Result<Vec<Value>, BadRequest> {
    let live = transcript::live_from(request)?;
    let (blocks, attached) = attachments(request, live, media);
    let request = transcript::with_attached(request, &attached);
    let mut body = transcript::transcribe(&request, transcript::BUDGET)?;
    if let Some(system) = system {
        body = format!("{}\n\n{body}", words::system_block(system));
    }
    let mut prompt = vec![text(&body)];
    prompt.extend(blocks);
    Ok(prompt)
}

/// 接着用的会话的下一轮：第 `from` 条起的新消息。
///
/// # Errors
///
/// 同 [`fresh`]。
pub fn next(request: &Value, from: usize, media: &Media<'_>) -> Result<Vec<Value>, BadRequest> {
    let (blocks, attached) = attachments(request, from, media);
    let request = transcript::with_attached(request, &attached);
    let mut prompt: Vec<Value> = transcript::new_turn(&request, from)?
        .iter()
        .map(|piece| text(piece))
        .collect();
    prompt.extend(blocks);
    Ok(prompt)
}

#[cfg(test)]
mod tests;
