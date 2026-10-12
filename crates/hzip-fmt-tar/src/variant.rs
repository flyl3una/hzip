//! 变体判定 + pax/GNU 扩展头解析与覆盖 + io→HzipError 映射（02 §8.4、T22 §3.1/§3.2）。
//!
//! 覆盖规则（02 §8.4 / T22 §3.2 表）：
//!
//! | pax 记录 | 覆盖目标 | 备注 |
//! |---|---|---|
//! | `path` | ustar name+prefix 拼接 | 完整替换（tar crate `entry.path()` 已处理，本层不再重复） |
//! | `linkpath` | ustar linkname | 同上（`entry.link_name()` 已处理） |
//! | `mtime` | ustar mtime | 同上（tar crate 解析 header.mtime() 即 pax 覆盖后的值） |
//! | `size` | ustar size | >8GiB 条目经 pax 声明；tar crate 已按 pax size 裁剪数据流 |
//! | `GNU.sparse.*` | 稀疏区间表 | 见 [`crate::sparse`]；1.0 pax 变体本层兜底 |
//! | 其他未知 | — | **忽略不报错**（POSIX 前向兼容，T22 §3.2 末行） |
//!
//! GNU `L`/`K` 长名头与 pax `path`/`linkpath` 语义等价（二选一出现），tar crate
//! 均已透明合并（T22 §4.2 要点①）——本层**信任其合并结果**，只兜底：pax
//! `GNU.sparse.*` 1.0 变体（tar crate 只处理旧式 `S` 头内区间表）与 `g` 全局头
//! 的跨条目缓存（POSIX 语义：覆盖后续全部条目，直到被同 key 的 `x` 头覆盖）。

use std::io;

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::FormatId;

use crate::consts::{GNU_MAGIC, TAR_MAGIC_OFFSET, USTAR_MAGIC};

/// tar 变体（02 §8.4 三变体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TarVariant {
    /// pre-POSIX v7（无魔数但 checksum 合法；prefix 字段无意义）。
    V7,
    /// POSIX ustar（`ustar\x00 00`）。
    Ustar,
    /// GNU tar（`ustar  \x00`，version 域为空格+空字符）。
    Gnu,
}

/// 逐字节判定 512B 头块变体（T22 §4.3；魔数陷阱见 `consts` 文档）。
///
/// 返回 None 表示既不认三变体之一（全零块由调用方在「结尾」路径处理）。主头
/// 的 checksum 校验由 tar crate 完成（坏 checksum → map_err 2004）。
pub(crate) fn detect(head: &[u8; 512]) -> Option<TarVariant> {
    let magic = &head[TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + 8];
    if *magic == GNU_MAGIC {
        Some(TarVariant::Gnu)
    } else if *magic == USTAR_MAGIC {
        Some(TarVariant::Ustar)
    } else if head[TAR_MAGIC_OFFSET..].iter().all(|&b| b == 0) {
        // 魔数区全零 = v7 老档（prefix/magic 区无内容）。
        Some(TarVariant::V7)
    } else {
        None
    }
}

/// pax 全局头缓存（`g` 头）：POSIX 语义为覆盖后续全部条目，直到同 key 被
/// 局部 `x` 头覆盖。本层只缓存 path/mtime/linkpath 三键（其余字段 02 不承诺）。
#[derive(Debug, Clone, Default)]
pub(crate) struct PaxGlobal {
    path: Option<String>,
    mtime: Option<f64>,
    linkpath: Option<String>,
}

impl PaxGlobal {
    /// 解析 `g` 头数据并入缓存。
    pub(crate) fn merge(&mut self, data: &[u8]) {
        for line in data.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = parse_record(line) {
                match k {
                    b"path" => self.path = std::str::from_utf8(v).ok().map(str::to_owned),
                    b"mtime" => {
                        self.mtime = std::str::from_utf8(v).ok().and_then(|s| s.parse().ok())
                    }
                    b"linkpath" => self.linkpath = std::str::from_utf8(v).ok().map(str::to_owned),
                    _ => {}
                }
            }
        }
    }

    /// 以全局缓存为基底，生成当前条目覆盖（局部 `x` 值随后 apply 上去覆盖同 key）。
    pub(crate) fn snapshot(&self) -> PaxOverride {
        PaxOverride {
            path: self.path.clone(),
            mtime: self.mtime,
            linkpath: self.linkpath.clone(),
            ..Default::default()
        }
    }
}

/// 一条目生效的 pax 覆盖（局部 `x` 优先，回落全局 `g`）。
#[derive(Debug, Clone, Default)]
pub(crate) struct PaxOverride {
    /// 合并后的 path 覆盖（若非 None 则以全路径替换 ustar name+prefix；仅 g 兜底）。
    pub(crate) path: Option<String>,
    /// 合并后的 mtime 覆盖（unix 秒 + 可选小数；仅 g 兜底，x 由 tar crate 处理）。
    pub(crate) mtime: Option<f64>,
    /// 合并后的 linkpath 覆盖（仅 g 兜底）。
    pub(crate) linkpath: Option<String>,
    /// GNU.sparse 1.0 pax 记录累积的区间（offset,size 成对）。
    pub(crate) sparse_map: Vec<(u64, u64)>,
    /// GNU.sparse.size（1.0 变体逻辑大小；realsize 等价键亦收）。
    pub(crate) sparse_size: Option<u64>,
    /// GNU.sparse.offset 待配对（finish 时校验：遗留 → 坏头）。
    pending_offset: Option<u64>,
}

