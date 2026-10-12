//! `SingleWriter`：单文件流写入端（02 §8.5 / T23 §4.6）。
//!
//! - **恰一条目**：`next_entry` 只能调用一次；第二次 → `Err Unsupported`
//!   （HZ-5001，单流档案恰一条目）；`meta.encrypted` → `Err Unsupported`
//!   （caps 无加密能力）；目录/符号链接条目 → `Err Unsupported`（单文件流只能
//!   表达一个普通文件的字节流；多文件输入由 core 在提交前拒绝，HZ-5003 路径）；
//! - **追加拒绝**：`create_writer(append=true)` → `Err Unsupported`（HZ-5001，
//!   TC-CONF-005 负断言）；
//! - **显式 finish**：`finish_entry` 调各库显式 finish（`try_finish`/`do_finish`）
//!   捕获尾块/尾 CRC 错误——GzEncoder / bzip2 `Encoder` 的 drop 不报错
//!   （T12 §4.4 同教训；TC-CONF-008 截断产物检出来源）；xz2/zstd 同理；
//! - **写端头字段**：gzip 经 `GzBuilder` 设 FNAME/mtime（FNAME 含非 Latin-1 →
//!   省略 + warning，§4.3）；bz2/xz/zst 无名域不写；zstd 默认**不写** hzip 私有
//!   mtime 帧（与标准工具字节兼容，02 §8.5）；
//! - **级别映射**：一律复用 `hzip_codec::level`（单一事实来源，不复制 02 §6.3 表）。
//!
//! # 写入进度 / 取消通道（遗留登记）
//!
//! T20 `ArchiveWriter` 契约的 `next_entry`/`finish` **未注入** `CancellationToken`
//! 与 `ProgressBus`（二者仅在 `WriteCtx`——编辑重写路径提供）。本实现以 handler
//! 自建令牌/总线满足 [`EntrySink`] 构造签名；写入方向的真实取消与进度聚合依赖
//! core（T33）在调用侧注入。详见任务卡 §7-4 与最终汇报遗留。
//!
//! # 统计模型
//!
//! `bytes_in` 由 [`SingleSinkBackend`] 在收到 EntrySink 的原始字节时累计，
//! `bytes_out` 由底层 [`CountingWriter`] 在 encoder 写出压缩产物时累计；二者
//! 经共享 `Arc<SinkStats>`（原子计数）供 `finish()` 读取（EntrySink drop 后数据
//! 不回传，需独立记账）。

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hzip_error::HzipError;
use hzip_format::{
    ArchiveWriter, CreateOptions, EntryMeta, EntrySink, EntrySinkBackend, PasswordRef, Sink,
    WriteReport,
};

use crate::handler::SingleFmt;

/// 写入统计（输入/输出字节；原子计数，backend/facade 与 writer 共享）。
#[derive(Debug, Default)]
struct SinkStats {
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
}

/// encoder 门面：擦除四种库 encoder 类型的差异。
///
/// `Write` 输入为**原始字节**（压缩产物写入内部计数 sink）；`finish_entry`
/// 冲刷编码器、写尾块/尾 CRC 并捕获错误（drop 不报错）。
pub(crate) trait EncoderFacade: Write + Send {
    /// 冲刷并写尾块（等价显式 finish；错误须上抛，禁止静默 drop）。
    fn finish_entry(&mut self) -> Result<(), HzipError>;
}

/// 计数写端包装：累加压缩产物字节到共享统计。
struct CountingWriter {
    inner: Box<dyn Write + Send>,
    stats: Arc<SinkStats>,
}

