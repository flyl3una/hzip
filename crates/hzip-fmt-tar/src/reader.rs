//! tar 读取：三变体（V7/ustar/GNU/pax）`ArchiveReader`（02 §8.4、T22 §4.1/§4.4/§4.5）。
//!
//! 设计（红线 1/2，02 §7.3 四条契约红线）：
//!
//! - `entries()`：以 tar crate `entries_with_seek` **一次顺序扫描**建索引
//!   （`Vec<IndexEntry>`：`EntryInfo` + `header_pos`/`data_off`）。seek 跳过数据区，
//!   **不读文件数据内容**（TC-CONF-010 大档案秒开；与数据区大小无关）。
//!   顺带消费 `g` 全局 pax 头（小元数据，缓存于 [`PaxGlobal`]）。
//! - `read_entry(id)`：
//!   - **普通条目**：按 `data_off` 直接 `File::take(logical)`（裸 tar 随机访问，
//!     无重放，红线 2）。
//!   - **pax 1.0 稀疏**（`GNU.sparse.*` 在 `x` 头）：物理数据自 `data_off` 连续，
//!     按区间表重建逻辑流（洞=零，见 `SparsePaxRead`）。
//!   - **GNU 旧式 `S` 稀疏**：`file_pos` 在扩展稀疏头存在时不可靠（扩展头介于
//!     主头与数据之间），故以「整档重放定位该头位置条目」取 tar crate 归一化逻辑流
//!     （经临时 spool 承载，防大稀疏 OOM）。
//! - pax 覆盖：path/linkpath 合并信任 tar crate（T22 §4.2 要点①）；mtime、
//!   `GNU.sparse.*` 1.0 由本层经 [`crate::variant`] 兜底；`g` 全局头跨条目缓存。
//! - 错误映射经 [`crate::variant::map_err`]（UnexpectedEof→2002 / 坏头→2004 /
//!   坏魔数→2001）。

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::{
    ArchiveReader, EntryAttrs, EntryInfo, ExtractCtx, ExtractReport, FormatCaps, FormatId,
    NameEncoding, PasswordRef, Selection, parse_entry_name,
};
use time::OffsetDateTime;

use crate::caps;
use crate::consts::BLOCK_BYTES;
use crate::variant::{self, PaxGlobal};

/// 数据形态（决定 `read_entry` 如何重建逻辑流）。
#[derive(Debug, Clone)]
enum DataShape {
    /// 普通条目：`data_off` 起连续字节即逻辑内容（logical == phys）。
    Dense,
    /// GNU 旧式 `S` 稀疏：需整档重放取 tar 归一化逻辑流。
    LegacySparse,
    /// pax 1.0（GNU.sparse.*）：物理自 `data_off` 连续，按区间表重建逻辑流。
    PaxSparse { extents: Vec<(u64, u64)> },
}

/// 索引条目。
#[derive(Debug, Clone)]
struct IndexEntry {
    /// 头在档案中的偏移（重放稀疏时定位）。
    header_pos: u64,
    /// 数据区起点（普通条目/pax 稀疏）。
    data_off: u64,
    /// 档案内物理字节（区间和；不含 padding / 扩展稀疏头）。
    phys: u64,
    /// 逻辑大小（还原后文件字节数）。
    logical: u64,
    shape: DataShape,
    info: EntryInfo,
}

/// tar 读取器。
pub(crate) struct TarReader {
    path: PathBuf,
    format: FormatId,
    /// 索引缓存（entries() 一次扫描；read_entry 复用）。
    cache: std::sync::Mutex<Option<Vec<IndexEntry>>>,
    /// Stream/Stdin 源临时 spool 保活（None = 用户 File）。
    _spool: Option<crate::spool::Spool>,
}

impl TarReader {
    pub(crate) fn from_path(
        path: PathBuf,
        format: FormatId,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        Self::check_head(&path, &format)?;
        Ok(Box::new(TarReader {
            path,
            format,
            cache: std::sync::Mutex::new(None),
            _spool: None,
        }))
    }

