//! 7z 读取：头解析（sevenz-rust `Archive::read`）+ folder 物化缓存 + 卷装配。
//!
//! # 设计（红线 1/2/3/4，02 §7.3 四条契约红线）
//!
//! - `entries()`：以 `sevenz_rust::Archive::read` 一次解析**头部结构**（签名头 +
//!   nextHeader 区，位于档案尾部），把 streamsInfo/foldersInfo/substreamsInfo/文件
//!   名/属性表读入内存（`Archive`）；**不触碰数据区**（TC-CONF-010 大档案秒开，
//!   与数据区大小无关）。
//! - `read_entry(id)`：经 `archive.stream_map.file_folder_index[id]` 定位 folder →
//!   该 folder 用 `sevenz_rust::BlockDecoder` 整块解码一次，物化到 [`FolderCache`]。
//!   **落点在解码之前就定**：按头声明的 folder 解出总量
//!   （[`folder_unpack_totals`]，打开期算一次缓存于 [`SevenZipReader`]）与
//!   [`crate::consts::FOLDER_CACHE_MEM_MAX`] 比较——
//!   ≤ 留内存，超限则解码时**直接写**临时文件（[`SpillSink`]）。故 RSS 峰值与
//!   固实块大小**无关**（旧实现是「先攒 `Vec`、攒完才判落盘」⇒ 峰值 = 整块大小；
//!   400 MiB 固实块取单条目实测 449 MiB，2026-09-29 修正）。物化后按 folder 内
//!   记录的文件偏移切出条目 `Read`（乱序/重复读取均命中所属 folder，红线 2；
//!   多文件 solid folder 亦一次物化）。目录/空文件不触碰解码路径。
//!   [`FolderCache`] 为 **LRU**（容量 [`FolderCache::MAX_MATERIALIZED`]）：物化结果
//!   不淘汰时内存随档案解出总量线性增长（20 folder × 50 MiB 实测「解压全部」
//!   1018 MiB → 167 MiB）。顺序访问（extract/test 的条目序）零重复解码。
//!   [`FolderCache`] 为 **LRU**（容量 [`FolderCache::MAX_MATERIALIZED`]）：物化结果
//!   不淘汰时内存随档案解出总量线性增长（20 folder × 50 MiB 实测「解压全部」
//!   1018 MiB → 167 MiB）。顺序访问（extract/test 的条目序）零重复解码。
//! - **取消**：`extract`/`test` 物化时每
//!   [`crate::consts::MATERIALIZE_CHECKPOINT_BYTES`] 检查一次 cancel（02 §4.2）。
//!   `read_entry` 无 ctx，单条目物化粒度 = 条目/folder 边界（core 在条目间轮询）。
//! - **CRC**：sevenz-rust 0.6.1 的 `Crc32VerifyingReader` 在「剩余字节归零的那次
//!   read」上抛错且**丢弃末块**，不暴露实际值。本实现在物化拷贝循环自行累计
//!   CRC，EOF 与头 `entry.crc` 比对（期望值来自 subStreamsInfo）→ 不符
//!   `Checksum` HZ-3001；解码中途 io 错误 → `Format` 2002/2004。
//! - **卷装配**：`Source::Volumes` 以 [`VolumeFile`] 字节按序拼接（首卷持签名头，
//!   数据跨卷连续 → 字节拼接 = 完整档案；TC-X-005 读半场）。缺失卷在打开即检 →
//!   `Format{MissingVolume}` HZ-2005。
//! - **Stream/Stdin 源**：spool 到临时文件后按 File 打开（7z 解析与 folder seek
//!   均需随机访问；`TempArea` 随 reader drop 清理）。
//! - **头加密分支**：本基础版不实现 AES 头解密（T46）。以 `Password::empty()`
//!   打开命中头加密 → sevenz-rust `PasswordRequired` → `Password{Header}`
//!   HZ-3102；**零条目名泄露**（构造 reader 阶段即返回错误）。
//!
//! 出处标注：02 §8.2 头解析流程（A~I 分支由 sevenz-rust 承担 A/B/E/F/G/H）、
//! 02 §7.3 契约、T25 §4.3/§4.5。

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::{
    ArchiveReader, EntryAttrs, EntryInfo, ExtractCtx, ExtractReport, FormatCaps, FormatId,
    NameEncoding, PasswordRef, Selection, parse_entry_name,
};
use hzip_progress::CancellationToken;
use time::OffsetDateTime;

use crate::consts::{FOLDER_CACHE_MEM_MAX, MATERIALIZE_CHECKPOINT_BYTES};
use crate::handler::caps;

