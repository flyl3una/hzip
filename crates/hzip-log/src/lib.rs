//! hzip 日志收口 crate（02 §12.3；主流程初始化）。
//!
//! # 职责
//!
//! 统一日志初始化：文件落盘（按天滚动 + 保留 N 天）+ 可选 stderr 镜像 +
//! 级别过滤 + [`redact!`] 脱敏宏。**初始化由主流程（hzip-cli / hzip-gui）
//! 调用控制**——库 crate 只经 `tracing` 宏打日志、不自行初始化。
//!
//! # 文件布局
//!
//! `{log_dir}/hzip.YYYY-MM-DD.log`（经 `hzip_platform::dirs::log_dir()`，
//! 三平台正确路径；02 §12.3 字面 data_dir 拼法在 mac/Linux 前缀错误，勿用）。
//! 滚动（daily）与保留（`max_log_files`）由 `tracing-appender` 承担。
//!
//! # 级别链（高→低）
//!
//! `RUST_LOG`（保留 CLI 既有语义）> `HZIP_LOG`（hzip 专有覆盖，排障用）>
//! 配置 `logging.level`（经 hzip-config 读，调用方传入）> 调用方基线
//! （CLI warn / GUI debug——02 §12.3）。
//!
//! # 脱敏
//!
//! [`redact!`] 把含敏感值（密码/家目录路径）的日志文本替换占位。正常
//! 密码路径经 `Zeroizing` 持有、本就不落日志（02 §1.2/T41 红线）。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// 默认日志保留天数（02 附录 B.2 `LOG_RETENTION_DAYS=7`）。
pub const LOG_RETENTION_DAYS: u8 = 7;

/// 日志初始化参数。
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// 日志目录（None → `hzip_platform::dirs::log_dir()`）。
    pub dir: Option<PathBuf>,
    /// 级别基线（调用方默认：CLI warn / GUI debug——02 §12.3）。
    pub default_level: String,
    /// 保留天数（默认 [`LOG_RETENTION_DAYS`]）。
    pub retention_days: u8,
    /// 同时镜像 stderr（CLI 需要；GUI 默认关——避免控制台噪音）。
    pub mirror_stderr: bool,
    /// detailed 格式（true = 带源文件名 + 代码行 + target；R-GUI2 需求 #1，
    /// 用户要求日志带代码行；config `[logging].format` 非 "plain" 即 true）。
    pub detailed: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            dir: None,
            default_level: "warn".into(),
            retention_days: LOG_RETENTION_DAYS,
            mirror_stderr: true,
            detailed: true,
        }
    }
}

/// 日志守护（持有保非阻塞 writer 线程存活；drop 冲刷）。
pub struct LogGuard {
    guard: Option<WorkerGuard>,
}

impl LogGuard {
    /// 显式冲刷并停 writer（供 `process::exit` 前的最后落盘——exit 不跑
    /// drop，缓冲可能丢）。消费 self。
    pub fn flush(mut self) {
        // take + drop 触发 WorkerGuard 的 shutdown 冲刷（≤1s）
        if let Some(g) = self.guard.take() {
            drop(g);
        }
    }
}

/// 初始化日志。重复调用仅首次生效（tracing global subscriber 单例）。
/// 失败降级：目录不可建等场景返回 None（不阻塞主流程，stderr 兜底由调用方）。
pub fn init(cfg: LogConfig) -> Option<LogGuard> {
    let (dir, fell_back) = resolve_dir(cfg.dir.as_deref());

    let filter = level_filter(&cfg.default_level);
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("hzip")
        .filename_suffix("log")
        .max_log_files((cfg.retention_days.max(1)) as usize)
        .build(&dir)
        .ok()?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    // detailed = 文件名+代码行+target；三参布尔式保证两个 layer 类型一致
    // （builder 链式条件分支返回类型不同，勿用 if/else 各自链完）。
    // 文件与 stderr 镜像层同格式（终端看到的即落盘内容）。
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_file(cfg.detailed)
        .with_line_number(cfg.detailed)
        .with_target(cfg.detailed);
    let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);
    if cfg.mirror_stderr {
        // stderr 镜像（保留 ANSI 供终端高亮；stdout 只出数据不污染——CLI 双流）
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_file(cfg.detailed)
            .with_line_number(cfg.detailed)
            .with_target(cfg.detailed);
        let _ = registry.with(stderr_layer).try_init();
    } else {
        let _ = registry.try_init();
    }
    // 自定位路标（R-GUI4 #4：用户找不到日志文件——首条 info 落盘**绝对**目录；
    // 相对 dir 按 CWD 解析打印，level < info 时静默跳过，不另造通道）
    let display_dir = if dir.is_absolute() {
        dir.clone()
    } else {
        std::env::current_dir().unwrap_or_default().join(&dir)
    };
    let _ = EFFECTIVE_DIR.set(display_dir.clone());
    if fell_back {
        // 配置目录不可用 → 已回退平台默认。必须显式告警：否则用户只看到
        // 「配置了目录却没有日志」，无从判断是配置没生效还是写不进去。
        tracing::warn!(
            "配置的日志目录不可用（建不出或写不进），已回退系统默认目录：{}",
            display_dir.display()
        );
    }
    tracing::info!("hzip 日志已启用 → {}", display_dir.display());
    Some(LogGuard { guard: Some(guard) })
}

