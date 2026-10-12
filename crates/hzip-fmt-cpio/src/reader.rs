//! cpio 读取：newc/crc/odc/bin 四变体 `ArchiveReader`（02 §8.7）。
//!
//! # 设计（02 §7.3 四条契约红线）
//!
//! - **`entries()` 一次顺序扫描建索引**（`Vec<IndexEntry>`：`data_off`/`span` +
//!   `EntryInfo`），数据区一律 `seek` 跳过（`data_span = align4(filesize)`）——
//!   除**符号链接目标**外不读任何数据字节。链接目标是唯一例外：cpio 把它存在
//!   数据区，不读就填不出 `EntryInfo::symlink_target`，故按
//!   [`MAX_SYMLINK_TARGET_BYTES`](crate::consts::MAX_SYMLINK_TARGET_BYTES) 有界读取
//!   （索引期 I/O 与数据区总大小无关 → 大档案列表照样秒开，TC-CONF-010）。
//! - **`read_entry(id)` 按 `data_off` 随机取**（`File::take(filesize)`）：不重放、
//!   可乱序、可重复（红线 2）。cpio 无稀疏、无加密、无压缩 → 数据形态唯一。
//! - **变体以首条头判定，全程必须一致**（GNU cpio 同口径）；不一致 → 2004。
//! - **归档结束**：`TRAILER!!!` 条目、尾部零填充块（GNU cpio `-B` 512 块对齐形态）
//!   或**记录边界处** EOF 三者皆可收尾（后者记 warning——真实世界存在被截掉的
//!   归档尾部）；但**记录内部**截断一律 2002（不静默半条）。
//! - 错误映射经 [`crate::variant::map_err`]（UnexpectedEof→2002 / 坏头→2004 /
//!   坏魔数→2001）。
//!
//! # 对齐口径（GNU cpio 2.14 实测校准）
//!
//! 名字区与数据区**各按自己的起点对齐**，不是「统一自头起点对齐」：
//!
//! | 字段 | 偏移（记录相对） |
//! |---|---|
//! | 数据区起点 | `align(头长 + namesize, align)`（newc 4 / bin 2 / odc 1） |
//! | 下一条头 | `数据区起点 + align(filesize, align)` |
//!
//! 实测样本（`newc`，`link` 条目：`namesize=5`、`filesize=5`）：名区补 1 → 数据
//! 起于 116，数据补 3 → 下一条头落在 124。若按「自头起点对齐」会算成 122（错位
//! 2 字节）——该口径同时决定 Linux initramfs 的可读性，故有 testkit 样例钉死。
//!
//! # 已知限制（如实记录，与 tar 同口径）
//!
//! - **硬链接**：cpio 把数据只存在同 inode 组的**末**条目，其余条目
//!   `filesize == 0`；本层不做链接重建（tar 的 hardlink 条目同样落成空文件），
//!   故解出的前置条目是空文件。
//! - **符号链接目标 > 4 KiB**：`symlink_target = None`（保索引有界）。

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use hzip_error::{FormatErrorKind, HzipError, QuotaKind};
use hzip_format::{
    ArchiveReader, EntryAttrs, EntryInfo, ExtractCtx, ExtractReport, FormatCaps, FormatId,
    NameEncoding, PasswordRef, Selection, parse_entry_name,
};
use time::OffsetDateTime;

use crate::caps;
use crate::consts;
use crate::variant::{self, CpioVariant};

/// 索引条目（[`CpioReader::index`] 一次扫描的结果；`read_entry` 复用）。
#[derive(Debug, Clone)]
struct IndexEntry {
    /// 数据区起点（档案绝对偏移，已对齐）。
    data_off: u64,
    /// 变体（`crc` 的 `check` 校验用）。
    variant: CpioVariant,
    /// 头里的 `check` 域（仅 crc 变体非 0）。
    check: u32,
    /// 条目信息（对调用方可见）。
    info: EntryInfo,
}

/// cpio 读取器（按路径随机访问；Stream/Stdin 源先 spool 成临时文件）。
pub(crate) struct CpioReader {
    path: PathBuf,
    format: FormatId,
    /// 索引缓存（`entries()` 一次扫描；`read_entry` 复用）。
    cache: std::sync::Mutex<Option<Vec<IndexEntry>>>,
    /// Stream/Stdin 源临时文件保活（None = 用户 File）。
    _spool: Option<crate::spool::Spool>,
}

