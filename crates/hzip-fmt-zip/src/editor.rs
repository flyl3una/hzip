//! zip 编辑：`ArchiveEditor::rewrite`（02 §7.3 / §9.9；T24 §4.7）。
//!
//! # 语义（TC-CONF-009 原子性）
//!
//! `rewrite(changes, dst, ctx)` **流式重写**新档案，不改动源（fmt 层只保证「写往
//! `dst`，不触碰 src」；原子替换由 core 编辑服务以「临时物化 → 原子替换」保证，
//! 02 §9.9 时序）。
//!
//! # 两条路径（用户口径 2026-09-28：重命名/新建目录不得改动其他条目的信息）
//!
//! | 路径 | 触发 | 未命中变更的条目 |
//! |---|---|---|
//! | **原样拷贝**〔默认〕 | 源是普通文件 + 能解析中央目录 + 条目名全为 UTF-8 | `raw_copy_file[_rename]`：压缩数据、权限位、时间、加密字节**原样搬运** |
//! | **流式重写**〔回退〕 | salvage 档案 / 流源 / 条目名非 UTF-8 | `read_entry → next_entry` 解压重压 |
//!
//! 原样拷贝路径下「只改名 / 新建目录」既**快**（不解压不重压——大档案改名是毫秒级）
//! 又**保真**（不因重压改变压缩方法/字节、不丢额外字段）。两处例外仍需重写该条目：
//!
//! - **加密码条目**：zip crate 的 `raw_copy_file` 以 `ZipFile::options()` 重建本地头，
//!   而该选项**不携带 AES 标记**（方法 99 / 0x9901 extra field）——原样拷贝会把
//!   密文写成「普通 deflate」＝坏档。故加密条目改走「解密 + 同算法同密码重加密」；
//! - **命中 `Replace` 的条目**：内容本就换成新文件。
//!
//! 变更集语义见 [`hzip_format::Change`]；`Rename`/`MakeDir` 见 [`EditOps::parse`]。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use hzip_error::{HzipError, PasswordTarget};
use hzip_format::{
    ArchiveEditor, ArchiveReader, Change, ChangeSet, EncryptionRequest, EntryInfo, EntryMeta,
    EntryName, FormatId, OpenOptions, PasswordRef, Sink, Source, WriteCtx, WriteReport,
};

use crate::handler;
use crate::reader::ZipReader;

/// zip 编辑器（持源 reader 打开期状态）。
pub(crate) struct ZipEditor {
    reader: Box<dyn ArchiveReader>,
    /// 源档案路径（仅 `Source::File` 有值）——原样拷贝路径需自开 `ZipArchive`。
    src_path: Option<PathBuf>,
    /// 档案密码（core 经 `OpenOptions.password` 传入）：加密条目解密/重加密用。
    password: Option<zeroize::Zeroizing<String>>,
}

impl ZipEditor {
    /// 打开（源须可读；`opts.password` 供加密条目解密）。
    pub(crate) fn open(
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        let src_path = match &src {
            Source::File(p) => Some(p.clone()),
            _ => None,
        };
        let reader = ZipReader::open(src)?;
        Ok(Box::new(ZipEditor {
            reader,
            src_path,
            password: opts.password.clone(),
        }))
    }

    /// 重写主体：优先原样拷贝，不可用则回退流式重写。
    fn rewrite_impl(
        &self,
        changes: &ChangeSet,
        dst: Sink,
        ctx: &WriteCtx<'_>,
    ) -> Result<WriteReport, HzipError> {
        let ops = EditOps::parse(changes);
        // 回退判定必须在**碰 dst 之前**完成（同一 Sink 不能写两遍）
        if let Some(dst_path) = match &dst {
            Sink::File(p) => Some(p.clone()),
            _ => None,
        } && let Some(report) = self.rewrite_preserving(&ops, &dst_path, ctx)?
        {
            return Ok(report);
        }
        self.rewrite_stream(&ops, dst, ctx)
    }

