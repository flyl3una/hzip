//! 「无目录 zip」salvage 读取（02 §8.1 写入路径要点末句；T24 §4.5）。
//!
//! 中途失败产物（只落了本地头+数据、finish 未写中央目录）中央目录缺失，zip crate
//! 的 `ZipArchive::new` 无法打开（找不到 EOCD）。本模块**顺序扫描 `PK\x03\x04`
//! 本地头**重建条目表，仍可恢复数据；打 warning（`tracing::warn!`，02 §8.1「打
//! warning」）。数据描述符条目（GP bit 3，CRC/尺寸未知）以扫描 `PK\x07\x08`
//! 定界恢复，CRC 未知时跳过校验并 warning。
//!
//! # 局限（rustdoc 注明）
//!
//! 本实现为 hzip 自产「无目录 zip」的恢复读取——即写路径保证的形态：**本地头携带
//! 完整 CRC/尺寸、无数据描述符**（seek 两遍写，见 writer 模块）。对外部产生、
//! 本地头也未知尺寸的流式 zip（典型 deflate 数据含签名字节）定界不可靠，只尽力
//! 而为。加密条目 salvage 无法解密（读返回 `Unsupported`）。
//!
//! # 条目读取
//!
//! 每条目按其压缩方法解码（store → 直取；deflate → flate2 裸流解码），读末以
//! 旁路 CRC32（02 §6.2）与本地头 CRC 比对（不符 → `Checksum 3001`）。deflate64/
//! bzip2/zstd 等 zipx 方法 salvage 暂不解码（`Unsupported`，T45 `zipx.rs` 路由后
//! 补齐）；store 与 deflate 是本任务交付范围。

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use hzip_codec::crc::Crc32;
use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::{
    ArchiveReader, EntryAttrs, EntryInfo, ExtractCtx, ExtractReport, FormatCaps, FormatId,
    NameEncoding, PasswordRef, Selection, parse_entry_name,
};

use crate::handler;

/// 本地文件头固定区（30 B，不含 4B 签名）。
const LOCAL_BODY: usize = 26;
/// GP bit 3（数据描述符）。
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
/// GP bit 11（UTF-8 名）。
const FLAG_UTF8: u16 = 1 << 11;
/// GP bit 0（加密）。
const FLAG_ENCRYPTED: u16 = 1;

/// salvage 扫描出的单条目（数据区物理布局）。
#[derive(Debug, Clone)]
struct SalvagedEntry {
    /// 数据区（本地头后）起点绝对偏移。
    data_off: u64,
    /// 压缩字节数（本地头声明；zip64 哨兵未解析 → None）。
    compressed_size: Option<u64>,
    /// 压缩方法 id。
    method: u16,
    /// 本地头 CRC（data descriptor 时未知 → None，跳过校验）。
    crc32: Option<u32>,
    /// 本地头原始尺寸（data descriptor 时未知 → None）。
    size_raw: Option<u64>,
    /// 是否加密。
    encrypted: bool,
}

/// salvage 模式读取器。
pub(crate) struct SalvageReader {
    format: FormatId,
    path: PathBuf,
    infos: Vec<EntryInfo>,
    entries: Vec<SalvagedEntry>,
}

impl SalvageReader {
    /// 打开并顺序扫描。
    pub(crate) fn open(path: PathBuf) -> Result<Box<dyn ArchiveReader>, HzipError> {
        let format = handler::fmt();
        let mut file = std::fs::File::open(&path).map_err(crate::map_io)?;
        let (infos, entries) = scan(&mut file, &format)?;
        if infos.is_empty() {
            return Err(HzipError::Format {
                format,
                kind: FormatErrorKind::Truncated,
            });
        }
        tracing::warn!(
            path = %path.display(),
            n = infos.len(),
            "zip 中央目录缺失，salvage 顺序扫描恢复条目（02 §8.1）"
        );
        Ok(Box::new(SalvageReader {
            format,
            path,
            infos,
            entries,
        }))
    }
}

