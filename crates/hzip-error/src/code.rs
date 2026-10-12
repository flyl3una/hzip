//! 错误码常量表（02 附录 A 一一对应，以附录 A 为评审基准；只增不改，附录 A.2）。
//!
//! 区段（02 §3.4）：1000–1999 IO、2000–2999 格式、3000–3099 校验、
//! 3100–3199 密码、4000–4499 安全、4500–4999 配额、5000–5999 业务、
//! 6000–6999 任务、0000 兜底。
//!
//! 登记制：新码先登记 02 附录 A，再写本模块常量与 i18n 条目，
//! 三处一致由 TC-CORE-021（`tests/registry_consistency.rs`）锁定。

use crate::{FormatErrorKind, QuotaKind, SecurityViolation, UserAction};

/// IO / 磁盘 / 权限（区段 1000–1999，02 §3.4）
pub mod io {
    /// 目标不可写（HZ-1001）
    pub const TARGET_NOT_WRITABLE: u32 = 1001;
    /// 磁盘空间不足（HZ-1002）
    pub const DISK_FULL: u32 = 1002;
    /// 权限拒绝（HZ-1003）
    pub const PERMISSION_DENIED: u32 = 1003;
    /// 源不存在，原始 IO 层 ENOENT；`JobSpec` 校验归类 5004（HZ-1004）
    pub const SOURCE_NOT_FOUND_IO: u32 = 1004;
}

/// 格式解析（区段 2000–2999）
pub mod format {
    /// 魔数不匹配（HZ-2001）
    pub const BAD_SIGNATURE: u32 = 2001;
    /// 档案被截断（HZ-2002）
    pub const TRUNCATED: u32 = 2002;
    /// 未知压缩方法（HZ-2003）
    pub const UNKNOWN_METHOD: u32 = 2003;
    /// 档案头损坏（HZ-2004）
    pub const BAD_HEADER: u32 = 2004;
    /// 分卷缺失（HZ-2005）
    pub const MISSING_VOLUME: u32 = 2005;
}

/// 校验（区段 3000–3099）
pub mod checksum {
    /// CRC 校验失败（HZ-3001）
    pub const CRC_MISMATCH: u32 = 3001;
}

/// 密码/加密（区段 3100–3199）
pub mod password {
    /// 密码错误（HZ-3101）
    pub const WRONG_PASSWORD: u32 = 3101;
    /// 头部加密，无法列出条目（HZ-3102）
    pub const HEADER_ENCRYPTED: u32 = 3102;
    /// 未知加密方案（HZ-3103；经 `Format{UnknownCrypto}` 报出）
    pub const UNKNOWN_CRYPTO: u32 = 3103;
}

/// 安全（区段 4000–4499）
pub mod security {
    /// 路径穿越（HZ-4001）
    pub const PATH_TRAVERSAL: u32 = 4001;
    /// 符号链接逃逸（HZ-4002）
    pub const SYMLINK_ESCAPE: u32 = 4002;
    /// Windows 保留设备名（HZ-4003）
    pub const RESERVED_DEVICE_NAME: u32 = 4003;
    /// NUL/控制字符（HZ-4004）
    pub const CONTROL_CHARS: u32 = 4004;
}

/// 配额（区段 4500–4999）
pub mod quota {
    /// 单文件解压超限（HZ-4501）
    pub const MAX_FILE_BYTES: u32 = 4501;
    /// 总解压量超限（HZ-4502）
    pub const MAX_TOTAL_BYTES: u32 = 4502;
    /// 压缩比率超限（HZ-4503）
    pub const MAX_RATIO: u32 = 4503;
    /// 条目数超限（HZ-4504）
    pub const MAX_ENTRIES: u32 = 4504;
}

/// 业务规则（区段 5000–5999）
pub mod business {
    /// 不支持的操作（HZ-5001）
    pub const UNSUPPORTED: u32 = 5001;
    /// core 决策超时（HZ-5002，02 §9.3）；本 crate 暂无对应变体，构造方在 core
    pub const DECISION_TIMEOUT: u32 = 5002;
    /// 提交前校验（HZ-5003）；构造方在 core/cli
    pub const INVALID_PARAM: u32 = 5003;
    /// 源不存在（HZ-5004，`JobSpec` 校验）；构造方在 core/cli
    pub const SOURCE_NOT_FOUND: u32 = 5004;
    /// 目标非空冲突（HZ-5005）；构造方在 core/cli
    pub const TARGET_NOT_EMPTY: u32 = 5005;
}

/// 任务（区段 6000–6999）
pub mod job {
    /// 任务已取消（HZ-6001；退出码 130）
    pub const CANCELLED: u32 = 6001;
    /// 部分失败（HZ-6002；退出码 1）
    pub const PARTIAL_FAILURE: u32 = 6002;
    /// Engine 关闭后提交（HZ-6003）；本 crate 暂无对应变体，构造方在 core
    pub const QUEUE_CLOSED: u32 = 6003;
}

/// HZ-0000 未知变体兜底（02 §3.5）；`_` 分支只能存在于 crate 外（CLI/GUI）。
pub const INTERNAL_ZERO: u32 = 0;

/// 网络盘/管道瞬断 os 错误集（`is_retryable`；内部补充：unix/Windows 双列）。
///
/// unix: EPIPE=32、ECONNRESET=104、ETIMEDOUT=110；
/// win: ERROR_SEM_TIMEOUT=121、ERROR_NETNAME_DELETED=64、ERROR_BAD_NETPATH=53。
pub(crate) const RETRYABLE_OS: &[i32] = &[32, 104, 110, 121, 64, 53];