/// 打开源：统一得到「可 seek 句柄 + 长度」；File 源直接用，卷源拼接，流 spool。
fn open_source(
    src: hzip_format::Source,
    format: &FormatId,
) -> Result<(SourceHandle, Option<crate::TempArea>), HzipError> {
    match src {
        hzip_format::Source::File(p) => {
            let f = std::fs::File::open(&p).map_err(|e| HzipError::Io {
                source: e,
                path: Some(p.clone()),
            })?;
            let len = f.metadata().map(|m| m.len()).map_err(|e| HzipError::Io {
                source: e,
                path: Some(p.clone()),
            })?;
            Ok((SourceHandle::new(Box::new(f), len), None))
        }
        hzip_format::Source::Volumes { first, rest } => {
            let mut files = vec![first];
            files.extend(rest);
            for f in &files {
                if !f.exists() {
                    return Err(HzipError::Format {
                        format: format.clone(),
                        kind: FormatErrorKind::MissingVolume(f.display().to_string()),
                    });
                }
            }
            let vol = VolumeFile::open(files).map_err(|e| HzipError::Io {
                source: e,
                path: None,
            })?;
            let len = vol.total;
            Ok((SourceHandle::new(Box::new(vol), len), None))
        }
        hzip_format::Source::Stdin => {
            let area = crate::TempArea::new("stdin")?;
            let path = area.join("input.7z");
            let mut out = std::fs::File::create(&path).map_err(HzipError::from)?;
            io::copy(&mut io::stdin().lock(), &mut out).map_err(HzipError::from)?;
            drop(out);
            let f = std::fs::File::open(&path).map_err(HzipError::from)?;
            let len = f.metadata().map(|m| m.len()).map_err(HzipError::from)?;
            Ok((SourceHandle::new(Box::new(f), len), Some(area)))
        }
        hzip_format::Source::Stream(mut s) => {
            let area = crate::TempArea::new("stream")?;
            let path = area.join("input.7z");
            let mut out = std::fs::File::create(&path).map_err(HzipError::from)?;
            io::copy(&mut s, &mut out).map_err(HzipError::from)?;
            drop(out);
            let f = std::fs::File::open(&path).map_err(HzipError::from)?;
            let len = f.metadata().map(|m| m.len()).map_err(HzipError::from)?;
            Ok((SourceHandle::new(Box::new(f), len), Some(area)))
        }
    }
}

/// 可 seek 源句柄。
pub(crate) struct SourceHandle {
    inner: Box<dyn SourceIo>,
    len: u64,
}

/// `Read + Seek + Send` 组合 trait（trait object 约束）。
pub(crate) trait SourceIo: Read + Seek + Send {}
impl<T: Read + Seek + Send> SourceIo for T {}

impl SourceHandle {
    fn new(inner: Box<dyn SourceIo>, len: u64) -> Self {
        Self { inner, len }
    }
}

/// 卷拼接视图：`Read + Seek` 串接多个卷字节。
pub(crate) struct VolumeFile {
    files: Vec<PathBuf>,
    cur: usize,
    cur_file: Option<std::fs::File>,
    pos: u64,
    lens: Vec<u64>,
    total: u64,
}

impl VolumeFile {
    fn open(files: Vec<PathBuf>) -> io::Result<Self> {
        let mut lens = Vec::with_capacity(files.len());
        let mut total = 0u64;
        for p in &files {
            let len = std::fs::metadata(p)?.len();
            total += len;
            lens.push(len);
        }
        let mut v = Self {
            files,
            cur: 0,
            cur_file: None,
            pos: 0,
            lens,
            total,
        };
        if !v.files.is_empty() {
            v.cur_file = Some(std::fs::File::open(&v.files[0])?);
        }
        Ok(v)
    }
}

impl Read for VolumeFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.cur >= self.files.len() {
                return Ok(0);
            }
            if self.cur_file.is_none() {
                self.cur_file = Some(std::fs::File::open(&self.files[self.cur])?);
            }
            let f = match self.cur_file.as_mut() {
                Some(f) => f,
                None => return Ok(0),
            };
            let n = f.read(buf)?;
            if n > 0 {
                self.pos += n as u64;
                return Ok(n);
            }
            self.cur_file = None;
            self.cur += 1;
        }
    }
}

impl Seek for VolumeFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_abs = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => self.total as i64 + d,
        };
        if new_abs < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        let target = new_abs as u64;
        let mut acc = 0u64;
        let mut idx = self.lens.len();
        let mut in_vol = 0u64;
        for (i, len) in self.lens.iter().enumerate() {
            if target < acc + len {
                idx = i;
                in_vol = target - acc;
                break;
            }
            acc += len;
        }
        self.cur = idx;
        if idx < self.files.len() {
            self.cur_file = Some(std::fs::File::open(&self.files[idx])?);
            if let Some(f) = self.cur_file.as_mut() {
                f.seek(SeekFrom::Start(in_vol))?;
            }
        } else {
            self.cur_file = None;
        }
        self.pos = target;
        Ok(target)
    }
}

