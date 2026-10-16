//! `fs_ext4::Error` → [`VolError`]：**拒绝理由即文案键**（与 `hzip-vol/src/error.rs`
//! 同一口径）。
//!
//! # 为什么要有这一层
//!
//! 上游 0.x 的错误枚举随时会加变体，而用户看到的那句「为什么不行」必须稳定：
//! 故本层把能给出**稳定键**的那几类挑出来，其余折成 [`VolError::Image`]
//! （细节进 `tracing`，用户侧给通用「结构不合法」）。
//!
//! | 上游 | 卷层 | 键 |
//! |---|---|---|
//! | `Io` | `VolError::Io` | （`std::io` 自带） |
//! | `ReadOnly` | `ReadOnly(DeviceReadOnly)` | `vol.readonly.device` |
//! | `JournalNotReplayed` | `ReadOnly(JournalPending)` | `vol.readonly.journal` |
//! | `NoSpaceLeftOnDevice` | `Quota` | `vol.err.no-space` |
//! | `DirectoryNotEmpty` | `Unsupported` | `vol.err.dir-not-empty` |
//! | `NameTooLong` | `Unsupported` | `vol.err.name-too-long` |
//! | `UnsupportedIncompat` / `UnsupportedRoCompat` | `Unsupported` | `vol.err.fs-feature` |
//! | 其余（含 `NotFound`/`InvalidInode`/`Corrupt*`） | `Image` | （细节落日志） |

use fs_ext4::Error as Upstream;
use hzip_vol::error::{ReadOnlyReason, VolError};

/// 卷太小，放不下 ext4 超级块（挂载前的前置检查）。
pub(crate) fn too_small(size: u64, want: u64) -> VolError {
    VolError::Image(format!(
        "卷只有 {size} 字节，放不下 ext4 超级块（需要 {want}）"
    ))
}

/// 上游错误 → 卷层错误。
pub(crate) fn map_err(e: Upstream) -> VolError {
    match e {
        Upstream::Io(io) => VolError::Io(io),
        // 上游的 `ReadOnly` 涵盖「设备不可写 / 写坏特性 / 上一次直接提交失败」三类，
        // 对用户来说都是「这个目标写不了」。
        Upstream::ReadOnly => VolError::ReadOnly(ReadOnlyReason::DeviceReadOnly),
        Upstream::JournalNotReplayed => VolError::ReadOnly(ReadOnlyReason::JournalPending),
        Upstream::NoSpaceLeftOnDevice => VolError::Quota("vol.err.no-space"),
        Upstream::DirectoryNotEmpty => VolError::Unsupported("vol.err.dir-not-empty"),
        Upstream::NameTooLong => VolError::Unsupported("vol.err.name-too-long"),
        Upstream::UnsupportedIncompat(bits) => {
            tracing::debug!(
                bits = format!("0x{bits:08x}"),
                "卷带我们不支持的 INCOMPAT 特性"
            );
            VolError::Unsupported("vol.err.fs-feature")
        }
        Upstream::UnsupportedRoCompat(bits) => {
            tracing::debug!(
                bits = format!("0x{bits:08x}"),
                "卷带我们不维护的 RO_COMPAT 特性"
            );
            VolError::Unsupported("vol.err.fs-feature")
        }
        other => VolError::Image(format!("{other}")),
    }
}
