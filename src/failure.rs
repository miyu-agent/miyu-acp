//! agent 出错归成核心认的类（设计 73 第四节第 8 条、照 `miyu-antigravity` 的 `failure` 搬）。纯函数。
//!
//! | 字里有 | 归成 |
//! |---|---|
//! | [`CONTEXT_TOO_LONG`] 里的一种 | `context_too_long`（核心先压缩再来） |
//! | [`REJECTED`] 里的一种 | `unclassified`（请求本身有错） |
//! | [`AUTH`] 里的一种 | `auth` |
//! | [`QUOTA`] 里的一种 | `quota` |
//! | [`RATE_LIMITED`] 里的一种 | `rate_limited` |
//! | 别的 | 还没交过增量的 `retryable`，交过的 `unclassified` |
//!
//! 重试由核心照类决定（只有 `retryable`、`rate_limited` 这几类再来，`auth`、`unclassified` 不来，`context_too_long` 先压缩；经池
//! 调用的 `auth` 还会冷却这个模型、换下一个）。所以再来也一样的错（账号被停用、请求本身有错）不能落到「别的」那一格。
//!
//! ACP 的 agent 背后是哪家模型都有：Gemini（照 gRPC 状态名报，`INVALID_ARGUMENT (code 400)`）的几句照 `miyu-antigravity` 实测的
//! 原话；Anthropic、OpenAI 的照它们的出错说明加了几句（`prompt is too long`、`context_length_exceeded`、`invalid x-api-key`）。
//! 先后照表：几种都有的取靠前的。不分大小写。协议层的「要登录」（`-32000`）不走这张表，见 [`crate::acp::RpcError::needs_login`]。

use serde_json::{Value, json};

/// 上下文超长的字眼。Gemini 超长时报 400 `INVALID_ARGUMENT`，原话是「The input token count (N) exceeds the maximum number of
/// tokens allowed (M).」（Google 官方开发者论坛的多份报告）；Vertex AI 的出错说明把「Request exceeds the model's input token
/// limit」列为 400 的例子，旧的 Vertex 写法是「Input request contains (N) tokens, which is more than the max tokens limit」。
/// 只认这几句说的是输入的 token 数，别的 400（工具参数格式、输出上限写错）照旧归 [`REJECTED`]。
pub const CONTEXT_TOO_LONG: &[&str] = &[
    "exceeds the maximum number of tokens allowed",
    "input token limit",
    "which is more than the max tokens limit",
    // Anthropic：「prompt is too long: 210000 tokens > 200000 maximum」。
    "prompt is too long",
    // OpenAI：错误码 `context_length_exceeded`，原话「This model's maximum context length is …」。
    "context_length_exceeded",
    "maximum context length",
];

/// 请求本身有错的字眼：agy 照 gRPC 的状态名报（`INVALID_ARGUMENT (code 400): …`，2026-11-20 主人本机）。Gemini 的
/// 出错说明里 400、404 是请求写错、模型不存在，`failed_precondition` 是账号或地区的前提不满足，都要人改了才行。
pub const REJECTED: &[&str] = &[
    "invalid_argument",
    "failed_precondition",
    "not_found",
    "(code 400)",
    "(code 404)",
];

/// 没登录、账号用不了的字眼。账号被停用的原话（2026-11-20 主人本机）：「This service has been disabled in this account for
/// violation of Terms of Service. Please submit an appeal to continue」；`permission_denied`（403）是账号没有权限。
pub const AUTH: &[&str] = &[
    "not logged in",
    "unauthenticated",
    "login required",
    "sign in",
    "authentication",
    "terms of service",
    "submit an appeal",
    "permission_denied",
    "(code 401)",
    "(code 403)",
    "invalid x-api-key",
    "invalid api key",
    "authentication_error",
];

/// 额度用完的字眼（agy 把额度用完报成 `RESOURCE_EXHAUSTED`，旧版 2026-09-15 实测）。
pub const QUOTA: &[&str] = &[
    "quota",
    "resource_exhausted",
    "resource exhausted",
    "usage limit",
    "credits",
];

/// 限流的字眼。
pub const RATE_LIMITED: &[&str] = &[
    "rate limit",
    "rate-limit",
    "rate_limit",
    "too many requests",
];

/// 回答里 `message` 至多多少个字（核心照 500 截）。
pub const MESSAGE_CHARS: usize = 500;

/// 核心认的出错类（参考文档 `driver.call` 的「回应」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// `retryable`。
    Retryable,
    /// `rate_limited`。
    RateLimited,
    /// `auth`。
    Auth,
    /// `quota`。
    Quota,
    /// `context_too_long`：输入超过了模型的上下文，核心先压缩再来。
    ContextTooLong,
    /// `content_policy`：agent 拒绝接着做（`stopReason: refusal`）。
    ContentPolicy,
    /// `unclassified`。
    Unclassified,
}

impl Class {
    /// 协议里的写法。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Retryable => "retryable",
            Class::RateLimited => "rate_limited",
            Class::Auth => "auth",
            Class::Quota => "quota",
            Class::ContextTooLong => "context_too_long",
            Class::ContentPolicy => "content_policy",
            Class::Unclassified => "unclassified",
        }
    }
}

/// agent 报的错 `said` 归哪一类；`delivered` 是这一次已经交过增量。
#[must_use]
pub fn classify(said: &str, delivered: bool) -> Class {
    let lower = said.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if has(CONTEXT_TOO_LONG) {
        Class::ContextTooLong
    } else if has(REJECTED) {
        Class::Unclassified
    } else if has(AUTH) {
        Class::Auth
    } else if has(QUOTA) {
        Class::Quota
    } else if has(RATE_LIMITED) {
        Class::RateLimited
    } else {
        otherwise(delivered)
    }
}

/// 认不出原因的出错（包括没有 `result` 就退出了、5xx 这种对面一时的毛病）：还没交过增量的 `retryable`，交过的 `unclassified`。
#[must_use]
pub fn otherwise(delivered: bool) -> Class {
    if delivered {
        Class::Unclassified
    } else {
        Class::Retryable
    }
}

/// 回答里的 `error`：`{"class", "message"}`，`message` 截到 [`MESSAGE_CHARS`] 个字。
#[must_use]
pub fn error(class: Class, message: &str) -> Value {
    let message: String = message.chars().take(MESSAGE_CHARS).collect();
    json!({"class": class.as_str(), "message": message})
}

#[cfg(test)]
mod tests;