/// 单个物化 folder 的记录。
struct Materialized {
    /// 全局文件 id → (folder 输出内字节偏移, 长度)。目录/空文件不在此表。
    slices: HashMap<usize, (u64, u64)>,
    data: FolderData,
}

enum FolderData {
    Mem(Arc<Vec<u8>>),
    Tmp(PathBuf),
}

/// folder 物化缓存（key = folder 序号；LRU，容量 [`FolderCache::MAX_MATERIALIZED`]）。
///
/// **必须有淘汰**：物化结果是整块解码产物，只增不减地缓存会让内存随「档案解出
/// 总量」线性增长——20 folder × 50 MiB（都 ≤ 阈值 ⇒ 全走内存）实测「解压全部」
/// 峰值 1018 MiB（2026-09-29）。淘汰即 `Mem` 释放 / `Tmp` 删临时文件（在飞的
/// 读取器自带 `Arc`/文件句柄，不受影响，顶多多解一次）。
pub(crate) struct FolderCache {
    folders: HashMap<usize, Materialized>,
    /// 最近使用序（尾 = 最新）；容量 = `MAX_MATERIALIZED`。
    order: std::collections::VecDeque<usize>,
    tmp_area: crate::TempArea,
    format: FormatId,
}

impl FolderCache {
    /// 同时保留的物化 folder 数。core 的 extract/test 按条目序推进（folder 成员
    /// 在 7z 里连续），顺序访问下取 2 即可零重复解码；乱序访问退化为重解（正确性
    /// 不受影响——红线 2 只承诺「独立可用」，不承诺「不重解码」）。
    const MAX_MATERIALIZED: usize = 2;

    fn new(format: FormatId) -> Self {
        Self {
            folders: HashMap::new(),
            order: std::collections::VecDeque::new(),
            tmp_area: crate::TempArea::new("cache").unwrap_or_else(|_| crate::TempArea {
                dir: std::env::temp_dir(),
            }),
            format,
        }
    }

    /// 命中即算「最近使用」（`&mut`：推进 LRU 序）。
    fn has(&mut self, folder: usize) -> bool {
        if !self.folders.contains_key(&folder) {
            return false;
        }
        self.touch(folder);
        true
    }

    /// 把 folder 移到「最近使用」位。
    fn touch(&mut self, folder: usize) {
        if let Some(pos) = self.order.iter().position(|&f| f == folder) {
            self.order.remove(pos);
        }
        self.order.push_back(folder);
    }

    /// 插入物化结果并淘汰超容量的最旧项（`Tmp` 顺手删临时文件）。
    fn insert_lru(&mut self, folder: usize, m: Materialized) {
        self.touch(folder);
        self.folders.insert(folder, m);
        while self.order.len() > Self::MAX_MATERIALIZED {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.folders.remove(&old)
                && let FolderData::Tmp(path) = evicted.data
            {
                // 在飞的读取器可能还开着该文件（Windows 删不掉 → 留给 TempArea 收尾）
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// 取某 folder 中全局文件 id 的字节切片读取器（无流/目录/空文件 → None）。
    fn slice_reader(
        &self,
        folder: usize,
        file_id: usize,
    ) -> Result<Option<Box<dyn Read + Send>>, HzipError> {
        let m = self
            .folders
            .get(&folder)
            .ok_or_else(|| crate::reader::bad_header(&self.format, "folder 未物化"))?;
        let Some((off, len)) = m.slices.get(&file_id).copied() else {
            return Ok(None);
        };
        if len == 0 {
            return Ok(None);
        }
        match &m.data {
            FolderData::Mem(bytes) => {
                let bytes = Arc::clone(bytes);
                let off = off as usize;
                let end = off + len as usize;
                Ok(Some(Box::new(MemSliceReader {
                    data: bytes,
                    pos: off,
                    end,
                })))
            }
            FolderData::Tmp(path) => {
                let mut f = std::fs::File::open(path).map_err(|e| HzipError::Io {
                    source: e,
                    path: Some(path.clone()),
                })?;
                f.seek(SeekFrom::Start(off)).map_err(|e| HzipError::Io {
                    source: e,
                    path: Some(path.clone()),
                })?;
                Ok(Some(Box::new(f.take(len))))
            }
        }
    }
}

/// `Arc<Vec<u8>>` 区间只读（内存零拷贝切片）。
pub(crate) struct MemSliceReader {
    data: Arc<Vec<u8>>,
    pos: usize,
    end: usize,
}

impl Read for MemSliceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.end || buf.is_empty() {
            return Ok(0);
        }
        let n = (self.end - self.pos).min(buf.len());
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// 映射 sevenz 错误 → HzipError（T25 §4.2 表；crate 文档亦有）。
pub(crate) fn map_err(e: sevenz_rust::Error, format: &FormatId) -> HzipError {
    use sevenz_rust::Error as S;
    match e {
        S::Io(io, _) => map_data_io(io, format),
        S::FileOpen(io, _) => HzipError::Io {
            source: io,
            path: None,
        },
        S::BadSignature(_) => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadSignature,
        },
        S::UnsupportedVersion { .. } => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::UnknownMethod(0),
        },
        S::ChecksumVerificationFailed => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        },
        S::NextHeaderCrcMismatch => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        },
        // 头加密而零密码 → HZ-3102（零条目名泄露：entries() 不可达）。
        S::PasswordRequired => HzipError::Password {
            target: hzip_error::PasswordTarget::Header,
        },
        S::MaybeBadPassword(_) => HzipError::Password {
            target: hzip_error::PasswordTarget::Archive,
        },
        S::UnsupportedCompressionMethod(m) => {
            tracing::warn!(method = %m, "7z 未知压缩方法");
            HzipError::Format {
                format: format.clone(),
                kind: FormatErrorKind::UnknownMethod(0),
            }
        }
        S::MaxMemLimited { .. } => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::UnknownMethod(0),
        },
        S::Unsupported(s) => HzipError::Unsupported { what: s },
        S::BadTerminatedStreamsInfo(_)
        | S::BadTerminatedUnpackInfo
        | S::BadTerminatedPackInfo(_)
        | S::BadTerminatedSubStreamsInfo
        | S::BadTerminatedheader(_)
        | S::ExternalUnsupported
        | S::Other(_) => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        },
    }
}

