//! tar 写入：`ArchiveWriter`（02 §8.4 / T22 §4.2 写路径 / §4.5）。
//!
//! # 条目写模型
//!
//! writer 持有**单个** `tar::Builder<TarOutput>`（0.4 的 `Builder::drop` 会自动
//! `finish()` 写 2×512B 尾零块——每次新建必产生垃圾尾块，故 builder 随 writer
//! 创建、仅在 `finish()` 时 `finish()` 一次）。每条目经 [`EntrySink`] 缓冲后
//! （≤4 MiB 内存，超出切临时文件）在 `finish_entry` 时经 `builder.append_data`
//! 落记录：GNU 头（`Header::new_gnu()` 基线）+ 数据 + 512B padding。`append_data`
//! 自动写长名扩展（GNU `L` 头 / pax `path`），保证 259/260+ 字符名可写
//! （TC-ALG-013）。缓冲在先保证头里 `size` 预声明与数据一致——tar 无流式未知
//! 大小（T22 §4.5：core 侧预统计 02 §9.5 正常路径不触发；触发时经临时文件
//! 两遍法，rustdoc 注明性能含义）。
//!
//! `finish()` 调 `builder.finish()` 恰好一次（写 `TRAILER_BLOCKS`×512B 尾零块）
//! 并经 `into_inner()` 回收输出；`Sink::Stdout` 走临时文件两遍法（02 §8.2 同型：
//! 先落盘完成尾块，再顺序拷出 stdout）。`Sink::Split`/`Stream` 不受支持
//! （`caps.split=false`、压缩组合走 core 转换管线，02 §8.4）。

use std::io::{self, Read, Seek, Write};
use std::sync::{Arc, Mutex};

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::{
    ArchiveWriter, EntryMeta, EntrySink, EntrySinkBackend, FormatId, PasswordRef, WriteReport,
};
use hzip_progress::{CancellationToken, ProgressBus};

use crate::Sink;
use crate::spool::Spool;
use crate::variant;

/// 条目级内存缓冲上限（超出切临时文件；防大条目内存峰值）。
const INMEM_LIMIT: u64 = 4 * 1024 * 1024;

/// 输出端：File 直写；Stdout 经临时文件两遍法。统一实现 `Write` 供 builder 持有。
enum TarOutput {
    File(std::fs::File),
    StdoutSpool(Spool),
}

impl Write for TarOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            TarOutput::File(f) => f.write(buf),
            TarOutput::StdoutSpool(sp) => sp.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            TarOutput::File(f) => f.flush(),
            TarOutput::StdoutSpool(sp) => sp.flush(),
        }
    }
}

fn sink_name(s: &Sink) -> &'static str {
    match s {
        Sink::File(_) => "File",
        Sink::Stdout => "Stdout",
        Sink::Split { .. } => "Split",
        Sink::Stream(_) => "Stream",
    }
}

/// 追加场景把 `&mut TarOutput` 视为 `&mut std::fs::File`（仅 File 型）。
fn as_file_mut(o: &mut TarOutput) -> Option<&mut std::fs::File> {
    match o {
        TarOutput::File(f) => Some(f),
        TarOutput::StdoutSpool(_) => None,
    }
}

fn round_up512(v: u64) -> u64 {
    v.checked_add(511)
        .map(|x| x / 512 * 512)
        .unwrap_or(u64::MAX)
}

/// 输出端的打开方式（新建 truncate / 追加保留既有）。
fn open_output(format: &FormatId, sink: Sink, truncate: bool) -> Result<TarOutput, HzipError> {
    let _ = format;
    match sink {
        Sink::File(p) => {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(HzipError::from)?;
            }
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(truncate)
                .open(&p)
                .map_err(HzipError::from)?;
            Ok(TarOutput::File(f))
        }
        Sink::Stdout => Ok(TarOutput::StdoutSpool(
            Spool::new().map_err(HzipError::from)?,
        )),
        other => Err(HzipError::Unsupported {
            what: format!(
                "tar 写入目标不支持（caps.split=false，02 §8.4）：{}",
                sink_name(&other)
            )
            .into(),
        }),
    }
}

/// writer/backend 共享状态（builder 单一实例，防 Drop 垃圾尾块）。
struct Shared {
    format: FormatId,
    /// 持有 output 的 builder（None 仅存在于 finish 取出后）。
    builder: Option<tar::Builder<TarOutput>>,
    entries_written: usize,
    bytes_in: u64,
    bytes_out: u64,
}

