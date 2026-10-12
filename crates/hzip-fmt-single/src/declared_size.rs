//! 容器自带的**原始大小**探测（02 §8.5；T23 契约修订 2026-09-24）。
//!
//! # 为什么需要
//!
//! 单文件流格式原先一律上报 `size_raw = 0`（未知）——契约红线 1（`entries()` 不读
//! 数据区）下的保守选择。但这条语义让 **core 的进度层拿不到总量**：解压任务的
//! `total_bytes = Σ size_raw` 恒为 0 ⇒ 百分比恒为未知/0%（用户实测：解压一个大
//! `.gz` 一直 0%，而按字节推进的速度信息其实一直是有的）。
//!
//! 事实上三种容器**在元数据里就写着**原始大小（不需要解码）：
//!
//! | 格式 | 来源 | 精确性 |
//! |---|---|---|
//! | gz | 尾部 8 B 成员尾的 `ISIZE`（`gzip -l` 同口径） | 单成员且 < 4 GiB 精确；多成员只看**末成员**、≥ 4 GiB 按 2³² 取模 |
//! | xz | 尾部索引（index）里各记录 `uncompressed_size` 之和 | 精确（含多块单流；多流串联只看末流） |
//! | zst | 首帧头的 `Frame_Content_Size` | 写了该字段时精确（外部 `zstd` 默认写；hzip 自产不写 ⇒ 未知） |
//! | bz2 | 无该字段 | 未知（`None`） |
//!
//! 返回 `None` = 未知/不可信 ⇒ 调用方保持 `size_raw = 0`（既有语义不变）。
//!
//! # 边界纪律
//!
//! 只做**有界 seek + 小读**（gz 读 4 B、xz 读索引 ≤ 16 MiB 并有记录数上限），
//! 绝不扫数据区（红线 1 / TC-CONF-010 语义仍成立：列表耗时与数据区大小无关）；
//! 任何畸形输入都返回 `None`（不 panic、不无界分配）。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::handler::SingleFmt;

/// 索引缓冲上限（xz：防畸形 `Backward Size` 构造的超大分配）。
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
/// 索引记录数上限（防 CPU/内存放大攻击；真实 xz 单流记录数远小于此）。
const MAX_INDEX_RECORDS: u64 = 1_000_000;

/// 容器自带的原始大小——**按编码器 id 查询**（`None` = 未知/不适用/畸形/该编码器
/// 不在此构建内）。
///
/// core 的**开档磁盘预检**用（`hzip_core::services::space::precheck_open_temp_space`）：
/// `.cpio.gz` 这类组合档，内层容器 handler 必须先把解压后的整层落临时文件才能
/// 随机访问（cpio/tar 的 seek 语义），若临时盘装不下就该**在解压之前**拒绝——
/// 而不是让用户等十来秒、写到一半才拿到 ENOSPC。声明大小（gz 尾 ISIZE / xz 尾
/// 索引 / zst 帧头）恰好给出这个「要写多少」。
///
/// 单一来源：与单流格式 `size_raw` 的探测是同一份实现（[`declared_raw_size`]）。
pub fn declared_payload_size(codec: &str, path: &Path, packed: u64) -> Option<u64> {
    let fmt = match codec {
        #[cfg(feature = "gz")]
        "gz" => SingleFmt::Gz,
        #[cfg(feature = "xz")]
        "xz" => SingleFmt::Xz,
        #[cfg(feature = "bz2")]
        "bz2" => SingleFmt::Bz2,
        #[cfg(feature = "zst")]
        "zst" => SingleFmt::Zst,
        _ => return None,
    };
    declared_raw_size(fmt, path, packed)
}

/// 容器自带的原始大小（`None` = 未知/不适用/畸形）。
pub(crate) fn declared_raw_size(fmt: SingleFmt, path: &Path, packed: u64) -> Option<u64> {
    match fmt {
        #[cfg(feature = "gz")]
        SingleFmt::Gz => gz_declared_size(path, packed),
        #[cfg(feature = "xz")]
        SingleFmt::Xz => xz_declared_size(path, packed),
        // zst 的大小在**帧头**（流式解析，见 reader::parse_zstd_head）；bz2 无该字段
        _ => None,
    }
}

/// gz：尾部 4 B `ISIZE`（= 原始大小 mod 2³²；`gzip -l` 同口径）。
#[cfg(feature = "gz")]
fn gz_declared_size(path: &Path, packed: u64) -> Option<u64> {
    // 最小成员 = 10 B 头 + 2 B 空 deflate + 8 B 尾
    if packed < 20 {
        return None;
    }
    let mut f = File::open(path).ok()?;
    f.seek(SeekFrom::End(-4)).ok()?;
    let mut t = [0u8; 4];
    f.read_exact(&mut t).ok()?;
    Some(u64::from(u32::from_le_bytes(t)))
}

/// xz：尾部索引里所有记录的 `uncompressed_size` 之和。
///
/// 结构（XZ 格式规范）：`Stream Footer`(12 B) = CRC32(4) + Backward Size(4 LE) +
/// Stream Flags(2) + `"YZ"`(2)；其中 `Backward Size = 索引域字节数 / 4 − 1`，
/// 索引域 = `0x00` 指示符 + VLI 记录数 + 记录（各 2 个 VLI）+ 4 B 对齐填充 + CRC32。
#[cfg(feature = "xz")]
fn xz_declared_size(path: &Path, packed: u64) -> Option<u64> {
    const FOOTER: u64 = 12;
    const STREAM_HEADER: u64 = 12;
    if packed < FOOTER + STREAM_HEADER {
        return None;
    }
    let mut f = File::open(path).ok()?;
    let mut foot = [0u8; 12];
    f.seek(SeekFrom::End(-(FOOTER as i64))).ok()?;
    f.read_exact(&mut foot).ok()?;
    if &foot[10..12] != b"YZ" {
        return None; // 尾部魔数不符（非 xz 或被截断）
    }
    let backward = u64::from(u32::from_le_bytes([foot[4], foot[5], foot[6], foot[7]]));
    let index_size = backward.checked_add(1)?.checked_mul(4)?;
    if index_size == 0 || index_size > MAX_INDEX_BYTES {
        return None;
    }
    let index_start = packed.checked_sub(FOOTER)?.checked_sub(index_size)?;
    if index_start < STREAM_HEADER {
        return None; // 索引越过头 → 不可信
    }
    f.seek(SeekFrom::Start(index_start)).ok()?;
    let mut buf = vec![0u8; index_size as usize];
    f.read_exact(&mut buf).ok()?;
    if buf[0] != 0x00 {
        return None; // 索引指示符
    }
    let mut pos = 1usize;
    let count = read_vli(&buf, &mut pos)?;
    if count > MAX_INDEX_RECORDS {
        return None;
    }
    let mut total: u64 = 0;
    for _ in 0..count {
        let _unpadded = read_vli(&buf, &mut pos)?;
        let uncompressed = read_vli(&buf, &mut pos)?;
        total = total.checked_add(uncompressed)?;
    }
    Some(total)
}

/// 读一个 XZ VLI（变长整数：最多 9 字节，低位在前，末字节最高位为续位）。
fn read_vli(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    for _ in 0..9 {
        let b = *buf.get(*pos)?;
        *pos += 1;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
    None // 超过 9 字节 → 畸形
}

#[cfg(test)]
#[path = "tests/declared_size.rs"]
mod tests;