/// 数据区 io 错误语义（2002/2004/真实 Io 保留）。
pub(crate) fn map_data_io(e: io::Error, format: &FormatId) -> HzipError {
    match e.kind() {
        io::ErrorKind::UnexpectedEof | io::ErrorKind::Other => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::Truncated,
        },
        io::ErrorKind::InvalidData => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        },
        _ => HzipError::Io {
            source: e,
            path: None,
        },
    }
}

/// 坏头构造。
pub(crate) fn bad_header(format: &FormatId, why: &str) -> HzipError {
    tracing::warn!(%why, "7z 头非法");
    HzipError::Format {
        format: format.clone(),
        kind: FormatErrorKind::BadHeader,
    }
}

/// folder 物化的溢出写流（先验判定超 `FOLDER_CACHE_MEM_MAX` 时启用）。
struct SpillSink {
    path: PathBuf,
    out: std::io::BufWriter<std::fs::File>,
}

/// 每个 folder 头声明的**解出总字节**（= 其成员条目 `size` 之和），按下标 = folder
/// 序号存放；表由 [`folder_unpack_totals`] 在打开期算一次。
///
/// 用于物化落点的**先验**判定（内存 vs 临时文件，见 [`SevenZipReader::materialize`]）：
/// 7z 的 `StreamMap` 不带 folder 解出量，但每个成员条目的 `size` 头里就有。口径与
/// 解码闭包的 `first_file + consumed` 一致——按 `file_folder_index` 归集（不假设
/// 连续，宁多算不少算：多算只是提前落盘，少算会把大块留在内存）。
///
/// 只与头相关、与读取顺序无关，故可一次算好：旧实现在**每次 folder 物化时**
/// 重扫全部条目（O(folders × files)），「解压全部」时把这条扫描重复了 N 次。
fn folder_unpack_totals(archive: &sevenz_rust::Archive) -> Vec<u64> {
    let mut totals: Vec<u64> = Vec::new();
    for (i, f) in archive.files.iter().enumerate() {
        let Some(folder) = archive
            .stream_map
            .file_folder_index
            .get(i)
            .copied()
            .flatten()
        else {
            continue;
        };
        if folder >= totals.len() {
            totals.resize(folder + 1, 0);
        }
        if f.has_stream {
            totals[folder] += f.size;
        }
    }
    totals
}

/// 7z 读取器。
pub(crate) struct SevenZipReader {
    format: FormatId,
    archive: Arc<sevenz_rust::Archive>,
    source: Mutex<SourceHandle>,
    _spool: Option<crate::TempArea>,
    cache: Mutex<FolderCache>,
    /// 打开期算一次的「folder → 头声明解出总字节」表（下标 = folder 序号）；
    /// 物化落点判定用，见 [`folder_unpack_totals`]。
    folder_unpack: Vec<u64>,
    /// 打开期携带的档密码（头加密档案；条目级密码仍逐次传入 materialize）
    open_password: Option<zeroize::Zeroizing<String>>,
}

