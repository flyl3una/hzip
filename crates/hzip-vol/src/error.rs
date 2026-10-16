//! 卷层错误模型：**拒绝理由即文案键**（UI 与 CLI 单源）。
//!
//! # 为什么理由要枚举而不是字符串
//!
//! 「为什么不能改」是这套功能里用户最常看到的一句话（只读镜像、脏卷、加密、布局
//! 不支持…）。散落的字符串会在三处漂移（fmt 侧、GUI locale、CLI 输出），故这里把
//! 理由做成枚举，每个变体给出**稳定键**（`vol.readonly.dirty` 之类），locale 表按
//! 键取值；`tests/i18n_keys.rs` 保证每种语言的键都在。

use hzip_error::{FormatErrorKind, HzipError, QuotaKind};

/// 卷层结果。
pub type VolResult<T> = Result<T, VolError>;

/// 卷层错误（`hzip-vol` 内部；出边界时经 [`From`] 折进 [`HzipError`]）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VolError {
    /// 块设备 / 文件 IO 失败。
    #[error("块设备 IO 失败: {0}")]
    Io(#[from] std::io::Error),
    /// 镜像或文件系统的结构不合法（损坏、越界、字段自相矛盾）。
    #[error("镜像结构不合法: {0}")]
    Image(String),
    /// 不能写，且**理由明确**（枚举 ⇒ 文案单源）。
    #[error("只读: {0}")]
    ReadOnly(ReadOnlyReason),
    /// 本期不支持的操作（稳定键，例：`vol.unsupported.layout-flat`）。
    #[error("不支持: {0}")]
    Unsupported(&'static str),
    /// 配额超限（稳定键，例：`vol.err.no-space`）。
    #[error("配额超限: {0}")]
    Quota(&'static str),
    /// 用户取消。
    #[error("已取消")]
    Cancelled,
}

/// 只读/拒写的**理由键**（每个变体一个 locale 键；`tests/i18n_keys.rs` 校验）：
///
/// | 变体 | 键 | 含义 |
/// |---|---|---|
/// | [`Self::Dirty`] | `vol.readonly.dirty` | 镜像/卷带脏标记（未干净卸载） |
/// | [`Self::JournalPending`] | `vol.readonly.journal` | 日志含事务，默认拒写（可显式重放） |
/// | [`Self::Hibernated`] | `vol.readonly.hibernated` | 休眠卷（Windows 快速启动残留） |
/// | [`Self::Encrypted`] | `vol.readonly.encrypted` | 加密镜像/卷 |
/// | [`Self::Layout`] | `vol.readonly.layout` | 该布局只读（如 streamOptimized） |
/// | [`Self::FsUnknown`] | `vol.readonly.fs-unknown` | 文件系统不认识 |
/// | [`Self::DeviceReadOnly`] | `vol.readonly.device` | 目标不可写（介质/权限） |
/// | [`Self::BySetting`] | `vol.readonly.by-setting` | 按设置以只读打开 |
/// | [`Self::OuterNotEditable`] | `vol.readonly.outer-not-editable` | 外层不可重建（tar/cpio/rar 链） |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadOnlyReason {
    /// 脏标记：未干净卸载（ext4 `s_state` 清 VALID_FS / qcow2 dirty / VMDK uncleanShutdown）。
    Dirty,
    /// 日志含事务（NTFS `$LogFile` / ext3/4 JBD2 待重放）。
    JournalPending,
    /// 休眠卷。
    Hibernated,
    /// 加密。
    Encrypted,
    /// 布局只读。
    Layout,
    /// 文件系统不认识。
    FsUnknown,
    /// 目标设备不可写。
    DeviceReadOnly,
    /// 按设置只读。
    BySetting,
    /// 外层不可重建。
    OuterNotEditable,
}

impl ReadOnlyReason {
    /// 全部变体（i18n 与能力矩阵的遍历源；加变体必须同步这里 ⇒ 编译期提醒）。
    pub const ALL: &'static [Self] = &[
        Self::Dirty,
        Self::JournalPending,
        Self::Hibernated,
        Self::Encrypted,
        Self::Layout,
        Self::FsUnknown,
        Self::DeviceReadOnly,
        Self::BySetting,
        Self::OuterNotEditable,
    ];

    /// locale 键（`hzip-gui/locales/*.toml` 与 CLI i18n 表按它取值）。
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Dirty => "vol.readonly.dirty",
            Self::JournalPending => "vol.readonly.journal",
            Self::Hibernated => "vol.readonly.hibernated",
            Self::Encrypted => "vol.readonly.encrypted",
            Self::Layout => "vol.readonly.layout",
            Self::FsUnknown => "vol.readonly.fs-unknown",
            Self::DeviceReadOnly => "vol.readonly.device",
            Self::BySetting => "vol.readonly.by-setting",
            Self::OuterNotEditable => "vol.readonly.outer-not-editable",
        }
    }
}

impl std::fmt::Display for ReadOnlyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.key())
    }
}

impl From<VolError> for HzipError {
    fn from(e: VolError) -> Self {
        match e {
            VolError::Io(source) => Self::Io { source, path: None },
            VolError::Cancelled => Self::Cancelled,
            VolError::Quota(key) => {
                tracing::debug!(key, "卷层配额拒绝");
                Self::Quota {
                    quota: QuotaKind::MaxTotalBytes,
                }
            }
            // 理由键与「不支持键」都走 Unsupported：UI 按 `what` 查 locale，
            // 查不到就原样显示（不会静默成空字符串——那才是真的「说不出理由」）。
            VolError::ReadOnly(reason) => Self::Unsupported {
                what: reason.key().into(),
            },
            VolError::Unsupported(key) => Self::Unsupported { what: key.into() },
            VolError::Image(detail) => {
                // HzipError::Format 不带自由文本 ⇒ 细节落日志，用户侧给稳定格式 id。
                tracing::debug!(detail, "卷层结构错误");
                Self::Format {
                    format: hzip_error::FormatId::from("vol"),
                    kind: FormatErrorKind::BadHeader,
                }
            }
        }
    }
}

/// `fs_core::Error` → [`VolError`]（设备层的错误折进卷层；语义类别保留）。
impl From<fs_core::Error> for VolError {
    fn from(e: fs_core::Error) -> Self {
        match e {
            fs_core::Error::Io(io) => Self::Io(io),
            fs_core::Error::ReadOnly => Self::ReadOnly(ReadOnlyReason::DeviceReadOnly),
            fs_core::Error::OutOfBounds { .. } | fs_core::Error::ShortRead { .. } => {
                Self::Image(format!("{e}"))
            }
            fs_core::Error::Custom(msg) => Self::Image(msg),
        }
    }
}
