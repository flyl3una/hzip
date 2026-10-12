//! hzip-error：全 workspace 统一错误模型（02 §3）。
//!
//! 本 crate 是依赖图的叶子节点（红线 4，02 §2.1）：`HzipError` 全部变体、
//! 错误码常量（附录 A）与 i18n key 派生在此唯一定义。交付后公开 API 进入
//! 冻结范围（02 §13.1），新增变体只能经 `#[non_exhaustive]` 追加 + RFC。
//!
//! 模块结构（02 §3.1）：
//!
//! - [`lib`]：`HzipError` 枚举、辅助类型与 `code()/is_retryable()/user_action()` 访问器；
//! - [`code`]：错误码常量表 + 附录 A 登记表 [`code::REGISTRY`]；
//! - [`context`]：[`ErrorContext`] 链式构造辅助（02 §3.1）。
//!
//! # Examples
//!
//! ```
//! use hzip_error::HzipError;
//!
//! // 错误码由变体语义推导（02 §3.3 / 附录 A）
//! let err: HzipError =
//!     std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into();
//! assert_eq!(err.code(), 1003);
//! assert_eq!(err.i18n_key(), "err.hz_1003");
//! assert!(!err.is_retryable());
//! ```

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod code;
pub mod context;

use std::{borrow::Cow, fmt, path::PathBuf};

use smol_str::SmolStr;

/// 格式标识（与 02 §7.2 定义一致；hzip-format 侧同名对齐，见任务卡 §7 接口约定）。
pub type FormatId = SmolStr;

/// 条目名承载类型：内部统一 '/' 分隔、无前导 './'、不允许 '\'（02 §7.2）。
/// 语法校验属 hzip-format 职责，本 crate 仅承载失败上下文。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryName(SmolStr);

impl EntryName {
    /// 以已有 `SmolStr` 构造承载值（不做语法校验，见 02 §7.2）。
    pub fn new(name: impl Into<SmolStr>) -> Self {
        Self(name.into())
    }

    /// 条目名字符串切片。
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<SmolStr> for EntryName {
    fn from(name: SmolStr) -> Self {
        Self(name)
    }
}

impl From<&str> for EntryName {
    fn from(name: &str) -> Self {
        Self(name.into())
    }
}

impl From<String> for EntryName {
    fn from(name: String) -> Self {
        Self(name.into())
    }
}

impl fmt::Display for EntryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

/// 格式错误细类（02 附录 A「变体/字段」列；经 `HzipError::Format{kind}` 报出）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatErrorKind {
    /// 魔数不匹配/无法识别格式（HZ-2001）
    BadSignature,
    /// 档案被截断（HZ-2002）
    Truncated,
    /// 未知压缩方法（HZ-2003；zip 方法 ID 为 u16）
    UnknownMethod(u16),
    /// 档案头损坏（HZ-2004）
    BadHeader,
    /// 分卷缺失（HZ-2005；错误信息含卷名）
    MissingVolume(String),
    /// 未知加密方案（HZ-3103；经 `Format{UnknownCrypto}` 报出）
    UnknownCrypto,
}

impl fmt::Display for FormatErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSignature => f.write_str("bad signature"),
            Self::Truncated => f.write_str("truncated archive"),
            Self::UnknownMethod(id) => write!(f, "unknown method {id}"),
            Self::BadHeader => f.write_str("bad header"),
            Self::MissingVolume(volume) => write!(f, "missing volume {volume}"),
            Self::UnknownCrypto => f.write_str("unknown crypto"),
        }
    }
}

/// 密码作用目标（02 §3.3：Archive / Entry / Header）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordTarget {
    /// 整档密码
    Archive,
    /// 单条目密码
    Entry,
    /// 头部加密，无法列出条目（HZ-3102）
    Header,
}

impl fmt::Display for PasswordTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Archive => f.write_str("Archive"),
            Self::Entry => f.write_str("Entry"),
            Self::Header => f.write_str("Header"),
        }
    }
}

/// 安全违规细类（02 附录 A「变体/字段」列；经 `HzipError::Security{violation}` 报出）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityViolation {
    /// 路径穿越（HZ-4001；携带原始条目名）
    PathTraversal(String),
    /// 符号链接逃逸（HZ-4002）
    SymlinkEscape,
    /// Windows 保留设备名（HZ-4003）
    ReservedDeviceName,
    /// 条目名含 NUL/控制字符（HZ-4004）
    ControlChars,
}