/// tar 写入器。
pub(crate) struct TarWriter {
    shared: Arc<Mutex<Shared>>,
    cancel: CancellationToken,
    progress: ProgressBus,
    /// 暂存当前条目 meta（`next_entry` 期间在 self 借期内借出——满足契约
    /// `EntrySink` 与 `&mut self` 同生命周期、且 `from_backend` 需同 `'a`）。
    pending: Option<EntryMeta>,
}

impl TarWriter {
    fn make_shared(
        format: FormatId,
        sink: Sink,
        truncate: bool,
        append_start: u64,
    ) -> Result<Arc<Mutex<Shared>>, HzipError> {
        let output = open_output(&format, sink, truncate)?;
        // 追加场景：打开后文件指针在 0，需 seek 到截断点续写
        let mut builder = tar::Builder::new(output);
        if append_start > 0 {
            use std::io::Seek;
            if let Some(f) = as_file_mut(builder.get_mut()) {
                f.seek(std::io::SeekFrom::Start(append_start))
                    .map_err(|e| variant::map_err(e, format.clone()))?;
            }
        }
        Ok(Arc::new(Mutex::new(Shared {
            format,
            builder: Some(builder),
            entries_written: 0,
            bytes_in: 0,
            bytes_out: append_start,
        })))
    }

    /// 新建写入器（新档案，truncate）。
    pub(crate) fn create(
        format: FormatId,
        sink: Sink,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        let shared = Self::make_shared(format, sink, true, 0)?;
        Ok(Box::new(TarWriter {
            shared,
            cancel: CancellationToken::new(),
            progress: ProgressBus::new(),
            pending: None,
        }))
    }

    /// 追加模式：目标须为已存在裸 tar 文件（尾零块截断后续写，T22 §4.6）。
    /// 目标不存在时退化为新建（`>>` 语义，满足 TC-CONF-005 表面契约
    /// 「create_writer(append=true) 可创建」）。
    pub(crate) fn append(
        format: FormatId,
        sink: Sink,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        let p = match sink {
            Sink::File(p) => p,
            other => {
                return Err(HzipError::Unsupported {
                    what: format!(
                        "tar 追加仅支持裸 tar 文件（{}）；压缩组合走 core 转换管线（02 §8.4）",
                        sink_name(&other)
                    )
                    .into(),
                });
            }
        };
        // 目标不存在 → 退化为新建（truncate）
        if !p.exists() {
            return Self::create(format, Sink::File(p));
        }
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&p)
            .map_err(HzipError::from)?;
        let trunc = crate::append::find_truncate_point(&mut f, &format)?;
        f.set_len(trunc).map_err(HzipError::from)?;
        drop(f);
        // 追加开：open without truncate（保留既有字节），builder 在 make_shared 中
        // seek 到截断点续写。
        let shared = Self::make_shared(format, Sink::File(p), false, trunc)?;
        Ok(Box::new(TarWriter {
            shared,
            cancel: CancellationToken::new(),
            progress: ProgressBus::new(),
            pending: None,
        }))
    }
}

/// 落一条记录（头 + 数据）。
fn emit_record(
    shared: &Mutex<Shared>,
    meta: &EntryMeta,
    data: &mut dyn Read,
    data_len: u64,
) -> Result<(), HzipError> {
    let mut g = shared.lock().unwrap_or_else(|p| p.into_inner());
    let fmt = g.format.clone();
    let mtime = meta
        .mtime
        .map(|t| t.unix_timestamp().max(0) as u64)
        .unwrap_or(0);
    let builder = g.builder.as_mut().ok_or_else(|| HzipError::Format {
        format: fmt.clone(),
        kind: FormatErrorKind::BadHeader,
    })?;
    if meta.is_dir {
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(meta.mode.unwrap_or(0o755));
        h.set_mtime(mtime);
        h.set_entry_type(tar::EntryType::dir());
        builder
            .append_data(&mut h, meta.name.as_str(), io::empty())
            .map_err(|e| variant::map_err(e, fmt))?;
    } else if meta.is_symlink {
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(meta.mode.unwrap_or(0o777));
        h.set_mtime(mtime);
        h.set_entry_type(tar::EntryType::symlink());
        let target = meta
            .symlink_target
            .as_deref()
            .ok_or_else(|| HzipError::Format {
                format: fmt.clone(),
                kind: FormatErrorKind::BadHeader,
            })?;
        builder
            .append_link(&mut h, meta.name.as_str(), target)
            .map_err(|e| variant::map_err(e, fmt))?;
    } else {
        let mut h = tar::Header::new_gnu();
        h.set_size(data_len);
        h.set_mode(meta.mode.unwrap_or(0o644));
        h.set_mtime(mtime);
        h.set_entry_type(tar::EntryType::file());
        builder
            .append_data(&mut h, meta.name.as_str(), data)
            .map_err(|e| variant::map_err(e, fmt))?;
    }
    g.entries_written += 1;
    g.bytes_in += data_len;
    g.bytes_out += 512 + round_up512(data_len);
    Ok(())
}

