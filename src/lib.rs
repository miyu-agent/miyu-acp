//! miyu-acp：Miyu 的扩展包 `acp`，把 ACP（Agent Client Protocol）官方登记表里的 agent 程序当模型供应商（核心设计 73 第四节）。
//!
//! - `miyu-acp serve`：核心拉起的扩展，提供模型驱动 `acp/acp`（[`rpc`]）。模型名写 agent 的编号（`claude-acp`、
//!   `claude-acp/<模型>`）；agent 照登记表（[`catalog`]）装（[`install`]），或者用设置里手写的（[`config`]、[`agents`]）。一次
//!   `driver.call`（[`call`]）认得出是哪个活着的 ACP 会话（[`sessions`]、[`pool`]、[`live`]）的下一轮的，只把新的话交过去；认不上的
//!   新开一个会话（[`acp`]），整份历史转写成第一次提问（[`transcript`]、[`prompt`]）。agent 的 `session/update` 照 [`turn`] 交成增量，
//!   要权限照 [`permission`] 答。
//! - `miyu-acp mcp`：agent 照 `mcpServers` 拉起的 MCP 服务（[`mcp`]），经本机套接字（[`bridge`]）把工具表和工具调用转给扩展进程
//!   （总台 [`hub`]）；Gemini 系的参数格式照 [`schema`] 整形。
//!
//! 做成库加一个薄的 `main`：集成测试要直接用里面的东西。进程组、信号、本机套接字照 Unix 写，只支持 Linux、macOS。

pub mod acp;
pub mod agents;
pub mod archive;
pub mod bridge;
pub mod call;
pub mod catalog;
pub mod config;
pub mod failure;
pub mod fetch;
pub mod hub;
pub mod install;
pub mod live;
pub mod mcp;
pub mod models;
pub mod names;
pub mod permission;
pub mod pool;
pub mod process;
pub mod prompt;
pub mod rpc;
pub mod schema;
pub mod sessions;
pub mod signal;
pub mod transcript;
pub mod turn;
pub mod words;
pub mod workspace;

#[cfg(test)]
mod test_support;