/// 顺序扫描本地头 → 条目表。
fn scan(
    file: &mut std::fs::File,
    format: &FormatId,
) -> Result<(Vec<EntryInfo>, Vec<SalvagedEntry>), HzipError> {
    let file_len = file.metadata().map(|m| m.len()).map_err(crate::map_io)?;
    let mut infos = Vec::new();
    let mut entries = Vec::new();
    let mut offset = 0u64;
    let mut guard_count = 0usize;
    let max_guard = 1_000_000usize; // 防恶意海量条目（02 §9.6 条数上限语义）

    while offset + 4 <= file_len && guard_count < max_guard {
        file.seek(SeekFrom::Start(offset)).map_err(crate::map_io)?;
        let mut sig = [0u8; 4];
        match file.read_exact(&mut sig) {
            Ok(()) => {}
            Err(_) => break,
        }
        if sig != *b"PK\x03\x04" {
            // 中央目录开始（PK\x01\x02）或 EOF → 结束
            if sig == *b"PK\x01\x02" || sig == *b"PK\x05\x06" {
                break;
            }
            offset += 1;
            continue;
        }
        let mut body = [0u8; LOCAL_BODY];
        if file.read_exact(&mut body).is_err() {
            break;
        }
        let flags = u16::from_le_bytes([body[2], body[3]]);
        let method = u16::from_le_bytes([body[4], body[5]]);
        let crc = u32::from_le_bytes([body[10], body[11], body[12], body[13]]);
        let csize = u32::from_le_bytes([body[14], body[15], body[16], body[17]]);
        let usize_ = u32::from_le_bytes([body[18], body[19], body[20], body[21]]);
        let nlen = u16::from_le_bytes([body[22], body[23]]) as u64;
        let elen = u16::from_le_bytes([body[24], body[25]]) as u64;

        let mut name_raw = vec![0u8; nlen as usize];
        if file.read_exact(&mut name_raw).is_err() {
            break;
        }
        let mut extra_raw = vec![0u8; elen.min(1 << 20) as usize];
        let to_skip = elen as usize;
        if extra_raw.len() < to_skip {
            break;
        }
        extra_raw.truncate(to_skip);
        if file.read_exact(&mut extra_raw).is_err() {
            break;
        }

        let data_off = offset + 4 + LOCAL_BODY as u64 + nlen + elen;
        let uses_dd = flags & FLAG_DATA_DESCRIPTOR != 0;
        let encrypted = flags & FLAG_ENCRYPTED != 0;
        let name_encoding = if flags & FLAG_UTF8 != 0 {
            NameEncoding::Utf8
        } else {
            NameEncoding::Unknown
        };

        let name_str = if flags & FLAG_UTF8 != 0 {
            String::from_utf8_lossy(&name_raw).into_owned()
        } else {
            // 非 UTF-8：zip crate cp437 语义，逐字节 → 字符（T45 编码修复前占位）
            name_raw.iter().map(|&b| b as char).collect()
        };
        let normalized = name_str.replace('\\', "/");
        let name = parse_entry_name(&normalized).map_err(|_| HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        })?;
        let is_dir = name_raw.ends_with(b"/");

        // 尺寸来源优先级：本地头声明的真实值（无 zip64 哨兵）→ zip64 extra 字段
        // → 数据描述符定界（GP bit 3）。
        let (real_csize, real_usize) = zip64_extract_sizes(&extra_raw, csize, usize_);
        let has_zip64_sentinel = csize == u32::MAX || usize_ == u32::MAX;
        let (known_csize, known_usize, known_crc, desc_end) = if uses_dd {
            // 数据描述符条目（GP bit 3）：扫描 PK\x07\x08 定界
            match find_descriptor(file, data_off, file_len)? {
                Some(desc_off) => match parse_descriptor(file, desc_off)? {
                    Some((dcrc, dcsize, dusize)) => {
                        let after_desc = desc_off + descriptor_len();
                        (Some(dcsize), Some(dusize), Some(dcrc), Some(after_desc))
                    }
                    None => (None, None, None, Some(desc_off + descriptor_len())),
                },
                None => (None, None, None, None),
            }
        } else if has_zip64_sentinel {
            // zip64 extra 已解析出真实值 → 直接采用
            (real_csize, real_usize, Some(crc), None)
        } else {
            (Some(csize as u64), Some(usize_ as u64), Some(crc), None)
        };

        let size_raw = if is_dir { 0 } else { known_usize.unwrap_or(0) };
        let info = EntryInfo {
            id: infos.len(),
            name,
            is_dir,
            is_symlink: false,
            symlink_target: None,
            size_raw,
            size_packed: 0, // 无中央目录：精确压缩字节不可知
            mtime: None,
            mode: None,
            attributes: EntryAttrs::default(),
            encrypted,
            header_encrypted: false,
            crc32: known_crc,
            codec: Some(codec_of(method)),
            name_encoding,
            // 抢救路径无中央目录：加密算法细分/备注不可知，只保留加密位
            encryption: encrypted.then_some("ZipCrypto"),
            compression_level: None,
        };
        infos.push(info);
        entries.push(SalvagedEntry {
            data_off,
            compressed_size: known_csize,
            method,
            crc32: known_crc,
            size_raw: known_usize,
            encrypted,
        });

        offset = match desc_end {
            Some(d) => d,
            None => data_off
                .checked_add(known_csize.unwrap_or(0))
                .unwrap_or(file_len),
        };
        guard_count += 1;
    }
    Ok((infos, entries))
}

