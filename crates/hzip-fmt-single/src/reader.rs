//! `SingleReader`：单文件流读取端（02 §8.5 / T23 §4.6）。
//!
//! - **单条目语义**：恒恰一条 `EntryInfo { id: 0 }`；`read_entry(id != 0)` →
//!   `Err Format{BadHeader}`；
//! - **`entries()` 不读数据区**（红线 1，TC-CONF-010）：`open_reader` 阶段只解析
//!   流头（gzip 头 mtime/FNAME、魔数确认）+ `stat` 档案；`size_raw` 取**容器声明的
//!   原始大小**（gz 尾部 ISIZE / xz 索引 / zstd 帧头；未知为 0——见
//!   [`crate::declared_size`]）、`size_packed` = 档案文件大小；
//! - **可重放源**（File）：每次 `read_entry(0)` 重开文件 → 天然支持乱序/重复
//!   读取（红线 2）；**不可重放源**（Stdin/Stream）：一次性，经 [`ReplayReader`]
//!   保留头预读字节后解码，二次调用返回 `Err Unsupported`（记录遗留：一次性流
//!   无法重放，02 §7.4 Stdin 语义）。
//! - **extract / test**：全量解码 + 容器尾校验（底层库）+ 取消检查点
//!   （每 `COPY_CHUNK_BYTES`）；extract 取消时删除半写文件（TC-CONF-006）。

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::consts::COPY_CHUNK_BYTES;
use hzip_format::{
    ArchiveReader, ConflictPolicy, EntryInfo, ExtractCtx, ExtractReport, FormatCaps, FormatId,
    NameEncoding, OpenOptions, Selection, Source,
};

use crate::handler::{SINGLE_CAPS, SingleFmt};
use crate::name::{NameShape, derive_entry_name};
use crate::streams::ReplayReader;

/// 头解析产物（`entries()` 用；数据区未触及）。
#[derive(Debug, Default, Clone)]
pub(crate) struct ParsedHead {
    /// 提取的 mtime（gzip 头字段 / zstd hzip 私有帧；xz/bz2 恒 `None`）。
    pub(crate) mtime: Option<time::OffsetDateTime>,
    /// gzip FNAME（已解码字符串；未过 `parse_entry_name`，供优先级 1 派生）。
    pub(crate) fname: Option<String>,
    /// **容器声明的原始大小**（目前只有 zstd 帧头带；gz/xz 的大小在容器**尾部**，
    /// 由 [`crate::declared_size`] 直接 seek 取——见 `open`）。
    pub(crate) raw_size: Option<u64>,
}

/// 读取源形态（不可重放源以 `RefCell` 支持 `&self` 契约下的 take-once）。
pub(crate) enum InnerSource {
    /// 可重放（每次重开文件）。
    File(PathBuf),
    /// 一次性流（保留头预读字节；二次取用报错）。
    Once(RefCell<Option<ReplayReader>>),
}

pub struct SingleReader {
    fmt: SingleFmt,
    src: InnerSource,
    info: EntryInfo,
}

