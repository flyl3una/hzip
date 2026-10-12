//! zip 写入：创建/追加（02 §8.1 writer 职责行、§7.3；T24 §4.6）。
//!
//! # 条目写模型
//!
//! writer 经共享状态 [`Shared`]（`Arc<Mutex>`，T22 tar 同构）持有**单个**
//! [`zip::ZipWriter`]（输出端统一为可 seek 的 [`DynOut`]）。`next_entry` 先落本地头
//! （`start_file`/`add_directory`/`add_symlink`），随后 `EntrySink`（hzip-format
//! 冻结类型）经 [`ZipBackend`] 写入数据——其内建 CRC 旁路累计、取消检查点、进度
//! `Bytes` 事件。zip crate 自身维护**另一通道** CRC（`ZipWriterStats`），在收尾时
//! 回填本地头与中央目录。本层 [`ZipBackend`] 再维护一条旁路 `Crc32`（02 §6.2），
//! `finish_entry`（含 drop 保底）时若预置 `expected`（editor 重写来源 CRC）不符 →
//! 挂起 [`Shared::pending`]，由 `finish()` 上抛 `Checksum 3001`。三通道一致性由
//! 单测断言（T24 §6.1 `sink_drop_crc_chain`）。
//!
//! # 追加（02 §8.1：直接改中央目录，无需重写数据区）
//!
//! `create_writer(append=true)` + `Sink::File`：以 `OpenOptions read+write` 打开，
//! [`zip::ZipWriter::new_append`] 定位 EOCD、解析中央目录入 `files` 表；后续
//! `next_entry` 从旧中央目录后顺序落新本地头（旧条目本地头与数据区零改动）；
//! `finish` 重写中央目录 + EOCD。旧条目字节不变（TC-CONF-005 断言）。
//! append 目标不存在 → 退化为新建（`>>` 语义，TC-CONF-005 表面契约）。
//!
//! # 输出端抽象（`Sink` → [`Dest`]）
//!
//! | Sink | 形态 | 说明 |
//! |---|---|---|
//! | `File` | 直写文件（read+write+truncate 句柄） | 新建 truncate / 追加 read+write |
//! | `Stdout` | 临时文件两遍法 | 先落临时完成回写，`finish()` 时顺序拷出 stdout |
//! | `Split` | 临时文件两遍法 + 字节切分 | `finish()` 时经 [`crate::split::Splitter`] 分卷 |
//! | `Stream` | 临时文件两遍法 | `finish()` 时顺序拷入组合链内层 |
//!
//! zip 中央目录要求 finish 回写（seek），Stdout/Stream 不可 seek 与 7z 同策略
//! （02 §8.2 关键边界；T24 §7-5 两遍法临时文件 drop 清理）。统一输出为
//! `Read + Write + Seek`（`Box<dyn ReadWriteSeek>`：File / tempfile::File 均满足），
//! 故 `finish()` 后可回读输出（stdout/split/stream 拷贝、File 取长度）。
//!
//! # `Sink::Split` 与 zip 结构
//!
//! 任务卡 §4.8 的 `SplitSink`（`Write + Seek` 前进态）与 zip 写入器的回退 seek
//! 冲突——zip 在条目收尾需回写本地头 CRC/尺寸（`finish_file`）。故对 `Sink::Split`
//! 走「临时文件两遍法 + 纯字节切分」（见 `crate::split` 模块文档）：`Splitter`
//! 只做字节滚动切分（无 Seek），中央目录落在临时文件尾部后一并切分。
//!
//! # 级别映射（02 §6.3 zip deflate 列）
//!
//! `Store → Stored`；`Fastest → Deflated(1)`；`Fast → Deflated(3)`；
//! `Normal → Deflated(6)`；`Maximum | Ultra → Deflated(9)`。deflate64 无写路径
//! （02 §13.2）；Store 级不设 compression_level（zip crate 拒收）。
//!
//! # 目录 / 符号链接
//!
//! - 目录：`add_directory`（zip 尾缀 `/`、权限 0o755，零长条目）；
//! - 符号链接：`add_symlink(name, target)`——zip 以**内容即目标文本**表达链接
//!   （unix_mode `S_IFLNK`，02 §5.3.2）；读侧 `is_symlink` 判定、内容为目标路径。

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hzip_codec::crc::Crc32;
use hzip_error::{FormatErrorKind, HzipError, PasswordTarget};
use hzip_format::{
    AesKey, ArchiveWriter, CreateOptions, EncryptionRequest, EntryMeta, EntrySink,
    EntrySinkBackend, PasswordRef, Sink, WriteReport,
};
use hzip_progress::{CancellationToken, ProgressBus};
use zip::write::SimpleFileOptions;

use crate::handler;
use crate::split::Splitter;