impl CountingWriter {
    fn new(inner: Box<dyn Write + Send>, stats: Arc<SinkStats>) -> Self {
        Self { inner, stats }
    }
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        if n > 0 {
            self.stats.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// [`EntrySinkBackend`] 适配（EntrySink 共享实现：CRC/取消检查点/进度自动获得）。
struct SingleSinkBackend {
    facade: Box<dyn EncoderFacade>,
    bytes_in: Arc<SinkStats>,
}

impl Write for SingleSinkBackend {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.facade.write(buf)?;
        if n > 0 {
            self.bytes_in
                .bytes_in
                .fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.facade.flush()
    }
}

impl EntrySinkBackend for SingleSinkBackend {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        self.facade.finish_entry()
    }
}

pub struct SingleWriter {
    fmt: SingleFmt,
    dst: Option<Sink>,
    opts: CreateOptions,
    /// 内部令牌/总线（契约未注入；见模块文档遗留）。
    cancel: hzip_progress::CancellationToken,
    bus: hzip_progress::ProgressBus,
    /// next_entry 的 EntryMeta 副本（供 EntrySink 借用；契约返回 EntrySink 仅
    /// 允许借 self，故 meta 须存于 writer 内）。
    meta_slot: Option<EntryMeta>,
    entry_open: bool,
    stats: Arc<SinkStats>,
}

impl SingleWriter {
    pub(crate) fn create(
        fmt: SingleFmt,
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Self, HzipError> {
        if opts.append {
            return Err(HzipError::Unsupported {
                what: format!(
                    "{} 不支持追加（caps.append=false；单文件流恰一条目，HZ-5001）",
                    fmt.id()
                )
                .into(),
            });
        }
        if opts.encryption.is_some() {
            return Err(HzipError::Unsupported {
                what: format!("{} 无加密能力（caps.encryption=None）", fmt.id()).into(),
            });
        }
        if opts.solid {
            return Err(HzipError::Unsupported {
                what: format!("{} 不支持固实（caps.solid=false）", fmt.id()).into(),
            });
        }
        if opts.comment.is_some() {
            return Err(HzipError::Unsupported {
                what: format!("{} 不支持档案注释（caps.comment=false）", fmt.id()).into(),
            });
        }
        if let Sink::Split { .. } = &dst {
            return Err(HzipError::Unsupported {
                what: format!("{} 不支持分卷写（caps.split=false）", fmt.id()).into(),
            });
        }
        Ok(Self {
            fmt,
            dst: Some(dst),
            opts: opts.clone(),
            cancel: hzip_progress::CancellationToken::new(),
            bus: hzip_progress::ProgressBus::new(),
            meta_slot: None,
            entry_open: false,
            stats: Arc::new(SinkStats::default()),
        })
    }

    /// 打开目标底层写端（消费 `self.dst`）。
    fn open_sink(&mut self) -> Result<Box<dyn Write + Send>, HzipError> {
        let dst = self.dst.take().ok_or_else(|| HzipError::Unsupported {
            what: "writer 目标已被消费（next_entry 只能调用一次）".into(),
        })?;
        match dst {
            Sink::File(p) => {
                if let Some(parent) = p.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).map_err(crate::map_io)?;
                }
                let f = std::fs::File::create(p).map_err(crate::map_io)?;
                Ok(Box::new(f))
            }
            Sink::Stdout => Ok(Box::new(std::io::stdout())),
            Sink::Stream(s) => Ok(s),
            Sink::Split { .. } => Err(HzipError::Unsupported {
                what: "分卷写已在上方拒绝".into(),
            }),
        }
    }

    /// 空档案收尾：产出空内容成员（恰 0 条目时；单文件流语义通常要求恰一条目，
    /// 此处保守产出可解析空流，供「空档案」TC-ALG-010 语义兜底）。
    fn write_empty_member(&mut self) -> Result<(), HzipError> {
        let raw = self.open_sink()?;
        let counting = CountingWriter::new(raw, Arc::clone(&self.stats));
        let meta = EntryMeta {
            name: hzip_format::EntryName::new(""),
            size_raw: Some(0),
            mtime: None,
            ..Default::default()
        };
        let mut facade = build_facade(self.fmt, counting, &self.opts, &meta)?;
        facade.finish_entry()?;
        Ok(())
    }
}

