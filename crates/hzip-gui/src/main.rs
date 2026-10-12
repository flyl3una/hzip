//! hzip GUI 入口（02 §11.2；T42）。
//!
//! 启动序列（02 §11.2 图）：main → 日志/配置初始化 → 命令行解析（`LaunchArgs::parse`）→
//! [`hzip_gui::ui::shell::run_ui`]（单实例锁 → Engine → 窗口 → 事件循环 →
//! 退出钩子）。
//!
//! 命令行协议（02 §11.5；T43 shell 扩展消费端）：
//! `hzip-gui [--open <path>] [--extract-here <path>] [--compress <paths...>]`
// 无条件（debug 也生效）：本次要消灭的是「从 explorer.exe 启动时弹出的命令行
// 窗口」，而部署用的正是 debug 构建——只在 release 关掉等于没修；两种 profile
// 的 PE 子系统也不该有差异（否则本地验证与线上行为对不上）。
// 输出不受影响：--help/--version/启动错误经 hzip_platform::console::attach_parent_console
// 附着到调用者终端（见下）。
#![windows_subsystem = "windows"]

use hzip_gui::ui::shell::{ArgvParse, LaunchArgs, run_ui};

fn main() {
    // 日志初始化（hzip-log 文件按天滚动；级别链 HZIP_LOG/RUST_LOG > 配置
    // logging.level > GUI 基线 debug）。guard 须存活至进程退出。
    let _log_guard = init_logging();
    // argv 解析（协议解析器在 lib 内：转发队列还原 argv 走的是**同一个**
    // 实现，动词解释不会两套——02 §11.5 协议冻结）。
    let raw_argv: Vec<String> = std::env::args().skip(1).collect();
    // 内部提权入口（**不进公开 argv 协议** 02 §11.5，故在解析之前截获）：
    // `apply_association` 直删 `UserChoice` 被 DACL 拒时，以 `runas` 重新拉起
    // 本程序并带上这个开关——只清键、随即退出，不建窗口、不参与单实例。
    #[cfg(windows)]
    if let Some(list) = raw_argv
        .iter()
        .find_map(|a| a.strip_prefix(hzip_platform::assoc::ELEVATE_FLAG))
    {
        let exts: Vec<String> = list
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let code = match hzip_platform::assoc::clear_user_choice_batch(&exts) {
            Ok(()) => {
                tracing::info!(?exts, "提权清理文件关联 UserChoice 完成");
                0
            }
            Err(e) => {
                tracing::error!("提权清理文件关联失败: {e}");
                1
            }
        };
        if let Some(g) = _log_guard {
            g.flush();
        }
        std::process::exit(code);
    }
    let args = match LaunchArgs::parse(&raw_argv) {
        ArgvParse::Run(args) => *args,
        ArgvParse::Help => {
            if let Some(c) = hzip_platform::console::attach_parent_console() {
                c.write_line(&hzip_gui::tr("cli.help.banner"));
                c.write_line(&hzip_gui::tr("cli.help.usage"));
            }
            return;
        }
        ArgvParse::Version => {
            if let Some(c) = hzip_platform::console::attach_parent_console() {
                c.write_line(&format!("hzip-gui {}", env!("CARGO_PKG_VERSION")));
            }
            return;
        }
    };
    if let Err(e) = run_ui(args, raw_argv) {
        // 终端调用 → 输出到附着来的父控制台；双击/资源管理器（无父控制台）→
        // 控制台静默，错误进日志文件（不为此弹窗、更不为此分配新控制台）。
        if let Some(c) = hzip_platform::console::attach_parent_console() {
            c.write_line(&format!("hzip-gui: {e}"));
        }
        // run_ui 的错误多经 `?` 直接上抛、自身不落日志，这里补一条：否则无父
        // 控制台时启动失败将完全无声，连日志都查不到。
        tracing::error!("hzip-gui 启动失败: {e}");
        // exit 不跑 drop——显式冲刷日志
        if let Some(g) = _log_guard {
            g.flush();
        }
        std::process::exit(2);
    }
}

/// 日志初始化（02 §12.3）。首次运行落默认配置模板；级别取配置
/// `logging.level`；`[logging].dir` 空 = 平台默认 log_dir（R-GUI2 需求 #1：
/// 日志路径/格式入配置）；`[logging].format` plain/detailed。
fn init_logging() -> Option<hzip_log::LogGuard> {
    let _ = hzip_config::Config::ensure_default_config();
    let cfg = hzip_config::Config::load();
    hzip_log::init(hzip_log::LogConfig {
        // [logging].dir：空 = 平台默认 log_dir；否则相对工作目录/绝对路径
        dir: if cfg.logging.dir.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(&cfg.logging.dir))
        },
        default_level: cfg.logging.level.clone(),
        retention_days: cfg.logging.retention_days,
        mirror_stderr: false, // GUI：文件为主；不镜像控制台
        // [logging].format：plain / detailed（detailed = 文件名+行号+target）
        detailed: cfg.logging.format != "plain",
    })
}