    /// Stream/Stdin 源：缓冲 spool 后打开（spool 保活于 reader 内）。
    pub(crate) fn from_path_spooled(
        path: PathBuf,
        spool: crate::spool::Spool,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        Self::check_head(&path, &crate::format_id())?;
        let format = crate::format_id();
        Ok(Box::new(TarReader {
            path,
            format,
            cache: std::sync::Mutex::new(None),
            _spool: Some(spool),
        }))
    }

    /// 魔数预检：空档案（全零字节，含 0 长/不足 512B 的零档）放行——`entries()`
    /// 自然为空（TC-ALG-010 空档案语义）；首块非零且非三变体 → BadSignature 2001。
    fn check_head(path: &Path, format: &FormatId) -> Result<(), HzipError> {
        let mut f = std::fs::File::open(path).map_err(|e| variant::map_err(e, format.clone()))?;
        let mut head = [0u8; BLOCK_BYTES];
        let n = f
            .read(&mut head)
            .map_err(|e| variant::map_err(e, format.clone()))?;
        // 全零（空档/尾块纯零）→ 合法空档案
        if head[..n].iter().all(|&b| b == 0) {
            return Ok(());
        }
        if n < BLOCK_BYTES || variant::detect(&head).is_none() {
            return Err(HzipError::Format {
                format: format.clone(),
                kind: FormatErrorKind::BadSignature,
            });
        }
        Ok(())
    }

    fn index(&self) -> Result<Vec<IndexEntry>, HzipError> {
        {
            let guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(ok) = guard.as_ref() {
                return Ok(ok.clone());
            }
        }
        let built = self.build_index()?;
        if let Ok(mut g) = self.cache.lock() {
            *g = Some(built.clone());
        }
        Ok(built)
    }

    fn build_index(&self) -> Result<Vec<IndexEntry>, HzipError> {
        let f = std::fs::File::open(&self.path)
            .map_err(|e| variant::map_err(e, self.format.clone()))?;
        let mut archive = tar::Archive::new(f);
        let mut g = PaxGlobal::default();
        let mut out: Vec<IndexEntry> = Vec::new();
        let entries = archive
            .entries_with_seek()
            .map_err(|e| variant::map_err(e, self.format.clone()))?;
        for item in entries {
            let mut entry = item.map_err(|e| variant::map_err(e, self.format.clone()))?;
            // 克隆头部（tar Header: Clone；避免长借用阻断 &mut 调用）
            let header = entry.header().clone();
            let ty = header.entry_type();

            // g 全局扩展头：读数据并入缓存（元数据，非文件数据区）
            if ty.is_pax_global_extensions() {
                let mut data = Vec::new();
                entry
                    .read_to_end(&mut data)
                    .map_err(|e| variant::map_err(e, self.format.clone()))?;
                g.merge(&data);
                continue;
            }

            let header_pos = entry.raw_header_position();
            let data_off = entry.raw_file_position();

            // pax 覆盖兜底（mtime + GNU.sparse.*）；path/linkpath 信任 tar 合并。
            // pax_extensions() 只在条目本身是 x 头时读其内容（小元数据）；否则返回
            // tar 已缓存的前置 x 头字节，不触碰文件数据区。
            let mut pax = g.snapshot();
            if let Some(exts) = entry
                .pax_extensions()
                .map_err(|e| variant::map_err(e, self.format.clone()))?
            {
                for ext in exts {
                    let ext = ext.map_err(|e| variant::map_err(e, self.format.clone()))?;
                    pax.apply(ext.key_bytes(), ext.value_bytes());
                }
            }
            pax.finish_sparse()
                .map_err(|_| self.bad_header("GNU.sparse 半套记录"))?;

            let is_legacy_sparse = ty.is_gnu_sparse();
            let phys = header
                .entry_size()
                .map_err(|e| variant::map_err(e, self.format.clone()))?;
            let (shape, logical) = if is_legacy_sparse {
                // tar crate 已归一化：entry.size() = 逻辑大小（洞填零）
                let logical = entry.size();
                (DataShape::LegacySparse, logical)
            } else if pax.is_sparse() {
                let extents = std::mem::take(&mut pax.sparse_map);
                let logical = pax.sparse_size.unwrap_or(phys);
                (DataShape::PaxSparse { extents }, logical)
            } else {
                let logical = entry.size();
                (DataShape::Dense, logical)
            };

            let raw_name = entry.path_bytes();
            // 归档根的自我描述条目（`tar cf x.tar .` 写出的 `./`）不带条目语义：
            // 跳过。否则它会被 `parse_entry_name` 按「剥净 → 空名」拒绝，整档报
            // 2004——`tar cf x.tar .` 这种最常见的产法将**完全打不开**。
            if hzip_format::is_root_entry_name(raw_name.as_ref()) {
                continue;
            }
            let name_lossy = String::from_utf8_lossy(raw_name.as_ref()).into_owned();
            let name_encoding = if std::str::from_utf8(raw_name.as_ref()).is_ok() {
                NameEncoding::Utf8
            } else {
                NameEncoding::Unknown
            };
            let name = parse_entry_name(&name_lossy).map_err(|_| self.bad_header("条目名非法"))?;

            let is_dir = ty.is_dir() || raw_name.as_ref().ends_with(b"/");
            let is_symlink = ty.is_symlink();
            let link_target = if is_symlink || ty.is_hard_link() {
                entry
                    .link_name()
                    .map_err(|e| variant::map_err(e, self.format.clone()))?
                    .map(|c| c.into_owned())
            } else {
                None
            };

            let mode = header.mode().ok();
            let mtime_hdr = header.mtime().ok();
            let mtime = mtime_of(pax.mtime, mtime_hdr);
            let info = EntryInfo {
                id: out.len(),
                name,
                is_dir,
                is_symlink,
                symlink_target: link_target,
                size_raw: if is_dir { 0 } else { logical },
                size_packed: BLOCK_BYTES as u64 + round_up(phys.max(logical), BLOCK_BYTES as u64),
                mtime,
                mode,
                attributes: EntryAttrs {
                    executable: mode.map(|m| m & 0o111 != 0),
                    ..Default::default()
                },
                encrypted: false,
                header_encrypted: false,
                crc32: None,
                codec: Some("store".into()),
                name_encoding,
                encryption: None,
                compression_level: None,
            };
            out.push(IndexEntry {
                header_pos,
                data_off,
                phys,
                logical: if is_dir { 0 } else { logical },
                shape,
                info,
            });
        }
        Ok(out)
    }