/// `Read + Write + Seek + Send` 对象安全联合（输出端统一承载）。
pub(crate) trait ReadWriteSeek: Read + Write + Seek + Send {}
impl<T: Read + Write + Seek + Send> ReadWriteSeek for T {}

/// 输出端统一抽象（File / 临时文件均满足）。
type DynOut = Box<dyn ReadWriteSeek>;

/// 共享状态（backend/facade 与 writer 经 Arc 共享；`ZipWriter` 需 `&mut` →
/// Mutex 串行化）。
struct Shared {
    /// zip 写入器（None 仅存在于 finish 取出后）。
    writer: Option<zip::ZipWriter<DynOut>>,
    entries_written: usize,
    bytes_in: u64,
    /// drop 校验挂起错误（T24 §4.6：finish 上抛）。
    pending: Option<HzipError>,
    /// 追加基线（旧档案文件尾即中央目录后；bytes_out 统计起点）。
    base_len: u64,
}

/// 输出目标收尾形态（`finish()` 时把临时产物拷出或直读文件位置）。
enum Dest {
    /// 直写文件（新建 truncate 或追加 read+write；`finish()` 后取文件长度）。
    File,
    /// stdout：临时文件拷出。
    Stdout,
    /// 分卷：临时文件 → Splitter 切分。
    Split {
        prefix: PathBuf,
        bytes_per_volume: u64,
    },
    /// 组合链内层：临时文件拷入。
    Stream(Box<dyn Write + Send>),
}

/// zip 写入器。
pub(crate) struct ZipWriterImpl {
    shared: Arc<Mutex<Shared>>,
    dest: Dest,
    /// 统一压缩级别（02 §6.3）。
    level: hzip_codec::level::Level,
    /// 档案级加密请求（`CreateOptions.encryption`；实现归 T45 收尾）。
    enc: Option<EncryptionRequest>,
    cancel: CancellationToken,
    progress: ProgressBus,
    /// 暂存当前条目 meta（供 EntrySink 借用）。
    pending: Option<EntryMeta>,
}

impl ZipWriterImpl {
    /// 新建或追加（按 `CreateOptions.append` 分派）。
    pub(crate) fn create(
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        // 加密方案合法性在 handler::create_writer 按 caps.write_encryption 校验
        if opts.solid {
            return Err(HzipError::Unsupported {
                what: "zip 不支持固实（caps.solid=false）".into(),
            });
        }
        if opts.comment.is_some() {
            return Err(HzipError::Unsupported {
                what: "zip 档案注释未实现（caps.comment=false）".into(),
            });
        }
        if opts.append {
            Self::append(dst, opts)
        } else {
            Self::create_new(dst, opts)
        }
    }

    fn build(
        writer: zip::ZipWriter<DynOut>,
        dest: Dest,
        base_len: u64,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        Ok(Box::new(ZipWriterImpl {
            shared: Arc::new(Mutex::new(Shared {
                writer: Some(writer),
                entries_written: 0,
                bytes_in: 0,
                pending: None,
                base_len,
            })),
            dest,
            level: opts.level,
            enc: opts.encryption,
            cancel: CancellationToken::new(),
            progress: ProgressBus::new(),
            pending: None,
        }))
    }