impl SingleReader {
    pub(crate) fn open(fmt: SingleFmt, src: Source, opts: &OpenOptions) -> Result<Self, HzipError> {
        // Volumes：单文件流无分卷语义（02 §8.5 无分卷）；直接拒绝
        if let Source::Volumes { .. } = &src {
            return Err(HzipError::Unsupported {
                what: format!("{} 单文件流不支持分卷源", fmt.id()).into(),
            });
        }
        // 名字推导的源形态（derive 在 head 就绪后执行，这里先提取借用信息）
        let shape = NameShape::from_source(&src);

        // 预解析头（gzip mtime/FNAME / zstd 私有帧 / 魔数确认）。head 随源分派：
        // File → 独立句柄（解析后丢弃，read_entry 重开）；Stdin/Stream →
        // ReplayReader 捕获头预读字节（供 read_entry 重放）。
        let (inner, size_packed, head) = match src {
            Source::File(p) => {
                let meta = std::fs::metadata(&p).map_err(crate::map_io)?;
                let f = std::fs::File::open(&p).map_err(crate::map_io)?;
                let mut probe = std::io::BufReader::new(f);
                let mut head = parse_head(fmt, &mut probe)?;
                // 容器声明的大小：帧头给的（zstd）或尾部给的（gz ISIZE / xz 索引）。
                // 只读元数据、不碰数据区（见 `declared_size` 模块文档）。
                head.raw_size = head
                    .raw_size
                    .or_else(|| crate::declared_size::declared_raw_size(fmt, &p, meta.len()));
                (InnerSource::File(p), Some(meta.len()), head)
            }
            Source::Volumes { .. } => unreachable!("上方已拒绝 Volumes"),
            Source::Stdin => {
                let mut rr = ReplayReader::new(Box::new(std::io::stdin()));
                rr.start_capture();
                let head = parse_head(fmt, &mut rr)?;
                rr.stop_capture();
                (InnerSource::Once(RefCell::new(Some(rr))), None, head)
            }
            Source::Stream(s) => {
                let mut rr = ReplayReader::new(s);
                rr.start_capture();
                let head = parse_head(fmt, &mut rr)?;
                rr.stop_capture();
                (InnerSource::Once(RefCell::new(Some(rr))), None, head)
            }
        };

        // 条目名推导（FNAME > entry_name > 文件名剥一层 > stdin/格式兜底）
        let fmt_id = FormatId::from(fmt.id());
        let name = derive_entry_name(
            &shape,
            &fmt_id,
            fmt.extensions(),
            head.fname.as_deref(),
            opts.entry_name.as_ref(),
        );

        let info = EntryInfo {
            id: 0,
            name,
            is_dir: false,
            is_symlink: false,
            symlink_target: None,
            // 容器声明时给出（gz/xz 尾部、zstd 帧头）；未知/不可信仍为 0
            size_raw: head.raw_size.unwrap_or(0),
            size_packed: size_packed.unwrap_or(0),
            mtime: head.mtime,
            mode: None,
            attributes: Default::default(),
            encrypted: false,
            header_encrypted: false,
            crc32: None,
            codec: Some(hzip_codec::traits::CodecId::from(fmt.id())),
            name_encoding: NameEncoding::Unknown,
            encryption: None,
            compression_level: None,
        };

        Ok(Self {
            fmt,
            src: inner,
            info,
        })
    }

    /// 取原始源解码流（File 重开 / Once take-once）。
    fn decoder_from_source(&self) -> Result<Box<dyn Read + Send>, HzipError> {
        let inner: Box<dyn Read + Send> = match &self.src {
            InnerSource::File(p) => Box::new(std::io::BufReader::new(
                std::fs::File::open(p).map_err(crate::map_io)?,
            )),
            InnerSource::Once(cell) => {
                let mut borrow = cell.borrow_mut();
                match borrow.take() {
                    Some(rr) => Box::new(rr),
                    None => {
                        return Err(HzipError::Unsupported {
                            what: format!("{} 一次性源已消费（Stdin/Stream）", self.fmt.id())
                                .into(),
                        });
                    }
                }
            }
        };
        wrap_decoder(self.fmt, inner)
    }
}

// ===================== ArchiveReader 实现 =====================