/// folder 是否挂 7zAES（数据加密判定；7z 无逐条目加密标志）。
fn folder_encrypted(archive: &sevenz_rust::Archive, folder: usize) -> bool {
    archive
        .folders
        .get(folder)
        .map(|fo| {
            fo.coders.iter().any(|c| {
                c.decompression_method_id() == sevenz_rust::SevenZMethod::AES256SHA256.id()
            })
        })
        .unwrap_or(false)
}

fn build_infos(archive: &sevenz_rust::Archive) -> Vec<EntryInfo> {
    let codecs = crate::coder::entry_codec_ids(archive);
    archive
        .files
        .iter()
        .enumerate()
        .map(|(id, f)| {
            let mut info = entry_info(id, f);
            info.codec = codecs[id].clone();
            // 数据加密 = 所属 folder 挂 7zAES；头加密档案能走到这里 ⇒ 头已解
            info.encrypted = archive
                .stream_map
                .file_folder_index
                .get(id)
                .copied()
                .flatten()
                .is_some_and(|fid| folder_encrypted(archive, fid));
            // 7zAES coder 不在头里声明密钥长度（7-Zip 产档恒 256 位）→ AES-256
            info.encryption = info.encrypted.then_some("AES-256");
            info
        })
        .collect()
}

fn entry_info(id: usize, f: &sevenz_rust::SevenZArchiveEntry) -> EntryInfo {
    let raw = f.name.as_str().replace('\\', "/");
    let is_dir = f.is_directory;
    let name = parse_entry_name(&raw).unwrap_or_else(|_| {
        // 头名非法（规则 N1~N6）：取文件段名兜底仍须合法；再失败用固定名。
        // 规范：净化决策归 core（02 §9.6），此处只保证语法穿越安全。
        let base = Path::new(&raw)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("entry");
        parse_entry_name(base).unwrap_or_else(|_| hzip_format::EntryName::new("entry"))
    });
    let (is_symlink, mode) = if f.has_windows_attributes {
        let unix_full = (f.windows_attributes >> 16) & 0xffff;
        let ft = unix_full & 0o170000;
        let is_symlink = ft == 0o120000;
        // mode 语义 = 权限位（类型位已由 is_dir/is_symlink 表达）
        let perms = unix_full & 0o7777;
        (
            is_symlink,
            Some(perms).filter(|p| *p != 0 || unix_full != 0),
        )
    } else {
        (false, None)
    };
    let mtime = if f.has_last_modified_date {
        OffsetDateTime::from_unix_timestamp(f.last_modified_date.to_unix_time()).ok()
    } else {
        None
    };
    EntryInfo {
        id,
        name,
        is_dir,
        is_symlink,
        symlink_target: None, // 7z 无独立链接目标字段（内容在数据区；净化归 core）
        size_raw: if is_dir { 0 } else { f.size },
        size_packed: f.compressed_size,
        mtime,
        mode,
        attributes: EntryAttrs {
            executable: mode.map(|m| m & 0o111 != 0),
            ..Default::default()
        },
        encrypted: false,
        header_encrypted: false,
        crc32: if f.has_crc { Some(f.crc as u32) } else { None },
        codec: None, // build_infos 填
        name_encoding: NameEncoding::Utf8,
        // 加密算法在 build_infos 补（folder 加密 → AES-256）；级别 7z 无该域
        encryption: None,
        compression_level: None,
    }
}

impl SevenZipReader {
    /// 打开源并解析头（头解析一次；数据区不读）。
    ///
    /// 头加密档案须经 `OpenOptions::password` 携带密码（否则 HZ-3102，由 core
    /// `open_archive_retry` 问密码重开）；错密码经 `map_err` 映射 Password 错。
    pub(crate) fn open(
        src: hzip_format::Source,
        opts: &hzip_format::OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        let format = FormatId::from(crate::FORMAT_ID);
        let (mut handle, spool) = open_source(src, &format)?;
        let len = handle.len;
        let open_password = opts.password.clone();
        let password = match open_password.as_deref() {
            Some(s) => sevenz_rust::Password::from(s.as_str()),
            None => sevenz_rust::Password::empty(),
        };
        let archive = {
            let inner = &mut handle.inner;
            sevenz_rust::Archive::read(inner, len, password.as_slice())
                .map_err(|e| map_err(e, &format))?
        };
        let archive = Arc::new(archive);
        // 头已知 ⇒ 物化落点判据一次算好：物化路径不再重扫全部条目（详见
        // `folder_unpack_totals` 文档）。
        let folder_unpack = folder_unpack_totals(&archive);
        let cache = Mutex::new(FolderCache::new(format.clone()));
        Ok(Box::new(SevenZipReader {
            format,
            archive,
            source: Mutex::new(handle),
            _spool: spool,
            cache,
            folder_unpack,
            open_password,
        }))
    }