    fn bad_header(&self, why: &str) -> HzipError {
        tracing::warn!(%why, "tar 头非法");
        HzipError::Format {
            format: self.format.clone(),
            kind: FormatErrorKind::BadHeader,
        }
    }

    fn open_file(&self) -> Result<std::fs::File, HzipError> {
        std::fs::File::open(&self.path).map_err(|e| variant::map_err(e, self.format.clone()))
    }

    /// GNU 旧式 `S` 稀疏：整档重放，定位 header_pos 处条目并取其归一化逻辑流
    /// （tar crate 洞=零），spool 到临时文件防 OOM。
    fn replay_legacy_sparse(&self, idx: &IndexEntry) -> Result<crate::spool::Spool, HzipError> {
        let f = self.open_file()?;
        let mut archive = tar::Archive::new(f);
        let mut iter = archive
            .entries()
            .map_err(|e| variant::map_err(e, self.format.clone()))?;
        for item in iter.by_ref() {
            let mut entry = item.map_err(|e| variant::map_err(e, self.format.clone()))?;
            // 跳过扩展头条目（迭代器已透明消费，不出现）与 g 头
            if entry.header().entry_type().is_pax_global_extensions() {
                continue;
            }
            if entry.raw_header_position() == idx.header_pos {
                let mut sp = crate::spool::Spool::new().map_err(HzipError::from)?;
                io::copy(&mut entry, &mut sp)
                    .map_err(|e| variant::map_err(e, self.format.clone()))?;
                sp.flush().map_err(HzipError::from)?;
                return Ok(sp);
            }
        }
        Err(self.bad_header("稀疏条目重放未找到目标"))
    }

