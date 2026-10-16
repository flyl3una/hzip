//! ext2/3/4 驱动入口：头部复核（`supports`）+ 挂载策略（`mount`）。
//!
//! # 探针口径（与 `hzip-fmt-raw/src/probe.rs` 同一条规则）
//!
//! ext4 的超级块**固定**起于偏移 1024（与块大小无关），`s_magic`（`0xEF53`）在超级块
//! 内偏移 56 ⇒ **绝对 1080**。⚠ 别按「`53 EF` @1024」认 ext4：1024 处是
//! `s_inodes_count`。本驱动只复核这一个字段——嗅探（分区表的 `FsKind`）已经做了第一
//! 道判断，这里是第二份独立证据（`hzip_vol::registry::mount_best` 的约定：两份都点头
//! 才挂载）。
//!
//! # 挂载策略（`FsOptions` 三个字段的全部语义）
//!
//! | 设备 | 卷 | 请求 | 动作 | 结果 |
//! |---|---|---|---|---|
//! | 不可写 | 任意 | 任意 | `Filesystem::mount` | 只读（上游把日志重放**落进缓存**⇒读侧看到重放后的元数据，设备一个字节不写） |
//! | 可写 | 干净 | `writable` | `mount_lazy` | 可写 |
//! | 可写 | 待核查 | `writable` + `replay_journal` | `mount_lazy` → `replay_journal_if_dirty()` → `recover_orphans()` | 可写（用户已同意） |
//! | 可写 | 待核查 | `writable`（无 `replay_journal`） | `mount_lazy` | **只读** + `ReadOnlyReason::JournalPending` |
//! | 可写 | 任意 | 只读 | `mount_lazy` | 只读 + `JournalPending`/`BySetting` |
//! | **不可写** | 任意 | `writable` | `Filesystem::mount` | 只读 + `ReadOnlyReason::DeviceReadOnly`（目标写不了，如实说） |
//!
//! ## 「待核查」的判据
//!
//! `s_state`（超级块内 `0x3A`）的 `EXT4_VALID_FS` 位为 0 ⇒ 上次没有干净卸载；加上
//! 「JBD2 日志超级块不干净」这一条独立证据（无内部日志的卷 ⇒ 后者恒为假）。
//!
//! ## 为什么可写分支不用 `mount` / `mount_recovering`
//!
//! - `mount_recovering` 要求卷带**内部日志**（`Error::Unsupported("checked recovery
//!   requires an internal journal")`），而 mkfs 造出的无日志 ext4 卷（也是夹具的形态）
//!   直接挂不上；
//! - `Filesystem::mount` 在设备可写时会**背着用户重放**脏日志——那正是
//!   `replay_journal: false` 要禁止的事。
//!
//! 故一律 `mount_lazy`：挂载期一个字节都不写设备，重放要么由用户显式同意后发生，
//! 要么不发生。若仍有脏日志漏过判据，上游的 `replay_pending` 会让每次写入返回
//! `JournalNotReplayed` ⇒ 本驱动映射为 `ReadOnly(JournalPending)`（宁可拒写，不静默重放）。
//!
//! ## `FsOptions::cache_blocks` 不生效（如实说明）
//!
//! 上游只提供默认缓存的 `mount*` 系列（`DEFAULT_CACHE_BLOCKS` = 256 块），没有
//! 「按调用方给的块数挂载」的惰性入口；而上层的读缓存本就由
//! `hzip_vol::VolOptions::cache_blocks`（`CachingDevice`，块 64 KiB）承担——按
//! `FsOptions` 再造一份缓存语义只会让两处口径打架。

use std::sync::Arc;

use fs_ext4::Filesystem;
use fs_ext4::fs_core_bridge::CoreDevice;
use hzip_vol::error::{ReadOnlyReason, VolResult};
use hzip_vol::{BlockDevice, BlockRead as _, FsCaps, FsDriver, FsKind, FsOptions, FsVolume};

use crate::error::{map_err, too_small};
use crate::volume::Ext4Volume;

/// 超级块内的 `s_magic` 偏移（`0x38`）＋超级块固定起始 1024。
pub const MAGIC_AT: usize = 1024 + 0x38;

/// 超级块内的 `s_state` 偏移（`0x3A`）＋超级块固定起始 1024。
pub const STATE_AT: usize = 1024 + 0x3A;

/// `s_state` 的「干净卸载」位（`EXT4_VALID_FS`）：为 0 ⇒ 卷待核查。
pub const EXT4_VALID_FS: u16 = 0x0001;

/// ext2/3/4 驱动（无状态：挂载态全在 [`Ext4Volume`] 里）。
#[derive(Debug, Default, Clone, Copy)]
pub struct Ext4Driver;

