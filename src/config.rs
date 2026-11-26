//! 这个包自己的配置项（清单 `[settings.*]`）：握手的 `config` 交全的，`extension.config` 交变了的（变成没设的是 `null`）。
//!
//! - `acp.own_tools`：对方自带的工具（设计 73 第四节第 7 条）：`miyu_only`（出厂，只放行只读的搜索、抓网页类）或 `allow`
//!   （也放行对方的工具，经主人确认）。
//! - `acp.keep_seconds`：一轮说完以后 agent 进程留多久等同一段对话的下一轮。
//! - `acp.tool_wait_seconds`：交回核心的一件工具调用（或一次确认）最多等多久。
//! - `acp.agents`：设置里手写的登记表以外的 agent（名字、启动命令、参数、环境变量）。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};

/// `acp.own_tools` 的键。
pub const OWN_TOOLS: &str = "acp.own_tools";
/// `acp.keep_seconds` 的键。
pub const KEEP_SECONDS: &str = "acp.keep_seconds";
/// `acp.tool_wait_seconds` 的键。
pub const TOOL_WAIT_SECONDS: &str = "acp.tool_wait_seconds";
/// `acp.agents` 的键。
pub const AGENTS: &str = "acp.agents";

/// `acp.keep_seconds` 的默认值：闲着的 agent 进程留 10 分钟（设计 73 第四节第 4 条）。
pub const DEFAULT_KEEP: Duration = Duration::from_mins(10);
/// `acp.tool_wait_seconds` 的默认值：一件工具调用等 1 小时。
pub const DEFAULT_TOOL_WAIT: Duration = Duration::from_hours(1);
/// `acp.tool_wait_seconds` 的范围（清单里的 `min`、`max`）。
pub const TOOL_WAIT_RANGE: std::ops::RangeInclusive<u64> = 60..=86_400;

/// 对方自带的工具放不放行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OwnTools {
    /// 只用 Miyu 的工具：只读的搜索、抓网页类放行，别的回拒绝。
    #[default]
    MiyuOnly,
    /// 也放行对方的工具：对方要权限时经核心的提问问主人。
    Allow,
}

/// 设置里手写的一个 agent（`acp.agents` 的一项：名字和一行启动命令）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAgent {
    /// 名字：模型写成 `acp/<名字>`。
    pub name: String,
    /// 启动命令（开头的 `~` 是家目录）。
    pub command: PathBuf,
    /// 参数。
    pub args: Vec<String>,
    /// 环境变量（启动命令开头的 `名字=值`）。
    pub env: Vec<(String, String)>,
}

/// 现在的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// 对方自带的工具。
    pub own_tools: OwnTools,
    /// 闲着的 agent 进程留多久。
    pub keep: Duration,
    /// 一件交回核心的工具调用等多久。
    pub tool_wait: Duration,
    /// 手写的 agent。
    pub agents: Vec<CustomAgent>,
    /// 给人看的字用哪种语言（握手回应的 `language`）：`zh` 写中文，别的写英文。
    pub language: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            own_tools: OwnTools::MiyuOnly,
            keep: DEFAULT_KEEP,
            tool_wait: DEFAULT_TOOL_WAIT,
            agents: Vec::new(),
            language: "en".to_string(),
        }
    }
}