impl fmt::Display for SecurityViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathTraversal(raw) => write!(f, "path traversal: {raw}"),
            Self::SymlinkEscape => f.write_str("symlink escape"),
            Self::ReservedDeviceName => f.write_str("reserved device name"),
            Self::ControlChars => f.write_str("control characters"),
        }
    }
}

/// 配额超限细类（02 附录 A；经 `HzipError::Quota{quota}` 报出，HZ-4501..4504）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaKind {
    /// 单文件解压超限（HZ-4501）
    MaxFileBytes,
    /// 总解压量超限（HZ-4502）
    MaxTotalBytes,
    /// 压缩比率超限，疑似炸弹（HZ-4503）
    MaxRatio,
    /// 条目数/组合层数超限（HZ-4504）
    MaxEntries,
}

impl fmt::Display for QuotaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MaxFileBytes => f.write_str("max file bytes"),
            Self::MaxTotalBytes => f.write_str("max total bytes"),
            Self::MaxRatio => f.write_str("max ratio"),
            Self::MaxEntries => f.write_str("max entries"),
        }
    }
}

/// 用户行动建议（02 §3.3 `user_action()` 返回值；语义方向取自附录 A「用户动作」列）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAction {
    /// 检查目标路径可写（HZ-1001）
    CheckTargetWritable,
    /// 更换目标盘或清理空间（HZ-1002）
    FreeSpaceOrChangeTarget,
    /// 以合适权限运行或换目录（HZ-1003）
    CheckPermissions,
    /// 确认输入路径（HZ-1004 / HZ-5004）
    ConfirmInputPath,
    /// 确认文件是受支持的档案（HZ-2001）
    VerifyArchive,
    /// 重新获取完整档案、检查分卷齐全（HZ-2002）
    RefetchArchive,
    /// 升级 hzip（HZ-2003 / HZ-3103）
    UpgradeHzip,
    /// 尝试修复性读取（HZ-2004）
    TrySalvageRead,
    /// 补齐缺失卷（HZ-2005）
    SupplyMissingVolume,
    /// 可尝试 best-effort 模式（HZ-3001）
    TryBestEffort,
    /// 重新输入密码（HZ-3101 / HZ-3102）
    RetryPassword,
    /// 安全拦截提示（HZ-4001 / HZ-4003 / HZ-4004）
    BlockedNotice,
    /// 调整符号链接策略（HZ-4002）
    AdjustSymlinkPolicy,
    /// 调整限额（HZ-4501..4504）
    AdjustQuotaLimit,
    /// 文案指明格式不支持该操作（HZ-5001）
    ExplainUnsupported,
    /// 响应超时、任务已中止（HZ-5002）
    TimeoutAborted,
    /// 指出具体参数与合法范围（HZ-5003）
    ShowValidRange,
    /// 选择覆盖策略或换目录（HZ-5005）
    ChooseOverwritePolicy,
    /// 展示逐条目失败明细（HZ-6002）
    ShowFailureDetails,
    /// 伴随回滚说明（HZ-6001）
    ShowRollbackReport,
    /// 程序正在退出（HZ-6003）
    ExitingSoon,
    /// 引导附带日志上报 issue（HZ-0000）
    ReportIssue,
}

