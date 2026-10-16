//! XFS 驱动入口：头部复核（`supports`）+ 挂载策略（`mount`）。
//!
//! # 探针口径（与 `hzip-fs-ext4/src/driver.rs`、`hzip-fmt-raw/src/probe.rs` 同一规则）
//!
//! XFS 的超级块**固定**起于偏移 0：`sb_magicnum` = `XFSB`（**大端** 4 字节，即盘上
//! 就是 ASCII `XFSB`）。本驱动只认这个魔数，**另起一份独立证据**：块大小/扇区大小/
//! inode 大小互为整数的幂次与倍数、版本号是 v4 或 v5（`sb_versionnum` 低 4 位）、
//! `sb_agcount` 与 `sb_dblocks` 非零——五条都对才点头。嗅探（分区表的 `FsKind`）与
//! 这里的复核是两份证据（`hzip_vol::registry::mount_best` 的约定：两份都点头才挂载）。
//!
//! 这也是「块大小字段是 0 但魔数恰好是 XFSB」这类位翻转被挡在挂载之前的地方：
//! 进去以后只会解析出乱七八糟的几何。
//!
//! # 大端，但校验和是小端
//!
//! 盘上每个多字节整数都是**大端**（`from_be_bytes`）；唯一的例外是**校验和字段**，
//! 内核 `xfs_end_cksum()` 返回 `~cpu_to_le32(crc)` ⇒ 校验和按**小端**读。本驱动不
//! 自己校验任何校验和（那是上游的事），但头部的每个整数都按大端取——按小端读会让
//! 每一个真实卷都像坏的。
//!
//! # 挂载策略（`FsOptions` 三个字段的全部语义）
//!
//! | 设备 | 日志 | 请求 | 动作 | 结果 |
//! |---|---|---|---|---|
//! | 任意 | 任意 | 只读 | `Filesystem::mount`（脏日志**内存重放**） | 只读 + `JournalPending`/`BySetting` |
//! | 不可写 | 任意 | `writable` | `Filesystem::mount` | 只读 + `ReadOnlyReason::DeviceReadOnly` |
//! | 可写 | 待重放 | `writable` | `Filesystem::mount`（内存重放） | **只读** + `JournalPending` |
//! | 可写 | 干净 | `writable` | `Filesystem::mount_rw` | 可写 |
//!
//! ## 脏日志：读侧内存重放、设备不动（上游 `log`/`log_recover` 的口径）
//!
//! 上游 `Filesystem::mount` 会把日志里未落盘的记录**重放进内存里的 overlay**，
//! 于是读到的就是「内核恢复之后」的样子，而**设备一个字节不写**——这正是我们从
//! 一个待恢复的磁盘上取数据时想要的。`Filesystem::was_replayed()` 如实报告这件事。
//!
//! ## 为什么 `replay_journal` 无法让脏卷变可写（如实说明）
//!
//! 上游 0.14.1 **没有**「把日志重放写进设备」的入口：`mount_rw` 遇到脏日志直接
//! 拒绝（`Error::DirtyLog`），唯一的落盘方式是**本挂载自己**写记录（overlay 的 push
//! 只推自己记的东西）。故脏卷上 `replay_journal: true` 也**只能只读**——我们不假装
//! 做到了（那会让用户在「已重放」的错觉下写进一个与磁盘不一致的卷），而是记一条
//! `tracing::warn!` 并给出 `JournalPending`。要写，先由内核（`xfs_repair`/挂载一次）
//! 把卷弄干净。
//!
//! ## `FsOptions::cache_blocks` 只在只读挂载上生效
//!
//! 上游有 `mount_with_cache(dev, blocks)`（只读）与 `mount_rw(dev)`（可写，无缓存
//! 参数）两个入口：可写挂载的缓存由卷层（`hzip_vol::VolOptions::cache_blocks` 的
//! `CachingDevice`，块 64 KiB）承担，本驱动**不再造第二份语义**。

use std::sync::Arc;

use fs_xfs::{Filesystem, Superblock};
use hzip_vol::error::{ReadOnlyReason, VolResult};
use hzip_vol::{BlockDevice, BlockRead as _, FsCaps, FsDriver, FsKind, FsOptions, FsVolume};

use crate::error::{map_err, too_small};
use crate::volume::{NAME_MAX, XfsVolume};

