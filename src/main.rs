//! miyu-acp 的命令行入口。骨架这一步还没有 `serve`：只说用法。

use std::process::ExitCode;

const USAGE: &str = "usage: miyu-acp serve   (started by the Miyu core; provides the model driver acp/acp)";

fn main() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