fn codec_of(method: u16) -> hzip_codec::traits::CodecId {
    match method {
        0 => "store".into(),
        8 => "deflate".into(),
        9 => "deflate64".into(),
        12 => "bzip2".into(),
        _ => "unknown".into(),
    }
}

/// 解析本地头 extra 区里的 ZIP64 扩展字段（ID 0x0001）恢复真实尺寸。
///
/// APPNOTE 4.5.3：zip64 extra 字段数据按「哪些本地头字段为 0xFFFFFFFF 哨兵就
/// 依序包含对应 8B 值」排列——顺序为 uncompressed size、compressed size、本地头
/// 偏移、disk 号。本层只关心前两个；extra 中也可能混入其他字段，故需按 extra
/// 结构遍历（各字段 = 2B id + 2B len + data）。
fn zip64_extract_sizes(extra: &[u8], csize32: u32, usize32: u32) -> (Option<u64>, Option<u64>) {
    let want_c = csize32 == u32::MAX;
    let want_u = usize32 == u32::MAX;
    if !want_c && !want_u {
        return (None, None);
    }
    let mut pos = 0usize;
    while pos + 4 <= extra.len() {
        let id = u16::from_le_bytes([extra[pos], extra[pos + 1]]);
        let len = u16::from_le_bytes([extra[pos + 2], extra[pos + 3]]) as usize;
        let data_start = pos + 4;
        let data_end = data_start + len;
        if data_end > extra.len() {
            break; // extra 截断
        }
        if id == 0x0001 {
            let mut off = data_start;
            let mut csize = None;
            let mut usize_ = None;
            if want_u && off + 8 <= data_end {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&extra[off..off + 8]);
                usize_ = Some(u64::from_le_bytes(buf));
                off += 8;
            }
            if want_c && off + 8 <= data_end {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&extra[off..off + 8]);
                csize = Some(u64::from_le_bytes(buf));
            }
            return (csize, usize_);
        }
        pos = data_end;
    }
    (None, None)
}

