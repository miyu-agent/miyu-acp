//! 集成测试：起 `miyu-acp serve`，测试扮核心，假 agent 扮 ACP agent（`support`、`tests/fake_acp.py`）。
//!
//! - `calls`：一问一答（正文、思考、用量带 `context`）、系统提示词照能力交、`driver.models`、选模型、带图。
//! - `sessions`：追加一条的接着用同一个会话，改了历史的新开并转录。
//! - `tools`：Miyu 的工具交回核心再接着；对不上的重放；Gemini 系的参数格式整形。
//! - `own_tools`：对方自带的工具两档、权限请求、问主人。
//! - `failures`：`stopReason` 各种、要登录、密钥登录、出错归类、agent 退了、取消。
//! - `install`：照登记表下载、核对、起；核不上的拒装。

#[cfg(test)]
mod calls;
#[cfg(test)]
mod failures;
#[cfg(test)]
mod install;
#[cfg(test)]
mod own_tools;
#[cfg(test)]
mod sessions;
#[cfg(test)]
mod support;
#[cfg(test)]
mod tools;