impl Config {
    /// 照 `keys` 改：只看这个包的键，别的不理。
    ///
    /// # Errors
    ///
    /// 哪一项的写法不对：说是哪个键，什么都不改。
    pub fn apply(&mut self, keys: &Map<String, Value>) -> Result<(), String> {
        let mut next = self.clone();
        match keys.get(OWN_TOOLS) {
            None => {}
            Some(Value::Null) => next.own_tools = OwnTools::MiyuOnly,
            Some(Value::String(text)) if text == "miyu_only" => next.own_tools = OwnTools::MiyuOnly,
            Some(Value::String(text)) if text == "allow" => next.own_tools = OwnTools::Allow,
            Some(other) => {
                return Err(format!(
                    "{OWN_TOOLS} must be \"miyu_only\" or \"allow\", got {other}"
                ));
            }
        }
        if let Some(value) = keys.get(KEEP_SECONDS) {
            next.keep = match value {
                Value::Null => DEFAULT_KEEP,
                other => Duration::from_secs(other.as_u64().ok_or_else(|| {
                    format!("{KEEP_SECONDS} must be a whole number of seconds, got {other}")
                })?),
            };
        }
        if let Some(value) = keys.get(TOOL_WAIT_SECONDS) {
            next.tool_wait = match value {
                Value::Null => DEFAULT_TOOL_WAIT,
                other => Duration::from_secs(
                    other
                        .as_u64()
                        .filter(|seconds| TOOL_WAIT_RANGE.contains(seconds))
                        .ok_or_else(|| {
                            format!(
                                "{TOOL_WAIT_SECONDS} must be a whole number of seconds from {} to {}, got {other}",
                                TOOL_WAIT_RANGE.start(),
                                TOOL_WAIT_RANGE.end()
                            )
                        })?,
                ),
            };
        }
        if let Some(value) = keys.get(AGENTS) {
            next.agents = match value {
                Value::Null => Vec::new(),
                Value::Array(items) => items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| custom_agent(index, item))
                    .collect::<Result<_, _>>()?,
                other => return Err(format!("{AGENTS} must be a list, got {other}")),
            };
        }
        *self = next;
        Ok(())
    }

    /// 设置里叫 `name` 的手写 agent。
    #[must_use]
    pub fn custom(&self, name: &str) -> Option<&CustomAgent> {
        self.agents.iter().find(|agent| agent.name == name)
    }
}

/// 读 `acp.agents` 的第 `index` 项：`name` 和一行 `command`（[`command_line`]）。
fn custom_agent(index: usize, item: &Value) -> Result<CustomAgent, String> {
    let field = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
    let name = field("name").trim();
    if !valid_name(name) {
        return Err(format!(
            "{AGENTS}[{index}].name must start with a lowercase letter and use only lowercase letters, digits, - and _ (at most 32), got {name:?}"
        ));
    }
    let (env, words) = command_line(field("command"))
        .map_err(|why| format!("{AGENTS}[{index}].command: {why}"))?;
    let Some((program, args)) = words.split_first() else {
        return Err(format!("{AGENTS}[{index}].command names no program"));
    };
    Ok(CustomAgent {
        name: name.to_string(),
        command: expand_home(Path::new(program), std::env::var_os("HOME").as_deref()),
        args: args.to_vec(),
        env,
    })
}

/// 环境变量：名字和值。
pub type Env = Vec<(String, String)>;

/// 读一行启动命令，照 shell 的常见写法：开头几个 `名字=值` 是环境变量，接着是程序和参数；词之间是空白，单引号里的照原样，
/// 双引号里的 `\` 可以转义 `"` 和 `\`。不展开变量、不认通配符（不经 shell 跑）。设置里写成一行是因为核心的清单不许多行的字
/// 默认为空（参数、环境变量常常是空的）。
///
/// # Errors
///
/// 引号没配对：英文的一句原因。
pub fn command_line(text: &str) -> Result<(Env, Vec<String>), String> {
    let mut words: Vec<String> = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => words.extend(word.take()),
            '\'' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err("a single quote is not closed".into()),
                    }
                }
            }
            '"' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => current.push(c),
                            Some(c) => {
                                current.push('\\');
                                current.push(c);
                            }
                            None => return Err("a double quote is not closed".into()),
                        },
                        Some(c) => current.push(c),
                        None => return Err("a double quote is not closed".into()),
                    }
                }
            }
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    let env_count = words
        .iter()
        .take_while(|word| {
            word.split_once('=').is_some_and(|(key, _)| {
                key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        })
        .count();
    let program = words.split_off(env_count);
    let env = words
        .into_iter()
        .filter_map(|word| {
            word.split_once('=')
                .map(|(key, value)| (key.to_string(), value.to_string()))
        })
        .collect();
    Ok((env, program))
}

/// 手写 agent 的名字合不合规矩：照核心「路径里的名字」的写法（小写字母开头，小写字母、数字、`-`、`_`，至多 32 个）。
#[must_use]
pub fn valid_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// `path` 的头一段正好是 `~` 的，换成 `home`；`~别人/…`、没有 `home`（或是空的）的原样。
#[must_use]
pub fn expand_home(path: &Path, home: Option<&OsStr>) -> PathBuf {
    match (path.strip_prefix("~"), home.filter(|home| !home.is_empty())) {
        (Ok(rest), Some(home)) => Path::new(home).join(rest),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests;
