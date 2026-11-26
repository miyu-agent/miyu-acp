//! 一次调用用哪个 agent、怎么起它：设置里手写的（[`crate::config::CustomAgent`]）优先，再照登记表（[`crate::catalog`]）装
//! （[`crate::install`]）。另外认几家已知的 agent（[`Family`]）：工具参数格式要不要照 Gemini 整形、自带的工具怎么关、系统
//! 提示词走哪种 `_meta`。
//!
//! 驱动名和模型名（设计 73 第四节第 1 条，做下来的样子见 README）：清单只能静态写 `[[driver]]`，登记表的 agent 是运行时才知道的，
//! 所以只有一个驱动 `acp/acp`，agent 写在模型名里：`<agent>`（用 agent 自己的默认模型）或 `<agent>/<agent 的模型>`。驱动名
//! 写成 `acp/<agent>`（以后清单给每个 agent 一段 `[[driver]]` 时）的也认。

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use crate::catalog::Catalog;
use crate::config::Config;
use crate::install::{self, Launch};

/// 本包唯一的驱动名（清单 `[[driver]] name`）。
pub const DRIVER: &str = "acp";

/// 已知的几家：做法有差别的地方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Claude 适配器（`claude-acp`）：自带的工具照 `_meta.claudeCode.options.tools` 关。
    Claude,
    /// Google Antigravity（`antigravity-acp`）：参数格式照 Gemini 整形、多声明 `toolAction`；系统提示词、自带工具走
    /// `_meta.antigravity`。
    Antigravity,
    /// Gemini CLI（`gemini`）：参数格式照 Gemini 整形。
    Gemini,
    /// 别的。
    Other,
}

impl Family {
    /// 照 agent 的编号认。
    #[must_use]
    pub fn of(id: &str) -> Family {
        match id {
            "claude-acp" => Family::Claude,
            "antigravity-acp" => Family::Antigravity,
            "gemini" => Family::Gemini,
            _ => Family::Other,
        }
    }

    /// 工具参数格式要照 Gemini 认的写法整形。
    #[must_use]
    pub fn gemini_schemas(self) -> bool {
        matches!(self, Family::Antigravity | Family::Gemini)
    }
}

/// 一次调用要用的 agent。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// 编号。
    pub id: String,
    /// 给人看的名字。
    pub name: String,
    /// 怎么起。
    pub launch: Launch,
    /// 哪一家。
    pub family: Family,
    /// 要登录时告诉主人去哪：网站或仓库地址；手写的没有。
    pub website: Option<String>,
}

/// 从驱动名、模型名里读出 agent 编号和它的模型（[`DRIVER`] 的说明）。读不出 agent 的交 `None`。
#[must_use]
pub fn split(driver: Option<&str>, model: &str) -> Option<(String, Option<String>)> {
    if let Some(agent) = driver
        .and_then(|driver| driver.split_once('/'))
        .map(|(_, name)| name)
        .filter(|name| *name != DRIVER && !name.is_empty())
    {
        let model = (!model.is_empty() && model != "default").then(|| model.to_string());
        return Some((agent.to_string(), model));
    }
    let (agent, rest) = match model.split_once('/') {
        Some((agent, rest)) => (agent, Some(rest)),
        None => (model, None),
    };
    (!agent.is_empty()).then(|| {
        (
            agent.to_string(),
            rest.filter(|rest| !rest.is_empty()).map(str::to_string),
        )
    })
}

/// 同一时刻只装一个：两次调用同时要装同一个 agent 时，后一个等前一个装完再看记号。
static INSTALLING: Mutex<()> = Mutex::new(());

/// 找到编号 `id` 的 agent 并装好：手写的直接用；登记表的照 [`install::plan`] 装（`catalog` 现取，过期了才重取）。
///
/// # Errors
///
/// 没有这个 agent、这台机器装不上、装不成：给人看的英文一句。
pub fn resolve(
    id: &str,
    config: &Config,
    catalog: impl FnOnce() -> Result<Catalog, String>,
    state: &Path,
    cache: &Path,
) -> Result<Agent, String> {
    if let Some(custom) = config.custom(id) {
        return Ok(Agent {
            id: id.to_string(),
            name: custom.name.clone(),
            launch: Launch {
                program: custom.command.clone(),
                args: custom.args.clone(),
                env: custom.env.clone(),
            },
            family: Family::of(id),
            website: None,
        });
    }
    let catalog = catalog()?;
    let entry = catalog.find(id).ok_or_else(|| {
        format!("there is no ACP agent named {id:?} in the registry or in the acp.agents setting")
    })?;
    let platform = install::platform();
    let plan = install::plan(&entry.distribution, &platform).ok_or_else(|| {
        format!(
            "{} cannot be installed on {platform}: the registry has no checked download, npx or uvx package for it",
            entry.name
        )
    })?;
    let launch = {
        let _guard = INSTALLING.lock().unwrap_or_else(PoisonError::into_inner);
        install::ensure(&plan, id, &entry.version, state, cache)?
    };
    Ok(Agent {
        id: id.to_string(),
        name: entry.name.clone(),
        launch,
        family: Family::of(id),
        website: entry.website.clone(),
    })
}

/// 这台机器装得上（照 [`install::plan`]）的登记表 agent 的编号。
#[must_use]
pub fn installable(catalog: &Catalog) -> Vec<String> {
    let platform = install::platform();
    catalog
        .agents
        .iter()
        .filter(|entry| install::plan(&entry.distribution, &platform).is_some())
        .map(|entry| entry.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_comes_from_the_model_or_from_a_per_agent_driver() {
        let some =
            |agent: &str, model: Option<&str>| Some((agent.to_string(), model.map(str::to_string)));
        assert_eq!(
            split(Some("acp/acp"), "claude-acp"),
            some("claude-acp", None)
        );
        assert_eq!(
            split(Some("acp/acp"), "claude-acp/opus"),
            some("claude-acp", Some("opus"))
        );
        assert_eq!(
            split(Some("acp/acp"), "opencode/anthropic/claude"),
            some("opencode", Some("anthropic/claude"))
        );
        assert_eq!(split(Some("acp/acp"), "gemini/"), some("gemini", None));
        assert_eq!(
            split(Some("acp/claude-acp"), "default"),
            some("claude-acp", None)
        );
        assert_eq!(
            split(Some("acp/claude-acp"), "sonnet"),
            some("claude-acp", Some("sonnet"))
        );
        assert_eq!(split(None, ""), None);
    }

    #[test]
    fn families_are_known_by_id() {
        assert_eq!(Family::of("claude-acp"), Family::Claude);
        assert!(Family::of("antigravity-acp").gemini_schemas());
        assert!(Family::of("gemini").gemini_schemas());
        assert!(!Family::of("codex-acp").gemini_schemas());
    }
}