/// `io::Error` → IO 区段错误码（02 §3.3 逻辑表：PermissionDenied→1003、
/// NotFound→1004、ENOSPC(28)/ERROR_DISK_FULL(112)→1002、其余→1001）。
pub(crate) fn io_code(e: &std::io::Error) -> u32 {
    use std::io::ErrorKind::{NotFound, PermissionDenied};
    match e.kind() {
        PermissionDenied => io::PERMISSION_DENIED,
        NotFound => io::SOURCE_NOT_FOUND_IO,
        _ => match e.raw_os_error() {
            Some(28) | Some(112) => io::DISK_FULL, // ENOSPC / ERROR_DISK_FULL
            _ => io::TARGET_NOT_WRITABLE,
        },
    }
}

/// [`FormatErrorKind`] → 错误码（kind→2001..2005；UnknownCrypto→3103）。
pub(crate) fn format_kind_code(k: &FormatErrorKind) -> u32 {
    match k {
        FormatErrorKind::BadSignature => format::BAD_SIGNATURE,
        FormatErrorKind::Truncated => format::TRUNCATED,
        FormatErrorKind::UnknownMethod(_) => format::UNKNOWN_METHOD,
        FormatErrorKind::BadHeader => format::BAD_HEADER,
        FormatErrorKind::MissingVolume(_) => format::MISSING_VOLUME,
        FormatErrorKind::UnknownCrypto => password::UNKNOWN_CRYPTO,
    }
}

/// [`SecurityViolation`] → 错误码（violation→4001..4004）。
pub(crate) fn security_violation_code(v: &SecurityViolation) -> u32 {
    match v {
        SecurityViolation::PathTraversal(_) => security::PATH_TRAVERSAL,
        SecurityViolation::SymlinkEscape => security::SYMLINK_ESCAPE,
        SecurityViolation::ReservedDeviceName => security::RESERVED_DEVICE_NAME,
        SecurityViolation::ControlChars => security::CONTROL_CHARS,
    }
}

/// [`QuotaKind`] → 错误码（quota→4501..4504）。
pub(crate) fn quota_code(q: QuotaKind) -> u32 {
    match q {
        QuotaKind::MaxFileBytes => quota::MAX_FILE_BYTES,
        QuotaKind::MaxTotalBytes => quota::MAX_TOTAL_BYTES,
        QuotaKind::MaxRatio => quota::MAX_RATIO,
        QuotaKind::MaxEntries => quota::MAX_ENTRIES,
    }
}

/// 附录 A 登记表：30 行（29 登记码 + HZ-0000 兜底）。
///
/// 字段：`(code, i18n key, user_action, exit_code, 是否有本 crate 变体绑定)`。
/// 与 02 附录 A.1 逐行对应，由 TC-CORE-021a/b/c 锁定三处一致性。
pub const REGISTRY: &[(u32, &str, UserAction, u8, bool)] = &[
    (
        1001,
        "err.hz_1001",
        UserAction::CheckTargetWritable,
        2,
        true,
    ),
    (
        1002,
        "err.hz_1002",
        UserAction::FreeSpaceOrChangeTarget,
        2,
        true,
    ),
    (1003, "err.hz_1003", UserAction::CheckPermissions, 2, true),
    (1004, "err.hz_1004", UserAction::ConfirmInputPath, 2, true),
    (2001, "err.hz_2001", UserAction::VerifyArchive, 2, true),
    (2002, "err.hz_2002", UserAction::RefetchArchive, 2, true),
    (2003, "err.hz_2003", UserAction::UpgradeHzip, 2, true),
    (2004, "err.hz_2004", UserAction::TrySalvageRead, 2, true),
    (
        2005,
        "err.hz_2005",
        UserAction::SupplyMissingVolume,
        2,
        true,
    ),
    (3001, "err.hz_3001", UserAction::TryBestEffort, 2, true), // best_effort 经 Partial 归并 1
    (3101, "err.hz_3101", UserAction::RetryPassword, 2, true),
    (3102, "err.hz_3102", UserAction::RetryPassword, 2, true),
    (3103, "err.hz_3103", UserAction::UpgradeHzip, 2, true),
    (4001, "err.hz_4001", UserAction::BlockedNotice, 2, true),
    (
        4002,
        "err.hz_4002",
        UserAction::AdjustSymlinkPolicy,
        2,
        true,
    ),
    (4003, "err.hz_4003", UserAction::BlockedNotice, 2, true),
    (4004, "err.hz_4004", UserAction::BlockedNotice, 2, true),
    (4501, "err.hz_4501", UserAction::AdjustQuotaLimit, 2, true),
    (4502, "err.hz_4502", UserAction::AdjustQuotaLimit, 2, true),
    (4503, "err.hz_4503", UserAction::AdjustQuotaLimit, 2, true),
    (4504, "err.hz_4504", UserAction::AdjustQuotaLimit, 2, true),
    (5001, "err.hz_5001", UserAction::ExplainUnsupported, 2, true),
    (5002, "err.hz_5002", UserAction::TimeoutAborted, 2, false), // core 构造（任务卡 §7）
    (5003, "err.hz_5003", UserAction::ShowValidRange, 2, false),
    (5004, "err.hz_5004", UserAction::ConfirmInputPath, 2, false),
    (
        5005,
        "err.hz_5005",
        UserAction::ChooseOverwritePolicy,
        2,
        false,
    ),
    (
        6001,
        "err.hz_6001",
        UserAction::ShowRollbackReport,
        130,
        true,
    ),
    (6002, "err.hz_6002", UserAction::ShowFailureDetails, 1, true),
    (6003, "err.hz_6003", UserAction::ExitingSoon, 2, false),
    (0, "err.hz_0000", UserAction::ReportIssue, 2, false), // 兜底（CLI/GUI `_` 分支）
];