/// 统一错误枚举（02 §3.3 签名逐字落地；变体与错误码语义进入 §13.1 冻结范围）。
///
/// 边界规则（02 §3.5）：`Partial` 是正常业务结果，永远由 core 聚合产生
/// （格式层只报单条目错误）；`Cancelled` 不携带副作用信息（回滚报告走
/// `ProgressEvent::JobFinished`）；未知变体由 crate 外 CLI/GUI 兜底为
/// HZ-0000 + 请求上报 issue（`#[non_exhaustive]` 使其天然必需）。
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum HzipError {
    /// IO / 磁盘 / 权限错误（HZ-1001..1004，区段 02 §3.4）
    #[error("IO 错误: {source}{}", path.as_ref().map(|p| format!(" ({})", p.display())).unwrap_or_default())]
    Io {
        /// 底层 IO 错误
        #[source]
        source: std::io::Error,
        /// 失败路径上下文（02 §3.2：Io 附路径上下文）
        path: Option<PathBuf>,
    },

    /// 格式解析错误（HZ-2001..2005 及 HZ-3103）
    #[error("格式错误 ({format}): {kind}")]
    Format {
        /// 产生错误的格式标识
        format: FormatId,
        /// 格式错误细类
        kind: FormatErrorKind,
    },

    /// CRC 校验失败（HZ-3001）
    #[error("校验失败: 条目 {entry} 期望 CRC {expected:08x} 实际 {actual:08x}")]
    Checksum {
        /// 校验失败的条目名
        entry: EntryName,
        /// 期望 CRC32
        expected: u32,
        /// 实际 CRC32
        actual: u32,
    },

    /// 密码错误或加密数据损坏（HZ-3101 / HZ-3102）
    #[error("密码错误或加密数据损坏: {target}")]
    Password {
        /// 密码作用目标
        target: PasswordTarget,
    },

    /// 安全策略拦截（HZ-4001..4004）
    #[error("安全策略拦截: {violation}")]
    Security {
        /// 安全违规细类
        violation: SecurityViolation,
    },

    /// 配额超限（HZ-4501..4504）
    #[error("配额超限: {quota}")]
    Quota {
        /// 配额细类
        quota: QuotaKind,
    },

    /// 不支持的操作（HZ-5001）
    #[error("不支持的操作: {what}")]
    Unsupported {
        /// 不支持的操作描述
        what: Cow<'static, str>,
    },

    /// 任务已取消（HZ-6001；CLI 退出码 130）
    #[error("任务已取消")]
    Cancelled,

    /// 部分失败（HZ-6002；CLI 退出码 1，由 core 聚合产生）
    #[error("部分失败: {succeeded} 成功, {} 失败", failed.len())]
    Partial {
        /// 成功条目数
        succeeded: usize,
        /// 逐条目失败明细（entry, 原始错误）
        failed: Vec<(EntryName, Box<HzipError>)>,
    },
}

impl From<std::io::Error> for HzipError {
    /// `Io{ source, path: None }` 便捷构造（02 §3.3 的 `#[from]` 语义；
    /// thiserror 的 `#[from]` 要求变体无其他字段，故 path 以手工 impl 补 None）。
    ///
    /// **例外：io 负载里本就是 `HzipError`** → 原样取回。fmt 的 `read_entry`
    /// 返回 `Box<dyn Read>`，流式解码途中发现的语义错误（zip 直读路径的 CRC 尾验
    /// `Checksum 3001` 是首个实例）只能借 `io::Error` 通道冒出来；不取回就会掉成
    /// `Io`（错误码 3001→1001，且丢掉 `best_effort` 认的 `Checksum` 语义）。
    /// 这是该透传约定的**唯一实现点**（core 的 `unattach_io` 只多做一层路径上下文）。
    fn from(source: std::io::Error) -> Self {
        if let Some(inner) = source.get_ref().and_then(|r| r.downcast_ref::<HzipError>()) {
            return inner.clone();
        }
        Self::Io { source, path: None }
    }
}

impl Clone for HzipError {
    /// `io::Error` 不实现 `Clone`（02 §3.1：ProgressEvent/Outcome 广播需要整体 Clone，
    /// T10 §7-4）。Io 变体按 `raw_os_error`/`kind` 重建，尽力保留语义；
    /// 其余变体字段全部 `Clone`。Display 消息可能因自定义 io payload 丢失而
    /// 略有差异——仅用于广播/重试路径，不承载审计语义。
    fn clone(&self) -> Self {
        match self {
            Self::Io { source, path } => Self::Io {
                source: clone_io_error(source),
                path: path.clone(),
            },
            Self::Format { format, kind } => Self::Format {
                format: format.clone(),
                kind: kind.clone(),
            },
            Self::Checksum {
                entry,
                expected,
                actual,
            } => Self::Checksum {
                entry: entry.clone(),
                expected: *expected,
                actual: *actual,
            },
            Self::Password { target } => Self::Password { target: *target },
            Self::Security { violation } => Self::Security {
                violation: violation.clone(),
            },
            Self::Quota { quota } => Self::Quota { quota: *quota },
            Self::Unsupported { what } => Self::Unsupported { what: what.clone() },
            Self::Cancelled => Self::Cancelled,
            Self::Partial { succeeded, failed } => Self::Partial {
                succeeded: *succeeded,
                failed: failed
                    .iter()
                    .map(|(e, err)| (e.clone(), Box::new((**err).clone())))
                    .collect(),
            },
        }
    }
}

