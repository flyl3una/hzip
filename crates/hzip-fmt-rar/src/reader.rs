//! unrar 绑定：rar 只读 `ArchiveReader`（02 §8.3 / T26 §4.2/§4.4）。
//!
//! # 设计（02 §7.3 四条契约红线对照）
//!
//! - **红线 1（entries 不读数据区）**：`open_reader` 在 [`panic_wall::call_unrar`]
//!   内以 `Archive::open_for_listing()` **一次列头缓存**（List 模式自动跳过数据
//!   区，TC-CONF-010 秒开）。列头先于卷发现，处理期跨卷 `EOpen` 也在此以
//!   2005 附卷名报出。
//! - **rar5 头加密**（`has_encrypted_headers()`）且无密码 → 无法列条目 →
//!   `Password{Header}` HZ-3102，**零条目名泄露**（T26 §7-6：错误信息/日志
//!   不得出现条目名）。
//! - **红线 2（read_entry 乱序/重复独立）**：unrar C API 只支持顺序流处理、以
//!   **文件**为输出单元——`read_entry(id, pw)` 每次**独立物化**到即弃临时文件
//!   （[`TempMaterialized`]，`Drop` 清理）后返回读取流（T26 §4.2 物化策略；
//!   乱序/重复天然成立，TC-CONF-004）。磁盘上界 ≈ 条目解压大小（双写成本）。
//! - **红线 3（错密码 → Password，禁乱码流）**：条目加密且未带密码 → 直接
//!   `Password{Archive}`（3101）；带密码但错误 → unrar `BadPassword` → 3101。
//!   正确密码内容一致、无乱码（TC-CONF-007）。
//! - **红线 4（任何输入不 panic）**：全部 unrar 调用经 [`panic_wall`]
//!   （全局互斥 + catch_unwind + 错误映射）；畸形档案 → `Format`/`Checksum`
//!   （TC-CONF-008）。
//!
//! # 密码生命周期（02 §7.3 / T26 §4.4）
//!
//! 本层不持有密码明文：`read_entry` 收 `PasswordRef`，`extract/test` 经
//! `ctx.passwords`（core 请求-应答）每次现取。零化串以局域 `Zeroizing` 存活至
//! unrar 打开返回（`Archive::with_password` 的借用仅存在于 open 调用期）。
//!
//! # 取消粒度（T26 §7.2 已知限制，挂跟踪表）
//!
//! unrar C API 单条目解包为不可中断阻塞调用——取消检查点在**条目边界**（落盘
//! 拷贝循环内每块检查）。单大条目无法即时取消（候选缓解：分卷/分块物化拆分，
//! 不阻塞本任务）。
//!
//! # 卷集（02 §8.3 / T26 §4.5）
//!
//! 列头确认卷标志：单卷 → `volumes=[p]`；分卷 → [`crate::volume`] 依序装配。
//! unrar 打开首卷后由 C 库自身跨卷读取；`volumes` 供打开期确定性缺口报错与
//! 处理期/列头期 `EOpen` → 2005（[`crate::volume::next_expected_name`]）。

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use hzip_error::{FormatErrorKind, HzipError, PasswordTarget};
use hzip_format::{
    ArchiveReader, ConflictPolicy, EntryInfo, ExtractCtx, ExtractReport, FormatCaps, FormatId,
    NameEncoding, PasswordRef, Selection, Source,
};
use time::OffsetDateTime;
use unrar::VolumeInfo;
use unrar::error::{Code, UnrarError};

use crate::caps;
use crate::panic_wall::{UnrarOutcome, call_unrar, map_unrar_err};

/// 流拷贝/取消检查块（02 附录 B.2：1 MiB）。
const COPY_CHUNK: usize = 1024 * 1024;

/// rar 读取器（open 时一次列头缓存；后续方法复用缓存）。
pub(crate) struct RarReader {
    /// 逻辑首卷路径（unrar 打开对象）。
    path: PathBuf,
    /// 依序卷集（单卷 = 1 元素）。
    volumes: Vec<PathBuf>,
    /// 是否分卷（unrar 卷标志）。
    is_multivol: bool,
    /// 条目缓存（红线 1 列头产物，id 即下标）。
    infos: Vec<EntryInfo>,
    /// 条目加密位（与 infos 对齐）。
    encrypted: Vec<bool>,
}