    fn entry_reader(&self, idx: &IndexEntry) -> Result<Box<dyn Read + Send>, HzipError> {
        match &idx.shape {
            DataShape::Dense => {
                let mut f = self.open_file()?;
                f.seek(SeekFrom::Start(idx.data_off))
                    .map_err(|e| variant::map_err(e, self.format.clone()))?;
                Ok(Box::new(f.take(idx.logical)))
            }
            DataShape::LegacySparse => {
                let sp = self.replay_legacy_sparse(idx)?;
                // spool 保活：包一层 SelfRead（spool 随读游移动），以路径重开读句柄
                let rf = sp.reopen().map_err(HzipError::from)?;
                Ok(Box::new(KeepAlive {
                    _sp: sp,
                    r: std::io::BufReader::new(rf),
                }))
            }
            DataShape::PaxSparse { extents } => {
                let mut f = self.open_file()?;
                f.seek(SeekFrom::Start(idx.data_off))
                    .map_err(|e| variant::map_err(e, self.format.clone()))?;
                let extents = extents.clone();
                Ok(Box::new(SparsePaxRead {
                    inner: std::io::BufReader::new(f.take(idx.phys)),
                    extents,
                    logical: idx.logical,
                    pos: 0,
                    extent_idx: 0,
                }))
            }
        }
    }
}

/// 保活包装：持有 spool（其 Drop 删临时文件）并转发读取。
struct KeepAlive {
    _sp: crate::spool::Spool,
    r: std::io::BufReader<std::fs::File>,
}
impl Read for KeepAlive {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.r.read(buf)
    }
}

/// mtime：pax 覆盖优先（f64 秒+小数→纳秒），否则头秒。溢出/非法 → None（不 Err）。
fn mtime_of(pax: Option<f64>, header: Option<u64>) -> Option<OffsetDateTime> {
    if let Some(f) = pax
        && f.is_finite()
        && f >= 0.0
    {
        let secs = f as i64;
        let frac_nanos = ((f - secs as f64) * 1e9).round() as i64;
        if let Ok(base) = OffsetDateTime::from_unix_timestamp(secs) {
            if frac_nanos != 0 {
                if let Some(t) = base.checked_add(time::Duration::nanoseconds(frac_nanos)) {
                    return Some(t);
                }
            } else {
                return Some(base);
            }
        }
    }
    header.and_then(|s| OffsetDateTime::from_unix_timestamp(s as i64).ok())
}

fn round_up(v: u64, a: u64) -> u64 {
    v.checked_add(a - 1).map(|x| x / a * a).unwrap_or(u64::MAX)
}

