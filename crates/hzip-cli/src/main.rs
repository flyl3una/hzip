//! hzip 命令行入口（02 §10.1 分发总流程 A~K 节点）。
//!
//! # 流程
//!
//! A 解析 argv（clap；错误 → 帮助文本 + 退出 2，02 §10.1 B 节点）
//! → C 全局初始化（tracing；信号桥接归 Engine/cfg——platform signal 占位期
//!   跳过，调度跟踪 §四记录）→ D~K 分发（`ops::dispatch`）。
//!
//! # 信号桥接说明（02 §5.3.9）
//!
//! Ctrl-C/SIGINT/SIGTERM → Engine 根取消 → 任务回滚 → 退出 130；第二次信号
//! 直通默认终止。`platform::signal::install_cancel_handler` 当前为占位
//! `todo!()`（T10 后仍占位，调度跟踪 §四；Engine 侧 cfg 门控跳过注册）——
//! 本入口在 **Engine 构建后**消费根取消语义：core 取消经 `Outcome::Cancelled`
//! 收敛退出码 130。占位期记录于 docs/todos/调度跟踪.md（T40 遗留，T11 补全
//! 时激活 platform 侧注册并删除本注释）。
//!
//! # 退出码
//!
//! 0/1/2/130（02 §10.4；冻结，§13.1）。`--json` 时 stdout 单文档结果 +
//! stderr NDJSON 进度（双流分离）。
#![allow(clippy::print_stdout)] // bin 输出例外（02 §1.2 约束对象为库 crate）
#![allow(clippy::print_stderr)] // eprintln 错误/警告通道

use clap::Parser;

use hzip_cli::ops::RunOutcome;

fn main() {
    // A：clap 解析（错误 → clap 自身打印帮助/错误到 stderr 并 exit 2）
    let cli = hzip_cli::Cli::parse();
    // C：日志初始化（hzip-log：文件按天滚动 + stderr 镜像；级别链见下）
    // guard 须存活至进程退出（drop/flush 冲刷并停 writer）
    let mut log_guard = init_tracing(cli.verbose, cli.json);
    // —— 临时缓冲自愈（启动一次；见 core::services::space 模块文档）——
    //
    // 组合档（`.cpio.gz` 等）打开/解压要把解压后的内层容器落临时文件（cpio/tar
    // 要 seek）。`NamedTempFile` 只在 drop 时删：进程被强杀/崩溃会留下**孤儿**，
    // 实测一次中断累积 29.6 GiB、把系统盘挤到 3 GiB 可用，直接导致用户打开
    // 940 MB `.cpio.gz` 时 ENOSPC 失败。启动扫一遍（毫秒级 `read_dir`）是唯一的
    // 自愈时机；阈值 24 h，不会误删在用的缓冲（见 `services::space`）。
    hzip_core::services::space::sweep_stale_spools(hzip_core::services::space::SPOOL_STALE_AFTER);
    // D~K
    let code = match hzip_cli::dispatch(&cli) {
        Ok(RunOutcome {
            exit_code,
            json_doc,
        }) => {
            // J：--json 结果 stdout（单文档；人读报告已在 ops 内输出）
            if let Some(doc) = json_doc {
                println!("{doc}");
            }
            exit_code
        }
        Err(e) => {
            // 提交前错误（5003 等即时退出）：文案 + 退出 2
            eprintln!("错误 HZ-{:04}: {e}", e.code());
            hzip_cli::exit_code_of_error(&e)
        }
    };
    cleanup_stdin_spools();
    // exit 不跑 drop——显式冲刷日志缓冲（最后落盘）
    if let Some(g) = log_guard.take() {
        g.flush();
    }
    std::process::exit(code);
}

/// 日志初始化（02 §12.3；hzip-log 文件落盘 + stderr 镜像双目标）。
///
/// 级别链：`RUST_LOG`/`HZIP_LOG` 环境变量 > 命令行 `-v`（≥1 显式指定）>
/// 配置文件 `logging.level` > 默认 warn。
/// 首次运行把 etc/ 默认配置模板落盘 config_dir（hzip-config 管理）。
fn init_tracing(verbose: u8, json: bool) -> Option<hzip_log::LogGuard> {
    // 首次运行落默认配置模板（已存在不覆盖）
    let _ = hzip_config::Config::ensure_default_config();
    let cfg = hzip_config::Config::load();
    // 基线：-v 显式（≥1）覆盖配置；-v 0 回落配置 logging.level
    let default_level = if verbose >= 1 {
        match verbose {
            1 => "info",
            _ => "debug",
        }
        .to_string()
    } else {
        match cfg.logging.level.as_str() {
            "debug" | "info" | "warn" | "error" | "trace" => cfg.logging.level.clone(),
            _ => "warn".to_string(),
        }
    };
    // hzip-log 内部链：HZIP_LOG > RUST_LOG > 基线（default_level）
    // [logging].dir 空 = 平台默认（None）；format plain/detailed（非 plain = detailed）
    // stderr 镜像在 --json 模式必须关：NDJSON 进度行是 stderr 的冻结协议
    // （02 §10.4 双流分离；TC-CLI-010 锁定），tracing 日志混入会污染流。
    hzip_log::init(hzip_log::LogConfig {
        dir: {
            let d = cfg.logging.dir.trim();
            if d.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(d))
            }
        },
        default_level,
        retention_days: cfg.logging.retention_days,
        mirror_stderr: !json,
        detailed: cfg.logging.format != "plain",
    })
}

/// 进程退出前清理 stdin 物化 spool（best-effort）。
fn cleanup_stdin_spools() {
    hzip_cli::ops::cleanup_spools();
}
