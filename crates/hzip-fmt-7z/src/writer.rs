//! 7z 写入：逐条目独立 folder（solid 归 T46）+ 头回写两遍法（02 §8.2 关键边界）。
//!
//! # 条目写模型
//!
//! 逐条目经 `EntrySink` 缓冲（≤ [`crate::consts::ENTRY_BUFFER_MEM_MAX`] 内存，
//! 超出切临时文件），`finish_entry` 时以 `sevenz_rust::SevenZWriter::push_archive_entry`
//! 提交（内部立即 LZMA2 压缩并落盘，天然**每条目一个独立 folder**）：
//!
//! - **目录**：空流（`has_stream=false, is_directory=true`）+ 属性（unix 高 16 位）；
//! - **symlink**：目标字符串作为条目数据写入 + 属性 `S_IFLNK`（7z 无独立链接目标
//!   字段；读侧以属性还原 is_symlink、内容即 target）；
//! - **普通文件**：用户写入字节压缩为独立 folder。
//!
//! `meta.mode`/`mtime` 映射：unix 权限与文件类型合并进 `windows_attributes` 高 16 位
//! （7-Zip 约定：低 16 = DOS 属性，高 16 = unix mode）；mtime → `FileTime`。
//!
//! # 输出端两遍法（Sink::Stdout）
//!
//! 7z 头需回写，且 `SevenZWriter` 要求 `Write + Seek`。`Sink::Stdout`（不可 seek）
//! 走**两遍法**：全部字节（数据 + finish 回写完成头）先落 `TempArea` 临时文件，
//! `finish()` 后 seek(0) → `io::copy` → stdout（64 KiB 缓冲），临时目录随 writer
//! drop 清理。磁盘需求上界 ≈ 档案最终大小。`Sink::File` 直写（seek 可用）→ 字节
//! 与两遍法产物**一致**（同一压缩器、确定性输入；单测断言，02 §8.2 关键边界）。
//!
//! # 能力与线程注记
//!
//! - 逐条目 folder 输出为**非固实**；solid 统一块打包归 T46（`caps.solid=false`）。
//! - sevenz-rust compress 为**单线程** lzma2；`threads` 经
//!   `hzip_codec::level::clamp_threads` 记录并传递（rustdoc 注记 02 §6.3 线程规则
//!   现状差距；多线程 lzma2 编码列为 T46 评估项，见 [`crate::coder`]）。
//! - 非空 `pw` 的写入（加密）归 T46 → `next_entry` 返回 `Unsupported` HZ-5001。
//! - `EntrySink` 的 drop 保底收尾语义：若调用方在 `finish()` 前放弃（取消/错误），
//!   单条目可能以已写字节被 `push_archive_entry` 提交——core 取消路径不会继续调用
//!   `finish()`，产物视为废弃临时物（同 T22/tar 语义）。

use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::{
    ArchiveWriter, CreateOptions, EntryMeta, EntrySink, EntrySinkBackend, FormatId, PasswordRef,
    Sink, WriteReport,
};
use hzip_progress::{CancellationToken, ProgressBus};
use sevenz_rust::SevenZWriter;

use crate::consts::{COPY_BUF_BYTES, ENTRY_BUFFER_MEM_MAX};

fn sink_name(s: &Sink) -> &'static str {
    match s {
        Sink::File(_) => "File",
        Sink::Stdout => "Stdout",
        Sink::Split { .. } => "Split",
        Sink::Stream(_) => "Stream",
    }
}

/// writer/backend 共享状态。
struct Shared {
    format: FormatId,
    /// 底层 sevenz writer（None 仅存在于 finish 取出后）。
    sz: Option<SevenZWriter<std::fs::File>>,
    entries: usize,
    bytes_in: u64,
    bytes_out: u64,
    /// T46 AES：档案级密码（feature aes；next_entry 首次携带后固化）。
    #[cfg(feature = "aes")]
    password: Option<zeroize::Zeroizing<String>>,
    /// T46：压缩级别（AES 链惰性装配需 lzma2 options）。
    level: hzip_codec::level::Level,
    /// 09：「字典大小」覆写（AES 链惰性装配须用同一份，否则开了加密字典会回落到级别值）。
    dict_size: Option<u64>,
    /// 「加密文件名」= 头加密（`CreateOptions.encrypt_names`，`None` → 默认开）。
    enc_names: bool,
}