/// 找数据描述符签名（`start` 起顺序扫描 `PK\x07\x08`）。
fn find_descriptor(
    file: &mut std::fs::File,
    start: u64,
    file_len: u64,
) -> Result<Option<u64>, HzipError> {
    let mut pos = start;
    let mut probe = [0u8; 4];
    while pos + 4 <= file_len {
        file.seek(SeekFrom::Start(pos)).map_err(crate::map_io)?;
        if file.read_exact(&mut probe).is_err() {
            return Ok(None);
        }
        if probe == *b"PK\x07\x08" {
            return Ok(Some(pos));
        }
        pos += 1;
    }
    Ok(None)
}

/// 数据描述符长度（PK\x07\x08 + crc/csize/usize 各 4B = 16；zip64 24B 由
/// [`parse_descriptor`] 的保守路径处理——若 zip64 未知，只按 16 计并尽力）。
fn descriptor_len() -> u64 {
    16
}

/// 解析 zip32 数据描述符（签名 + crc/csize/usize）。
fn parse_descriptor(
    file: &mut std::fs::File,
    desc_off: u64,
) -> Result<Option<(u32, u64, u64)>, HzipError> {
    let mut buf = [0u8; 16];
    file.seek(SeekFrom::Start(desc_off))
        .map_err(crate::map_io)?;
    if file.read_exact(&mut buf).is_err() {
        return Ok(None);
    }
    if &buf[0..4] != b"PK\x07\x08" {
        return Ok(None);
    }
    let crc = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let csize = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let usize_ = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    if csize == u32::MAX || usize_ == u32::MAX {
        return Ok(None); // zip64 → 本层不解析
    }
    Ok(Some((crc, csize as u64, usize_ as u64)))
}

impl ArchiveReader for SalvageReader {
    fn caps(&self) -> FormatCaps {
        handler::caps()
    }

    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        Ok(self.infos.clone())
    }

    fn read_entry(
        &self,
        id: usize,
        _pw: PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        let e = self.entries.get(id).ok_or_else(|| HzipError::Format {
            format: self.format.clone(),
            kind: FormatErrorKind::BadHeader,
        })?;
        if e.encrypted {
            return Err(HzipError::Unsupported {
                what: "salvage 模式不解密条目（无中央目录密码/CRC 信息）".into(),
            });
        }
        if self.infos[id].is_dir {
            return Ok(Box::new(io::empty()));
        }
        // 数据边界必须已知（无数据描述符或描述符解析成功）
        let csize = e.compressed_size.ok_or_else(|| HzipError::Unsupported {
            what: "salvage 无法确定条目边界（数据描述符定界失败）".into(),
        })?;
        let mut f = std::fs::File::open(&self.path).map_err(crate::map_io)?;
        f.seek(SeekFrom::Start(e.data_off)).map_err(crate::map_io)?;
        let inner: Box<dyn Read + Send> = match e.method {
            0 => Box::new(f.take(csize)),
            8 => Box::new(flate2::read::DeflateDecoder::new(f.take(csize))),
            _ => {
                return Err(HzipError::Unsupported {
                    what: format!("salvage 暂不支持压缩方法 {}（zipx 路由归 T45）", e.method)
                        .into(),
                });
            }
        };
        let name = self.infos[id].name.clone();
        let _ = name; // name 保留给未来 best_effort 明细（现 Err 直抛）
        Ok(Box::new(SalvageStream {
            inner: io::BufReader::new(inner),
            crc: Crc32::new(),
            expected_crc: e.crc32,
            expected_len: e.size_raw,
            eof_checked: false,
        }))
    }

    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        // 复用通用 extract：entries + read_entry 驱动（SalvageReader 为 dyn reader）
        reader_extract(self, sel, ctx, dest)
    }

    fn test(&self, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        reader_test(self, ctx)
    }
}