/// 即弃临时文件物化读取流（T26 §4.2 read_entry 物化策略）。
///
/// `_dir` 保活临时目录；`file` 为物化出的 `entry.bin` 只读句柄；`Drop` 随目录
/// 清理（即弃）。读至 EOF 置位 `eof`（防目录残留被误读）。
pub(crate) struct TempMaterialized {
    _dir: tempfile::TempDir,
    file: fs::File,
    eof: bool,
}

impl Read for TempMaterialized {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.eof || buf.is_empty() {
            return Ok(0);
        }
        let n = self.file.read(buf)?;
        if n == 0 {
            self.eof = true;
        }
        Ok(n)
    }
}

impl TempMaterialized {
    fn new(dir: tempfile::TempDir, out: PathBuf) -> io::Result<Self> {
        let f = fs::File::open(&out)?;
        Ok(Self {
            _dir: dir,
            file: f,
            eof: false,
        })
    }
}

/// 打开产物。
struct Listing {
    infos: Vec<EntryInfo>,
    encrypted: Vec<bool>,
    is_multivol: bool,
    volumes: Vec<PathBuf>,
}

impl RarReader {
    pub(crate) fn open(src: Source, opts: &hzip_format::OpenOptions) -> Result<Self, HzipError> {
        let first = match src {
            Source::File(p) => p,
            Source::Volumes { first, .. } => first,
            Source::Stdin | Source::Stream(_) => {
                return Err(HzipError::Unsupported {
                    what: "rar 读取需可随机访问文件源（unrar C API 只接受路径）".into(),
                });
            }
        };
        let listing = list_headers(&first, opts.password.as_ref().map(|z| z.as_str()))?;
        Ok(Self {
            path: first,
            volumes: listing.volumes,
            is_multivol: listing.is_multivol,
            infos: listing.infos,
            encrypted: listing.encrypted,
        })
    }

    fn bad_header(&self, why: &str) -> HzipError {
        tracing::warn!(%why, "rar 结构非法");
        HzipError::Format {
            format: crate::format_id(),
            kind: FormatErrorKind::BadHeader,
        }
    }

    /// 单条目物化（read_entry / extract / test 共用）。每次**独立重开** + 顺序
    /// 推进到 id → `extract_to`（C 库顺序流；乱序/重复天然独立）。
    fn materialize_entry(
        &self,
        id: usize,
        pw: PasswordRef<'_>,
    ) -> Result<TempMaterialized, HzipError> {
        if *self.encrypted.get(id).unwrap_or(&false) && pw.as_str().is_none() {
            // 红线 3：加密条目无密码 → Password 错误（禁乱码流）
            return Err(HzipError::Password {
                target: PasswordTarget::Archive,
            });
        }
        let dir = tempfile::Builder::new()
            .prefix("hzip-rar-")
            .tempdir()
            .map_err(HzipError::from)?;
        let out = dir.path().join("entry.bin");

        // 密码局域零化串（以字节承载；仅本函数存活，未经闭包逃逸）。
        let pw_owned: Option<Vec<u8>> = pw.as_str().map(|s| s.as_bytes().to_vec());
        let path = self.path.clone();
        let out_for_extract = out.clone();
        let multivol = self.is_multivol;
        let last_vol = self.volumes.last().cloned();
        // 目标条目是否加密（错误映射上下文：rar4 专有加密下 unrar 对错密码报
        // BadData@Process 而非 BadPassword——需按「带密码解加密条目」改判 3101）
        let target_encrypted = *self.encrypted.get(id).unwrap_or(&false);

        // ① 打开（with_unrar 内；可选密码）
        let opened = call_unrar("materialize-open", || {
            let a = match pw_owned.as_deref() {
                Some(p) => unrar::Archive::with_password(&path, p),
                None => unrar::Archive::new(&path),
            };
            let open = a.open_for_processing()?;
            Ok(open)
        })?;
        let open = match opened {
            UnrarOutcome::Ok(o) => o,
            UnrarOutcome::Err(e) => {
                return Err(map_processing_err_enc(
                    &e,
                    multivol,
                    last_vol.as_deref(),
                    target_encrypted,
                    pw.as_str().is_some(),
                ));
            }
            UnrarOutcome::Panicked => return Err(self.bad_header("unrar 打开期 panic 已拦截")),
        };

        // ② 顺序推进到 id 并 extract_to（物化核心；每次独立重开 → 乱序/重复独立）
        let extracted = call_unrar("materialize-extract", || {
            let mut cur = open; // 所有权移入闭包（read_header 消费式游标）
            let mut i = 0usize;
            loop {
                match cur.read_header()? {
                    Some(hdr) => {
                        if i == id {
                            // 命中：extract_to 物化到绝对路径（目录已存在）
                            let _ = hdr.extract_to(&out_for_extract)?;
                            return Ok(());
                        }
                        cur = hdr.skip()?;
                        i += 1;
                    }
                    None => {
                        return Err(UnrarError::from(Code::EndArchive, unrar::error::When::Read));
                    }
                }
            }
        })?;
        match extracted {
            UnrarOutcome::Ok(_) => {
                // extract_to 落盘成功（unrar 已校验 CRC）；打开读取流
                TempMaterialized::new(dir, out).map_err(HzipError::from)
            }
            UnrarOutcome::Err(e) => Err(map_processing_err_enc(
                &e,
                multivol,
                last_vol.as_deref(),
                target_encrypted,
                pw.as_str().is_some(),
            )),
            UnrarOutcome::Panicked => Err(self.bad_header("unrar 物化期 panic 已拦截")),
        }
    }
}

