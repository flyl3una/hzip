//! 请求-应答类型（02 §9.11 冻结签名；T30 交付后进入 §13.1 冻结范围）。
//!
//! worker 在业务执行中需要用户决策（密码/冲突）时经 [`JobRequest`] 上抛，
//! UI/CLI 侧经 [`JobResponse`] 应答。通道容量 1（[`crate::jobs::consts`]）。

use std::path::PathBuf;

use hzip_error::{EntryName, HzipError};
use hzip_format::EntryInfo;
use zeroize::Zeroizing;

use super::consts::REQUEST_TIMEOUT_SECS;

/// 请求 ID（每任务内单调递增；过期应答拒绝）。
pub type RequestId = u64;

/// 密码请求原因（UI 重试计数/文案依据）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordReason {
    /// 初次需要密码（档案级/条目级）
    Initial,
    /// 上次提供的密码错误（重试）
    WrongPassword,
    /// 头加密档案需要密码才能列出条目（HZ-3102）
    HeaderEncrypted,
}

/// worker → UI 的待决请求（02 §9.11 原文，冻结）。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum JobRequest {
    /// 需要密码（`entry: None` = 档案级）
    Password {
        /// 目标条目（None = 整档/头）
        entry: Option<EntryName>,
        /// 请求原因
        reason: PasswordReason,
    },
    /// 冲突决策（目标路径已存在）
    Conflict {
        /// 待写路径
        path: PathBuf,
        /// 现存条目简要信息
        existing: FileBrief,
    },
}

/// 冲突应答决策（02 §9.11 `ConflictDecision`）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictDecision {
    /// 覆盖
    Overwrite,
    /// 跳过
    Skip,
    /// 自动改名
    RenameAuto,
    /// 中止任务
    Abort,
}

/// UI → worker 的应答。
#[non_exhaustive]
#[derive(Debug)]
pub enum JobResponse {
    /// 密码（Zeroizing 持有，02 §1.2）
    Password(Zeroizing<String>),
    /// 冲突决策
    Conflict(ConflictDecision),
}

/// 现存文件简要（冲突请求携带）。数据类（非冻结——本 crate 新类型）。
#[derive(Debug, Clone)]
pub struct FileBrief {
    /// 是否为目录
    pub is_dir: bool,
    /// 大小（目录为 0）
    pub size: u64,
    /// 修改时间（Unix 秒；无则 None）
    pub mtime: Option<i64>,
}

impl FileBrief {
    /// 从 `EntryInfo` 构造简要（用于「档案内已有同名条目」对照）。
    pub fn from_entry(e: &EntryInfo) -> Self {
        Self {
            is_dir: e.is_dir,
            size: e.size_raw,
            mtime: e.mtime.map(|t| t.unix_timestamp()),
        }
    }
}

/// 决策超时错误（5002 语义；测试可用缩短超时注入）。
///
/// 以 `HzipError::Unsupported{what: "hz-5002 decision-timeout"}` 字符串带码
/// 承载（T30 §7.2 裁定：HzipError 无专属变体，T40 前按码出文案）。
pub fn decision_timeout_err() -> HzipError {
    HzipError::Unsupported {
        what: format!("hz-5002 decision-timeout (>{REQUEST_TIMEOUT_SECS}s)").into(),
    }
}