/// 超级块魔数（偏移 0，**大端**；盘上即 ASCII）。
pub const MAGIC: [u8; 4] = *b"XFSB";
/// `sb_blocksize`（**大端** `u32`）。
pub const BLOCKSIZE_AT: usize = 4;
/// `sb_agcount`（**大端** `u32`）。
pub const AGCOUNT_AT: usize = 88;
/// `sb_versionnum`（**大端** `u16`；低 4 位是版本号）。
pub const VERSIONNUM_AT: usize = 100;
/// `sb_sectsize`（**大端** `u16`）。
pub const SECTSIZE_AT: usize = 102;
/// `sb_inodesize`（**大端** `u16`）。
pub const INODESIZE_AT: usize = 104;
/// `sb_dblocks`（**大端** `u64`）。
pub const DBLOCKS_AT: usize = 8;

/// 复核所需的最短头部（覆盖到 `sb_inodesize`；取 512 = 一个最小扇区，留足余量）。
pub const HEAD_MIN: usize = 512;

/// 上游 `mount` 读的超级块窗口（4 KiB：覆盖所有受支持的扇区大小）。
pub const SUPERBLOCK_BYTES: usize = 4096;

/// 版本号的位掩码（`XFS_SB_VERSION_NUMBITS`；上游 `superblock` 同值）。
pub const VERSION_NUMBITS: u16 = 0x000f;

/// XFS 驱动（无状态：挂载态全在 [`XfsVolume`] 里）。
#[derive(Debug, Default, Clone, Copy)]
pub struct XfsDriver;

impl XfsDriver {
    /// 造一个驱动（`hzip-vol` 的注册表按 [`hzip_vol::FsKind`] 挑它）。
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl FsDriver for XfsDriver {
    fn id(&self) -> &'static str {
        "xfs"
    }

    fn supports(&self, kind: FsKind, head: &[u8]) -> bool {
        kind == FsKind::Xfs && is_xfs_superblock(head)
    }

