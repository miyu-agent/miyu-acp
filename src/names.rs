//! 认 agent 报的工具调用里哪些是 Miyu 的工具（经本包的 MCP 服务 `miyu` 交给 agent 的那些）。agent 报 `tool_call` 时用它自己的
//! 叫法：Claude 适配器写 `mcp__miyu__<工具名>`（0.112.0 源码的 `mcp__<server>__<tool>`），agy 系写 `mcp_miyu_<工具名>`
//! （`miyu-antigravity` 实测），别家可能写 `miyu__<名>`、`miyu/<名>`、`<名> (miyu MCP Server)`。认不出来的照对方自带的工具
//! 画（供应商侧工具块），交回核心那一边以 MCP 服务收到的调用为准，不靠这里。

use std::collections::BTreeSet;

use serde_json::Value;

/// 名字前面的几种写法。
const PREFIXES: &[&str] = &[
    "mcp__miyu__",
    "mcp_miyu_",
    "miyu__",
    "miyu_",
    "miyu/",
    "miyu.",
    "miyu:",
];

/// 名字后面的写法（Gemini CLI 的显示名）。
const SUFFIX: &str = " (miyu MCP Server)";

/// 这一次请求的 Miyu 工具名。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names {
    tools: BTreeSet<String>,
}

impl Names {
    /// 照工具名建。
    #[must_use]
    pub fn new(tools: impl IntoIterator<Item = String>) -> Names {
        Names {
            tools: tools.into_iter().collect(),
        }
    }

    /// 一个写法 `text` 是不是 Miyu 的哪件工具：是的交工具名。
    #[must_use]
    pub fn match_text(&self, text: &str) -> Option<String> {
        let text = text.trim();
        PREFIXES
            .iter()
            .find_map(|prefix| text.strip_prefix(prefix))
            .or_else(|| text.strip_suffix(SUFFIX))
            .filter(|name| self.tools.contains(*name))
            .map(str::to_string)
    }

    /// 一条 `tool_call`（或 `tool_call_update`、要权限时的 `toolCall`）是不是 Miyu 的工具：先看 `name`，再看 `title`。
    #[must_use]
    pub fn of_call(&self, call: &Value) -> Option<String> {
        ["name", "title"]
            .iter()
            .filter_map(|key| call.get(*key).and_then(Value::as_str))
            .find_map(|text| self.match_text(text))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn known_spellings_are_matched_only_for_declared_tools() {
        let names = Names::new(["read".to_string(), "shell".to_string()]);
        for text in [
            "mcp__miyu__read",
            "mcp_miyu_read",
            "miyu__read",
            "miyu/read",
            "read (miyu MCP Server)",
        ] {
            assert_eq!(names.match_text(text).as_deref(), Some("read"), "{text}");
        }
        for text in ["read", "mcp__other__read", "mcp__miyu__write", "Read file"] {
            assert_eq!(names.match_text(text), None, "{text}");
        }
        assert_eq!(
            names
                .of_call(&json!({"title": "mcp__miyu__shell", "kind": "other"}))
                .as_deref(),
            Some("shell")
        );
        assert_eq!(
            names.of_call(&json!({"name": "WebSearch", "title": "Search"})),
            None
        );
    }
}