impl CpioReader {
    /// 以档案路径打开（`Source::File`）。
    pub(crate) fn from_path(
        path: PathBuf,
        format: FormatId,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        Self::check_head(&path, &format)?;
        Ok(Box::new(CpioReader {
            path,
            format,
            cache: std::sync::Mutex::new(None),
            _spool: None,
        }))
    }

    /// 以 spool 临时文件打开（`Source::Stream`/`Source::Stdin`；spool 保活于 reader）。
    pub(crate) fn from_path_spooled(
        path: PathBuf,
        spool: crate::spool::Spool,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        let format = crate::format_id();
        Self::check_head(&path, &format)?;
        Ok(Box::new(CpioReader {
            path,
            format,
            cache: std::sync::Mutex::new(None),
            _spool: Some(spool),
        }))
    }

    /// 魔数预检：空档案（0 字节 / 纯零）放行——`entries()` 自然为空；其余首块必须
    /// 命中四变体之一，否则 [`BadSignature`](FormatErrorKind::BadSignature) 2001。
    ///
    /// （与 tar 侧同口径：空档 = 空列表而非错误——`.cpio` 被 `touch` 出来的场景
    /// 不值得报格式错。）
    fn check_head(path: &Path, format: &FormatId) -> Result<(), HzipError> {
        let mut f = std::fs::File::open(path).map_err(|e| variant::map_err(e, format.clone()))?;
        let mut head = [0u8; 6];
        let n = read_full(&mut f, &mut head).map_err(|e| variant::map_err(e, format.clone()))?;
        if n == 0 || head[..n].iter().all(|&b| b == 0) {
            return Ok(());
        }
        if n < 6 || variant::detect(&head[..n]).is_none() {
            return Err(HzipError::Format {
                format: format.clone(),
                kind: FormatErrorKind::BadSignature,
            });
        }
        Ok(())
    }