// ===================== 打开 / 列头 =====================

/// 列头（红线 1）：轻量打开取 flags → 卷发现 → 全量迭代建缓存。
///
/// `password`（`OpenOptions::password`）：头加密档案必须携带才能列头——
/// 无密码 → HZ-3102（零泄露，由 core `open_archive_retry` 问密码重开）；
/// `with_password` 对未加密档案无副作用。错密码经 `map_open_listing_err`
/// 映射 Password 错 → 同链重问。
fn list_headers(path: &Path, password: Option<&str>) -> Result<Listing, HzipError> {
    let fmt = crate::format_id();

    // ① 轻量打开（不迭代）：卷标志 / 头加密 / rar5。
    let pw_probe = password;
    let probe = call_unrar("open-probe", || {
        let a = match pw_probe {
            Some(p) => unrar::Archive::with_password(path, p),
            None => unrar::Archive::new(path),
        };
        let o = a.open_for_listing()?;
        Ok((
            o.volume_info(),
            o.has_encrypted_headers(),
            caps::header_is_rar5(path),
        ))
    })?;
    let (volume_info, header_encrypted, _is_rar5) = match probe {
        UnrarOutcome::Ok(x) => x,
        UnrarOutcome::Err(e) => return Err(map_open_listing_err(&fmt, &e)),
        UnrarOutcome::Panicked => {
            return Err(HzipError::Format {
                format: fmt,
                kind: FormatErrorKind::BadHeader,
            });
        }
    };

    // rar5 头加密：无法列条目 → 3102（零条目名泄露）
    if header_encrypted {
        return Err(HzipError::Password {
            target: PasswordTarget::Header,
        });
    }

    // ② 卷发现（仅分卷进入；确定性缺口在此 2005）
    let (volumes, is_multivol) = match volume_info {
        VolumeInfo::None => (vec![path.to_path_buf()], false),
        _ => (crate::volume::discover_multipart(path)?, true),
    };
    let last_vol = volumes.last().cloned();

    // ③ 全量列头迭代（List 自动跳过数据区）。闭包只收集原始头；parse_entry_name
    // （返回 HzipError）在闭包外执行——unrar 错误通道无法承载 HzipError。
    let list = call_unrar("open-listing", || {
        let a = match password {
            Some(p) => unrar::Archive::with_password(path, p),
            None => unrar::Archive::new(path),
        };
        let mut open = a.open_for_listing()?;
        let mut headers = Vec::new();
        let mut encrypted = Vec::new();
        for item in open.by_ref() {
            let hdr = item?;
            encrypted.push(hdr.is_encrypted());
            headers.push(hdr);
        }
        Ok((headers, encrypted))
    })?;
    let (headers, encrypted) = match list {
        UnrarOutcome::Ok(x) => x,
        UnrarOutcome::Err(e) => {
            // 列头期跨卷 EOpen（内容超出既有卷）→ 2005 附卷名；其余照映射
            return Err(map_processing_err(&e, is_multivol, last_vol.as_deref()));
        }
        UnrarOutcome::Panicked => {
            return Err(HzipError::Format {
                format: fmt,
                kind: FormatErrorKind::BadHeader,
            });
        }
    };
    let mut infos = Vec::with_capacity(headers.len());
    for (i, hdr) in headers.into_iter().enumerate() {
        infos.push(info_from_header(&fmt, &hdr, i)?);
    }

    Ok(Listing {
        infos,
        encrypted,
        is_multivol,
        volumes,
    })
}