impl ArchiveWriter for SingleWriter {
    fn next_entry(
        &mut self,
        meta: &EntryMeta,
        _pw: PasswordRef<'_>,
    ) -> Result<EntrySink<'_>, HzipError> {
        if self.entry_open {
            return Err(HzipError::Unsupported {
                what: format!(
                    "{} 单文件流档案恰一条目：next_entry 只能调用一次（HZ-5001）",
                    self.fmt.id()
                )
                .into(),
            });
        }
        if meta.encrypted {
            return Err(HzipError::Unsupported {
                what: format!("{} 无加密能力（caps.encryption=None）", self.fmt.id()).into(),
            });
        }
        if meta.is_dir || meta.is_symlink {
            return Err(HzipError::Unsupported {
                what: format!(
                    "{} 单文件流只能写入普通文件内容（收到 is_dir={} is_symlink={}，多文件输入请经 core 校验 HZ-5003）",
                    self.fmt.id(),
                    meta.is_dir,
                    meta.is_symlink
                ).into(),
            });
        }
        self.entry_open = true;

        let raw = self.open_sink()?;
        // 存入 EntryMeta 副本（契约的 EntrySink 只能借 self；meta 由调用方传入，
        // 需转存到 self 内供 EntrySink 借用）。
        self.meta_slot = Some(meta.clone());
        let meta_ref = self
            .meta_slot
            .as_ref()
            .ok_or_else(|| HzipError::Unsupported {
                what: "writer meta_slot 状态异常（已消费）".into(),
            })?;
        let counting = CountingWriter::new(raw, Arc::clone(&self.stats));
        let facade = build_facade(self.fmt, counting, &self.opts, meta_ref)?;

        // backend 完全自有（不借 self 的 encoder 状态）；EntrySink 借 self
        // （meta_ref/cancel/bus 均为 self 成员引用，返回 EntrySink<'_> 合法）。
        let backend: Box<dyn EntrySinkBackend + 'static> = Box::new(SingleSinkBackend {
            facade,
            bytes_in: Arc::clone(&self.stats),
        });
        let sink = EntrySink::from_backend(backend, meta_ref, &self.cancel, &self.bus);
        Ok(sink)
    }

    fn finish(mut self: Box<Self>) -> Result<WriteReport, HzipError> {
        // 未 open 条目：产出空内容成员（可解析；entries_written=0 由调用方处理）
        if !self.entry_open {
            self.write_empty_member().ok();
        }
        let (bytes_in, bytes_out) = {
            let s = &self.stats;
            (
                s.bytes_in.load(Ordering::Relaxed),
                s.bytes_out.load(Ordering::Relaxed),
            )
        };
        Ok(WriteReport {
            entries_written: if self.entry_open { 1 } else { 0 },
            bytes_in,
            bytes_out,
            volume_paths: vec![],
        })
    }
}

// ===================== 各格式 EncoderFacade 实现 =====================

/// 按格式构建 encoder facade（feature 门控）。
fn build_facade(
    fmt: SingleFmt,
    counting: CountingWriter,
    opts: &CreateOptions,
    meta: &EntryMeta,
) -> Result<Box<dyn EncoderFacade>, HzipError> {
    // zst-only 裁剪下 gz/bz2/xz 分支被剥除，meta 仅在相应 feature 分支消费；
    // 显式锚定防 unused_variables。
    let _ = (meta,);
    match fmt {
        #[cfg(feature = "gz")]
        SingleFmt::Gz => build_gz_facade(counting, opts, meta),
        #[cfg(feature = "bz2")]
        SingleFmt::Bz2 => build_bz2_facade(counting, opts),
        #[cfg(feature = "xz")]
        SingleFmt::Xz => build_xz_facade(counting, opts),
        #[cfg(feature = "zst")]
        SingleFmt::Zst => build_zst_facade(counting, opts),
        #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
        SingleFmt::Placeholder => Err(HzipError::Unsupported {
            what: "无启用的单文件流格式".into(),
        }),
    }
}