/// 解析单条 pax 记录行：`"{len} {key}={value}\n"`。
///
/// 遵循 POSIX 解析：`len` 是整行（含换行）字节数；宽容处理行尾已剥/未剥 `\n`
/// 的情形（`declared == line.len()` 或 `== line.len()+1`），并在值侧剥尾 `\n`。
/// 畸形行返回 None（调用方忽略——前向兼容优先，T22 §3.2）。
pub(crate) fn parse_record(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let sp = line.iter().position(|&b| b == b' ')?;
    let len_field = std::str::from_utf8(&line[..sp]).ok()?;
    let declared: usize = len_field.parse().ok()?;
    if declared != line.len() && declared != line.len() + 1 {
        return None;
    }
    let kv = &line[sp + 1..];
    let eq = kv.iter().position(|&b| b == b'=')?;
    let mut value = &kv[eq + 1..];
    if value.last() == Some(&b'\n') {
        value = &value[..value.len() - 1];
    }
    Some((&kv[..eq], value))
}

/// 十进制 u64（pax 数字值解析）。
fn dec(value: &[u8]) -> Option<u64> {
    std::str::from_utf8(value)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
}

impl PaxOverride {
    /// 把一条解析出的 (key, value) 应用进覆盖集。
    ///
    /// - `path` / `linkpath`：以 UTF-8 解码保存（非 UTF-8 → 忽略该条，保持既有值）。
    /// - `mtime`：解析为 f64（可带小数秒；失败忽略）。
    /// - `GNU.sparse.*`：见 [`Self::apply_gnu_sparse`]（半套由 [`Self::finish_sparse`] 报坏头）。
    /// - 其余：**忽略**（POSIX 前向兼容）。
    pub(crate) fn apply(&mut self, key: &[u8], value: &[u8]) {
        match key {
            b"path" => {
                if let Ok(s) = std::str::from_utf8(value) {
                    self.path = Some(s.to_owned());
                }
            }
            b"linkpath" => {
                if let Ok(s) = std::str::from_utf8(value) {
                    self.linkpath = Some(s.to_owned());
                }
            }
            b"mtime" => {
                if let Ok(s) = std::str::from_utf8(value)
                    && let Ok(f) = s.parse::<f64>()
                {
                    self.mtime = Some(f);
                }
            }
            k if k.starts_with(b"GNU.sparse.") => self.apply_gnu_sparse(k, value),
            _ => {}
        }
    }

    fn apply_gnu_sparse(&mut self, key: &[u8], value: &[u8]) {
        match key {
            b"GNU.sparse.size" | b"GNU.sparse.realsize" => self.sparse_size = dec(value),
            b"GNU.sparse.offset" => self.pending_offset = dec(value),
            b"GNU.sparse.numbytes" => {
                if let Some(off) = self.pending_offset.take()
                    && let Some(sz) = dec(value)
                {
                    self.sparse_map.push((off, sz));
                }
                // 有 numbytes 无 offset 的半套：静默丢弃待 finish 报错由调用方兜底
            }
            b"GNU.sparse.map" => {
                // "offset,numbytes offset,numbytes ..."（旧式 pax map；成对）
                if let Ok(s) = std::str::from_utf8(value) {
                    let mut it = s.split_whitespace();
                    while let (Some(a), Some(b)) = (it.next(), it.next()) {
                        if let (Ok(off), Ok(sz)) =
                            (a.trim().parse::<u64>(), b.trim().parse::<u64>())
                        {
                            self.sparse_map.push((off, sz));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// 校验 GNU.sparse 记录完整性：若出现「有 offset 无 size」的半套记录，视为坏头
    /// （T22 §7-3：`GNU.sparse.*` 半套 → Err 2004，不得静默当普通文件）。
    pub(crate) fn finish_sparse(&mut self) -> Result<(), ()> {
        if self.pending_offset.is_some() {
            return Err(());
        }
        Ok(())
    }

    /// 是否带 GNU.sparse 1.0 区间信息。
    pub(crate) fn is_sparse(&self) -> bool {
        !self.sparse_map.is_empty() || self.sparse_size.is_some()
    }
}

/// io::Error → HzipError 映射（T22 §4.2 要点②统一入口）。
///
/// | io 错误 | HzipError | 码 |
/// |---|---|---|
/// | `UnexpectedEof` | `Format{Truncated}` | 2002 |
/// | 坏 checksum / 坏头 / 解析失败 | `Format{BadHeader}` | 2004 |
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
        io::ErrorKind::InvalidData | io::ErrorKind::Other => {
            // 缺省：结构性问题一律按坏头（TC-CONF-008 fail-fast 优先于 salvage，
            // T22 §7-7；无目录 zip 的 salvage 语义在 zip 侧，tar 无此场景）。
            HzipError::Format {
                format,
                kind: FormatErrorKind::BadHeader,
            }
        }
        _ => HzipError::Io {
            source: e,
            path: None,
        },
    }
}

#[cfg(test)]
#[path = "tests/variant.rs"]
mod tests;
