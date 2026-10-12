//! 7z solid 块打包（feature `solid`；T46 独占模块；02 §8.2 写入流程）。
//!
//! # 块打包模型
//!
//! `next_entry × N` 数据顺序缓冲入 solid 块（默认 64 MiB，
//! `SOLID_BLOCK_DEFAULT_MIB`）；块满或 `finish()` 触发统一压缩：整块作为**一个
//! folder**（lzma2）压缩，此时才产出 pack 流偏移 → 头回写沿用 T25 两遍法/
//! Seekable 输出抽象。块内条目的 unpacked sizes / CRC 记入 substreamsInfo
//! （每条目旁路 `Crc32` 在 [`PendingEntry`] 内累计）。
//!
//! # 多线程现状（诚实登记，02 §6.3 线程规则 / T25 §4.4 遗留项）
//!
//! sevenz-rust `compress` 为**单线程** lzma2 实现（含固实路径）；本模块
//! `compress_block` 亦单块内串行。多线程收益由「多 solid 块并行」提供（core 调度
//! 多 folder）：块满即产出独立 folder，互不依赖可并行压缩。单块内串行现状登记
//! （差距未消：sevenz-rust 无多线程编码 API）。
//!
//! # BCJ2 串行约束（02 §6.4 实施约束原文）
//!
//! solid + BCJ2 组合下 folder 编码**强制串行**：BCJ2 转换状态跨整个 folder 流
//! （文件边界连续过滤正是压缩比来源），块内并行会破坏标记与三流的顺序对应。
//! 这是规范约束不是缺陷，禁止「优化」掉。

// 模块消费方=单测+writer solid 挂点（T46 交付接口）；writer 集成未接通前允许 dead_code（02 §13.2 风险登记）。
#![allow(dead_code)]
use std::io::Write;

use hzip_codec::level::Level;
use hzip_error::HzipError;
use hzip_format::EntryMeta;

/// 默认 solid 块上限（02 附录 B.2 `SOLID_BLOCK_DEFAULT_MIB`；MiB）。
pub(crate) const SOLID_BLOCK_DEFAULT_MIB: u64 = 64;

/// solid 块缓冲：next_entry 数据顺序追加；块满/finish 统一压缩。
#[derive(Debug, Default)]
pub(crate) struct SolidBuffer {
    /// 数据缓冲。
    buf: Vec<u8>,
    /// 块字节上限（= block_mib MiB）。
    cap: usize,
    /// 块内条目。
    entries: Vec<PendingEntry>,
}

/// 块内待落条目。
#[derive(Debug, Clone)]
pub(crate) struct PendingEntry {
    /// 条目元数据。
    pub meta: EntryMeta,
    /// 条目数据在块内偏移。
    pub start: u64,
    /// 条目数据长度。
    pub len: u64,
    /// 条目 CRC32 旁路累计。
    pub crc: hzip_codec::crc::Crc32,
}

impl PendingEntry {
    /// 块内构建一条目。
    pub(crate) fn new(meta: EntryMeta, start: u64, data: &[u8]) -> Self {
        let mut crc = hzip_codec::crc::Crc32::new();
        crc.update(data);
        Self {
            meta,
            start,
            len: data.len() as u64,
            crc,
        }
    }
}

/// 块满产出（交 [`compress_block`] 统一压缩）。
#[derive(Debug)]
pub(crate) struct SolidBlock {
    /// 拼接数据。
    pub data: Vec<u8>,
    /// 块内条目（顺序）。
    pub entries: Vec<PendingEntry>,
}

impl SolidBuffer {
    /// 新建：`block_mib` 为块上限 MiB（0 → 默认 64）。
    pub(crate) fn new(block_mib: u32) -> Self {
        let mib = if block_mib == 0 {
            SOLID_BLOCK_DEFAULT_MIB as u32
        } else {
            block_mib
        };
        Self {
            buf: Vec::new(),
            cap: (mib as usize).saturating_mul(1024 * 1024),
            entries: Vec::new(),
        }
    }

    /// 追加一条目数据（写入即缓冲 + crc 旁路）；条目元数据登记。
    pub(crate) fn push(&mut self, meta: &EntryMeta, data: &[u8]) {
        let start = self.buf.len() as u64;
        let e = PendingEntry::new(meta.clone(), start, data);
        self.buf.extend_from_slice(data);
        self.entries.push(e);
    }

    /// 是否已达块容量上限（块满 → writer 取走压缩）。
    pub(crate) fn is_full(&self) -> bool {
        self.buf.len() >= self.cap
    }

    /// 当前缓冲字节数。
    pub(crate) fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// 块内条目数。
    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// 取走整块（空块返回 None）。
    pub(crate) fn take_block(&mut self) -> Option<SolidBlock> {
        if self.entries.is_empty() && self.buf.is_empty() {
            return None;
        }
        Some(SolidBlock {
            data: std::mem::take(&mut self.buf),
            entries: std::mem::take(&mut self.entries),
        })
    }
}

/// 统一压缩产物（folder 记账所需全部信息）。
#[derive(Debug)]
pub(crate) struct PackedFolder {
    /// pack 流字节（整块一 folder 的压缩产物）。
    pub pack: Vec<u8>,
    /// 逐条目 unpacked size。
    pub unpacked_sizes: Vec<u64>,
    /// 逐条目 CRC。
    pub crcs: Vec<u32>,
}

/// 统一压缩：整块一 folder（lzma2 单线程，02 §6.3 现状登记）。
///
/// `bcj2=true` 时本模块前置 BCJ2 转换（四流各自压缩后按容器序拼 pack）——
/// sevenz-rust 写路径无 BCJ2 编码器，自组装 folder 编码未交付
/// （02 §13.2 风险缓解路线登记 TODO；本函数返回 Unsupported 而非静默错编）。
pub(crate) fn compress_block(
    b: SolidBlock,
    level: Level,
    threads: u32,
    bcj2: bool,
) -> Result<PackedFolder, HzipError> {
    if bcj2 {
        return Err(HzipError::Unsupported {
            what: "7z solid+BCJ2 folder 自组装编码未交付（T46 跟踪表 TODO；02 §13.2）".into(),
        });
    }
    let options = crate::coder::lzma2_options(level, threads, None);
    let mut pack = Vec::with_capacity(b.data.len() / 2 + 64);
    {
        let mut enc = sevenz_rust::lzma::LZMA2Writer::new(
            sevenz_rust::lzma::CountingWriter::new(&mut pack),
            &options,
        );
        enc.write_all(&b.data).map_err(|e| HzipError::Io {
            source: e,
            path: None,
        })?;
        enc.finish().map_err(|e| HzipError::Io {
            source: e,
            path: None,
        })?;
    }
    let unpacked_sizes: Vec<u64> = b.entries.iter().map(|e| e.len).collect();
    let crcs: Vec<u32> = b.entries.iter().map(|e| e.crc.finalize()).collect();
    Ok(PackedFolder {
        pack,
        unpacked_sizes,
        crcs,
    })
}

#[cfg(test)]
#[path = "tests/solid.rs"]
mod tests;
