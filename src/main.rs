//! miyu-acp 的命令行入口：分派到库里的 `serve`、`mcp`（说明见库的头注释）。

use std::process::ExitCode;

use miyu_acp::mcp::{self, Bridge};
use miyu_acp::rpc;

const USAGE: &str = "usage: miyu-acp serve   (started by the Miyu core; provides the model driver acp/acp)\n       miyu-acp mcp     (started by an ACP agent; the MCP server that hands tool calls to Miyu)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => rpc::serve(std::io::stdin().lock(), &rpc::Out::stdout()),
        Some("mcp") => mcp::run(
            std::io::stdin().lock(),
            &rpc::Out::stdout(),
            Bridge::from_env(),
        ),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