/// 通用 extract（借 dyn ArchiveReader 的 entries/read_entry；Salvage 用）。
fn reader_extract(
    r: &dyn ArchiveReader,
    sel: &Selection,
    ctx: &ExtractCtx<'_>,
    dest: &Path,
) -> Result<ExtractReport, HzipError> {
    let infos = r.entries()?;
    let ids = crate::reader::resolve_ids(&infos, sel)?;
    let mut report = ExtractReport::default();
    for id in ids {
        if ctx.cancel.is_cancelled() {
            return Err(HzipError::Cancelled);
        }
        let info = &infos[id];
        let out_path = dest.join(info.name.as_str());
        if info.is_dir {
            match std::fs::create_dir_all(&out_path) {
                Ok(()) => {
                    report.entries_done += 1;
                    continue;
                }
                Err(e) => {
                    if ctx.best_effort {
                        report.failures.push((id, Box::new(crate::map_io(e))));
                        continue;
                    }
                    return Err(crate::map_io(e));
                }
            }
        }
        if let Some(parent) = out_path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Err(crate::map_io(e));
        }
        let mut src = r.read_entry(id, PasswordRef::none())?;
        let result = std::fs::File::create(&out_path)
            .map_err(crate::map_io)
            .and_then(|mut f| {
                let mut buf = vec![0u8; hzip_format::consts::COPY_CHUNK_BYTES as usize];
                let mut total = 0u64;
                loop {
                    if ctx.cancel.is_cancelled() {
                        return Err(HzipError::Cancelled);
                    }
                    let n = src.read(&mut buf).map_err(crate::map_io)?;
                    if n == 0 {
                        break;
                    }
                    f.write_all(&buf[..n]).map_err(crate::map_io)?;
                    total += n as u64;
                    ctx.progress
                        .send(hzip_format::ProgressEvent::Bytes { delta: n as u64 });
                }
                f.flush().map_err(crate::map_io)?;
                Ok(total)
            });
        match result {
            Ok(bytes) => {
                report.entries_done += 1;
                report.bytes_written += bytes;
            }
            Err(err) => {
                let _ = std::fs::remove_file(&out_path);
                if ctx.best_effort {
                    report.failures.push((id, Box::new(err)));
                } else {
                    return Err(err);
                }
            }
        }
    }
    Ok(report)
}

/// 通用 test（Salvage 用）。
fn reader_test(r: &dyn ArchiveReader, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
    let infos = r.entries()?;
    let mut report = ExtractReport::default();
    for info in &infos {
        if ctx.cancel.is_cancelled() {
            return Err(HzipError::Cancelled);
        }
        if info.is_dir || info.is_symlink {
            report.entries_done += 1;
            continue;
        }
        let mut src = r.read_entry(info.id, PasswordRef::none())?;
        let mut buf = vec![0u8; hzip_format::consts::COPY_CHUNK_BYTES as usize];
        loop {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            let n = src.read(&mut buf).map_err(crate::map_io)?;
            if n == 0 {
                break;
            }
            ctx.progress
                .send(hzip_format::ProgressEvent::Bytes { delta: n as u64 });
        }
        report.entries_done += 1;
    }
    Ok(report)
}

/// salvage 读取流：EOF 时校验 CRC/长度（不符 → Checksum 3001 / Truncated 2002）。
struct SalvageStream {
    inner: io::BufReader<Box<dyn Read + Send>>,
    crc: Crc32,
    expected_crc: Option<u32>,
    expected_len: Option<u64>,
    eof_checked: bool,
}

impl Read for SalvageStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.crc.update(&buf[..n]);
        }
        if n == 0 && !self.eof_checked {
            self.eof_checked = true;
            if let Some(exp) = self.expected_len
                && self.crc.bytes() != exp
            {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "salvage 条目长度不足",
                ));
            }
            if let Some(exp) = self.expected_crc {
                let act = self.crc.finalize();
                if exp != act {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Invalid checksum: expected {exp:08x} actual {act:08x}"),
                    ));
                }
            }
        }
        Ok(n)
    }
}