    fn mount(&self, dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Box<dyn FsVolume>> {
        let device_writable = dev.is_writable();
        let sb = read_superblock(&dev)?;
        // 日志状态：`Empty`/`CleanlyUnmounted` = 没有待重放的记录（新格式化的卷与
        // 干净卸载的卷都在此列）；`NeedsReplay` = 有。外部日志 ⇒ 上游直接拒绝。
        let log_state = fs_xfs::log::inspect(dev.as_ref(), &sb).map_err(map_err)?;
        let dirty = log_state == fs_xfs::log::LogState::NeedsReplay;
        tracing::debug!(
            ?log_state,
            device_writable,
            writable = opts.writable,
            replay_journal = opts.replay_journal,
            "XFS 卷头部已复核"
        );

        // ① 调用方要求只读：一律走只读挂载（脏日志在内存里重放，设备不动）。
        if !opts.writable {
            let fs = mount_ro(&dev, opts)?;
            let reason = if dirty {
                ReadOnlyReason::JournalPending
            } else {
                ReadOnlyReason::BySetting
            };
            tracing::debug!(?reason, "XFS 只读挂载");
            return Ok(Box::new(XfsVolume::new(
                dev,
                fs,
                opts.clone(),
                FsCaps::read_only(reason),
            )));
        }

        // ② 请求可写但目标根本写不了（只读介质/权限/上层给的是只读设备）：如实标
        //    `DeviceReadOnly`，而不是给一个写起来才知道的错。
        if !device_writable {
            let fs = mount_ro(&dev, opts)?;
            tracing::debug!("XFS 目标设备不可写 ⇒ 只读");
            return Ok(Box::new(XfsVolume::new(
                dev,
                fs,
                opts.clone(),
                FsCaps::read_only(ReadOnlyReason::DeviceReadOnly),
            )));
        }

        // ③ 日志待重放：上游只有「内存重放」，没有「重放写进设备」的入口 ⇒ 只能
        //    只读。`replay_journal` 无法被兑现，如实记一条 warn（见模块文档）。
        if dirty {
            if opts.replay_journal {
                tracing::warn!(
                    "上游 rust-fs-xfs 0.14.1 只把日志重放进内存（没有回写设备的入口）\
                     ⇒ 即使允许重放，脏卷也只能只读；请先用内核挂载一次或 xfs_repair 弄干净"
                );
            }
            let fs = mount_ro(&dev, opts)?;
            tracing::debug!("XFS 卷日志待重放 ⇒ 只读");
            return Ok(Box::new(XfsVolume::new(
                dev,
                fs,
                opts.clone(),
                FsCaps::read_only(ReadOnlyReason::JournalPending),
            )));
        }

        // ④ 干净卷 + 可写设备 + 请求可写：真可写（`mount_rw` 自己也会复核日志干净）。
        let fs = Filesystem::mount_rw(dev.clone()).map_err(map_err)?;
        Ok(Box::new(XfsVolume::new(
            dev,
            fs,
            opts.clone(),
            writable_caps(),
        )))
    }
}

/// 只读挂载（`cache_blocks > 0` 时按调用方的块数挂缓存；见模块文档）。
fn mount_ro(dev: &Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<Filesystem> {
    let rd = dev.clone() as Arc<dyn hzip_vol::BlockRead>;
    if opts.cache_blocks == 0 {
        Filesystem::mount(rd).map_err(map_err)
    } else {
        Filesystem::mount_with_cache(rd, opts.cache_blocks).map_err(map_err)
    }
}

/// 读超级块窗口并解析（几何与 CRC 由上游 `Superblock::parse` 判；v5 的 CRC 也在这里查）。
fn read_superblock(dev: &Arc<dyn BlockDevice>) -> VolResult<Superblock> {
    let size = dev.size_bytes();
    if size < SUPERBLOCK_BYTES as u64 {
        return Err(too_small(size, SUPERBLOCK_BYTES as u64));
    }
    let mut buf = vec![0u8; SUPERBLOCK_BYTES];
    dev.read_at(0, &mut buf)?;
    Superblock::parse(&buf).map_err(map_err)
}

/// 头部复核：`head` 是不是一个**自洽**的 XFS 超级块起始。
///
/// 五条独立证据（缺一不接，**不 panic、不夹取**）：
///
/// 1. 长度够（[`HEAD_MIN`]）；
/// 2. 偏移 0 是 `XFSB`（**大端**魔数）；
/// 3. `sb_versionnum` 低 4 位是 4 或 5（v4/v5；上游 `Superblock::parse` 同口径）；
/// 4. `sb_blocksize` 是 2 的幂且落在 512..=65536；`sb_sectsize` 是 2 的幂且落在
///    512..=32768 且整除块大小；`sb_inodesize` 是 2 的幂且落在 256..=2048 且整除块大小；
/// 5. `sb_agcount` 与 `sb_dblocks` 都不是 0（零个分配组/零个块的卷不存在）。
#[must_use]
pub fn is_xfs_superblock(head: &[u8]) -> bool {
    if head.len() < HEAD_MIN || head[..4] != MAGIC {
        return false;
    }
    let be_u16 = |at: usize| u16::from_be_bytes([head[at], head[at + 1]]);
    let be_u32 =
        |at: usize| u32::from_be_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    let be_u64 = |at: usize| {
        let mut b = [0u8; 8];
        b.copy_from_slice(&head[at..at + 8]);
        u64::from_be_bytes(b)
    };

    let version = be_u16(VERSIONNUM_AT) & VERSION_NUMBITS;
    if !(4..=5).contains(&version) {
        return false;
    }
    let blocksize = be_u32(BLOCKSIZE_AT);
    if !(512..=65536).contains(&blocksize) || !blocksize.is_power_of_two() {
        return false;
    }
    let sectsize = u32::from(be_u16(SECTSIZE_AT));
    if !(512..=32768).contains(&sectsize) || !sectsize.is_power_of_two() {
        return false;
    }
    let inodesize = u32::from(be_u16(INODESIZE_AT));
    if !(256..=2048).contains(&inodesize) || !inodesize.is_power_of_two() {
        return false;
    }
    // 扇区与 inode 都必须是块大小的整数分片（XFS 的几何约束）。
    if blocksize % sectsize != 0 || blocksize % inodesize != 0 {
        return false;
    }
    be_u32(AGCOUNT_AT) != 0 && be_u64(DBLOCKS_AT) != 0
}

/// 可写挂载的能力集（写侧全开）。
///
/// `symlink` 为真：上游 `create_symlink` 已接好（符号链接的**读**侧在 `entries()`
/// 里给出目标）；当前 `hzip_format::Change` 的五类变体里没有「建符号链接」这一条，
/// 故这一位描述的是卷的能力，而不是本期的调用路径——与 ext4 侧同口径。
///
/// `times`/`mode` 为真：上游 `change_attributes` 已用于 `Replace`/`Append` 的本地
/// mtime 保真（权限位目前只有 `Change` 之外的上层在用）。
pub(crate) const fn writable_caps() -> FsCaps {
    FsCaps {
        write: true,
        delete: true,
        rename: true,
        mkdir: true,
        symlink: true,
        times: true,
        mode: true,
        name_max: NAME_MAX,
        read_only_reason: None,
    }
}
