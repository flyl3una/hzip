//! cpio 变体判定、头字段解析与 io→HzipError 映射（02 §8.7）。
//!
//! # 四变体（GNU cpio 口径）
//!
//! | 变体 | 魔数 | 头长 | 字段编码 | 对齐 |
//! |---|---|---|---|---|
//! | [`CpioVariant::Newc`] | ASCII `070701` | 110 | 8 位十六进制 ×13 | 4 |
//! | [`CpioVariant::Crc`] | ASCII `070702` | 110 | 同上（`check` = 数据字节和） | 4 |
//! | [`CpioVariant::Odc`] | ASCII `070707` | 76 | 6/11 位八进制 | 无 |
//! | [`CpioVariant::BinLe`] / [`BinBe`](CpioVariant::BinBe) | `0x71C7`（LE/BE） | 26 | 13 个 16 位字 | 2 |
//!
//! 判定必须**长魔数优先**：`070707`（odc）是 `070701`/`070702`（newc/crc）的
//! 前缀，故 ASCII 变体先于二进制变体、且 `070701`/`070702` 先于 `070707` 逐个
//! 字节对照——禁止 `starts_with("0707")` 之类混判。
//!
//! # 头字段布局（三种字符编码变体）
//!
//! | 变体 | 字段序列 |
//! |---|---|
//! | newc/crc | magic, ino, mode, uid, gid, nlink, mtime, filesize, devmajor, devminor, rdevmajor, rdevminor, namesize, check |
//! | odc | magic, dev, ino, mode, uid, gid, nlink, rdev, mtime, namesize, filesize |
//! | bin | magic, dev, ino, mode, uid, gid, nlink, rdev, mtime[hi,lo], namesize, filesize[hi,lo] |
//!
//! bin 的 `mtime`/`filesize` 各占两个 16 位字且**高位字在前**（GNU cpio
//! `read_in_binary` 语义：`mtime = w[0] << 16 | w[1]`）；字序由魔数形态决定
//! （`c7 71` = 小端主机产物，`71 c7` = 大端主机产物），本层按魔数选定读法，
//! 不做跨端字节交换（旧二进制 cpio 仅存于同端老档，跨端样本按坏头拒绝更诚实）。

use std::io;

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::FormatId;

use crate::consts::{
    BIN_ALIGN, BIN_HEADER_BYTES, BIN_MAGIC_BE, BIN_MAGIC_LE, CRC_MAGIC, NEWC_ALIGN,
    NEWC_HEADER_BYTES, NEWC_MAGIC, ODC_ALIGN, ODC_HEADER_BYTES, ODC_MAGIC,
};

/// cpio 变体（判定结果即 reader 的解析口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CpioVariant {
    /// SVR4 newc（`070701`）。
    Newc,
    /// SVR4 crc（`070702`；`check` 域为数据区字节和）。
    Crc,
    /// 旧可移植 ASCII odc（`070707`）。
    Odc,
    /// 旧二进制，小端字序。
    BinLe,
    /// 旧二进制，大端字序。
    BinBe,
}

impl CpioVariant {
    /// 头字节数。
    pub(crate) fn header_bytes(self) -> usize {
        match self {
            Self::Newc | Self::Crc => NEWC_HEADER_BYTES,
            Self::Odc => ODC_HEADER_BYTES,
            Self::BinLe | Self::BinBe => BIN_HEADER_BYTES,
        }
    }

    /// 对齐粒度（头+名字与数据区共用）。
    pub(crate) fn align(self) -> usize {
        match self {
            Self::Newc | Self::Crc => NEWC_ALIGN,
            Self::BinLe | Self::BinBe => BIN_ALIGN,
            Self::Odc => ODC_ALIGN,
        }
    }

    /// 是否 16 位字按小端读（仅二进制变体有意义）。
    fn little_endian(self) -> bool {
        matches!(self, Self::BinLe)
    }

    /// 诊断/日志用短名。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Newc => "newc",
            Self::Crc => "crc",
            Self::Odc => "odc",
            Self::BinLe => "bin-le",
            Self::BinBe => "bin-be",
        }
    }
}

/// 逐字节判定头块变体（长魔数优先）。
///
/// 返回 `None` = 既不认四种变体之一（全零尾块由调用方在「归档结尾」路径处理）。
pub(crate) fn detect(head: &[u8]) -> Option<CpioVariant> {
    if head.len() >= 6 {
        // 长魔数优先：070703/070702 不得被 070707 抢走
        if &head[..6] == NEWC_MAGIC {
            return Some(CpioVariant::Newc);
        }
        if &head[..6] == CRC_MAGIC {
            return Some(CpioVariant::Crc);
        }
        if &head[..6] == ODC_MAGIC {
            return Some(CpioVariant::Odc);
        }
    }
    if head.len() >= 2 {
        if head[..2] == BIN_MAGIC_LE {
            return Some(CpioVariant::BinLe);
        }
        if head[..2] == BIN_MAGIC_BE {
            return Some(CpioVariant::BinBe);
        }
    }
    None
}