    /// 确保某 folder 物化（整块解码，缓存；幂等）。`pw`：调用方条目级密码
    /// （缺省回落打开期档密码 → 空）。
    fn materialize(
        &self,
        folder: usize,
        cancel: Option<&CancellationToken>,
        pw: Option<&str>,
    ) -> Result<(), HzipError> {
        {
            let mut guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            if guard.has(folder) {
                return Ok(());
            }
        }
        let archive = Arc::clone(&self.archive);
        let format = self.format.clone();
        let mut source = self.source.lock().unwrap_or_else(|p| p.into_inner());
        // 三级回退：条目/调用方密码 → 打开期档密码 → 空
        let pw_str = pw.or_else(|| self.open_password.as_deref().map(String::as_str));
        let password = match pw_str {
            Some(s) => sevenz_rust::Password::from(s),
            None => sevenz_rust::Password::empty(),
        };
        let mut block: Vec<u8> = Vec::new();
        let mut slices: HashMap<usize, (u64, u64)> = HashMap::new();
        let mut acc: u64 = 0;
        let mut consumed: u64 = 0;
        // CRC 失败现场（自校验给出 expected/actual，见模块 rustdoc）。
        let mut crc_fail: Option<(String, u32, u32)> = None;
        let first_file = archive
            .stream_map
            .folder_first_file_index
            .get(folder)
            .copied()
            .ok_or_else(|| bad_header(&format, "folder 无首文件索引"))?;

        // —— 落点**先定**：按头声明的 folder 解出总量决定内存还是临时文件 ——
        //
        // 旧行为是「先整块攒进 `Vec`，攒完再判 `> FOLDER_CACHE_MEM_MAX` 才落盘」：
        // 阈值只对**驻留**生效，对**峰值**无效——400 MiB 固实块（7-Zip 默认固实，
        // 很常见）取其中**单个** 100 MiB 条目实测 RSS 峰值 449 MiB（2026-09-29）。
        // 故改为解码**之前**用头里的 `size` 预判：超阈值就边解边写临时文件，
        // 峰值与「当前条目大小」无关，只与 folder 总量的落点有关。
        // 总量取自打开期算好的表（`folder_unpack_totals`；同一口径，零重扫）。
        let unpack_total = self.folder_unpack.get(folder).copied().unwrap_or(0);
        let mut spill: Option<SpillSink> = if unpack_total > FOLDER_CACHE_MEM_MAX as u64 {
            let path = self
                .cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .tmp_area
                .join(&format!("folder-{folder}.bin"));
            let file = std::fs::File::create(&path).map_err(|e| HzipError::Io {
                source: e,
                path: Some(path.clone()),
            })?;
            Some(SpillSink {
                path,
                out: std::io::BufWriter::with_capacity(1024 * 1024, file),
            })
        } else {
            None
        };

        {
            let source = &mut source.inner;
            let bd = sevenz_rust::BlockDecoder::new(folder, &archive, password.as_slice(), source);
            let res = bd.for_each_entries(&mut |entry: &sevenz_rust::SevenZArchiveEntry,
                                                rdr: &mut dyn Read|
             -> Result<bool, sevenz_rust::Error> {
                if let Some(c) = cancel
                    && c.is_cancelled()
                {
                    return Err(sevenz_rust::Error::other("cancelled"));
                }
                let file_id = first_file + consumed as usize;
                if !entry.has_stream || entry.size == 0 {
                    slices.insert(file_id, (acc, 0));
                    consumed += 1;
                    return Ok(true);
                }
                let start = acc;
                let expected = entry.size;
                let mut n = 0u64;
                let mut crc = hzip_codec::crc::Crc32::new();
                let mut buf = [0u8; 65536];
                loop {
                    match rdr.read(&mut buf) {
                        Ok(0) => break,
                        Ok(k) => {
                            crc.update(&buf[..k]);
                            match spill.as_mut() {
                                // 溢出路径：边解边写临时文件（峰值不随 folder 增长）
                                Some(s) => {
                                    s.out.write_all(&buf[..k]).map_err(sevenz_rust::Error::io)?
                                }
                                None => block.extend_from_slice(&buf[..k]),
                            }
                            n += k as u64;
                            acc += k as u64;
                            if n.is_multiple_of(MATERIALIZE_CHECKPOINT_BYTES)
                                && let Some(c) = cancel
                                && c.is_cancelled()
                            {
                                return Err(sevenz_rust::Error::other("cancelled"));
                            }
                        }
                        // sevenz-rust 内部 Crc32VerifyingReader 在剩余归零的
                        // read 上以 io::Error 报错且丢末块（见模块 rustdoc）——
                        // 归入数据区 io 语义（map_data_io 兜底 2002/2004）。
                        Err(e) => return Err(sevenz_rust::Error::io(e)),
                    }
                }
                if n != expected {
                    return Err(sevenz_rust::Error::other(format!(
                        "unexpected eof: expected {expected} got {n}"
                    )));
                }
                // 干净 EOF 自校验 CRC（期望值来自头 subStreamsInfo）。
                if entry.has_crc {
                    let actual = crc.finalize();
                    if actual != entry.crc as u32 {
                        tracing::warn!(
                            entry = %entry.name,
                            expected = format!("{:08x}", entry.crc),
                            actual = format!("{actual:08x}"),
                            "7z 条目 CRC 不符"
                        );
                        crc_fail = Some((entry.name.clone(), entry.crc as u32, actual));
                        return Err(sevenz_rust::Error::other("crcmismatch"));
                    }
                }
                slices.insert(file_id, (start, n));
                consumed += 1;
                Ok(true)
            });
            if let Err(e) = res {
                // 溢出路径失败：先关句柄再删半写临时文件（Windows 下开着删不掉）。
                if let Some(s) = spill.take() {
                    drop(s.out);
                    let _ = std::fs::remove_file(&s.path);
                }
                return Err(match &e {
                    sevenz_rust::Error::Other(s) if s == "cancelled" => HzipError::Cancelled,
                    sevenz_rust::Error::Other(s) if s == "crcmismatch" => {
                        if let Some((name, expected, actual)) = crc_fail {
                            let entry_name = parse_entry_name(&name.replace('\\', "/"))
                                .unwrap_or_else(|_| hzip_format::EntryName::new("entry"));
                            HzipError::Checksum {
                                entry: entry_name,
                                expected,
                                actual,
                            }
                        } else {
                            bad_header(&format, "数据区 CRC 损坏（无现场）")
                        }
                    }
                    _ => map_err(e, &format),
                });
            }
        }

        let data = match spill {
            // 溢出路径：写流已随解码完成，收尾 flush（`slices` 偏移 = 文件内偏移）。
            Some(mut s) => {
                s.out.flush().map_err(|e| HzipError::Io {
                    source: e,
                    path: Some(s.path.clone()),
                })?;
                FolderData::Tmp(s.path)
            }
            None => FolderData::Mem(Arc::new(block)),
        };
        let mut guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        guard.insert_lru(folder, Materialized { slices, data });
        Ok(())
    }
}

