//! 握手和开会话怎么写、怎么读（ACP「Initialization」「Session Setup」「Authentication」三页，2026-11-26 读）。纯的部分在这里，
//! 发出去由调用的人用 [`super::Conn`] 做。
//!
//! - `initialize`：协议版本 1；客户端能力只说「不给文件、不给终端」（`fs` 两项、`terminal` 都是 `false`，设计 73 第四节第 7
//!   条），不声明 `auth.terminal`（不替主人在终端里跑登录）。
//! - agent 回的能力读成 [`Caps`]：会话能不能接着用（`loadSession`、`sessionCapabilities.resume`）、提问能带什么（图、音频、
//!   嵌入的资源）、系统提示词怎么交（[`SystemMode`]）、登录方式。
//! - 系统提示词（设计 73 第四节第 5 条）：有 `override` 的整个替换；只有 `append`（`systemPrompt: {}`）的追加；Antigravity 照
//!   它自己的 `_meta.antigravity.systemPrompt.append`；都没有的放在第一次提问的最前面（[`SystemMode::InPrompt`]）。
//! - 只用 Miyu 的工具时，认得的 agent 照它们的说法关掉自带的工具（[`session_meta`]）。

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::agents::Family;
use crate::config::OwnTools;

/// 说的 ACP 协议版本。
pub const PROTOCOL_VERSION: u64 = 1;

/// MCP 服务在 `mcpServers` 里的名字（设计 73 第四节第 6 条）。
pub const SERVER_NAME: &str = "miyu";

/// 系统提示词怎么交。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMode {
    /// `systemPrompt: {mode: "override"}`。
    Override,
    /// `systemPrompt: {mode: "append"}`。
    Append,
    /// `_meta.antigravity.systemPrompt.append`。
    AntigravityMeta,
    /// 放在第一次提问的最前面。
    InPrompt,
}

/// 一种登录方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethod {
    /// 编号。
    pub id: String,
    /// 给人看的名字。
    pub name: String,
}

/// agent 在 `initialize` 回的能力里本包用得上的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    /// agent 回的协议版本。
    pub protocol: u64,
    /// `session/resume` 可用。
    pub resume: bool,
    /// 提问能带什么。
    pub prompt: PromptCaps,
    /// 系统提示词怎么交。
    pub system: SystemMode,
    /// 登录方式。
    pub auth_methods: Vec<AuthMethod>,
    /// agent 自己报的名字（`agentInfo.title`，没有的用 `name`）。
    pub title: Option<String>,
}

/// 提问能带什么（`promptCapabilities`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptCaps {
    /// 图。
    pub image: bool,
    /// 音频。
    pub audio: bool,
    /// 嵌入的资源。
    pub embedded: bool,
}

/// `initialize` 的参数。
#[must_use]
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": {"readTextFile": false, "writeTextFile": false},
            "terminal": false
        },
        "clientInfo": {"name": "miyu-acp", "title": "Miyu", "version": env!("CARGO_PKG_VERSION")}
    })
}