/// 重建 `io::Error`：优先保留原始 os 错误码；否则按 `ErrorKind` 重建。
/// `io::Error` 不 `Clone`，这是广播/重试路径下的语义近似（02 §3.3 的
/// `code()`/`is_retryable()` 依赖的 `raw_os_error`/`kind` 均被保留）。
fn clone_io_error(source: &std::io::Error) -> std::io::Error {
    if let Some(code) = source.raw_os_error() {
        std::io::Error::from_raw_os_error(code)
    } else {
        std::io::Error::new(source.kind(), source.to_string())
    }
}

impl HzipError {
    /// HZ-xxxx 的数字部分（02 §3.3）。实现为全变体显式 match（本 crate 内可穷举）。
    pub fn code(&self) -> u32 {
        match self {
            Self::Io { source, .. } => code::io_code(source),
            Self::Format { kind, .. } => code::format_kind_code(kind),
            Self::Checksum { .. } => code::checksum::CRC_MISMATCH,
            Self::Password { target } => match target {
                PasswordTarget::Header => code::password::HEADER_ENCRYPTED,
                PasswordTarget::Archive | PasswordTarget::Entry => code::password::WRONG_PASSWORD,
            },
            Self::Security { violation } => code::security_violation_code(violation),
            Self::Quota { quota } => code::quota_code(*quota),
            Self::Unsupported { .. } => code::business::UNSUPPORTED,
            Self::Cancelled => code::job::CANCELLED,
            Self::Partial { .. } => code::job::PARTIAL_FAILURE,
        }
    }

    /// Io 且 os 错误属网络/管道瞬断集（EPIPE/ECONNRESET/ETIMEDOUT、
    /// win ERROR_NETNAME_DELETED/SEM_TIMEOUT/BAD_NETPATH）→ true，其余 false（02 §3.3）。
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Io { source, .. } => {
                code::RETRYABLE_OS.contains(&source.raw_os_error().unwrap_or(0))
            }
            _ => false,
        }
    }

    /// 文案选择（重输密码/换目标盘/忽略等，02 §3.3；映射见任务卡 §3.3 逻辑表）。
    pub fn user_action(&self) -> UserAction {
        match self {
            Self::Io { source, .. } => match code::io_code(source) {
                code::io::DISK_FULL => UserAction::FreeSpaceOrChangeTarget,
                code::io::SOURCE_NOT_FOUND_IO => UserAction::ConfirmInputPath,
                code::io::PERMISSION_DENIED => UserAction::CheckPermissions,
                _ => UserAction::CheckTargetWritable,
            },
            Self::Format { kind, .. } => match kind {
                FormatErrorKind::BadSignature => UserAction::VerifyArchive,
                FormatErrorKind::Truncated => UserAction::RefetchArchive,
                FormatErrorKind::UnknownMethod(_) | FormatErrorKind::UnknownCrypto => {
                    UserAction::UpgradeHzip
                }
                FormatErrorKind::BadHeader => UserAction::TrySalvageRead,
                FormatErrorKind::MissingVolume(_) => UserAction::SupplyMissingVolume,
            },
            Self::Checksum { .. } => UserAction::TryBestEffort,
            Self::Password { .. } => UserAction::RetryPassword,
            Self::Security { violation } => match violation {
                SecurityViolation::SymlinkEscape => UserAction::AdjustSymlinkPolicy,
                SecurityViolation::PathTraversal(_)
                | SecurityViolation::ReservedDeviceName
                | SecurityViolation::ControlChars => UserAction::BlockedNotice,
            },
            Self::Quota { .. } => UserAction::AdjustQuotaLimit,
            Self::Unsupported { .. } => UserAction::ExplainUnsupported,
            Self::Cancelled => UserAction::ShowRollbackReport,
            Self::Partial { .. } => UserAction::ShowFailureDetails,
        }
    }

    /// i18n key（02 §12.2：错误文案 key 即 `err.hz_{code}`；清单见 `i18n-keys.md`）。
    pub fn i18n_key(&self) -> String {
        format!("err.hz_{:04}", self.code())
    }
}