/// gzip facade：`GzBuilder` 设 FNAME/mtime + deflate 级别映射（复用
/// `hzip_codec::level::deflate_level` 表值——单一事实来源）。
#[cfg(feature = "gz")]
fn build_gz_facade(
    counting: CountingWriter,
    opts: &CreateOptions,
    meta: &EntryMeta,
) -> Result<Box<dyn EncoderFacade>, HzipError> {
    use hzip_codec::level::Level;
    let level = match opts.level {
        Level::Store => 0,
        l => hzip_codec::level::deflate_level(l),
    };
    let mut b = flate2::GzBuilder::new();
    // FNAME：Latin-1（逐字节 ≤ 0xFF）可表才写；否则省略 + warning
    let name = meta.name.as_str();
    let latin1 = name.chars().all(|c| (c as u32) <= 0xFF);
    if !name.is_empty() && latin1 {
        let bytes: Vec<u8> = name.chars().map(|c| c as u8).collect();
        b = b.filename(bytes);
    } else if !name.is_empty() {
        tracing::warn!(
            name = %name,
            "gzip FNAME 含非 Latin-1 字符，按规范省略（读端回退推导名）"
        );
    }
    if let Some(mt) = meta.mtime {
        b = b.mtime(crate::mtime::gzip_mtime_field(Some(mt)));
    }
    let enc = b.write(counting, flate2::Compression::new(level));
    Ok(Box::new(GzFacade::new(enc)))
}

/// bzip2 facade（级别复用 `hzip_codec::level::bzip2_level`；Store 不可用 →
/// Fastest，02 §6.3「—」列，codec 层同防御）。
#[cfg(feature = "bz2")]
fn build_bz2_facade(
    counting: CountingWriter,
    opts: &CreateOptions,
) -> Result<Box<dyn EncoderFacade>, HzipError> {
    use hzip_codec::level::Level;
    let lvl = match opts.level {
        Level::Store => 1,
        l => hzip_codec::level::bzip2_level(l),
    };
    let enc = bzip2::write::BzEncoder::new(counting, bzip2::Compression::new(lvl));
    Ok(Box::new(Bz2Facade::new(enc)))
}

/// xz facade（级别/线程映射复用 `hzip_codec::level::{xz_preset, xz_extreme,
/// clamp_threads}`；Ultra → 9e）。
#[cfg(feature = "xz")]
fn build_xz_facade(
    counting: CountingWriter,
    opts: &CreateOptions,
) -> Result<Box<dyn EncoderFacade>, HzipError> {
    use hzip_codec::level::Level;
    // liblzma LZMA_PRESET_EXTREME 位（1<<31；codec/xz.rs 同值）
    const XZ_EXTREME_BIT: u32 = 1 << 31;
    let preset_base = match opts.level {
        Level::Store => 0,
        l => hzip_codec::level::xz_preset(l),
    };
    let extreme = hzip_codec::level::xz_extreme(opts.level);
    let preset = preset_base | if extreme { XZ_EXTREME_BIT } else { 0 };
    let mut b = xz2::stream::MtStreamBuilder::new();
    // threads=0（自动）→ liblzma mt encoder 不接受 0；保底 1（单流内 xz 的
    // 多线程由 liblzma 内部按块并行，线程数映射仍复用 clamp_threads）
    let n = hzip_codec::level::clamp_threads(opts.threads).max(1);
    b.threads(n).timeout_ms(u32::MAX).preset(preset);
    let stream = b
        .encoder()
        .map_err(|e| crate::map_io(std::io::Error::other(e)))?;
    let enc = xz2::write::XzEncoder::new_stream(counting, stream);
    Ok(Box::new(XzFacade::new(enc)))
}