/// 打开/列头期错误 → HzipError。
///
/// - MissingPassword/BadPassword 于**无密码**打开（头加密档案需密码才可读头）→
///   `Password{Header}` 3102（与 §7-6 零条目名泄露一致）；
/// - UnknownFormat → 2001（魔数不识别，TC-CONF-008 / 垃圾输入）；
/// - 其余 → [`map_unrar_err`]（format 字段补 rar）。
fn map_open_listing_err(fmt: &FormatId, e: &UnrarError) -> HzipError {
    match e.code {
        Code::MissingPassword | Code::BadPassword => HzipError::Password {
            target: PasswordTarget::Header,
        },
        Code::UnknownFormat => HzipError::Format {
            format: fmt.clone(),
            kind: FormatErrorKind::BadSignature,
        },
        _ => map_unrar_err(e).with_format(fmt.clone()),
    }
}

/// 处理期错误 → HzipError。
///
/// 分卷档案处理/列头跨卷 `EOpen` → `MissingVolume(下一期待卷名)` HZ-2005
/// （卷层发现已确认在分卷集合内；unrar 需要但文件缺失的卷即下一期待卷）。
fn map_processing_err(e: &UnrarError, is_multivol: bool, last_vol: Option<&Path>) -> HzipError {
    if is_multivol
        && e.code == Code::EOpen
        && let Some(last) = last_vol
    {
        let name = crate::volume::next_expected_name(last);
        return crate::volume::missing_volume(name);
    }
    let fmt = crate::format_id();
    map_unrar_err(e).with_format(fmt)
}

/// 物化/解包期错误 → HzipError（带「条目加密 + 是否带密码」上下文）。
///
/// rar4 专有数据加密在**错密码**时 unrar 返回 `BadData@Process`（CRC 校验失败）
/// 而非 `BadPassword`——若不改判会误报 3001（CRC）。凡「带密码解**加密条目**」
/// 期间报 `BadData@Process` → 按 `Password{Archive}` HZ-3101 处理（红线 3：
/// 错密码必须返回 Password，TC-CONF-007）。无密码的加密条目在本层入口已拦截；
/// 故到达此分支的 BadData 必属「密码有误或数据损坏」——密码语义优先。
fn map_processing_err_enc(
    e: &UnrarError,
    is_multivol: bool,
    last_vol: Option<&Path>,
    target_encrypted: bool,
    has_pw: bool,
) -> HzipError {
    if target_encrypted
        && has_pw
        && e.code == Code::BadData
        && e.when == unrar::error::When::Process
    {
        return HzipError::Password {
            target: PasswordTarget::Archive,
        };
    }
    map_processing_err(e, is_multivol, last_vol)
}

/// rar 条目名 → [`hzip_format::EntryName`]（**`\` → `/` 预转换**）。
///
/// rar 出身 Windows，条目名**常用 `\` 作路径分隔符**（实测样本：
/// `U4P4用户手册\启明版-….docx`），而 [`hzip_format::parse_entry_name`] 的
/// N2 规则直接拒绝 `\`（内部统一 `/`，见 types.rs）。不预转换的后果是
/// **整个档案打不开**（`bad header`）——哪怕只有一条这样的条目（256MB 样本
/// 实测如此）。zip / 7z 侧都在调用前做同一转换
/// （`hzip-fmt-zip/src/reader.rs::name_of`、`hzip-fmt-7z/src/reader.rs`），
/// rar 此前漏了这一步。
fn entry_name_of(filename: &std::path::Path) -> Result<hzip_format::EntryName, HzipError> {
    let raw = filename.to_string_lossy().replace('\\', "/");
    hzip_format::parse_entry_name(&raw)
}

