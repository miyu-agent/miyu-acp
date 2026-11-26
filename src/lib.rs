//! miyu-acp：Miyu 的扩展包 `acp`，把 ACP（Agent Client Protocol）官方登记表里的 agent 程序当模型供应商（核心设计 73 第四节）。
//!
//! 这一步是骨架：登记表的取和缓存（[`catalog`]、[`fetch`]），照系统和架构装（[`install`]、[`archive`]），用哪个 agent（[`agents`]，
//! 设置里手写的见 [`config`]），`driver.models` 的回答（[`models`]）。
//!
//! 做成库加一个薄的 `main`：集成测试要直接用里面的东西。进程组、信号、本机套接字照 Unix 写，只支持 Linux、macOS。

pub mod agents;
pub mod archive;
pub mod catalog;
pub mod config;
pub mod fetch;
pub mod install;
pub mod models;
pub mod workspace;

#[cfg(test)]
mod test_support;