    /// 原样拷贝路径：`Ok(None)` = 本路径不适用（尚未写入任何字节，可安全回退）。
    fn rewrite_preserving(
        &self,
        ops: &EditOps,
        dst: &std::path::Path,
        ctx: &WriteCtx<'_>,
    ) -> Result<Option<WriteReport>, HzipError> {
        let Some(src) = self.src_path.as_deref() else {
            return Ok(None);
        };
        let Ok(file) = std::fs::File::open(src) else {
            return Ok(None);
        };
        let Ok(mut archive) = zip::ZipArchive::new(file) else {
            return Ok(None); // salvage/坏中央目录 → 流式路径
        };
        // 条目名必须全是合法 UTF-8：非 UTF-8 名经 zip crate 解码后重写会改字节
        // （cp437/GBK 误转），这类档案整档回退流式路径（reader 侧解码保持一致）。
        for i in 0..archive.len() {
            let f = archive
                .by_index_raw(i)
                .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
            if std::str::from_utf8(f.name_raw()).is_err() {
                return Ok(None);
            }
        }
        let infos = self.reader.entries()?;
        // 元数据表与原始条目表逐索引对齐（`ZipReader::from_archive` 逐 index 建表）；
        // 不齐（异常档案）→ 回退，避免错位改错条目。
        if infos.len() != archive.len() {
            return Ok(None);
        }
        // 改名目标必须命中至少一个条目：命中率为零时若照样「成功」，用户看到的
        // 是「任务完成但什么都没改」——静默欺骗（7z 侧同一情形报 BadHeader，
        // 此处对齐）。
        if !ops.renames.is_empty() {
            let names: Vec<String> = (0..archive.len())
                .map(|i| {
                    archive
                        .by_index_raw(i)
                        .map(|f| f.name().to_string())
                        .map_err(|e| crate::map_zip_err(e, &handler::fmt()))
                })
                .collect::<Result<_, _>>()?;
            for (from, _) in &ops.renames {
                if !names.iter().any(|n| rename_hit(n, from)) {
                    return Err(HzipError::Format {
                        format: handler::fmt(),
                        kind: hzip_error::FormatErrorKind::BadHeader,
                    });
                }
            }
        }
        let archive_enc = self.encryption_of(&infos);
        let out = std::fs::File::create(dst).map_err(HzipError::from)?;
        let mut zw = zip::ZipWriter::new(std::io::BufWriter::new(out));
        let mut written = 0usize;
        let mut bytes_in = 0u64;
        for (i, info) in infos.iter().enumerate() {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            if ops.deleted.get(i).copied().unwrap_or(false) {
                continue;
            }
            let raw_name = archive
                .by_index_raw(i)
                .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?
                .name()
                .to_string();
            let target = map_rename(&raw_name, &ops.renames).unwrap_or_else(|| raw_name.clone());
            if let Some(new_content) = ops.replaced.get(&i) {
                // 替换：新内容按原条目方法/权限/时间 + 档案加密算法重写
                let opts = self.write_options(info, ctx, archive_enc)?;
                zw.start_file(target, opts)
                    .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
                let f = std::fs::File::open(new_content).map_err(HzipError::from)?;
                bytes_in += stream_into(f, &mut zw, ctx)?;
                written += 1;
                continue;
            }
            if info.encrypted && !info.is_dir && !info.is_symlink {
                // 加密条目：解密重写（同算法同密码）——原样拷贝会丢 AES 标记
                let Some(pw) = self.password.clone() else {
                    return Err(HzipError::Password {
                        target: PasswordTarget::Archive,
                    });
                };
                let opts = self.write_options(info, ctx, archive_enc)?;
                zw.start_file(target, opts)
                    .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
                let mut r = self.reader.read_entry(i, PasswordRef::new(&pw))?;
                bytes_in += stream_into(&mut r, &mut zw, ctx)?;
                written += 1;
                continue;
            }
            // 普通条目：原样拷贝（字节保真）
            let f = archive
                .by_index_raw(i)
                .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
            let r = if target == raw_name {
                zw.raw_copy_file(f)
            } else {
                zw.raw_copy_file_rename(f, target)
            };
            r.map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
            written += 1;
        }
        // 新建目录：时间取执行时刻（归档惯例：新条目无历史时间可继承）
        for name in &ops.mkdirs {
            let opts = zip::write::SimpleFileOptions::default()
                .unix_permissions(0o755)
                .last_modified_time(now_datetime());
            zw.add_directory(name.as_str().trim_end_matches('/'), opts)
                .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
            written += 1;
        }
        // 追加条目：按档案加密算法（有则加密）+ ctx 级别
        for (src_file, name) in &ops.appended {
            let meta = EntryMeta {
                name: name.clone(),
                size_raw: std::fs::metadata(src_file).ok().map(|m| m.len()),
                encrypted: archive_enc.is_some(),
                ..Default::default()
            };
            let opts = self.write_options_plain(&meta, None, ctx, archive_enc)?;
            zw.start_file(name.as_str(), opts)
                .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
            let f = std::fs::File::open(src_file).map_err(HzipError::from)?;
            bytes_in += stream_into(f, &mut zw, ctx)?;
            written += 1;
        }
        let mut out = zw
            .finish()
            .map_err(|e| crate::map_zip_err(e, &handler::fmt()))?;
        out.flush().map_err(HzipError::from)?;
        Ok(Some(WriteReport {
            entries_written: written,
            bytes_in,
            bytes_out: std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0),
            volume_paths: Vec::new(),
        }))
    }

    /// 流式重写路径（原实现；回退时使用）。
    fn rewrite_stream(
        &self,
        ops: &EditOps,
        dst: Sink,
        ctx: &WriteCtx<'_>,
    ) -> Result<WriteReport, HzipError> {
        let infos = self.reader.entries()?;
        // 同原样拷贝路径：改名目标必须命中至少一个条目（静默无操作＝欺骗）
        for (from, _) in &ops.renames {
            if !infos.iter().any(|i| rename_hit(i.name.as_str(), from)) {
                return Err(HzipError::Format {
                    format: handler::fmt(),
                    kind: hzip_error::FormatErrorKind::BadHeader,
                });
            }
        }
        let mut writer =
            crate::writer::ZipWriterImpl::create(dst, ctx.opts).map_err(annotate_rewrite)?;
        let mut written = 0usize;
        for (i, info) in infos.iter().enumerate() {
            if ops.deleted.get(i).copied().unwrap_or(false) {
                continue; // Delete：跳过
            }
            let name = map_rename(info.name.as_str(), &ops.renames)
                .map(|n| hzip_format::parse_entry_name(&n).unwrap_or_else(|_| info.name.clone()))
                .unwrap_or_else(|| info.name.clone());
            let meta = EntryMeta {
                name,
                is_dir: info.is_dir,
                is_symlink: info.is_symlink,
                symlink_target: info.symlink_target.clone(),
                size_raw: Some(info.size_raw),
                mtime: info.mtime,
                mode: info.mode,
                attrs: info.attributes,
                encrypted: info.encrypted,
            };
            // Replace：以 src 本地文件内容替换（大小/CRC 未知，流式）
            if let Some(src) = ops.replaced.get(&i) {
                let f = std::fs::File::open(src).map_err(HzipError::from)?;
                let sink = writer
                    .next_entry(&meta, self.password_ref())
                    .map_err(annotate_rewrite)?;
                stream_into(f, sink, ctx)?;
                written += 1;
                continue;
            }
            // 直通拷贝（目录/符号链接经 next_entry 零数据落盘）
            if info.is_dir || info.is_symlink {
                let sink = writer
                    .next_entry(&meta, self.password_ref())
                    .map_err(annotate_rewrite)?;
                sink.finish().map_err(annotate_rewrite)?;
                written += 1;
                continue;
            }
            // 普通条目：read_entry → next_entry 直通
            let r = self
                .reader
                .read_entry(i, self.password_ref())
                .map_err(annotate_rewrite)?;
            let sink = writer
                .next_entry(&meta, self.password_ref())
                .map_err(annotate_rewrite)?;
            stream_into(r, sink, ctx)?;
            written += 1;
        }
        // MakeDir 变更：空目录条目
        for name in &ops.mkdirs {
            let meta = EntryMeta {
                name: name.clone(),
                is_dir: true,
                size_raw: Some(0),
                mtime: Some(time::OffsetDateTime::now_utc()),
                ..Default::default()
            };
            let sink = writer
                .next_entry(&meta, self.password_ref())
                .map_err(annotate_rewrite)?;
            sink.finish().map_err(annotate_rewrite)?;
            written += 1;
        }
        // Append 变更：按序追加
        for (src, name) in &ops.appended {
            let f = std::fs::File::open(src).map_err(HzipError::from)?;
            let data_len = f.metadata().map(|m| m.len()).unwrap_or(0);
            let meta = EntryMeta {
                name: name.clone(),
                size_raw: Some(data_len),
                ..Default::default()
            };
            let sink = writer
                .next_entry(&meta, self.password_ref())
                .map_err(annotate_rewrite)?;
            stream_into(f, sink, ctx)?;
            written += 1;
        }
        let report = writer.finish().map_err(annotate_rewrite)?;
        Ok(WriteReport {
            entries_written: report.entries_written.max(written),
            ..report
        })
    }

    /// 密码引用（无密码 = 未加密路径）。
    fn password_ref(&self) -> PasswordRef<'_> {
        match &self.password {
            Some(p) => PasswordRef::new(p),
            None => PasswordRef::none(),
        }
    }

    /// 档案级加密算法（取首个加密条目的算法；无加密条目 → None）。
    fn encryption_of(&self, infos: &[EntryInfo]) -> Option<EncryptionRequest> {
        infos
            .iter()
            .find(|i| i.encrypted)
            .map(|i| encryption_request(i.encryption))
    }

    /// 条目写入选项（原条目方法/权限/时间 + 档案加密算法）。
    fn write_options<'a>(
        &'a self,
        meta: &EntryInfo,
        ctx: &WriteCtx<'_>,
        enc: Option<EncryptionRequest>,
    ) -> Result<zip::write::FileOptions<'a, ()>, HzipError> {
        let m = EntryMeta {
            name: meta.name.clone(),
            is_dir: meta.is_dir,
            is_symlink: meta.is_symlink,
            size_raw: Some(meta.size_raw),
            mtime: meta.mtime,
            mode: meta.mode,
            encrypted: meta.encrypted,
            ..Default::default()
        };
        self.write_options_plain(&m, meta.codec.as_deref(), ctx, enc)
    }

    /// 写入选项构造（`ctx` 给级别、`enc` 给加密；条目 method/mode/mtime 沿用）。
    fn write_options_plain<'a>(
        &'a self,
        meta: &EntryMeta,
        codec: Option<&str>,
        ctx: &WriteCtx<'_>,
        enc: Option<EncryptionRequest>,
    ) -> Result<zip::write::FileOptions<'a, ()>, HzipError> {
        let method = method_of(codec, ctx.opts.method);
        let mut o = zip::write::FileOptions::default()
            .compression_method(method)
            .large_file(true);
        // 级别：zip 头不存原始级别（`EntryInfo::compression_level` 恒 None）——
        // 与压缩路径同语义，按 `ctx.opts` 级别写（Stored 无级别）
        if method != zip::CompressionMethod::Stored
            && let Some(lv) = level_of(ctx.opts.level)
        {
            o = o.compression_level(Some(lv));
        }
        if let Some(mode) = meta.mode {
            o = o.unix_permissions(mode);
        }
        if let Some(mt) = meta.mtime
            && let Ok(dt) = zip::DateTime::try_from(mt)
        {
            o = o.last_modified_time(dt);
        }
        // 加密：仅文件条目（目录零长、符号链接内容（目标路径）按 zip 契约不加密）
        if meta.encrypted && !meta.is_dir && !meta.is_symlink {
            let Some(req) = enc else {
                return Err(HzipError::Unsupported {
                    what: "zip 重写请求加密但档案无加密算法可沿用".into(),
                });
            };
            let Some(pw) = self.password.as_ref() else {
                return Err(HzipError::Password {
                    target: PasswordTarget::Archive,
                });
            };
            use zip::unstable::write::FileOptionsExt;
            o = match req {
                EncryptionRequest::ZipCrypto => o.with_deprecated_encryption(pw.as_bytes()),
                EncryptionRequest::Aes(hzip_format::AesKey::Bits128) => {
                    o.with_aes_encryption(zip::AesMode::Aes128, pw.as_str())
                }
                EncryptionRequest::Aes(hzip_format::AesKey::Bits192) => {
                    o.with_aes_encryption(zip::AesMode::Aes192, pw.as_str())
                }
                EncryptionRequest::Aes(_) => {
                    o.with_aes_encryption(zip::AesMode::Aes256, pw.as_str())
                }
                other => {
                    return Err(HzipError::Unsupported {
                        what: format!("zip 不支持该写入加密方案：{other:?}").into(),
                    });
                }
            };
        }
        Ok(o)
    }
}

