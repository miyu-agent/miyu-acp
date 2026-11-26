//! 会话的权限模式（ACP「Session Modes」一页：`session/new` 回的 `modes`、`session/set_mode`；新的写法是会话配置项里
//! `category: "mode"` 的那一项、`session/set_config_option`）。纯函数。
//!
//! 开会话以后把对方切到「每次都问」的那一档（施工 ACP-10）：不切的话对方照它自己的默认、或者照主人本机给那个程序的设置起，
//! 自带的工具不来要权限就跑了（真人实测：Claude 适配器 0.112.0 没有设置时起在 `auto`，主人设成「不问」的照
//! `bypassPermissions` 起）。哪一档是「每次都问」照 [`ASKING`] 的编号认，2026-11-26 读各家的源码：
//!
//! - Claude 适配器 0.112.0、Gemini CLI 0.71.1、Antigravity 1.5.2：`default`（「Always ask before making changes」「Prompts
//!   for approval」「Default permission prompt flow」）；
//! - Codex 适配器 2.12.0：没有每次都问的一档，最紧的是 `read-only`（改文件、上网要批准，命令在只读的沙盒里跑）。
//!
//! 对方报的里一个都对不上的不切（日志里记一句），没报模式的维持原样。

use serde_json::{Value, json};

/// 认作「每次都问」的模式编号，排在前面的先挑。
pub const ASKING: &[&str] = &["default", "manual", "ask", "read-only"];

/// `session/new` 回的模式：现在是哪一档、能选哪几档、怎么切。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modes {
    /// 现在的。
    pub current: Option<String>,
    /// 能选的编号。
    pub available: Vec<String>,
    /// 会话配置项里选模式那一项的编号；只报了老的 `modes` 写法的是 `None`。
    pub option: Option<String>,
    /// 报了老的 `modes`（能用 `session/set_mode`）。
    pub legacy: bool,
}

/// 读 `session/new` 的回应；两种写法都没报的是 `None`。
#[must_use]
pub fn read(result: &Value) -> Option<Modes> {
    let option = result
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options
                .iter()
                .find(|option| option.get("category").and_then(Value::as_str) == Some("mode"))
        });
    let legacy = result.get("modes").filter(|modes| modes.is_object());
    if option.is_none() && legacy.is_none() {
        return None;
    }
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
    let mut modes = Modes {
        current: None,
        available: Vec::new(),
        option: option.and_then(|option| text(option.get("id"))),
        legacy: legacy.is_some(),
    };
    if let Some(legacy) = legacy {
        modes.current = text(legacy.get("currentModeId"));
        modes.available = legacy
            .get("availableModes")
            .and_then(Value::as_array)
            .map(|all| all.iter().filter_map(|mode| text(mode.get("id"))).collect())
            .unwrap_or_default();
    }
    if let Some(option) = option {
        modes.current = modes.current.or_else(|| text(option.get("currentValue")));
        if modes.available.is_empty() {
            modes.available = option
                .get("options")
                .and_then(Value::as_array)
                .map(|all| {
                    all.iter()
                        .filter_map(|item| text(item.get("value")))
                        .collect()
                })
                .unwrap_or_default();
        }
    }
    Some(modes)
}

/// 「每次都问」的那一档：[`ASKING`] 里对方报了的第一个。
#[must_use]
pub fn asking(modes: &Modes) -> Option<&'static str> {
    ASKING
        .iter()
        .copied()
        .find(|id| modes.available.iter().any(|offered| offered == id))
}

/// 会话 `session` 切到 `target` 的请求（方法和参数）；已经是它的、没法切的是 `None`。报了老写法的用 `session/set_mode`
/// （「Session Modes」一页，各家都实现了），只有配置项的用 `session/set_config_option`。
#[must_use]
pub fn switch(session: &str, modes: &Modes, target: &str) -> Option<(&'static str, Value)> {
    if modes.current.as_deref() == Some(target) {
        return None;
    }
    if modes.legacy {
        return Some((
            "session/set_mode",
            json!({"sessionId": session, "modeId": target}),
        ));
    }
    let option = modes.option.as_deref()?;
    Some((
        "session/set_config_option",
        json!({"sessionId": session, "configId": option, "value": target}),
    ))
}

#[cfg(test)]
mod tests;
