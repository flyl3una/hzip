//! VMDK 稀疏 extent 的**头部探针**（魔数 `KDMV`）。
//!
//! # 与 qcow2 相反的字节序
//!
//! VMDK 的 sparse extent 头（`SparseExtentHeader`，前 512 字节）是**小端**：
//! 文件开头 4 字节按小端读出的 u32 等于 `0x564D444B`（即 ASCII 的 `K` `D` `M` `V`）。
//! qcow2 的魔数是**大端**——两者恰好互为镜像，故这行字节序**写死在这里并写明理由**，
//! 防「顺手统一成大端」。
//!
//! # 只认 sparse 头
//!
//! flat 布局（`monolithicFlat` / `twoGbMaxExtent*` 的**描述符文件**）里没有任何魔数，
//! 只有几百字节纯文本。它们由**扩展名回退**进到本 handler，再由描述符里的
//! `createType` 判定并**按名字拒绝**（见 [`crate::volume::classify_layout`]），
//! 绝不靠猜。
//!
//! # 字段合理性
//!
//! 只有魔数是不够的：随便一段以 `KDMV` 开头的字节也能骗过它。探针再核三件事
//! （都取自上游 `img_vmdk::header::offsets` 的公开偏移，不在本文件里抄第二遍布局）：
//! 版本 ∈ {1,2,3}（本家族读得了的三档修订）、`grainSize != 0`、`numGTEsPerGT != 0`。

use hzip_format::Confidence;
use img_vmdk::header::offsets;

/// 探针窗口：稀疏头正好 512 字节（探针只读头，**不碰数据区**）。
pub const PROBE_HEAD_BYTES: usize = 512;

/// sparse extent 魔数 `KDMV`（按**小端** u32 读文件开头 4 字节）。
pub const VMDK_SPARSE_MAGIC: u32 = img_vmdk::MAGIC;

/// ESXi `vmfsSparse` extent 的魔数 `COWD`（同法：小端 u32，磁盘上即 `C` `O` `W` `D`）。
///
/// 它**不是**本 crate 读得动的布局（那是 delta/redo 日志），但文件确是一个 VMDK
/// 稀疏 extent，说它「不是 VMDK 镜像」是错的——故由 [`crate::volume`] 按名字拒绝。
pub const VMDK_VMFS_MAGIC: u32 = img_vmdk::header::MAGIC_VMFS_SPARSE;

/// 内容探针：`Some(High)` = 认领为 VMDK `monolithicSparse` / `streamOptimized` extent。
#[must_use]
pub fn vmdk_probe(head: &[u8]) -> Option<Confidence> {
    if head.len() < PROBE_HEAD_BYTES {
        return None; // 头都不够：没证据（不猜）
    }
    if le_u32(head, offsets::MAGIC)? != VMDK_SPARSE_MAGIC {
        return None;
    }
    // 字段合理性（魔数之外的第二个证据）
    if !matches!(le_u32(head, offsets::VERSION)?, 1..=3) {
        return None;
    }
    if le_u64(head, offsets::GRAIN_SIZE)? == 0 {
        return None;
    }
    if le_u32(head, offsets::NUM_GTES_PER_GT)? == 0 {
        return None;
    }
    Some(Confidence::High)
}

fn le_u32(bytes: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(off..off + 4)?.try_into().ok()?,
    ))
}

fn le_u64(bytes: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(off..off + 8)?.try_into().ok()?,
    ))
}