/// 级别链：HZIP_LOG > RUST_LOG > 基线。
/// 进程**实际**落盘的日志目录（`init` 落定后填入；绝对路径）。
///
/// 供设置页「打开日志目录」使用：启动后再改配置不会换目录（需重启），
/// 这里报的是**当前进程真在写的那一个**，不是重新推导出来的。
static EFFECTIVE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// 生效日志目录（未初始化 → 平台默认）。
pub fn effective_log_dir() -> PathBuf {
    EFFECTIVE_DIR
        .get()
        .cloned()
        .unwrap_or_else(hzip_platform::dirs::log_dir)
}

/// 落定实际使用的日志目录：优先配置值，不可用则回退平台默认（返回是否回退）。
///
/// 消除「配置了目录却没有日志」的静默失败：配置值可能是**相对路径**，按进程
/// 工作目录解析，而经右键菜单/双击拉起时工作目录不可控（可能落在只读位置）
/// ——旧实现建目录失败直接 `?` 返回 `None`，日志**一条都不落**且无任何提示。
fn resolve_dir(configured: Option<&std::path::Path>) -> (PathBuf, bool) {
    let fallback = hzip_platform::dirs::log_dir();
    let Some(dir) = configured else {
        let _ = std::fs::create_dir_all(&fallback);
        return (fallback, false);
    };
    if usable_dir(dir) {
        return (dir.to_path_buf(), false);
    }
    let _ = std::fs::create_dir_all(&fallback);
    (fallback, true)
}

/// 目录可用 = 建得出 **且** 写得进（用一次真实写入探测，不靠权限位猜）。
fn usable_dir(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".hzip-log-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn level_filter(default_level: &str) -> EnvFilter {
    std::env::var("HZIP_LOG")
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|s| EnvFilter::try_new(s).ok())
        .or_else(|| EnvFilter::try_from_default_env().ok())
        .unwrap_or_else(|| EnvFilter::new(default_level))
}

/// 日志目录（与 init 的 None 默认一致）。
pub fn log_dir() -> PathBuf {
    hzip_platform::dirs::log_dir()
}

/// GUI 基线：debug 落文件、不镜像 stderr（02 §12.3「GUI 默认 warn」由配置
/// `logging.level` 覆盖；debug 是未配置时文件基线——文档见调用方）。
pub fn gui_default() -> LogConfig {
    LogConfig {
        default_level: "debug".into(),
        mirror_stderr: false,
        ..Default::default()
    }
}

/// CLI 基线：warn + stderr 镜像（保留 CLI 既有 -v 语义由调用方覆盖 default_level）。
pub fn cli_default() -> LogConfig {
    LogConfig::default()
}

/// 敏感值脱敏（密码/家目录路径等落日志前的值级替换）。
///
/// 用法：
/// ```
/// let safe = hzip_log::redact("用户口令");       // "用户[redacted]"
/// tracing::info!("尝试 {}", safe);
/// ```
/// 正常密码路径经 `Zeroizing` 持有、本就不落日志（02 §1.2/T41 红线）；
/// 本函数供确实需要把敏感值拼进日志的边界点显式脱敏。
pub fn redact(value: impl AsRef<str>) -> String {
    let v = value.as_ref();
    if v.is_empty() {
        String::new()
    } else {
        "[redacted]".into()
    }
}

/// [`redact`] 的宏形态（允许不经 use 直呼）。
#[macro_export]
macro_rules! redact {
    ($value:expr) => {
        $crate::redact($value)
    };
    ($($arg:tt)*) => {
        // 复合参数兜底：整体视作敏感 → 脱敏占位（勿把非敏感与敏感混拼后传入）
        $crate::redact(format!($($arg)*))
    };
}

#[cfg(test)]
#[path = "tests/lib_rs.rs"]
mod tests;