/// 7z 写入器。
pub(crate) struct SevenZipWriter {
    shared: Arc<Mutex<Shared>>,
    cancel: CancellationToken,
    progress: ProgressBus,
    /// 暂存当前条目 meta（`next_entry` 期间在 self 借期内借出——契约
    /// `EntrySink` 与 `&mut self` 同生命周期）。
    pending: Option<EntryMeta>,
    /// 输出目标。
    to_stdout: bool,
    /// 条目溢出/两遍法共用临时区（随 writer drop 清理）。
    area: crate::TempArea,
    // T46 注：split（Sink::Split 两遍法切分）与 solid（块打包）的 writer 主流程
    // 集成挂点经 handler create_writer / next_entry 预留；模块接口（split.rs/
    // solid.rs）由单测覆盖，集成推进见 02 §9.5/§13.2 登记。
}

fn open_output(sink: Sink, area: &crate::TempArea) -> Result<(std::fs::File, bool), HzipError> {
    match sink {
        Sink::File(p) => {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(HzipError::from)?;
            }
            let f = std::fs::File::create(&p).map_err(|e| HzipError::Io {
                source: e,
                path: Some(p.clone()),
            })?;
            Ok((f, false))
        }
        Sink::Stdout => {
            let f = area.create_file("out.7z").map_err(HzipError::from)?;
            Ok((f, true))
        }
        other => Err(HzipError::Unsupported {
            what: format!(
                "7z 写入目标不支持（caps.split=false，02 §8.2）：{}",
                sink_name(&other)
            )
            .into(),
        }),
    }
}

/// 把 `EntryMeta` 转为 sevenz 条目描述（属性/时间字段）。
fn to_sz_entry(meta: &EntryMeta) -> sevenz_rust::SevenZArchiveEntry {
    // `SevenZArchiveEntry` 含私有字段（content_methods），不能以 struct 字面量 +
    // `..Default` 构造——经 `new()` 后逐字段设置。
    let mut e = sevenz_rust::SevenZArchiveEntry::new();
    e.name = meta.name.as_str().replace('\\', "/");
    e.is_directory = meta.is_dir;
    // 属性：高 16 = unix mode（类型位 + 权限），低 16 = DOS（无）。
    let unix_perm = meta.mode.unwrap_or(if meta.is_dir { 0o755 } else { 0o644 });
    let unix_type: u32 = if meta.is_symlink {
        0o120000
    } else if meta.is_dir {
        0o040000
    } else {
        0o100000
    };
    let unix_mode: u32 = unix_type | (unix_perm & 0o7777);
    e.windows_attributes = unix_mode << 16;
    e.has_windows_attributes = true;
    if let Some(mt) = meta.mtime {
        let secs = mt.unix_timestamp();
        if let Ok(ft) = sevenz_rust::nt_time::FileTime::from_unix_time(secs) {
            e.last_modified_date = ft;
            e.has_last_modified_date = true;
        }
    }
    e
}