/// unrar FileHeader → EntryInfo（经 [`entry_name_of`] 规范化）。
fn info_from_header(
    fmt: &FormatId,
    header: &unrar::FileHeader,
    id: usize,
) -> Result<EntryInfo, HzipError> {
    let is_dir = header.is_directory();
    let name =
        entry_name_of(&header.filename).map_err(|_| bad_header_fmt(fmt, "rar 条目名非法"))?;
    let mode = unix_mode_from_attr(header.file_attr, is_dir);
    let mtime = dos_time_to_mtime(header.file_time);
    Ok(EntryInfo {
        id,
        name,
        is_dir,
        is_symlink: false,
        symlink_target: None,
        size_raw: header.unpacked_size,
        size_packed: header.unpacked_size, // unrar 头未提供打包大小——保守同 raw（rustdoc：仅展示）
        mtime,
        mode,
        attributes: hzip_format::EntryAttrs {
            executable: mode.map(|m| m & 0o111 != 0),
            ..Default::default()
        },
        encrypted: header.is_encrypted(),
        header_encrypted: false,
        crc32: Some(header.file_crc),
        codec: Some("store".into()),
        name_encoding: NameEncoding::Unknown,
        // RAR5 加密恒 AES-256；RAR4 头不细分（unrar 不暴露）→ 统一 AES
        encryption: header.is_encrypted().then_some("AES"),
        compression_level: None,
    })
}

fn bad_header_fmt(fmt: &FormatId, why: &str) -> HzipError {
    tracing::warn!(%why, "rar 头非法");
    HzipError::Format {
        format: fmt.clone(),
        kind: FormatErrorKind::BadHeader,
    }
}

/// MS-DOS attr 位 → unix mode（近似：只读 0x01 / 目录 0x10；unrar 亦把 unix
/// mode 高字节放 file_attr，此处按 DOS 语义估，core 展示用途）。
fn unix_mode_from_attr(attr: u32, is_dir: bool) -> Option<u32> {
    let ro = attr & 0x01 != 0;
    Some(if ro {
        if is_dir { 0o555 } else { 0o444 }
    } else if is_dir {
        0o755
    } else {
        0o644
    })
}

/// unrar file_time（DOS 时间，2 秒精度）→ OffsetDateTime；非法 → None。
fn dos_time_to_mtime(t: u32) -> Option<OffsetDateTime> {
    let secs = (t & 0x1F) as u8 * 2;
    let mins = ((t >> 5) & 0x3F) as u8;
    let hours = ((t >> 11) & 0x1F) as u8;
    let day = ((t >> 16) & 0x1F) as u8;
    let month = ((t >> 21) & 0x0F) as u8;
    let year = ((t >> 25) & 0x7F) as i32 + 1980;
    if day == 0 || month == 0 {
        return None;
    }
    let month = time::Month::try_from(month).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    let tm = time::Time::from_hms(hours, mins, secs).ok()?;
    Some(date.with_time(tm).assume_utc())
}

// ===================== ArchiveReader 实现 =====================

impl ArchiveReader for RarReader {
    fn caps(&self) -> FormatCaps {
        caps::caps()
    }

    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        Ok(self.infos.clone())
    }

    fn read_entry(
        &self,
        id: usize,
        pw: PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        let info = self
            .infos
            .get(id)
            .ok_or_else(|| self.bad_header("read_entry id 越界"))?;
        if info.is_dir {
            return Ok(Box::new(io::empty()));
        }
        let mat = self.materialize_entry(id, pw)?;
        Ok(Box::new(mat))
    }

    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        let ids = resolve_selection(&self.infos, sel)?;
        let mut report = ExtractReport::default();
        for id in ids {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            let info = &self.infos[id];
            let out_path = dest.join(info.name.as_str());
            if info.is_dir {
                if dir_exists_conflict(&out_path, ctx.conflicts) {
                    continue; // Skip：目录条目跳过（已存在）
                }
                fs::create_dir_all(&out_path).map_err(HzipError::from)?;
                report.entries_done += 1;
                continue;
            }
            let pw_owned = ctx.passwords.password(Some(&info.name));
            let pw = match pw_owned.as_ref() {
                Some(z) => PasswordRef::new(z),
                None => PasswordRef::none(),
            };
            match self.extract_one(id, pw, ctx, &out_path) {
                Ok(bytes) => {
                    report.entries_done += 1;
                    report.bytes_written += bytes;
                }
                Err(err) if ctx.best_effort => {
                    let _ = fs::remove_file(&out_path);
                    report.failures.push((id, Box::new(err)));
                }
                Err(err) => return Err(err),
            }
        }
        Ok(report)
    }

    fn test(&self, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        let mut report = ExtractReport::default();
        for (id, info) in self.infos.iter().enumerate() {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            if info.is_dir {
                report.entries_done += 1;
                continue;
            }
            let pw_owned = ctx.passwords.password(Some(&info.name));
            let pw = match pw_owned.as_ref() {
                Some(z) => PasswordRef::new(z),
                None => PasswordRef::none(),
            };
            // 物化（unrar 校验 CRC；成功即字节正确）→ 读丢
            match self.materialize_entry(id, pw) {
                Ok(mut mat) => {
                    let n = io::copy(&mut mat, &mut io::sink()).map_err(HzipError::from)?;
                    report.entries_done += 1;
                    report.bytes_written += n;
                }
                Err(err) if ctx.best_effort => {
                    report.failures.push((id, Box::new(err)));
                }
                Err(err) => return Err(err),
            }
        }
        Ok(report)
    }
}