    /// 索引（带缓存；红线下 `entries()`/`read_entry`/`extract` 共用一次扫描）。
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
        let mut f = std::io::BufReader::new(self.open_file()?);
        self.index_from(&mut f)
    }

    /// 顺序扫描建索引（对**任何** `Read + Seek` 源；单测以计数包络验证「不读数据区」）。
    fn index_from<R: Read + Seek>(&self, r: &mut R) -> Result<Vec<IndexEntry>, HzipError> {
        let mut out: Vec<IndexEntry> = Vec::new();
        let mut pos: u64 = 0;
        let mut variant: Option<CpioVariant> = None;

        loop {
            if out.len() >= consts::MAX_INDEX_ENTRIES {
                return Err(HzipError::Quota {
                    quota: QuotaKind::MaxEntries,
                });
            }
            r.seek(SeekFrom::Start(pos))
                .map_err(|e| variant::map_err(e, self.format.clone()))?;
            let mut probe = [0u8; 6];
            let n =
                read_full(r, &mut probe).map_err(|e| variant::map_err(e, self.format.clone()))?;
            if n == 0 {
                tracing::warn!(pos, "cpio 记录边界处 EOF（无尾条目）");
                break;
            }
            if probe[..n].iter().all(|&b| b == 0) {
                break; // 尾部零填充块（GNU cpio -B / 512 块对齐形态）
            }
            if n < 6 {
                return Err(self.truncated("头不足 6 字节"));
            }
            let detected = variant::detect(&probe[..]).ok_or_else(|| HzipError::Format {
                format: self.format.clone(),
                kind: FormatErrorKind::BadSignature,
            })?;
            let v = match variant {
                Some(prev) if prev != detected => {
                    tracing::warn!(
                        prev = prev.as_str(),
                        got = detected.as_str(),
                        "cpio 变体不一致"
                    );
                    return Err(self.bad_header("同一档案内变体不一致（GNU cpio 只允许单一格式）"));
                }
                Some(prev) => prev,
                None => detected,
            };
            variant = Some(v);

            let hdr_bytes = v.header_bytes();
            let mut head = vec![0u8; hdr_bytes];
            // 探针已把游标推到 `pos + 6`，读整头前必须回到记录起点
            r.seek(SeekFrom::Start(pos))
                .map_err(|e| variant::map_err(e, self.format.clone()))?;
            read_exact_or_truncated(r, &mut head)
                .map_err(|e| variant::map_err(e, self.format.clone()))?;
            let h = variant::parse_header(v, &head)
                .ok_or_else(|| self.bad_header("头字段非法（长度/域编码）"))?;

            // namesize 先验后分配（防头字段构造的内存炸弹）
            if h.namesize == 0 {
                return Err(self.bad_header("namesize 为 0"));
            }
            if h.namesize > consts::MAX_NAME_BYTES {
                tracing::warn!(namesize = h.namesize, "cpio namesize 越界");
                return Err(self.bad_header("namesize 超出上限"));
            }
            let mut name_buf = vec![0u8; h.namesize as usize];
            read_exact_or_truncated(r, &mut name_buf)
                .map_err(|e| variant::map_err(e, self.format.clone()))?;
            // newc/crc/odc/bin 的 namesize 都计入尾 NUL
            let name_raw: &[u8] = name_buf.strip_suffix(&[0u8]).unwrap_or(&name_buf);

            if name_raw == consts::TRAILER_NAME {
                break; // 尾条目：归档正常结束
            }

            // 记录几何（对齐口径见模块文档）
            let data_off_rel = variant::align_up(hdr_bytes as u64 + h.namesize, v.align())
                .ok_or_else(|| self.bad_header("名单区偏移溢出"))?;
            let span = variant::align_up(h.filesize, v.align())
                .ok_or_else(|| self.bad_header("数据区长度溢出"))?;
            let data_off = pos
                .checked_add(data_off_rel)
                .ok_or_else(|| self.bad_header("数据区偏移溢出"))?;
            let next_pos = data_off
                .checked_add(span)
                .ok_or_else(|| self.bad_header("下一条目偏移溢出"))?;

            // 归档根的自我描述条目（`find . | cpio -o` 会写 `.`）不展示成条目
            if hzip_format::is_root_entry_name(name_raw) {
                pos = next_pos;
                continue;
            }

            let mode = h.mode & 0o7777;
            let is_dir = (h.mode & consts::S_IFMT) == consts::S_IFDIR || name_raw.ends_with(b"/");
            let is_symlink = (h.mode & consts::S_IFMT) == consts::S_IFLNK;
            let symlink_target = if is_symlink && h.filesize > 0 {
                if h.filesize > consts::MAX_SYMLINK_TARGET_BYTES {
                    tracing::warn!(filesize = h.filesize, "符号链接目标超上限，按未知处理");
                    None
                } else {
                    let mut t = vec![0u8; h.filesize as usize];
                    r.seek(SeekFrom::Start(data_off))
                        .map_err(|e| variant::map_err(e, self.format.clone()))?;
                    read_exact_or_truncated(r, &mut t)
                        .map_err(|e| variant::map_err(e, self.format.clone()))?;
                    Some(PathBuf::from(String::from_utf8_lossy(&t).into_owned()))
                }
            } else {
                None
            };

            let name_lossy = String::from_utf8_lossy(name_raw).into_owned();
            let name_encoding = if std::str::from_utf8(name_raw).is_ok() {
                NameEncoding::Utf8
            } else {
                NameEncoding::Unknown
            };
            let name = parse_entry_name(&name_lossy).map_err(|_| self.bad_header("条目名非法"))?;

            let info = EntryInfo {
                id: out.len(),
                name,
                is_dir,
                is_symlink,
                symlink_target,
                size_raw: if is_dir { 0 } else { h.filesize },
                size_packed: data_off_rel + span,
                mtime: OffsetDateTime::from_unix_timestamp(h.mtime as i64).ok(),
                mode: Some(mode),
                attributes: EntryAttrs {
                    // 属主写位缺失 = 只读（core 的 apply_attrs 按 is_file 兜底，
                    // 故目录设了也无副作用）
                    read_only: mode & 0o200 == 0,
                    executable: Some(mode & 0o111 != 0),
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
                data_off,
                variant: v,
                check: h.check,
                info,
            });
            pos = next_pos;
        }
        Ok(out)
    }

    fn bad_header(&self, why: &str) -> HzipError {
        tracing::warn!(%why, "cpio 头非法");
        HzipError::Format {
            format: self.format.clone(),
            kind: FormatErrorKind::BadHeader,
        }
    }

    /// 记录内部截断（头/名字/数据区被切掉）——不静默半条。
    fn truncated(&self, why: &str) -> HzipError {
        tracing::warn!(%why, "cpio 记录被截断");
        HzipError::Format {
            format: self.format.clone(),
            kind: FormatErrorKind::Truncated,
        }
    }

    fn open_file(&self) -> Result<std::fs::File, HzipError> {
        std::fs::File::open(&self.path).map_err(|e| variant::map_err(e, self.format.clone()))
    }

    /// 条目数据流（红线 2：随机访问，无重放）。
    fn entry_reader(&self, idx: &IndexEntry) -> Result<Box<dyn Read + Send>, HzipError> {
        let mut f = self.open_file()?;
        f.seek(SeekFrom::Start(idx.data_off))
            .map_err(|e| variant::map_err(e, self.format.clone()))?;
        Ok(Box::new(f.take(idx.info.size_raw)))
    }

    /// crc 变体的数据校验（字节和；`check == 0` = 无校验，GNU 对空文件/链接如此）。
    fn verify_check(&self, idx: &IndexEntry, sum: u32) -> Result<(), HzipError> {
        if idx.variant != CpioVariant::Crc || idx.check == 0 {
            return Ok(());
        }
        if sum != idx.check {
            return Err(HzipError::Checksum {
                entry: idx.info.name.clone(),
                expected: idx.check,
                actual: sum,
            });
        }
        Ok(())
    }

    /// 读条目全部数据并返回（字节和按 `u32` 回绕累加、总长）——`test()` 用。
    fn read_and_sum(&self, idx: &IndexEntry) -> Result<(u64, u32), HzipError> {
        let mut r = self.entry_reader(idx)?;
        let mut buf = vec![0u8; 64 * 1024];
        let (mut total, mut sum) = (0u64, 0u32);
        loop {
            let n = r
                .read(&mut buf)
                .map_err(|e| variant::map_err(e, self.format.clone()))?;
            if n == 0 {
                break;
            }
            sum = buf[..n]
                .iter()
                .map(|&b| u32::from(b))
                .fold(sum, u32::wrapping_add);
            total += n as u64;
        }
        Ok((total, sum))
    }
}