    /// 新建目标（read+write 打开；File 直接成为输出，Stdout/Split/Stream 走临时）。
    fn create_new(dst: Sink, opts: &CreateOptions) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        match dst {
            Sink::File(p) => {
                if let Some(parent) = p.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).map_err(HzipError::from)?;
                }
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&p)
                    .map_err(HzipError::from)?;
                let out: DynOut = Box::new(f);
                Self::build(zip::ZipWriter::new(out), Dest::File, 0, opts)
            }
            Sink::Stdout => Self::temp_writer(Dest::Stdout, opts),
            Sink::Split {
                prefix,
                bytes_per_volume,
            } => Self::temp_writer(
                Dest::Split {
                    prefix,
                    bytes_per_volume,
                },
                opts,
            ),
            Sink::Stream(s) => Self::temp_writer(Dest::Stream(s), opts),
        }
    }

    /// 追加：`Sink::File` + 已存在 zip → `new_append`；不存在 → 新建；
    /// 非 File 目标 → Unsupported（追加需可 seek 的 read+write 句柄）。
    fn append(dst: Sink, opts: &CreateOptions) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        let p = match dst {
            Sink::File(p) => p,
            other => {
                return Err(HzipError::Unsupported {
                    what: format!(
                        "zip 追加仅支持 Sink::File（{}；追加需 read+write seek 句柄）",
                        sink_name(&other)
                    )
                    .into(),
                });
            }
        };
        if !p.exists() {
            return Self::create_new(Sink::File(p), opts);
        }
        let base_len = std::fs::metadata(&p)
            .map(|m| m.len())
            .map_err(HzipError::from)?;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&p)
            .map_err(HzipError::from)?;
        let out: DynOut = Box::new(f);
        let writer =
            zip::ZipWriter::new_append(out).map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
        Self::build(writer, Dest::File, base_len, opts)
    }

    /// 临时文件承载输出（Stdout/Split/Stream）：writer 写临时文件，finish 拷出。
    fn temp_writer(dest: Dest, opts: &CreateOptions) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        let f = tempfile::tempfile().map_err(HzipError::from)?;
        let out: DynOut = Box::new(f);
        Self::build(zip::ZipWriter::new(out), dest, 0, opts)
    }

    /// 收尾：取回底层 writer 数据并据 `Dest` 拷出 / 取长度。
    fn finish_dest(&mut self, mut out: DynOut) -> Result<WriteReport, HzipError> {
        out.flush().map_err(HzipError::from)?;
        let g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        let entries_written = g.entries_written;
        let bytes_in = g.bytes_in;
        let base_len = g.base_len;
        drop(g);
        let bytes_out = match &mut self.dest {
            Dest::File => out.seek(SeekFrom::End(0)).map_err(HzipError::from)?,
            Dest::Stdout => {
                out.seek(SeekFrom::Start(0)).map_err(HzipError::from)?;
                let mut so = io::stdout().lock();
                io::copy(&mut out, &mut so).map_err(HzipError::from)?
            }
            Dest::Split {
                prefix,
                bytes_per_volume,
            } => {
                out.seek(SeekFrom::Start(0)).map_err(HzipError::from)?;
                let mut sp =
                    Splitter::new(prefix.clone(), *bytes_per_volume).map_err(HzipError::from)?;
                let n = io::copy(&mut out, &mut sp).map_err(HzipError::from)?;
                sp.finalize().map_err(HzipError::from)?;
                n
            }
            Dest::Stream(s) => {
                out.seek(SeekFrom::Start(0)).map_err(HzipError::from)?;
                io::copy(&mut out, &mut **s).map_err(HzipError::from)?
            }
        };
        Ok(WriteReport {
            entries_written,
            bytes_in,
            bytes_out: bytes_out.max(base_len),
            volume_paths: Vec::new(),
        })
    }

    fn file_options(&self, meta: &EntryMeta) -> SimpleFileOptions {
        use zip::CompressionMethod;
        let (method, level) = match self.level {
            hzip_codec::level::Level::Store => (CompressionMethod::Stored, None),
            hzip_codec::level::Level::Fastest => (CompressionMethod::Deflated, Some(1)),
            hzip_codec::level::Level::Fast => (CompressionMethod::Deflated, Some(3)),
            hzip_codec::level::Level::Normal => (CompressionMethod::Deflated, Some(6)),
            hzip_codec::level::Level::Maximum | hzip_codec::level::Level::Ultra => {
                (CompressionMethod::Deflated, Some(9))
            }
        };
        let mut o = SimpleFileOptions::default()
            .compression_method(method)
            .compression_level(level)
            .large_file(true); // zip64：≥4 GiB 条目必需（T24 §4.4）
        if let Some(mode) = meta.mode {
            o = o.unix_permissions(mode);
        }
        if let Some(mt) = meta.mtime
            && let Ok(dt) = zip::DateTime::try_from(mt)
        {
            o = o.last_modified_time(dt);
        }
        o
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

impl ArchiveWriter for ZipWriterImpl {
    fn next_entry(
        &mut self,
        meta: &EntryMeta,
        pw: PasswordRef<'_>,
    ) -> Result<EntrySink<'_>, HzipError> {
        // 档案级加密（encryption 与密码成对由 core 校验 HZ-5003）：仅文件条目
        // 加密——目录零长、符号链接内容（目标路径）均公开，不加密。
        let enc_pw = if meta.is_dir || meta.is_symlink {
            None
        } else if self.enc.is_some() {
            match pw.as_str() {
                Some(s) => Some(s),
                None => {
                    return Err(HzipError::Password {
                        target: PasswordTarget::Archive,
                    });
                }
            }
        } else {
            None
        };
        // 目录条目：零长 add_directory
        if meta.is_dir {
            let opts = self.file_options(meta);
            let name = meta.name.as_str().trim_end_matches('/');
            {
                let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                g.writer
                    .as_mut()
                    .ok_or_else(bad_state)?
                    .add_directory(name, opts)
                    .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
                g.entries_written += 1;
            }
            return self.dir_sink(meta);
        }
        // 符号链接：内容 = 目标文本（add_symlink 内部落数据）
        if meta.is_symlink {
            let opts = self.file_options(meta);
            let target = meta
                .symlink_target
                .as_ref()
                .ok_or_else(|| HzipError::Format {
                    format: handler::fmt(),
                    kind: FormatErrorKind::BadHeader,
                })?;
            let target_str = target.to_string_lossy();
            {
                let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                g.writer
                    .as_mut()
                    .ok_or_else(bad_state)?
                    .add_symlink(meta.name.as_str(), target_str.as_ref(), opts)
                    .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
                g.entries_written += 1;
            }
            return self.dir_sink(meta);
        }
        // 普通文件：start_file 本地头先落盘（挂档案级加密密码）
        {
            let mut opts = self.file_options(meta);
            if let (Some(req), Some(p)) = (&self.enc, enc_pw) {
                use zip::unstable::write::FileOptionsExt;
                opts = match req {
                    EncryptionRequest::ZipCrypto => opts.with_deprecated_encryption(p.as_bytes()),
                    EncryptionRequest::Aes(AesKey::Bits128) => {
                        opts.with_aes_encryption(zip::AesMode::Aes128, p)
                    }
                    EncryptionRequest::Aes(AesKey::Bits192) => {
                        opts.with_aes_encryption(zip::AesMode::Aes192, p)
                    }
                    EncryptionRequest::Aes(AesKey::Bits256) => {
                        opts.with_aes_encryption(zip::AesMode::Aes256, p)
                    }
                    other => {
                        return Err(HzipError::Unsupported {
                            what: format!("zip 不支持该写入加密方案：{other:?}").into(),
                        });
                    }
                };
            }
            let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            let w = g.writer.as_mut().ok_or_else(bad_state)?;
            w.start_file(meta.name.as_str(), opts)
                .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
        }
        // backend 不借 self：共享 Arc + self.pending meta 借用（EntrySink 同 'a）
        self.pending = Some(meta.clone());
        let m = self.pending.as_ref().ok_or_else(bad_state)?;
        let backend: Box<dyn EntrySinkBackend + '_> = Box::new(ZipBackend {
            shared: Arc::clone(&self.shared),
            crc: Crc32::new(),
            expected: None,
            finished: false,
        });
        Ok(EntrySink::from_backend(
            backend,
            m,
            &self.cancel,
            &self.progress,
        ))
    }

    fn finish(mut self: Box<Self>) -> Result<WriteReport, HzipError> {
        // pending 错误（drop 校验链挂起）优先上抛：产物即「无目录 zip」可由
        // salvage 恢复（02 §8.1 预期）
        {
            let g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &g.pending {
                return Err(e.clone());
            }
        }
        let writer = {
            let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            g.writer.take().ok_or_else(bad_state)?
        };
        let out = writer
            .finish()
            .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
        self.finish_dest(out)
    }
}

