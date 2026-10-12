//! xtask：仓库内开发辅助 crate（04 §10），不进入发布产物。
#![allow(clippy::print_stdout)] // 02 §1.2 约束对象为库 crate；开发工具终端输出例外

mod env_check;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("env-check") => env_check::run(),
        _ => {
            eprintln!("usage: cargo run -p xtask -- env-check");
            ExitCode::from(2)
        }
    }
}