/// 读满 `buf`；返回实际读到的字节数（EOF 时 < `buf.len()`）。
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// 读满 `buf`，不足即 `UnexpectedEof`（经 [`variant::map_err`] 转 2002）。
fn read_exact_or_truncated<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<()> {
    let n = read_full(r, buf)?;
    if n < buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "cpio 记录被截断",
        ));
    }
    Ok(())
}

impl ArchiveReader for CpioReader {
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
                    // 符号链接条目照常落成普通文件（内容 = 目标路径）——链接的
                    // 创建/策略归 core 安全层（`SymlinkGuard`/`LinkAction`），
                    // 与 tar 侧同口径：fmt 的 `extract` 只负责字节落盘。
                    let mut rdr = self.entry_reader(e)?;
                    io::copy(&mut rdr, &mut f)
                        .map_err(|ie| variant::map_err(ie, self.format.clone()))?;
                    Ok(())
                });
            match r {
                Ok(()) => {
                    report.entries_done += 1;
                    report.bytes_written += e.info.size_raw;
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
            // 目录与符号链接不承载文件数据（GNU cpio 对链接写 check=0）
            if e.info.is_dir || e.info.is_symlink {
                report.entries_done += 1;
                continue;
            }
            let (n, sum) = self.read_and_sum(e)?;
            if n != e.info.size_raw {
                return Err(self.truncated("数据区长度与头声明不符"));
            }
            self.verify_check(e, sum)?;
            report.entries_done += 1;
            report.bytes_written += n;
        }
        Ok(report)
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

#[cfg(test)]
#[path = "tests/reader.rs"]
mod tests;