impl ArchiveReader for SevenZipReader {
    fn caps(&self) -> FormatCaps {
        caps()
    }

    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        Ok(build_infos(&self.archive))
    }

    fn read_entry(
        &self,
        id: usize,
        pw: PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        let f = self
            .archive
            .files
            .get(id)
            .ok_or_else(|| bad_header(&self.format, "条目 id 越界"))?;
        if f.is_directory || !f.has_stream || f.size == 0 {
            return Ok(Box::new(io::empty()));
        }
        let folder = self
            .archive
            .stream_map
            .file_folder_index
            .get(id)
            .copied()
            .flatten()
            .ok_or_else(|| bad_header(&self.format, "条目无 folder（非法流图）"))?;
        self.materialize(folder, None, pw.as_str())?;
        let guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let r = guard.slice_reader(folder, id)?;
        Ok(r.unwrap_or_else(|| Box::new(io::empty())))
    }

    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        let ids = resolve_selection(&self.archive, sel, &self.format)?;
        let mut report = ExtractReport::default();
        for id in ids {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            let info = &self.archive.files[id];
            let out_path = dest.join(info.name.as_str());
            if info.is_directory {
                std::fs::create_dir_all(&out_path).map_err(HzipError::from)?;
                report.entries_done += 1;
                continue;
            }
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(HzipError::from)?;
            }
            // 密码（仅加密条目问询 ctx.passwords——未加密档案零弹窗）
            let owned_pw = self.entry_password(id, info.name.as_str(), ctx);
            // 物化（带取消）后拷出
            let mut rdr =
                self.read_entry_checked(id, ctx.cancel, owned_pw.as_deref().map(String::as_str))?;
            let res = std::fs::File::create(&out_path)
                .map_err(HzipError::from)
                .and_then(|mut f| copy_with_cancel(&mut rdr, &mut f, ctx.cancel, &self.format));
            match res {
                Ok(n) => {
                    report.entries_done += 1;
                    report.bytes_written += n;
                }
                Err(e) if ctx.best_effort => {
                    let _ = std::fs::remove_file(&out_path);
                    report.failures.push((id, Box::new(e)));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(report)
    }

    fn test(&self, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        let mut report = ExtractReport::default();
        for (id, f) in self.archive.files.iter().enumerate() {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            if f.is_directory || !f.has_stream {
                report.entries_done += 1;
                continue;
            }
            // 密码（仅加密条目问询 ctx.passwords——未加密档案零弹窗）
            let owned_pw = self.entry_password(id, f.name.as_str(), ctx);
            let mut rdr =
                self.read_entry_checked(id, ctx.cancel, owned_pw.as_deref().map(String::as_str))?;
            let n = copy_with_cancel(&mut rdr, &mut io::sink(), ctx.cancel, &self.format)?;
            if n != f.size {
                return Err(HzipError::Format {
                    format: self.format.clone(),
                    kind: FormatErrorKind::Truncated,
                });
            }
            report.entries_done += 1;
            report.bytes_written += n;
        }
        Ok(report)
    }
}