/// 读 `initialize` 的回应。
#[must_use]
pub fn read_caps(result: &Value) -> Caps {
    let agent = result.get("agentCapabilities").unwrap_or(&Value::Null);
    let flag = |pointer: &str| agent.pointer(pointer).and_then(Value::as_bool) == Some(true);
    let present = |pointer: &str| agent.pointer(pointer).is_some_and(|value| !value.is_null());
    let system = if present("/sessionCapabilities/systemPrompt/mode/override") {
        SystemMode::Override
    } else if present("/sessionCapabilities/systemPrompt") {
        SystemMode::Append
    } else if flag("/_meta/antigravity/systemPrompt/append") {
        SystemMode::AntigravityMeta
    } else {
        SystemMode::InPrompt
    };
    let auth_methods = result
        .get("authMethods")
        .and_then(Value::as_array)
        .map(|methods| {
            methods
                .iter()
                .filter_map(|method| {
                    let id = method.get("id")?.as_str()?.to_string();
                    let name = method
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&id)
                        .to_string();
                    Some(AuthMethod { id, name })
                })
                .collect()
        })
        .unwrap_or_default();
    let info = result.get("agentInfo").unwrap_or(&Value::Null);
    Caps {
        protocol: result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        resume: present("/sessionCapabilities/resume"),
        prompt: PromptCaps {
            image: flag("/promptCapabilities/image"),
            audio: flag("/promptCapabilities/audio"),
            embedded: flag("/promptCapabilities/embeddedContext"),
        },
        system,
        auth_methods,
        title: info
            .get("title")
            .or_else(|| info.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// 用密钥登录的那种方式（编号里有 `api-key`、`api_key`、`apikey`）：供应商里填了 `key` 的照它 `authenticate`。
#[must_use]
pub fn api_key_method(caps: &Caps) -> Option<&AuthMethod> {
    caps.auth_methods.iter().find(|method| {
        let id = method.id.to_ascii_lowercase();
        ["api-key", "api_key", "apikey"]
            .iter()
            .any(|needle| id.contains(needle))
    })
}

/// 用密钥登录的 `authenticate` 参数：密钥放在 `_meta` 里（Antigravity 1.5.2 照这一格读，几种写法都认；ACP 本身的
/// `authenticate` 只有 `methodId`）。
#[must_use]
pub fn api_key_params(method: &AuthMethod, key: &str) -> Value {
    json!({"methodId": method.id, "_meta": {"api-key": key}})
}

/// `mcpServers` 里本包那一条：程序、`mcp` 子命令、套接字和口令。
#[must_use]
pub fn mcp_server(program: &Path, socket: &Path, token: &str) -> Value {
    json!({
        "name": SERVER_NAME,
        "command": program.display().to_string(),
        "args": ["mcp"],
        "env": [
            {"name": crate::bridge::ENV_BRIDGE, "value": socket.display().to_string()},
            {"name": crate::bridge::ENV_TOKEN, "value": token}
        ]
    })
}

/// `session/new`（`session/resume` 也一样带）的参数：工作目录 `cwd`、`mcp`（没有工具的是空表）、系统提示词照 `caps`，认得的
/// agent 另加 `_meta`。`system` 是要交的系统提示词全文；[`SystemMode::InPrompt`] 的不在这里交。
#[must_use]
pub fn session_params(
    cwd: &Path,
    mcp: Option<Value>,
    caps: &Caps,
    system: &str,
    family: Family,
    own: OwnTools,
) -> Value {
    let mut params = Map::new();
    params.insert("cwd".into(), json!(cwd.display().to_string()));
    params.insert("mcpServers".into(), Value::Array(mcp.into_iter().collect()));
    let content = json!([{"type": "text", "text": system}]);
    match caps.system {
        SystemMode::Override => {
            params.insert(
                "systemPrompt".into(),
                json!({"mode": "override", "content": content}),
            );
        }
        SystemMode::Append => {
            params.insert(
                "systemPrompt".into(),
                json!({"mode": "append", "content": content}),
            );
        }
        SystemMode::AntigravityMeta | SystemMode::InPrompt => {}
    }
    let meta = session_meta(caps, system, family, own);
    if !meta.is_empty() {
        params.insert("_meta".into(), Value::Object(meta));
    }
    Value::Object(params)
}

/// 认得的 agent 的 `_meta`：
///
/// - Antigravity（1.5.2 的源码里读的几格）：`systemPrompt.append`（系统提示词）、`eagerMcpServers`（Miyu 的工具直接挂进模型
///   的工具表，不要它先查）、只用 Miyu 的工具时 `enabledTools: ["search_web"]`（自带的只留搜索；它的 `read_url_content` 只交回
///   一个本机文件路径，`miyu-antigravity` 照主人定的不开）。
/// - Claude 适配器（0.112.0 的源码）：只用 Miyu 的工具时 `claudeCode.options.tools: ["WebSearch", "WebFetch"]`（自带的只留
///   搜索、抓网页）。
#[must_use]
pub fn session_meta(
    caps: &Caps,
    system: &str,
    family: Family,
    own: OwnTools,
) -> Map<String, Value> {
    let mut meta = Map::new();
    let only = own == OwnTools::MiyuOnly;
    match family {
        Family::Antigravity => {
            let mut antigravity = Map::new();
            if caps.system == SystemMode::AntigravityMeta {
                antigravity.insert("systemPrompt".into(), json!({"append": system}));
            }
            antigravity.insert("eagerMcpServers".into(), json!([SERVER_NAME]));
            if only {
                antigravity.insert("enabledTools".into(), json!(["search_web"]));
            }
            meta.insert("antigravity".into(), Value::Object(antigravity));
        }
        Family::Claude if only => {
            meta.insert(
                "claudeCode".into(),
                json!({"options": {"tools": ["WebSearch", "WebFetch"]}}),
            );
        }
        _ => {}
    }
    meta
}

/// `session/new` 回的里本包用得上的：会话编号，模型的选项（`configOptions` 里 `category: "model"` 的那一项，或者老的
/// `models`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// 会话编号。
    pub session: String,
    /// 选模型那一项的编号（`configOptions` 的写法）；用老的 `models` 写法的是 `None`。
    pub model_option: Option<String>,
    /// agent 给的模型。
    pub models: Vec<String>,
    /// 现在用的模型。
    pub current: Option<String>,
}

/// 读 `session/new` 的回应；没有 `sessionId` 的是 `None`。
#[must_use]
pub fn read_session(result: &Value) -> Option<NewSession> {
    let session = result.get("sessionId")?.as_str()?.to_string();
    let mut found = NewSession {
        session,
        model_option: None,
        models: Vec::new(),
        current: None,
    };
    if let Some(option) = result
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options.iter().find(|option| {
                option.get("category").and_then(Value::as_str) == Some("model")
                    && option
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("select")
                        == "select"
            })
        })
    {
        found.model_option = option.get("id").and_then(Value::as_str).map(str::to_string);
        found.current = option
            .get("currentValue")
            .and_then(Value::as_str)
            .map(str::to_string);
        found.models = select_values(option.get("options"));
    } else if let Some(models) = result.get("models") {
        found.current = models
            .get("currentModelId")
            .and_then(Value::as_str)
            .map(str::to_string);
        found.models = models
            .get("availableModels")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("modelId")?.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
    }
    Some(found)
}

/// 选项里的值：平的一列，或者分了组的（每组一列）。
fn select_values(options: Option<&Value>) -> Vec<String> {
    let Some(items) = options.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .flat_map(|item| match item.get("options").and_then(Value::as_array) {
            Some(group) => group.iter().collect::<Vec<_>>(),
            None => vec![item],
        })
        .filter_map(|item| item.get("value")?.as_str().map(str::to_string))
        .collect()
}

/// 选模型 `model` 的请求：方法和参数。`configOptions` 写法的是 `session/set_config_option`，老写法的是 `session/set_model`。
#[must_use]
pub fn select_model(found: &NewSession, model: &str) -> (&'static str, Value) {
    match &found.model_option {
        Some(option) => (
            "session/set_config_option",
            json!({"sessionId": found.session, "configId": option, "value": model}),
        ),
        None => (
            "session/set_model",
            json!({"sessionId": found.session, "modelId": model}),
        ),
    }
}

#[cfg(test)]
mod tests;