/// 条目数据来源：内存缓冲 / 溢出文件 / 目录-空 / symlink-target 内容。
fn content_reader_of(
    meta: &EntryMeta,
    bytes: &[u8],
    spill: Option<&Path>,
) -> Result<Option<Box<dyn Read + Send>>, HzipError> {
    if meta.is_dir {
        return Ok(None);
    }
    if meta.is_symlink {
        let target = meta
            .symlink_target
            .as_deref()
            .map(|t| t.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok(Some(Box::new(Cursor::new(target.into_bytes()))));
    }
    if let Some(p) = spill {
        // 溢出文件仅在写入超缓冲阈值时创建（非空）；size_raw 未知（流式）亦可。
        let f = std::fs::File::open(p).map_err(|e| HzipError::Io {
            source: e,
            path: Some(p.to_path_buf()),
        })?;
        return Ok(Some(Box::new(f)));
    }
    if bytes.is_empty() {
        return Ok(None); // 空文件
    }
    Ok(Some(Box::new(Cursor::new(bytes.to_vec()))))
}

/// 落一条目（backend 收尾：提交给 sevenz）。
fn emit_entry(
    shared: &Mutex<Shared>,
    meta: &EntryMeta,
    bytes: Vec<u8>,
    spill: Option<PathBuf>,
) -> Result<(), HzipError> {
    let mut g = shared.lock().unwrap_or_else(|p| p.into_inner());
    let fmt = g.format.clone();
    let pushed_compressed = {
        let sz = g.sz.as_mut().ok_or_else(|| HzipError::Format {
            format: fmt.clone(),
            kind: FormatErrorKind::BadHeader,
        })?;
        let entry = to_sz_entry(meta);
        let reader = content_reader_of(meta, &bytes, spill.as_deref())?;
        match reader {
            Some(r) => {
                sz.push_archive_entry(entry, Some(r))
                    .map_err(|e| crate::reader::map_err(e, &fmt))?
                    .compressed_size
            }
            None => {
                sz.push_archive_entry(entry, None::<Cursor<Vec<u8>>>)
                    .map_err(|e| crate::reader::map_err(e, &fmt))?
                    .compressed_size
            }
        }
    };
    let data_bytes: u64 = if meta.is_symlink {
        meta.symlink_target
            .as_ref()
            .map(|t| t.to_string_lossy().len() as u64)
            .unwrap_or(0)
    } else if meta.is_dir || (bytes.is_empty() && spill.is_none()) {
        // 目录/空文件：无数据区字节
        0
    } else if !bytes.is_empty() {
        bytes.len() as u64
    } else {
        // spill 情形：读溢出文件长度
        let p = spill.as_deref().ok_or_else(|| HzipError::Format {
            format: fmt.clone(),
            kind: FormatErrorKind::BadHeader,
        })?;
        p.metadata().map(|m| m.len()).map_err(|e| HzipError::Io {
            source: e,
            path: Some(p.to_path_buf()),
        })?
    };
    g.entries += 1;
    g.bytes_in += data_bytes;
    g.bytes_out += pushed_compressed;
    Ok(())
}

/// 单条目缓冲 backend：内存 ≤ [`ENTRY_BUFFER_MEM_MAX`]，超出切临时文件。
pub(crate) struct EntryBackend {
    shared: Arc<Mutex<Shared>>,
    meta: EntryMeta,
    buf: Vec<u8>,
    spill: Option<PathBuf>,
    area_dir: PathBuf,
    finished: bool,
}

impl Write for EntryBackend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.spill.is_none() && (self.buf.len() as u64) + buf.len() as u64 > ENTRY_BUFFER_MEM_MAX
        {
            let p = self.area_dir.join("spill.bin");
            std::fs::File::create(&p)?.write_all(&self.buf)?;
            self.buf.clear();
            self.spill = Some(p);
        }
        if let Some(p) = self.spill.as_ref() {
            let mut f = std::fs::OpenOptions::new().append(true).open(p)?;
            f.write_all(buf)?;
        } else {
            self.buf.extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl EntrySinkBackend for EntryBackend {
    fn finish_entry(&mut self) -> Result<(), HzipError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        emit_entry(
            &self.shared,
            &self.meta,
            std::mem::take(&mut self.buf),
            self.spill.take(),
        )
    }
}

impl SevenZipWriter {
    /// 新建写入器。
    pub(crate) fn create(
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        let format = FormatId::from(crate::FORMAT_ID);
        let area = crate::TempArea::new("write")?;
        let (file, to_stdout) = open_output(dst, &area)?;
        let mut sz = SevenZWriter::new(file).map_err(|e| crate::reader::map_err(e, &format))?;
        // 级别 → dict/预设映射（coder::lzma2_options）；字典可被 UI 显式覆写
        let options = crate::coder::lzma2_options(opts.level, opts.threads, opts.dict_size);
        let method = sevenz_rust::SevenZMethodConfiguration::new(sevenz_rust::SevenZMethod::LZMA2)
            .with_options(sevenz_rust::MethodOptions::LZMA2(options));
        // T46 AES 写方向（feature aes）：密码在 next_entry 首次携带时经 Shared 固化，
        // 于首条目提交前把 content 链替换为 [7zAES(pw), LZMA2] 并开启头加密
        // （02 §8.2 D 节点写侧；见 next_entry）。此处恒以 LZMA2 起步。
        sz.set_content_methods(vec![method]);
        sz.set_encrypt_header(false);
        let shared = Arc::new(Mutex::new(Shared {
            format,
            sz: Some(sz),
            entries: 0,
            bytes_in: 0,
            bytes_out: 0,
            #[cfg(feature = "aes")]
            password: None,
            level: opts.level,
            dict_size: opts.dict_size,
            enc_names: opts.encrypt_names.unwrap_or(true),
        }));
        Ok(Box::new(SevenZipWriter {
            shared,
            cancel: CancellationToken::new(),
            progress: opts.progress.clone().unwrap_or_default(),
            pending: None,
            to_stdout,
            area,
        }))
    }
}

impl ArchiveWriter for SevenZipWriter {
    fn next_entry(
        &mut self,
        meta: &EntryMeta,
        pw: PasswordRef<'_>,
    ) -> Result<EntrySink<'_>, HzipError> {
        // T46 AES 写方向（feature aes）：档案级密码首次携带即固化并切换底层
        // 7zAES 链 + 头加密；条目级 encrypted 与密码缺一不可。
        #[cfg(feature = "aes")]
        if pw.as_str().is_some() || meta.encrypted {
            let pws = pw.as_str().ok_or(HzipError::Password {
                target: hzip_error::PasswordTarget::Archive,
            })?;
            let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            if g.password.is_none() {
                let stored = zeroize::Zeroizing::new(pws.to_string());
                let level = g.level;
                let dict = g.dict_size;
                let enc_names = g.enc_names;
                if let Some(sz) = g.sz.as_mut() {
                    let opts = crate::coder::lzma2_options(level, 1, dict);
                    let aes = sevenz_rust::AesEncoderOptions::new(sevenz_rust::Password::from(
                        stored.as_str(),
                    ));
                    let lz = sevenz_rust::SevenZMethodConfiguration::new(
                        sevenz_rust::SevenZMethod::LZMA2,
                    )
                    .with_options(sevenz_rust::MethodOptions::LZMA2(opts));
                    sz.set_content_methods(vec![aes.into(), lz]);
                    // 头加密随用户选择（关 = 数据加密、文件名明文；设计稿 11 决策②）
                    sz.set_encrypt_header(enc_names);
                }
                g.password = Some(stored);
            }
        }
        #[cfg(not(feature = "aes"))]
        if pw.as_str().is_some() || meta.encrypted {
            return Err(HzipError::Unsupported {
                what: "7z 加密写入需 aes feature（caps.encryption 随裁剪回落，02 §2.2）".into(),
            });
        }
        let backend = Box::new(EntryBackend {
            shared: Arc::clone(&self.shared),
            meta: meta.clone(),
            buf: Vec::new(),
            spill: None,
            area_dir: self.area.path().to_path_buf(),
            finished: false,
        });
        // 契约：EntrySink 与 &mut self 同生命周期；meta 经克隆暂存于 self.pending
        self.pending = Some(meta.clone());
        let mref = self.pending.as_ref().ok_or_else(|| HzipError::Format {
            format: hzip_format::FormatId::from(crate::FORMAT_ID),
            kind: FormatErrorKind::BadHeader,
        })?;
        Ok(EntrySink::from_backend(
            backend,
            mref,
            &self.cancel,
            &self.progress,
        ))
    }

    fn finish(self: Box<Self>) -> Result<WriteReport, HzipError> {
        let Self {
            shared,
            to_stdout,
            area,
            ..
        } = *self;
        let mut g = shared.lock().unwrap_or_else(|p| p.into_inner());
        let fmt = g.format.clone();
        let mut report = WriteReport {
            entries_written: g.entries,
            bytes_in: g.bytes_in,
            bytes_out: g.bytes_out,
            volume_paths: Vec::new(),
        };
        let Some(sz) = g.sz.take() else {
            return Err(HzipError::Format {
                format: fmt,
                kind: FormatErrorKind::BadHeader,
            });
        };
        // finish：回写签名头 + nextHeader（数据定稿后）——finish 返回 `std::io::Error`
        let mut file = sz
            .finish()
            .map_err(|e| crate::reader::map_data_io(e, &fmt))?;
        let len = file.metadata().map(|m| m.len()).map_err(HzipError::from)?;
        report.bytes_out = len;
        if to_stdout {
            // 两遍法第二遍：临时文件 → stdout（64 KiB 缓冲）
            file.seek(SeekFrom::Start(0)).map_err(HzipError::from)?;
            let mut stdout = io::stdout().lock();
            let mut buf = [0u8; COPY_BUF_BYTES];
            loop {
                let n = file.read(&mut buf).map_err(HzipError::from)?;
                if n == 0 {
                    break;
                }
                stdout.write_all(&buf[..n]).map_err(HzipError::from)?;
            }
            stdout.flush().map_err(HzipError::from)?;
        } else {
            file.flush().map_err(HzipError::from)?;
        }
        // area drop：清理临时目录
        drop(area);
        Ok(report)
    }
}