impl SevenZipReader {
    /// 条目所属 folder 是否数据加密（`build_infos` 的 `EntryInfo.encrypted` 同源判定）。
    fn entry_encrypted(&self, id: usize) -> bool {
        self.archive
            .stream_map
            .file_folder_index
            .get(id)
            .copied()
            .flatten()
            .is_some_and(|fid| folder_encrypted(&self.archive, fid))
    }

    /// 加密条目经 `ctx.passwords` 取档密码（未加密 → `None`，零弹窗）。
    ///
    /// 名参用 `&str` 而非 `&EntryName`：未加密档案占绝大多数，调用方为凑参数
    /// 造 `EntryName` 就是**每条目**一次白白的构造/分配——改为命中加密分支才构造。
    fn entry_password(
        &self,
        id: usize,
        name: &str,
        ctx: &ExtractCtx<'_>,
    ) -> Option<zeroize::Zeroizing<String>> {
        if self.entry_encrypted(id) {
            let name = hzip_format::EntryName::new(name);
            ctx.passwords.password(Some(&name))
        } else {
            None
        }
    }

    /// 与 `read_entry` 相同，但物化循环可响应取消。
    fn read_entry_checked(
        &self,
        id: usize,
        cancel: &CancellationToken,
        pw: Option<&str>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        let f = self
            .archive
            .files
            .get(id)
            .ok_or_else(|| bad_header(&self.format, "条目 id 越界"))?;
        if f.is_directory || !f.has_stream || f.size == 0 {
            return Ok(Box::new(io::empty()));
        }
        let folder = self
            .archive
            .stream_map
            .file_folder_index
            .get(id)
            .copied()
            .flatten()
            .ok_or_else(|| bad_header(&self.format, "条目无 folder（非法流图）"))?;
        self.materialize(folder, Some(cancel), pw)?;
        let guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let r = guard.slice_reader(folder, id)?;
        Ok(r.unwrap_or_else(|| Box::new(io::empty())))
    }
}

/// 带取消轮询的流拷贝。
fn copy_with_cancel<R: Read + ?Sized, W: io::Write + ?Sized>(
    rdr: &mut R,
    wtr: &mut W,
    cancel: &CancellationToken,
    format: &FormatId,
) -> Result<u64, HzipError> {
    let mut buf = [0u8; 65536];
    let mut total = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(HzipError::Cancelled);
        }
        let n = rdr.read(&mut buf).map_err(|e| map_data_io(e, format))?;
        if n == 0 {
            break;
        }
        wtr.write_all(&buf[..n]).map_err(HzipError::from)?;
        total += n as u64;
    }
    Ok(total)
}

fn resolve_selection(
    archive: &sevenz_rust::Archive,
    sel: &Selection,
    format: &FormatId,
) -> Result<Vec<usize>, HzipError> {
    let fmt = format.clone();
    match sel {
        Selection::All => Ok((0..archive.files.len()).collect()),
        Selection::Ids(ids) => {
            if ids.iter().any(|&i| i >= archive.files.len()) {
                return Err(HzipError::Format {
                    format: fmt,
                    kind: FormatErrorKind::BadHeader,
                });
            }
            Ok(ids.clone())
        }
        Selection::Names(names) => Ok(archive
            .files
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                names
                    .iter()
                    .any(|n| n.as_str() == e.name.as_str().replace('\\', "/"))
            })
            .map(|(i, _)| i)
            .collect()),
        _ => Err(HzipError::Unsupported {
            what: "未知 Selection 变体".into(),
        }),
    }
}