/// 条目加密算法标记 → 写入请求（`EntryInfo::encryption` 的取值约定见其文档）。
fn encryption_request(mark: Option<&'static str>) -> EncryptionRequest {
    match mark {
        Some("ZipCrypto") => EncryptionRequest::ZipCrypto,
        Some("AES-128") => EncryptionRequest::Aes(hzip_format::AesKey::Bits128),
        Some("AES-192") => EncryptionRequest::Aes(hzip_format::AesKey::Bits192),
        // AES-256 / "AES"（7z/rar 头不细分密钥长度）/未知标记：保守取最强
        _ => EncryptionRequest::Aes(hzip_format::AesKey::Bits256),
    }
}

/// 写入方法：优先沿用条目原方法（保真），未知/未给 → `ctx.opts` 指定的方法。
fn method_of(
    info_codec: Option<&str>,
    ctx_method: Option<hzip_codec::traits::CodecMethod>,
) -> zip::CompressionMethod {
    use hzip_codec::traits::CodecMethod;
    match info_codec {
        Some("store") => zip::CompressionMethod::Stored,
        Some("deflate") => zip::CompressionMethod::Deflated,
        Some("deflate64") => zip::CompressionMethod::Deflate64,
        Some("bzip2") => zip::CompressionMethod::Bzip2,
        Some("zstd") => zip::CompressionMethod::Zstd,
        // 未知方法（如 zipx LZMA）：按 `ctx.opts` 指定的方法重压
        _ => match ctx_method {
            Some(CodecMethod::BZip2) => zip::CompressionMethod::Bzip2,
            Some(CodecMethod::Zstd) => zip::CompressionMethod::Zstd,
            _ => zip::CompressionMethod::Deflated,
        },
    }
}