impl ArchiveReader for TarReader {
    fn caps(&self) -> FormatCaps {
        caps::caps()
    }

    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        Ok(self.index()?.into_iter().map(|i| i.info).collect())
    }

    fn read_entry(
        &self,
        id: usize,
        _pw: PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        let idx = self.index()?;
        let e = idx.get(id).ok_or_else(|| self.bad_header("条目 id 越界"))?;
        if e.info.is_dir {
            return Ok(Box::new(io::empty()));
        }
        self.entry_reader(e)
    }

    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        let idx = self.index()?;
        let ids = resolve_selection(&idx, sel)?;
        let mut report = ExtractReport::default();
        for id in ids {
            let e = &idx[id];
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            let out_path = dest.join(e.info.name.as_str());
            if e.info.is_dir {
                std::fs::create_dir_all(&out_path).map_err(HzipError::from)?;
                report.entries_done += 1;
                continue;
            }
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(HzipError::from)?;
            }
            let r = std::fs::File::create(&out_path)
                .map_err(HzipError::from)
                .and_then(|mut f| {
                    // PaxSparse：显式稀疏落盘（fs_util 联动，02 §8.4「按映射稀疏写」）；
                    // 其余形态经 entry_reader 逻辑流（含洞零）dense 直写，字节一致。
                    if let DataShape::PaxSparse { extents } = &e.shape {
                        let mut pf = self.open_file()?;
                        pf.seek(SeekFrom::Start(e.data_off))
                            .map_err(|ioe| variant::map_err(ioe, self.format.clone()))?;
                        return crate::sparse::write_sparse_pax(
                            &mut f,
                            extents,
                            e.logical,
                            &mut std::io::BufReader::new(pf.take(e.phys)),
                            &self.format,
                        );
                    }
                    let mut rdr = self.entry_reader(e)?;
                    io::copy(&mut rdr, &mut f)
                        .map_err(|ioe| variant::map_err(ioe, self.format.clone()))?;
                    Ok(())
                });
            match r {
                Ok(()) => {
                    report.entries_done += 1;
                    report.bytes_written += e.logical;
                    if let Some(mt) = e.info.mtime {
                        let _ = apply_mtime(&out_path, mt);
                    }
                }
                Err(err) if ctx.best_effort => {
                    let _ = std::fs::remove_file(&out_path);
                    report.failures.push((id, Box::new(err)));
                }
                Err(err) => return Err(err),
            }
        }
        Ok(report)
    }

    fn test(&self, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        let idx = self.index()?;
        let mut report = ExtractReport::default();
        for e in &idx {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            if e.info.is_dir || e.info.is_symlink {
                report.entries_done += 1;
                continue;
            }
            let mut rdr = self.entry_reader(e)?;
            let n = io::copy(&mut rdr, &mut io::sink())
                .map_err(|ioe| variant::map_err(ioe, self.format.clone()))?;
            if n != e.logical {
                return Err(HzipError::Format {
                    format: self.format.clone(),
                    kind: FormatErrorKind::Truncated,
                });
            }
            report.entries_done += 1;
            report.bytes_written += e.logical;
        }
        Ok(report)
    }
}

/// pax 1.0 稀疏逻辑流重建（洞=零）。
struct SparsePaxRead<R: Read> {
    inner: R,
    extents: Vec<(u64, u64)>,
    logical: u64,
    pos: u64,
    extent_idx: usize,
}

impl<R: Read> Read for SparsePaxRead<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.logical || out.is_empty() {
            return Ok(0);
        }
        let mut filled = 0usize;
        while self.pos < self.logical && filled < out.len() {
            let (off, len) = self
                .extents
                .get(self.extent_idx)
                .copied()
                .unwrap_or((u64::MAX, 0));
            if off + len <= self.pos {
                self.extent_idx += 1;
                continue;
            }
            if self.pos < off {
                let z = ((off - self.pos) as usize)
                    .min(out.len() - filled)
                    .min(65536);
                out[filled..filled + z].fill(0);
                self.pos += z as u64;
                filled += z;
            } else {
                let in_ext = (self.pos - off) as usize;
                let avail = (len as usize)
                    .saturating_sub(in_ext)
                    .min(out.len() - filled);
                if avail == 0 {
                    self.extent_idx += 1;
                    continue;
                }
                let n = self.inner.read(&mut out[filled..filled + avail])?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "稀疏数据不足"));
                }
                self.pos += n as u64;
                filled += n;
            }
        }
        Ok(filled)
    }
}

fn resolve_selection(idx: &[IndexEntry], sel: &Selection) -> Result<Vec<usize>, HzipError> {
    let fmt = FormatId::from(crate::FORMAT_ID);
    match sel {
        Selection::All => Ok((0..idx.len()).collect()),
        Selection::Ids(ids) => {
            if ids.iter().any(|&i| i >= idx.len()) {
                return Err(HzipError::Format {
                    format: fmt,
                    kind: FormatErrorKind::BadHeader,
                });
            }
            Ok(ids.clone())
        }
        Selection::Names(names) => Ok(idx
            .iter()
            .enumerate()
            .filter(|(_, e)| names.contains(&e.info.name))
            .map(|(i, _)| i)
            .collect()),
        _ => Err(HzipError::Unsupported {
            what: "未知 Selection 变体".into(),
        }),
    }
}

fn apply_mtime(path: &Path, t: OffsetDateTime) -> Result<(), HzipError> {
    let mut meta = hzip_platform::metadata::FileMeta::default();
    meta.mtime = Some(t);
    hzip_platform::metadata::apply_meta(path, &meta).map_err(HzipError::from)
}
