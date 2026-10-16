//! `fs_xfs::Error` → [`VolError`]：**拒绝理由即稳定键**（与 `hzip-fs-ext4/src/error.rs`
//! 同一口径）。
//!
//! # 为什么要有这一层
//!
//! 上游 0.x 的错误枚举随时会加变体，而用户看到的那句「为什么不行」必须稳定：
//! 故本层把能给出**稳定键**的那几类挑出来，其余折成 [`VolError::Image`]
//! （细节进 `tracing`，用户侧给通用「结构不合法」）。
//!
//! # 键的来源（**不新增键**）
//!
//! 键必须已经在 locale 表里（`vol.*` 一族由 UI/CLI 的 i18n 表维护，而本 crate
//! 无权改那些表）——故这里只用**既有键**，语义对齐处与 NTFS/ext4 驱动一致：
//!
//! | 上游 | 卷层 | 键 |
//! |---|---|---|
//! | `Io` | `Io(io::Error)` | （io） |
//! | `ReadOnly` | `ReadOnly(DeviceReadOnly)` | `vol.readonly.device` |
//! | `DirtyLog` | `ReadOnly(JournalPending)` | `vol.readonly.journal` |
//! | `NoSpace` | `Quota` | `vol.err.no-space` |
//! | `DirectoryNotEmpty` | `Unsupported` | `vol.err.dir-not-empty` |
//! | `AlreadyExists` | `Unsupported` | `vol.err.exists` |
//! | `NotFound` | `Unsupported` | `vol.err.entry-gone` |
//! | `NotADirectory` / `NotAFile` | `Unsupported` | `vol.err.bad-type` |
//! | `UnsupportedFeature`（含 realtime/reflink/btree fork/extent 预算） | `Unsupported` | `vol.err.fs-feature` |
//! | `RealtimeDeviceAbsent` | `Unsupported` | `vol.err.fs-feature` |
//! | `NotXfs` / `BadSuperblock` / `ChecksumMismatch` / `BlockIdentityMismatch` / `CorruptLog` / `Internal` / `InvalidGeometry` | `Image` | （细节落日志） |
//!
//! 「短格式目录」与「跨目录改名」两条**本驱动自己先判**（不看上游错误文本），
//! 因此能给出更准的键：`vol.unsupported.dir-too-large` 与 `vol.err.dir-move`
//! （见 [`ERR_DIR_TOO_LARGE`]、[`ERR_DIR_MOVE`]）。

use std::io;

use fs_xfs::Error as Upstream;
use hzip_vol::error::{ReadOnlyReason, VolError};

// ---------------------------------------------------------------- 稳定键

/// 目录非空（不递归删：镜像里的删除应当是显式的）。
pub const ERR_DIR_NOT_EMPTY: &str = "vol.err.dir-not-empty";
/// 目标名字已被占用。
pub const ERR_EXISTS: &str = "vol.err.exists";
/// 名字超过 XFS 的 255 字节上限。
pub const ERR_NAME_TOO_LONG: &str = "vol.err.name-too-long";
/// 源不是常规文件（目录/FIFO/设备/符号链接）。
pub const ERR_SOURCE_NOT_FILE: &str = "vol.err.source-not-file";
/// `EntryId` 不在本次列表里（调用方没先 `entries()`，或 id 越界）。
pub const ERR_UNKNOWN_ENTRY: &str = "vol.err.unknown-entry";
/// 该 id 指向的条目/其父目录已不在卷里。
pub const ERR_ENTRY_GONE: &str = "vol.err.entry-gone";
/// 本驱动不认识的 `Change` 变体（`Change` 是 `#[non_exhaustive]`）。
pub const ERR_UNKNOWN_CHANGE: &str = "vol.err.unknown-change";
/// 类型不符（拿目录当文件写、拿符号链接当常规文件读）。
pub const ERR_BAD_TYPE: &str = "vol.err.bad-type";
/// 卷内空间不足（上游 `NoSpace`）。
pub const ERR_NO_SPACE: &str = "vol.err.no-space";
/// 该特性/形态上游不写（realtime、reflink、B+tree 数据叉、extent 预算…）。
pub const ERR_FS_FEATURE: &str = "vol.err.fs-feature";
/// 跨目录改名（上游只提供「短格式目录内改名」）。
pub const ERR_DIR_MOVE: &str = "vol.err.dir-move";
/// 目录已超出短格式（上游的改名只覆盖短格式目录）。
pub const ERR_DIR_TOO_LARGE: &str = "vol.unsupported.dir-too-large";