/// 单条目缓冲 backend：`write` 收集，`finish_entry` 落记录。
///
/// 持有 `meta` 克隆与共享 Arc（无 `&mut self` 借用）。性能注（T22 §4.5）：小条目
/// 内存缓冲；超 [`INMEM_LIMIT`] 切临时文件流式写出。
pub(crate) struct TarEntryBackend {
    shared: Arc<Mutex<Shared>>,
    meta: EntryMeta,
    buf: Vec<u8>,
    tmp: Option<Spool>,
    finished: bool,
}

impl Write for TarEntryBackend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.tmp.is_none() && (self.buf.len() as u64) + buf.len() as u64 > INMEM_LIMIT {
            let mut sp = Spool::new()?;
            sp.write_all(&self.buf)?;
            self.buf.clear();
            self.tmp = Some(sp);
        }
        if let Some(sp) = self.tmp.as_mut() {
            sp.write_all(buf)?;
        } else {
            self.buf.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Some(sp) = self.tmp.as_mut() {
            sp.flush()?;
        }
        Ok(())
    }
}

impl EntrySinkBackend for TarEntryBackend {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.flush().map_err(HzipError::from)?;
        if self.meta.is_dir || self.meta.is_symlink {
            return emit_record(&self.shared, &self.meta, &mut io::empty(), 0);
        }
        if let Some(sp) = self.tmp.as_mut() {
            let len = sp
                .path()
                .metadata()
                .map(|m| m.len())
                .map_err(HzipError::from)?;
            let mut rf = sp.reopen().map_err(HzipError::from)?;
            emit_record(&self.shared, &self.meta, &mut rf, len)
        } else {
            let len = self.buf.len() as u64;
            let mut cur = io::Cursor::new(&self.buf[..]);
            emit_record(&self.shared, &self.meta, &mut cur, len)
        }
    }
}

impl ArchiveWriter for TarWriter {
    fn next_entry(
        &mut self,
        meta: &EntryMeta,
        _pw: PasswordRef<'_>,
    ) -> Result<EntrySink<'_>, HzipError> {
        if meta.encrypted {
            return Err(HzipError::Unsupported {
                what: "tar 无加密（02 §8.4：encryption=None）".into(),
            });
        }
        let backend = Box::new(TarEntryBackend {
            shared: Arc::clone(&self.shared),
            meta: meta.clone(),
            buf: Vec::new(),
            tmp: None,
            finished: false,
        });
        // 契约：EntrySink 与 &mut self 同生命周期；meta 经克隆暂存于 backend 与
        // self.pending（在 self 借期内借出，满足 from_backend 同 'a 约束）。
        let pending_meta = meta.clone();
        self.pending = Some(pending_meta);
        let m = self.pending.as_ref().ok_or_else(|| HzipError::Format {
            format: hzip_format::FormatId::from(crate::FORMAT_ID),
            kind: FormatErrorKind::BadHeader,
        })?;
        Ok(EntrySink::from_backend(
            backend,
            m,
            &self.cancel,
            &self.progress,
        ))
    }

    fn finish(self: Box<Self>) -> Result<WriteReport, HzipError> {
        // 取出 builder，finish（写尾零块）后 into_inner 回收 output
        let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        let fmt = g.format.clone();
        let mut report = WriteReport {
            entries_written: g.entries_written,
            bytes_in: g.bytes_in,
            bytes_out: g.bytes_out,
            volume_paths: Vec::new(),
        };
        let Some(builder) = g.builder.take() else {
            return Err(HzipError::Format {
                format: fmt,
                kind: FormatErrorKind::BadHeader,
            });
        };
        let mut builder = builder;
        builder
            .finish()
            .map_err(|e| variant::map_err(e, fmt.clone()))?;
        let output = builder.into_inner().map_err(|e| variant::map_err(e, fmt))?;
        match output {
            TarOutput::File(mut f) => {
                f.flush().map_err(HzipError::from)?;
                report.bytes_out = f.stream_position().unwrap_or(report.bytes_out);
            }
            TarOutput::StdoutSpool(mut sp) => {
                sp.flush().map_err(HzipError::from)?;
                let mut rf = sp.reopen().map_err(HzipError::from)?;
                let mut stdout = io::stdout().lock();
                io::copy(&mut rf, &mut stdout).map_err(HzipError::from)?;
                report.bytes_out = sp.path().metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        Ok(report)
    }
}