/// 解析后的头字段（只保留条目语义需要的域；dev/rdev/uid/gid 本层不消费）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawHeader {
    /// 权限位 + 文件类型（`mode & S_IFMT` 判定目录/链接）。
    pub(crate) mode: u32,
    /// 修改时间（unix 秒）。
    pub(crate) mtime: u64,
    /// 数据区字节数。
    pub(crate) filesize: u64,
    /// 名字字节数（newc/crc/odc **含**尾 NUL；bin 亦然）。
    pub(crate) namesize: u64,
    /// crc 变体的数据字节和（其余变体恒 0）。
    pub(crate) check: u32,
}

/// 8 位十六进制域（newc/crc）。
fn hex8(field: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(field).ok()?;
    u32::from_str_radix(s, 16).ok()
}

/// 定长八进制域（odc）。
fn oct(field: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(field).ok()?;
    u64::from_str_radix(s, 8).ok()
}

/// 16 位字（bin；按变体选定读法）。
fn word16(buf: &[u8], off: usize, little: bool) -> Option<u16> {
    let b = buf.get(off..off + 2)?;
    let pair = [b[0], b[1]];
    Some(if little {
        u16::from_le_bytes(pair)
    } else {
        u16::from_be_bytes(pair)
    })
}

/// 解析头块字段（长度不足/域非法 → `None`，调用方转 2004 坏头）。
pub(crate) fn parse_header(variant: CpioVariant, buf: &[u8]) -> Option<RawHeader> {
    match variant {
        CpioVariant::Newc | CpioVariant::Crc => {
            if buf.len() < NEWC_HEADER_BYTES {
                return None;
            }
            // 域自 magic 之后依次排列：ino(0) mode(1) uid(2) gid(3) nlink(4)
            // mtime(5) filesize(6) devmajor(7) devminor(8) rdevmajor(9)
            // rdevminor(10) namesize(11) check(12)
            let f = |i: usize| hex8(&buf[6 + i * 8..6 + i * 8 + 8]);
            Some(RawHeader {
                mode: f(1)?,
                mtime: u64::from(f(5)?),
                filesize: u64::from(f(6)?),
                namesize: u64::from(f(11)?),
                check: if variant == CpioVariant::Crc {
                    f(12)?
                } else {
                    0
                },
            })
        }
        CpioVariant::Odc => {
            if buf.len() < ODC_HEADER_BYTES {
                return None;
            }
            // dev(6) ino(12) mode(18) uid(24) gid(30) nlink(36) rdev(42)
            // mtime(48..59) namesize(59..65) filesize(65..76)
            let mode = oct(&buf[18..24])? as u32;
            let mtime = oct(&buf[48..59])?;
            let namesize = oct(&buf[59..65])?;
            let filesize = oct(&buf[65..76])?;
            Some(RawHeader {
                mode,
                mtime,
                filesize,
                namesize,
                check: 0,
            })
        }
        CpioVariant::BinLe | CpioVariant::BinBe => {
            if buf.len() < BIN_HEADER_BYTES {
                return None;
            }
            let le = variant.little_endian();
            // 字序：magic(0) dev(1) ino(2) mode(3) uid(4) gid(5) nlink(6)
            // rdev(7) mtime[hi,lo](8,9) namesize(10) filesize[hi,lo](11,12)
            let mode = u32::from(word16(buf, 3 * 2, le)?);
            let mtime =
                (u64::from(word16(buf, 8 * 2, le)?) << 16) | u64::from(word16(buf, 9 * 2, le)?);
            let namesize = u64::from(word16(buf, 10 * 2, le)?);
            let filesize =
                (u64::from(word16(buf, 11 * 2, le)?) << 16) | u64::from(word16(buf, 12 * 2, le)?);
            Some(RawHeader {
                mode,
                mtime,
                filesize,
                namesize,
                check: 0,
            })
        }
    }
}

/// 向上对齐（`a` 为 1 时恒等；溢出 → `None`，调用方按坏头处理）。
pub(crate) fn align_up(value: u64, align: usize) -> Option<u64> {
    let a = align as u64;
    if a <= 1 {
        return Some(value);
    }
    value.checked_add(a - 1).map(|v| v / a * a)
}

/// io::Error → HzipError 映射（与 tar 同口径）。
///
/// | io 错误 | HzipError | 码 |
/// |---|---|---|
/// | `UnexpectedEof`（头/名字/数据区被截断） | `Format{Truncated}` | 2002 |
/// | 坏头 / 域非法 / 变体不一致 | `Format{BadHeader}` | 2004 |
/// | 坏魔数（`InvalidInput`） | `Format{BadSignature}` | 2001 |
/// | 其余（权限/磁盘等真实 IO） | `Io{source}` | 1xxx |
pub(crate) fn map_err(e: io::Error, format: FormatId) -> HzipError {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => HzipError::Format {
            format,
            kind: FormatErrorKind::Truncated,
        },
        io::ErrorKind::InvalidInput => HzipError::Format {
            format,
            kind: FormatErrorKind::BadSignature,
        },
        io::ErrorKind::InvalidData | io::ErrorKind::Other => HzipError::Format {
            format,
            kind: FormatErrorKind::BadHeader,
        },
        _ => HzipError::Io {
            source: e,
            path: None,
        },
    }
}

#[cfg(test)]
#[path = "tests/variant.rs"]
mod tests;