// ---------------------------------------------------------------- 映射

/// 卷太小，放不下 XFS 超级块窗口（挂载前的前置检查）。
pub(crate) fn too_small(size: u64, want: u64) -> VolError {
    VolError::Image(format!(
        "卷只有 {size} 字节，放不下 XFS 超级块（需要 {want}）"
    ))
}

/// 上游错误 → 卷层错误。
pub(crate) fn map_err(e: Upstream) -> VolError {
    match e {
        Upstream::Io(msg) => VolError::Io(io::Error::other(msg)),
        // 上游的 `ReadOnly` 涵盖「mount 只读 / v4 / 该形态不写」三类，对用户来说
        // 都是「这个目标写不了」。
        Upstream::ReadOnly => VolError::ReadOnly(ReadOnlyReason::DeviceReadOnly),
        // 读侧遇到脏日志走内存重放；写侧遇到它只能只读 —— 与 ext4 侧同口径。
        Upstream::DirtyLog => VolError::ReadOnly(ReadOnlyReason::JournalPending),
        Upstream::NoSpace(detail) => {
            tracing::debug!(detail, "XFS 卷空间不足");
            VolError::Quota(ERR_NO_SPACE)
        }
        Upstream::DirectoryNotEmpty => VolError::Unsupported(ERR_DIR_NOT_EMPTY),
        Upstream::AlreadyExists => VolError::Unsupported(ERR_EXISTS),
        Upstream::NotFound => VolError::Unsupported(ERR_ENTRY_GONE),
        Upstream::NotADirectory | Upstream::NotAFile => VolError::Unsupported(ERR_BAD_TYPE),
        Upstream::UnsupportedFeature(detail) => {
            // 上游把所有「这个形态我不写」都塞进这一个变体（文本会变），故键取通用
            // 的那一个，细节进日志：**不按文本猜键**（上游改文案不该改我们的键）。
            tracing::debug!(detail, "XFS 卷带我们不写的形态/特性");
            VolError::Unsupported(ERR_FS_FEATURE)
        }
        Upstream::RealtimeDeviceAbsent { ino } => {
            tracing::debug!(ino, "inode 的数据在 realtime 设备上，本次挂载没给它");
            VolError::Unsupported(ERR_FS_FEATURE)
        }
        Upstream::NotXfs { magic } => {
            VolError::Image(format!("不是 XFS 卷（偏移 0 的魔数 0x{magic:08x} ≠ XFSB）"))
        }
        Upstream::ChecksumMismatch { what, block } => {
            VolError::Image(format!("{what} 的校验和不符（块 {block}）"))
        }
        Upstream::BlockIdentityMismatch {
            what,
            expected,
            found,
        } => VolError::Image(format!(
            "{what} 自述身份与读出位置不符（读自块 {expected}，自称块 {found}）"
        )),
        Upstream::BadSuperblock(detail) => VolError::Image(format!("超级块不合法：{detail}")),
        Upstream::CorruptLog(detail) => VolError::Image(format!("日志不合法：{detail}")),
        Upstream::InvalidGeometry(detail) => VolError::Image(format!("几何不合法：{detail}")),
        Upstream::Internal(detail) => VolError::Image(format!("上游内部不变量不符：{detail}")),
    }
}
