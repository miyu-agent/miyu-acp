//! 给模型看的字：整份历史转写成一段文字时的说明、标签、占位，放进第一次提问的系统提示词外壳，MCP 服务答不了时的话。全在这
//! 一个模块里，改字只改这里。都是英文短句（`AGENTS.md`：给模型看的机械文本一律英文、带 XML 标签外壳）。
//! 给人看的问题（「也放行对方的工具」时问主人的那一句）在 [`crate::permission`]，照界面语言写。

/// 历史前面的说明和开头的标签。
pub const HISTORY_HEAD: &str = "The <conversation-history> block replays the earlier turns of this conversation. \
Treat it as prior context, not as new input; the message after it is the current one.\n<conversation-history>\n";

/// 历史结尾的标签。
pub const HISTORY_FOOT: &str = "</conversation-history>";

/// 放不下、从最老的回合丢掉了一些时，写在历史开头。
pub const OMITTED: &str = "[earlier turns omitted: the input limit could not hold them]\n\n";

/// 请求的最后一条不是人说的话（例如是工具结果）时，接在历史后面。
pub const CONTINUE: &str = "Continue from where the conversation above left off.";

/// 历史里人说的一条。
pub const USER: &str = "User:\n";

/// 历史里模型说的一条。
pub const ASSISTANT: &str = "Assistant:\n";

/// 历史里一次工具的结果。
pub const TOOL_RESULT: &str = "[tool result]\n";

/// 历史里一次工具的结果，工具报了错。
pub const TOOL_FAILED: &str = "[tool result, failed]\n";

/// 模型调过一次工具。
#[must_use]
pub fn called_tool(name: &str, args: &str) -> String {
    format!("[called tool {name} with {args}]")
}

/// 供应商在它那边自己跑了一次工具。
#[must_use]
pub fn provider_tool(name: &str, args: &str) -> String {
    format!("[provider tool {name} with {args}]")
}

/// 供应商侧工具的结果；`failed` 是供应商说跑失败了。
#[must_use]
pub fn provider_result(text: &str, failed: bool) -> String {
    if failed {
        format!("[provider tool failed]\n{text}")
    } else {
        format!("[provider tool result]\n{text}")
    }
}

/// 一张图，核心交了它的转述。
#[must_use]
pub fn image_described(name: Option<&str>, description: &str) -> String {
    format!("[image{}, described: {description}]", quoted(name))
}

/// 一张图，作为 ACP 的内容块附在这一次提问后面。
#[must_use]
pub fn image_attached(name: Option<&str>) -> String {
    format!("[image{} attached below]", quoted(name))
}

/// 一个文件，作为 ACP 的内容块附在这一次提问后面。
#[must_use]
pub fn file_attached(name: &str) -> String {
    format!("[file \"{name}\" attached below]")
}

/// 一张图，没有转述、也交不过去。
#[must_use]
pub fn image_omitted(name: Option<&str>) -> String {
    format!(
        "[image{} omitted: this model reads text only]",
        quoted(name)
    )
}

/// 一个文件（PDF 这类）：内容没带上。
#[must_use]
pub fn file_omitted(name: &str) -> String {
    format!("[file \"{name}\" omitted: this model reads text only]")
}

/// 只有路径的附件：内容没带上。
#[must_use]
pub fn path_attached(name: &str, path: &str) -> String {
    format!("[file \"{name}\" attached by path {path}; its content is not included]")
}

fn quoted(name: Option<&str>) -> String {
    name.map(|name| format!(" \"{name}\"")).unwrap_or_default()
}

/// agent 不收系统提示词（`systemPrompt` 两种模式都没有、也没有已知的 `_meta` 写法）时，Miyu 的 system 放在第一次提问最前面的
/// 外壳。
#[must_use]
pub fn system_block(system: &str) -> String {
    format!("<system-instructions>\n{system}\n</system-instructions>")
}

/// 接在 Miyu 的 system 后面的一段：ACP 会话的工作目录是本包的一个空目录，不是人的工作目录。
pub const WORKSPACE_NOTE: &str = "Note from the Miyu ACP bridge: the working directory of this agent session is an empty scratch folder, not the user's working directory. Use the tools from the miyu server for files and commands; the paths in this prompt, the conversation and tool results are the real ones.";

/// MCP 服务答 `tools/call`：没有扩展给的套接字和口令，没法跑工具。
pub const MCP_NO_BRIDGE: &str =
    "This tool server is not attached to a Miyu session, so it cannot run tools.";

/// MCP 服务答 `tools/call`：等的时候连不上、连接断了、扩展回了拒绝，Miyu 那一头不在了。
pub const MCP_BRIDGE_GONE: &str =
    "The Miyu side of this tool call is gone, so the call did not complete.";