impl ArchiveReader for SingleReader {
    fn caps(&self) -> FormatCaps {
        SINGLE_CAPS
    }

    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        Ok(vec![self.info.clone()])
    }

    fn read_entry(
        &self,
        id: usize,
        _pw: hzip_format::PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        if id != 0 {
            return Err(HzipError::Format {
                format: FormatId::from(self.fmt.id()),
                kind: FormatErrorKind::BadHeader,
            });
        }
        self.decoder_from_source()
    }

    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        // 单条目 Selection 语义（T23 §6 note：single 特例降级）
        let want = match sel {
            Selection::All => true,
            Selection::Ids(ids) => ids.contains(&0),
            Selection::Names(names) => names.contains(&self.info.name),
            _ => return Ok(ExtractReport::default()),
        };
        if !want {
            return Ok(ExtractReport::default());
        }

        let target = dest.join(self.info.name.as_str());
        if let Some(parent) = target.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(crate::map_io)?;
        }

        // 冲突策略（core 已决策净化/冲突；本层按 ctx 基本覆盖语义处理已存在目标）
        if target.exists() && !target.is_file() {
            return Err(HzipError::Unsupported {
                what: format!("目标存在且非普通文件: {}", target.display()).into(),
            });
        }
        match (ctx.conflicts, target.exists()) {
            (ConflictPolicy::Skip, true) => return Ok(ExtractReport::default()),
            (ConflictPolicy::RenameAuto, true) => {
                return Err(HzipError::Unsupported {
                    what: "RenameAuto 决策归 core（本层单条目无改名语义）".into(),
                });
            }
            (ConflictPolicy::Ask, true) => {
                return Err(HzipError::Unsupported {
                    what: "Ask 决策归 core（本层无请求-应答通道）".into(),
                });
            }
            // Overwrite / NewerOnly / 默认：直接覆盖
            _ => {}
        }

        let mut decoder = self
            .read_entry(0, hzip_format::PasswordRef::none())
            .map_err(|e| {
                tracing::warn!(err = ?e, "extract read_entry 失败");
                e
            })?;

        let mut out = std::fs::File::create(&target).map_err(crate::map_io)?;
        let copy_result = (|| -> std::io::Result<u64> {
            let mut buf = vec![0u8; COPY_CHUNK_BYTES as usize];
            let mut total = 0u64;
            loop {
                if ctx.cancel.is_cancelled() {
                    return Err(std::io::Error::other("cancelled"));
                }
                let n = decoder.read(&mut buf)?;
                if n == 0 {
                    return Ok(total);
                }
                out.write_all(&buf[..n])?;
                total += n as u64;
                ctx.progress
                    .send(hzip_format::ProgressEvent::Bytes { delta: n as u64 });
            }
        })();

        match copy_result {
            Ok(bytes) => Ok(ExtractReport {
                entries_done: 1,
                bytes_written: bytes,
                failures: vec![],
            }),
            Err(e) => {
                // 清理半写文件（取消/失败时删除，TC-CONF-006）
                let _ = std::fs::remove_file(&target);
                if ctx.cancel.is_cancelled() {
                    Err(HzipError::Cancelled)
                } else {
                    Err(crate::map_io(e))
                }
            }
        }
    }

    fn test(&self, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        let mut decoder = self
            .read_entry(0, hzip_format::PasswordRef::none())
            .map_err(|e| {
                tracing::warn!(err = ?e, "test read_entry 失败");
                e
            })?;
        let result = (|| -> std::io::Result<u64> {
            let mut buf = vec![0u8; COPY_CHUNK_BYTES as usize];
            let mut total = 0u64;
            loop {
                if ctx.cancel.is_cancelled() {
                    return Err(std::io::Error::other("cancelled"));
                }
                let n = decoder.read(&mut buf)?;
                if n == 0 {
                    return Ok(total);
                }
                total += n as u64;
                ctx.progress
                    .send(hzip_format::ProgressEvent::Bytes { delta: n as u64 });
            }
        })();
        match result {
            Ok(bytes) => Ok(ExtractReport {
                entries_done: 1,
                bytes_written: bytes,
                failures: vec![],
            }),
            Err(e) => {
                if ctx.cancel.is_cancelled() {
                    Err(HzipError::Cancelled)
                } else {
                    Err(crate::map_io(e))
                }
            }
        }
    }
}

// ===================== 头解析与解码包装 =====================

/// 解析流头（不触数据区）。gzip 读 mtime/FNAME；zstd 读前置 hzip 私有帧；
/// bz2/xz 仅确认魔数。
fn parse_head(fmt: SingleFmt, r: &mut dyn Read) -> Result<ParsedHead, HzipError> {
    match fmt {
        #[cfg(feature = "gz")]
        SingleFmt::Gz => parse_gzip_head(r),
        #[cfg(feature = "bz2")]
        SingleFmt::Bz2 => parse_bz2_head(r),
        #[cfg(feature = "xz")]
        SingleFmt::Xz => parse_xz_head(r),
        #[cfg(feature = "zst")]
        SingleFmt::Zst => parse_zstd_head(r),
        #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
        SingleFmt::Placeholder => Err(HzipError::Unsupported {
            what: "无启用的单文件流格式".into(),
        }),
    }
}

/// 包装解码器（多成员串联 + 容器尾校验；§4.5）。
fn wrap_decoder(
    fmt: SingleFmt,
    inner: Box<dyn Read + Send>,
) -> Result<Box<dyn Read + Send>, HzipError> {
    // gz/bz2/xz 的解码器构造不返回 io::Result（Multi*::new）；错误延迟到首次 read。
    // zstd::Decoder::new 会立即校验帧头 → io 错误在此处转 Format。
    match fmt {
        #[cfg(feature = "gz")]
        SingleFmt::Gz => Ok(Box::new(flate2::read::MultiGzDecoder::new(inner))),
        #[cfg(feature = "bz2")]
        SingleFmt::Bz2 => Ok(Box::new(bzip2::read::MultiBzDecoder::new(inner))),
        #[cfg(feature = "xz")]
        SingleFmt::Xz => Ok(Box::new(xz2::read::XzDecoder::new_multi_decoder(inner))),
        #[cfg(feature = "zst")]
        SingleFmt::Zst => zstd::stream::read::Decoder::new(inner)
            .map(|d| Box::new(d) as Box<dyn Read + Send>)
            .map_err(|e| decode_init_err(fmt, e)),
        #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
        SingleFmt::Placeholder => Err(HzipError::Unsupported {
            what: "无启用的单文件流格式".into(),
        }),
    }
}