impl RarReader {
    /// extract 单条目：物化 → 流拷到最终目标（净化/冲突决策归 core；本层按
    /// ctx 基本语义落盘）。
    fn extract_one(
        &self,
        id: usize,
        pw: PasswordRef<'_>,
        ctx: &ExtractCtx<'_>,
        out_path: &Path,
    ) -> Result<u64, HzipError> {
        if out_path.exists() {
            match ctx.conflicts {
                ConflictPolicy::Skip => return Ok(0),
                ConflictPolicy::RenameAuto | ConflictPolicy::Ask => {
                    // 决策归 core（本层无改名/请求-应答通道）——core 驱动下这些
                    // 策略应已在上层决策；此处按 Overwrite 兜底并留 debug 诊断。
                    tracing::debug!(
                        conflict = ?ctx.conflicts,
                        "冲突决策归 core；本层按 Overwrite 落盘"
                    );
                }
                ConflictPolicy::Overwrite | ConflictPolicy::NewerOnly => {}
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }
        if let Some(parent) = out_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(HzipError::from)?;
        }
        let mut rdr = self.materialize_entry(id, pw)?;
        let mut out = fs::File::create(out_path).map_err(HzipError::from)?;
        let mut buf = vec![0u8; COPY_CHUNK];
        let mut total = 0u64;
        let copy_result: io::Result<u64> = loop {
            if ctx.cancel.is_cancelled() {
                break Err(io::Error::other("cancelled"));
            }
            let n = match rdr.read(&mut buf) {
                Ok(0) => break Ok(total),
                Ok(n) => n,
                Err(e) => break Err(e),
            };
            out.write_all(&buf[..n]).map_err(HzipError::from)?;
            total += n as u64;
            ctx.progress
                .send(hzip_format::ProgressEvent::Bytes { delta: n as u64 });
        };
        match copy_result {
            Ok(total) => {
                out.flush().map_err(HzipError::from)?;
                Ok(total)
            }
            Err(e) => {
                let _ = fs::remove_file(out_path);
                if ctx.cancel.is_cancelled() {
                    Err(HzipError::Cancelled)
                } else {
                    Err(HzipError::from(e))
                }
            }
        }
    }
}

// ===================== 辅助 =====================

/// 目录目标已存在时的冲突处理（Skip → true=跳过）。
fn dir_exists_conflict(path: &Path, policy: ConflictPolicy) -> bool {
    path.exists() && matches!(policy, ConflictPolicy::Skip)
}

/// Selection → 目标 id 列表。
fn resolve_selection(infos: &[EntryInfo], sel: &Selection) -> Result<Vec<usize>, HzipError> {
    let fmt = crate::format_id();
    match sel {
        Selection::All => Ok((0..infos.len()).collect()),
        Selection::Ids(ids) => {
            if ids.iter().any(|&i| i >= infos.len()) {
                return Err(HzipError::Format {
                    format: fmt,
                    kind: FormatErrorKind::BadHeader,
                });
            }
            Ok(ids.clone())
        }
        Selection::Names(names) => Ok(infos
            .iter()
            .enumerate()
            .filter(|(_, e)| names.contains(&e.name))
            .map(|(i, _)| i)
            .collect()),
        _ => Err(HzipError::Unsupported {
            what: "未知 Selection 变体".into(),
        }),
    }
}

/// HzipError::Format 的 format 字段补全（map_unrar_err 用常量 "rar"，调用方补
/// 真实 FormatId 以保持展示一致）。
trait WithFormat {
    fn with_format(self, format: FormatId) -> HzipError;
}

impl WithFormat for HzipError {
    fn with_format(self, format: FormatId) -> HzipError {
        match self {
            HzipError::Format { kind, .. } => HzipError::Format { format, kind },
            other => other,
        }
    }
}

#[cfg(test)]
#[path = "tests/reader.rs"]
mod tests;