/// 级别数值（`Stored` 不设级别）。
fn level_of(level: hzip_codec::level::Level) -> Option<i64> {
    match level {
        hzip_codec::level::Level::Store => None,
        hzip_codec::level::Level::Fastest => Some(1),
        hzip_codec::level::Level::Fast => Some(3),
        hzip_codec::level::Level::Normal => Some(6),
        hzip_codec::level::Level::Maximum | hzip_codec::level::Level::Ultra => Some(9),
    }
}

/// 当前时刻（DOS 时间；越界回落 `DateTime` 默认值）。
fn now_datetime() -> zip::DateTime {
    zip::DateTime::try_from(time::OffsetDateTime::now_utc())
        .unwrap_or_else(|_| zip::DateTime::default())
}

/// 变更集分类（两条路径共用；id 越界与既有语义一致：忽略）。
struct EditOps {
    deleted: Vec<bool>,
    replaced: HashMap<usize, PathBuf>,
    appended: Vec<(PathBuf, EntryName)>,
    /// 改名表（`from` → `to`，均为归一化名）
    renames: Vec<(String, String)>,
    /// 新建目录（档案内完整路径）
    mkdirs: Vec<EntryName>,
}

impl EditOps {
    fn parse(changes: &ChangeSet) -> Self {
        let mut ops = Self {
            deleted: Vec::new(),
            replaced: HashMap::new(),
            appended: Vec::new(),
            renames: Vec::new(),
            mkdirs: Vec::new(),
        };
        for c in &changes.changes {
            match c {
                Change::Delete(id) => {
                    if ops.deleted.len() <= *id {
                        ops.deleted.resize(id + 1, false);
                    }
                    ops.deleted[*id] = true;
                }
                Change::Replace { entry, src } => {
                    ops.replaced.insert(*entry, src.clone());
                }
                Change::Append { src, name } => ops.appended.push((src.clone(), name.clone())),
                Change::Rename { from, to } if from.as_str() != to.as_str() => {
                    ops.renames.push((
                        from.as_str().trim_end_matches('/').to_string(),
                        to.as_str().trim_end_matches('/').to_string(),
                    ));
                }
                Change::Rename { .. } => {} // 同名 = 无动作
                Change::MakeDir { name } => ops.mkdirs.push(name.clone()),
                _ => {} // Change 非穷尽（T20 冻结契约可扩展）：未知变更忽略
            }
        }
        ops
    }
}