/// 解码器构造错误映射（zstd 帧头在构造期即校验）。
fn decode_init_err(fmt: SingleFmt, e: std::io::Error) -> HzipError {
    match e.kind() {
        std::io::ErrorKind::UnexpectedEof => HzipError::Format {
            format: FormatId::from(fmt.id()),
            kind: FormatErrorKind::Truncated,
        },
        _ => HzipError::Format {
            format: FormatId::from(fmt.id()),
            kind: FormatErrorKind::BadHeader,
        },
    }
}

/// 读满 `want` 字节；EOF 提前返回不足部分（不视为错误）。
fn read_up_to(r: &mut dyn Read, want: usize) -> std::io::Result<Vec<u8>> {
    let mut out = vec![0u8; want];
    let mut filled = 0usize;
    while filled < want {
        let n = r.read(&mut out[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    out.truncate(filled);
    Ok(out)
}

fn bad_head(fmt: &SingleFmt, kind: FormatErrorKind) -> HzipError {
    HzipError::Format {
        format: FormatId::from(fmt.id()),
        kind,
    }
}

/// 确认魔数（不足/不匹配 → BadSignature/Truncated）。
#[cfg(any(feature = "bz2", feature = "xz"))]
fn confirm_magic(fmt: SingleFmt, r: &mut dyn Read, magic: &[u8]) -> Result<(), HzipError> {
    let got = read_up_to(r, magic.len()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            bad_head(&fmt, FormatErrorKind::Truncated)
        } else {
            crate::map_io(e)
        }
    })?;
    if got.len() < magic.len() {
        return Err(bad_head(&fmt, FormatErrorKind::Truncated));
    }
    if got != magic {
        return Err(bad_head(&fmt, FormatErrorKind::BadSignature));
    }
    Ok(())
}

// ---------- gzip ----------

/// gzip 头：魔数(2) + 方法(1) + flags(1) + mtime(4) + xfl(1) + os(1)
/// [+ FEXTRA] [+ FNAME(NUL 结尾)] [+ FCOMMENT(NUL 结尾)] [+ FHCRC(2)]
/// 解析 mtime 与 FNAME（NUL 截断；字节 → UTF-8/Latin-1 解码，任务卡 §4.3）。
#[cfg(feature = "gz")]
fn parse_gzip_head(r: &mut dyn Read) -> Result<ParsedHead, HzipError> {
    const FLAG_FHCRC: u8 = 0x02;
    const FLAG_FEXTRA: u8 = 0x04;
    const FLAG_FNAME: u8 = 0x08;
    const FLAG_FCOMMENT: u8 = 0x10;
    const FLAG_RESERVED: u8 = 0xE0;

    let fmt = SingleFmt::Gz;
    let hdr = read_up_to(r, 10).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            bad_head(&fmt, FormatErrorKind::Truncated)
        } else {
            crate::map_io(e)
        }
    })?;
    if hdr.len() < 10 {
        return Err(bad_head(&fmt, FormatErrorKind::Truncated));
    }
    if hdr[0] != 0x1f || hdr[1] != 0x8b {
        return Err(bad_head(&fmt, FormatErrorKind::BadSignature));
    }
    if hdr[2] != 8 {
        return Err(bad_head(&fmt, FormatErrorKind::BadHeader));
    }
    let flags = hdr[3];
    if flags & FLAG_RESERVED != 0 {
        return Err(bad_head(&fmt, FormatErrorKind::BadHeader));
    }
    let mtime_secs = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    let mtime = crate::mtime::mtime_from_gzip_field(mtime_secs);

    let mut fname: Option<String> = None;

    if flags & FLAG_FEXTRA != 0 {
        let xl = read_up_to(r, 2).map_err(crate::map_io)?;
        if xl.len() < 2 {
            return Err(bad_head(&fmt, FormatErrorKind::Truncated));
        }
        let xlen = u16::from_le_bytes([xl[0], xl[1]]) as usize;
        let skip = read_up_to(r, xlen).map_err(crate::map_io)?;
        if skip.len() < xlen {
            return Err(bad_head(&fmt, FormatErrorKind::Truncated));
        }
    }
    if flags & FLAG_FNAME != 0 {
        // NUL 结尾读取
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = r.read(&mut byte).map_err(crate::map_io)?;
            if n == 0 {
                return Err(bad_head(&fmt, FormatErrorKind::Truncated));
            }
            if byte[0] == 0 {
                break;
            }
            raw.push(byte[0]);
        }
        fname = decode_fname(&fmt, &raw);
    }
    if flags & FLAG_FCOMMENT != 0 {
        let mut byte = [0u8; 1];
        loop {
            let n = r.read(&mut byte).map_err(crate::map_io)?;
            if n == 0 {
                return Err(bad_head(&fmt, FormatErrorKind::Truncated));
            }
            if byte[0] == 0 {
                break;
            }
        }
    }
    if flags & FLAG_FHCRC != 0 {
        let _ = read_up_to(r, 2).map_err(crate::map_io)?;
    }
    Ok(ParsedHead {
        mtime,
        fname,
        raw_size: None, // gz 的大小在尾部，由 declared_size 另取
    })
}