impl Ext4Driver {
    /// 造一个驱动（`hzip-vol` 的注册表按 [`hzip_vol::FsKind`] 挑它）。
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl FsDriver for Ext4Driver {
    fn id(&self) -> &'static str {
        "ext4"
    }

    fn supports(&self, kind: FsKind, head: &[u8]) -> bool {
        // ext2/3/4 在 `FsKind` 里是同一族（布局相同，特性位区分）。
        matches!(kind, FsKind::Ext4)
            && head.len() >= MAGIC_AT + 2
            && head[MAGIC_AT..MAGIC_AT + 2] == [0x53, 0xEF]
    }

    fn mount(&self, dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Box<dyn FsVolume>> {
        let state = superblock_state(&dev)?;
        let unclean = state & EXT4_VALID_FS == 0;
        let device_writable = dev.is_writable();
        let core = Arc::new(CoreDevice::new(dev));

        let mut fs = if device_writable {
            Filesystem::mount_lazy(core).map_err(map_err)?
        } else {
            Filesystem::mount(core).map_err(map_err)?
        };
        let needs_recovery = unclean || journal_is_dirty(&fs)?;

        if !opts.writable {
            // 只读请求：不写设备，也不重放（读侧不改写镜像是我们的红线）。
            // 上游在**不可写设备**上已把重放落进缓存，故这里不妨说明：可写设备上的
            // 只读挂载看到的是重放前的元数据——脏卷的读由 `replay_journal: true`
            // 的可写挂载（或其后的只读挂载）提供。
            let reason = if needs_recovery {
                ReadOnlyReason::JournalPending
            } else {
                ReadOnlyReason::BySetting
            };
            tracing::debug!(?reason, "ext4 只读挂载");
            return Ok(Box::new(Ext4Volume::read_only(fs, reason)));
        }

        if !device_writable {
            // 请求可写但目标根本写不了（只读介质/权限/上层给的是只读设备）：
            // 如实标 `DeviceReadOnly`，而不是给一个写起来才知道的错。
            tracing::debug!("ext4 目标设备不可写 ⇒ 只读");
            return Ok(Box::new(Ext4Volume::read_only(
                fs,
                ReadOnlyReason::DeviceReadOnly,
            )));
        }

        if needs_recovery {
            if !opts.replay_journal {
                tracing::debug!("ext4 卷待核查且未允许重放 ⇒ 只读");
                return Ok(Box::new(Ext4Volume::read_only(
                    fs,
                    ReadOnlyReason::JournalPending,
                )));
            }
            // 用户显式同意（设置页的「先重放日志再写」）：重放完再收孤儿链
            // ——崩溃留下的半个 truncate/unlink 属于同一次核查。
            let replayed = fs.replay_journal_if_dirty().map_err(map_err)?;
            let orphans = fs.recover_orphans().map_err(map_err)?;
            tracing::info!(replayed, orphans, "ext4 已重放日志并收尾孤儿链");
        }

        // 干净卷走 `mount_lazy` 也不会少了什么：孤儿链只可能由崩溃产生，而干净卷的
        // 判据（`s_state` 置位 + 日志干净）正是「上次收尾完成」。
        Ok(Box::new(Ext4Volume::writable(fs)))
    }
}

/// 读超级块的 `s_state`（绝对 [`STATE_AT`]，u16 LE）。
fn superblock_state(dev: &Arc<dyn BlockDevice>) -> VolResult<u16> {
    let need = (STATE_AT + 2) as u64;
    let size = dev.size_bytes();
    if size < need {
        return Err(too_small(size, need));
    }
    let mut buf = [0u8; 2];
    dev.read_at(STATE_AT as u64, &mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

/// JBD2 日志是否含未重放的事务；无内部日志（`journal_inode == 0`）⇒ `false`。
fn journal_is_dirty(fs: &Filesystem) -> VolResult<bool> {
    let jsb = fs_ext4::jbd2::read_superblock(fs).map_err(map_err)?;
    Ok(jsb.is_some_and(|j| !j.is_clean()))
}

/// 可写挂载的能力集（写侧全开）。
///
/// `symlink` 为真：上游 `apply_symlink` 已接好（符号链接的**读**侧在 `entries()` 里
/// 给出目标）；当前 `hzip_format::Change` 的五类变体里没有「建符号链接」这一条，
/// 故这一位描述的是卷的能力，而不是本期的调用路径。
pub(crate) const fn writable_caps() -> FsCaps {
    FsCaps {
        write: true,
        delete: true,
        rename: true,
        mkdir: true,
        symlink: true,
        times: true,
        mode: true,
        name_max: crate::volume::NAME_MAX,
        read_only_reason: None,
    }
}