/// 改名命中判定（与 [`map_rename`] 同口径：同名条目，或 `from + '/'` 前缀的
/// 子树成员）。
fn rename_hit(raw: &str, from: &str) -> bool {
    let core = raw.trim_end_matches('/');
    core == from
        || core
            .strip_prefix(from)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// 改名映射（`raw` 为档案内名，尾斜杠原样保留）。
///
/// - 命中 `from` 本身 → `to`；
/// - 命中 `from + '/'` 前缀（目录整棵子树）→ `to + '/' + 余下段`；
/// - 目录条目的尾斜杠按输入保留（zip 目录条目名以 `/` 结尾，丢失会变普通条目）。
fn map_rename(raw: &str, renames: &[(String, String)]) -> Option<String> {
    if renames.is_empty() {
        return None;
    }
    let tail = if raw.ends_with('/') { "/" } else { "" };
    let core = raw.trim_end_matches('/');
    for (from, to) in renames {
        if core == from {
            return Some(format!("{to}{tail}"));
        }
        if let Some(rest) = core.strip_prefix(from.as_str())
            && let Some(rest) = rest.strip_prefix('/')
        {
            return Some(format!("{to}/{rest}{tail}"));
        }
    }
    None
}

/// 流拷贝（源 → 写入端；含取消检查点）。
fn stream_into(
    mut src: impl Read,
    mut sink: impl Write,
    ctx: &WriteCtx<'_>,
) -> Result<u64, HzipError> {
    let mut buf = vec![0u8; hzip_format::consts::COPY_CHUNK_BYTES as usize];
    let mut total = 0u64;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(HzipError::Cancelled);
        }
        let n = src.read(&mut buf).map_err(HzipError::from)?;
        if n == 0 {
            break;
        }
        sink.write_all(&buf[..n]).map_err(HzipError::from)?;
        total += n as u64;
    }
    sink.flush().map_err(HzipError::from)?;
    Ok(total)
}

fn annotate_rewrite(e: HzipError) -> HzipError {
    match e {
        HzipError::Format { format, kind } => HzipError::Format {
            format: if format.is_empty() {
                FormatId::from(handler::FORMAT_ID)
            } else {
                format
            },
            kind,
        },
        other => other,
    }
}

impl ArchiveEditor for ZipEditor {
    fn rewrite(
        &self,
        changes: ChangeSet,
        dst: Sink,
        ctx: &WriteCtx<'_>,
    ) -> Result<WriteReport, HzipError> {
        self.rewrite_impl(&changes, dst, ctx)
    }
}