/// FNAME 字节 → 字符串：合法 UTF-8 用 UTF-8；否则按 Latin-1 逐字节映射
/// （gzip 规范 ISO-8859-1；带 NUL/控制字符由 `parse_entry_name` 在派生阶段拒绝
/// 后降级省略——本层解码保持无损，降级决策集中在 name.rs）。
#[cfg(feature = "gz")]
fn decode_fname(fmt: &SingleFmt, raw: &[u8]) -> Option<String> {
    match std::str::from_utf8(raw) {
        Ok(s) => Some(s.to_owned()),
        Err(_) => {
            tracing::debug!(format = %fmt.id(), "gzip FNAME 非 UTF-8，按 Latin-1 解码");
            Some(raw.iter().map(|&b| b as char).collect())
        }
    }
}

// ---------- bzip2 ----------

/// bzip2：仅确认魔数 `BZh` + 级别位（'1'..='9'），无 mtime/FNAME。
#[cfg(feature = "bz2")]
fn parse_bz2_head(r: &mut dyn Read) -> Result<ParsedHead, HzipError> {
    let fmt = SingleFmt::Bz2;
    confirm_magic(fmt, r, b"BZh")?;
    let lvl = read_up_to(r, 1).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            bad_head(&fmt, FormatErrorKind::Truncated)
        } else {
            crate::map_io(e)
        }
    })?;
    if lvl.is_empty() || !(b'1'..=b'9').contains(&lvl[0]) {
        return Err(bad_head(&fmt, FormatErrorKind::BadHeader));
    }
    Ok(ParsedHead::default())
}

// ---------- xz ----------

/// xz：仅确认魔数（6 B），无 mtime/FNAME。
#[cfg(feature = "xz")]
fn parse_xz_head(r: &mut dyn Read) -> Result<ParsedHead, HzipError> {
    let fmt = SingleFmt::Xz;
    confirm_magic(fmt, r, b"\xfd7zXZ\x00")?;
    Ok(ParsedHead::default())
}