/// zstd facade（级别/线程复用 `hzip_codec::level::{zstd_level, clamp_threads}`；
/// Store 不可用 → 防御回落默认级；写帧尾 xxhash64 供读端校验）。
#[cfg(feature = "zst")]
fn build_zst_facade(
    counting: CountingWriter,
    opts: &CreateOptions,
) -> Result<Box<dyn EncoderFacade>, HzipError> {
    use hzip_codec::level::Level;
    let level = match opts.level {
        Level::Store => 3,
        l => hzip_codec::level::zstd_level(l),
    };
    let enc = zstd::stream::write::Encoder::new(counting, level).map_err(crate::map_io)?;
    let mut enc = enc;
    enc.include_checksum(true).map_err(crate::map_io)?;
    let threads = hzip_codec::level::clamp_threads(opts.threads);
    if threads > 1 {
        enc.multithread(threads).map_err(crate::map_io)?;
    }
    Ok(Box::new(ZstFacade::new(enc)))
}

// ---------- gz ----------

#[cfg(feature = "gz")]
struct GzFacade {
    enc: flate2::write::GzEncoder<CountingWriter>,
}

#[cfg(feature = "gz")]
impl GzFacade {
    fn new(enc: flate2::write::GzEncoder<CountingWriter>) -> Self {
        Self { enc }
    }
}

#[cfg(feature = "gz")]
impl Write for GzFacade {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.enc.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.enc.flush()
    }
}

#[cfg(feature = "gz")]
impl EncoderFacade for GzFacade {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        // 显式 try_finish：写尾 CRC32+ISIZE 并冲刷到底层；GzEncoder drop 只静默吞错。
        // 注意：try_finish 之后不得再 flush/write（flate2 内部断言 crc_bytes_written==0）。
        self.enc.try_finish().map_err(crate::map_io)?;
        Ok(())
    }
}

// ---------- bz2 ----------

#[cfg(feature = "bz2")]
struct Bz2Facade {
    enc: bzip2::write::BzEncoder<CountingWriter>,
}

#[cfg(feature = "bz2")]
impl Bz2Facade {
    fn new(enc: bzip2::write::BzEncoder<CountingWriter>) -> Self {
        Self { enc }
    }
}

#[cfg(feature = "bz2")]
impl Write for Bz2Facade {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.enc.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.enc.flush()
    }
}

#[cfg(feature = "bz2")]
impl EncoderFacade for Bz2Facade {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        // try_finish 写流尾块并冲刷；完成后不得再 flush/write（写关闭流会 Err）
        self.enc.try_finish().map_err(crate::map_io)?;
        Ok(())
    }
}

// ---------- xz ----------

#[cfg(feature = "xz")]
struct XzFacade {
    enc: xz2::write::XzEncoder<CountingWriter>,
}

#[cfg(feature = "xz")]
impl XzFacade {
    fn new(enc: xz2::write::XzEncoder<CountingWriter>) -> Self {
        Self { enc }
    }
}

#[cfg(feature = "xz")]
impl Write for XzFacade {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.enc.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.enc.flush()
    }
}

#[cfg(feature = "xz")]
impl EncoderFacade for XzFacade {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        self.enc.try_finish().map_err(crate::map_io)?;
        Ok(())
    }
}

// ---------- zst ----------

#[cfg(feature = "zst")]
struct ZstFacade {
    enc: zstd::stream::write::Encoder<'static, CountingWriter>,
}

#[cfg(feature = "zst")]
impl ZstFacade {
    fn new(enc: zstd::stream::write::Encoder<'static, CountingWriter>) -> Self {
        Self { enc }
    }
}

#[cfg(feature = "zst")]
impl Write for ZstFacade {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.enc.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.enc.flush()
    }
}

#[cfg(feature = "zst")]
impl EncoderFacade for ZstFacade {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        self.enc.do_finish().map_err(crate::map_io)?;
        Ok(())
    }
}