impl ZipWriterImpl {
    /// 目录/符号链接条目的 EntrySink（条目已零长落盘；backend 拒绝数据写入）。
    fn dir_sink(&mut self, meta: &EntryMeta) -> Result<EntrySink<'_>, HzipError> {
        self.pending = Some(meta.clone());
        let m = self.pending.as_ref().ok_or_else(bad_state)?;
        let backend: Box<dyn EntrySinkBackend + '_> = Box::new(EmptyBackend);
        Ok(EntrySink::from_backend(
            backend,
            m,
            &self.cancel,
            &self.progress,
        ))
    }
}

fn bad_state() -> HzipError {
    HzipError::Format {
        format: handler::fmt(),
        kind: FormatErrorKind::BadHeader,
    }
}

/// 单条目 backend：写 zip writer（经共享锁），旁路 CRC 累计 + 预置 expected 校验。
struct ZipBackend {
    shared: Arc<Mutex<Shared>>,
    crc: Crc32,
    /// 已知源 CRC（editor 重写路径注入）；None = 未知不参与校验。
    expected: Option<u32>,
    finished: bool,
}

impl Write for ZipBackend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        let w = g
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "zip writer 已 finish"))?;
        let n = w.write(buf)?;
        if n > 0 {
            self.crc.update(&buf[..n]);
            g.bytes_in += n as u64;
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(w) = g.writer.as_mut() {
            w.flush()?;
        }
        Ok(())
    }
}

impl EntrySinkBackend for ZipBackend {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.flush().map_err(HzipError::from)?;
        // drop 校验链：expected 存在且 != 旁路 CRC → 挂起（finish 上抛）
        if let Some(exp) = self.expected {
            let act = self.crc.finalize();
            if exp != act {
                let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                if g.pending.is_none() {
                    g.pending = Some(HzipError::Checksum {
                        entry: "".into(),
                        expected: exp,
                        actual: act,
                    });
                }
            }
        }
        Ok(())
    }
}

/// 目录/符号链接 backend：零数据条目（next_entry 已落盘）。
struct EmptyBackend;

impl Write for EmptyBackend {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other(
            "目录/符号链接条目不接受数据写入（条目已零长落盘）",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl EntrySinkBackend for EmptyBackend {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        Ok(())
    }
}