/// zstd 帧头的 `Frame_Content_Size`（可选字段；未写 → `Ok(None)`）。
///
/// 帧头布局（RFC 8878 §3.1.1）：`Frame_Header_Descriptor`(1 B) 之后按标志位依次是
/// `Window_Descriptor`（仅非单段帧）、`Dictionary_ID`（0/1/2/4 B）、`Frame_Content_Size`
/// （0/1/2/4/8 B，取值规则见规范：2 B 形式需 +256，1 B 形式仅在单段帧出现）。
/// 解析失败（截断/畸形）→ `Ok(None)`（未知），**不**让整档打不开。
fn parse_zstd_content_size(r: &mut dyn Read) -> Result<Option<u64>, HzipError> {
    let fhd = read_up_to(r, 1).map_err(crate::map_io)?;
    let Some(&fhd) = fhd.first() else {
        return Ok(None);
    };
    // 保留位（bit 3）必须为 0，否则不是标准帧头
    if fhd & 0x08 != 0 {
        return Ok(None);
    }
    let fcs_flag = (fhd & 0xC0) >> 6; // bit 7-6
    let single_segment = fhd & 0x20 != 0; // bit 5
    let dict_flag = fhd & 0x03; // bit 1-0
    if !single_segment {
        // Window_Descriptor
        if read_up_to(r, 1).map_err(crate::map_io)?.is_empty() {
            return Ok(None);
        }
    }
    let dict_bytes = match dict_flag {
        0 => 0usize,
        1 => 1,
        2 => 2,
        _ => 4,
    };
    if dict_bytes > 0 && read_up_to(r, dict_bytes).map_err(crate::map_io)?.len() < dict_bytes {
        return Ok(None);
    }
    let fcs_bytes = match (fcs_flag, single_segment) {
        (0, false) => return Ok(None), // 未写该字段
        (0, true) => 1,
        (1, _) => 2,
        (2, _) => 4,
        _ => 8,
    };
    let raw = read_up_to(r, fcs_bytes).map_err(crate::map_io)?;
    if raw.len() < fcs_bytes {
        return Ok(None);
    }
    let mut v: u64 = 0;
    for (i, b) in raw.iter().enumerate() {
        v |= u64::from(*b) << (8 * i);
    }
    if fcs_flag == 1 {
        v += 256; // 2 字节形式：实际值 = 读到的值 + 256
    }
    Ok(Some(v))
}

// ---------- zstd ----------

/// zstd 帧魔数（标准帧起始）。
const ZSTD_MAGIC: &[u8; 4] = &[0x28, 0xb5, 0x2f, 0xfd];

/// 判断 zstd skippable 帧（标准魔数 `0x184D2A50..=0x184D2A5F` 小端字节序：
/// `[0x50..=0x5F, 0x2A, 0x4D, 0x18]`；T23 §4.5-4「魔数高 28 位 0x184D2A5?」）。
fn is_zstd_skippable(head4: &[u8]) -> bool {
    head4.len() >= 4
        && head4[0] >= 0x50
        && head4[0] <= 0x5f
        && head4[1] == 0x2a
        && head4[2] == 0x4d
        && head4[3] == 0x18
}

/// zstd 头：扫描**前置** skippable 帧；命中 hzip 私有 mtime 帧 → 提取 mtime。
/// 标准帧魔数即停（后续 skippable 帧由 zstd Decoder 自动跳过，帧间拼接内建）。
#[cfg(feature = "zst")]
fn parse_zstd_head(r: &mut dyn Read) -> Result<ParsedHead, HzipError> {
    let fmt = SingleFmt::Zst;
    let mut head = ParsedHead::default();
    // 最多扫 16 个前置 skippable 帧防恶意循环（实际 hzip 私有帧极小）。
    for _ in 0..16 {
        let magic = read_up_to(r, 4).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                bad_head(&fmt, FormatErrorKind::Truncated)
            } else {
                crate::map_io(e)
            }
        })?;
        if magic.is_empty() {
            return Err(bad_head(&fmt, FormatErrorKind::Truncated));
        }
        if magic == *ZSTD_MAGIC {
            // 标准帧开始：顺带取帧头里的 Frame_Content_Size（写了才有；见
            // `declared_size` 模块文档——外部 zstd 默认写、hzip 自产不写）。
            head.raw_size = parse_zstd_content_size(r).ok().flatten();
            return Ok(head);
        }
        if !is_zstd_skippable(&magic) {
            return Err(bad_head(&fmt, FormatErrorKind::BadSignature));
        }
        // skippable：读 4B 帧大小 + 载荷
        let len_b = read_up_to(r, 4).map_err(crate::map_io)?;
        if len_b.len() < 4 {
            return Err(bad_head(&fmt, FormatErrorKind::Truncated));
        }
        let len = u32::from_le_bytes([len_b[0], len_b[1], len_b[2], len_b[3]]) as usize;
        if len > 64 * 1024 * 1024 {
            // 防御恶意超大帧
            return Err(bad_head(&fmt, FormatErrorKind::BadHeader));
        }
        let payload = read_up_to(r, len).map_err(crate::map_io)?;
        if payload.len() < len {
            return Err(bad_head(&fmt, FormatErrorKind::Truncated));
        }
        if head.mtime.is_none()
            && let Some(t) = crate::mtime::parse_hzip_mtime_payload(&payload)
        {
            head.mtime = Some(t);
        }
    }
    // 16 帧后仍未到标准帧：拒绝（防前置 skippable 洪泛）
    Err(bad_head(&fmt, FormatErrorKind::BadHeader))
}

// ===================== 单元测试 =====================

#[cfg(all(test, feature = "gz"))]
mod tests {
    use super::*;
    use hzip_format::{FormatHandler, Source};
    use std::io::Write;

    fn sample_data() -> Vec<u8> {
        b"the quick brown fox jumps over the lazy dog".repeat(100)
    }

    fn gz_bytes(data: &[u8], mtime: u32, fname: Option<&str>) -> Vec<u8> {
        let mut b = flate2::GzBuilder::new();
        if let Some(n) = fname {
            b = b.filename(n);
        }
        b = b.mtime(mtime);
        let mut e = b.write(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn to_temp(data: &[u8], ext: &str) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(format!("f.{ext}"));
        std::fs::write(&p, data).unwrap();
        (d, p)
    }

    #[test]
    fn gzip_head_parses_mtime_and_fname() {
        let data = gz_bytes(b"hello", 1_700_000_000, Some("a.txt"));
        let (d, p) = to_temp(&data, "gz");
        let h = crate::handler::gz();
        let r = h.open_reader(Source::File(p), &Default::default()).unwrap();
        let es = r.entries().unwrap();
        assert_eq!(es[0].mtime.unwrap().unix_timestamp(), 1_700_000_000);
        assert_eq!(es[0].name.as_str(), "a.txt");
        drop(d);
    }

    #[test]
    fn gzip_head_zero_mtime_none() {
        let data = gz_bytes(b"hello", 0, None);
        let (d, p) = to_temp(&data, "gz");
        let h = crate::handler::gz();
        let r = h.open_reader(Source::File(p), &Default::default()).unwrap();
        let es = r.entries().unwrap();
        assert!(es[0].mtime.is_none());
        drop(d);
    }

    #[test]
    fn gzip_head_bad_magic() {
        let (d, p) = to_temp(b"not a gzip file at all!!", "gz");
        let h = crate::handler::gz();
        let err = h
            .open_reader(Source::File(p), &Default::default())
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.code(), 2001); // BadSignature
        drop(d);
    }

    #[test]
    fn gzip_head_truncated() {
        let data = gz_bytes(b"hello", 0, None);
        let (d, p) = to_temp(&data[..6], "gz");
        let h = crate::handler::gz();
        let err = h
            .open_reader(Source::File(p), &Default::default())
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.code(), 2002); // Truncated
        drop(d);
    }

    #[test]
    fn entry_single_and_name_derivation() {
        let data = gz_bytes(&sample_data(), 1_700_000_100, Some("f.txt"));
        let (d, p) = to_temp(&data, "gz");
        let h = crate::handler::gz();
        let r = h.open_reader(Source::File(p), &Default::default()).unwrap();
        let es = r.entries().unwrap();
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].id, 0);
        assert_eq!(es[0].name.as_str(), "f.txt"); // FNAME 优先
        // 契约修订（2026-09-24）：容器元数据写着原始大小时上报它（gz 尾部 ISIZE）
        // ——core 进度层的 total 靠它才能算出百分比（此前恒 0 ⇒ 解压大文件永远 0%）
        assert_eq!(
            es[0].size_raw,
            sample_data().len() as u64,
            "gz 尾部 ISIZE = 原始大小"
        );
        assert_eq!(es[0].mtime.unwrap().unix_timestamp(), 1_700_000_100);
        drop(d);
    }

    #[test]
    fn name_falls_back_to_filename_without_fname() {
        let data = gz_bytes(&sample_data(), 0, None);
        let (d, p) = to_temp(&data, "gz");
        // 存档文件名 a.txt.gz（tempfile 无后缀）→ 用 f.gz 剥 gz 得 f
        let h = crate::handler::gz();
        let r = h
            .open_reader(Source::File(p.clone()), &Default::default())
            .unwrap();
        let es = r.entries().unwrap();
        assert_eq!(es[0].name.as_str(), "f");
        drop(d);
    }

    #[test]
    fn wrong_id_rejected() {
        let data = gz_bytes(&sample_data(), 0, None);
        let (d, p) = to_temp(&data, "gz");
        let h = crate::handler::gz();
        let r = h.open_reader(Source::File(p), &Default::default()).unwrap();
        let err = r
            .read_entry(1, hzip_format::PasswordRef::none())
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.code(), 2004);
        drop(d);
    }
}
