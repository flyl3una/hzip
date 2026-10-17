//! zip 读取：基于 zip crate 的 `ArchiveReader`（02 §8.1 解压流程、T24 §4.5）。
//!
//! # 设计（契约红线 1/2/3/4，02 §7.3）
//!
//! - **`open`**：`ZipArchive::new` 读入中央目录（不触数据区，红线 1）。元数据快照
//!   经 `by_index_raw`（不触数据体）；快照缓于 [`ZipReader::infos`]，
//!   `entries()` 零 IO。`Source::Stdin`/`Stream`（不可 seek）先落临时文件缓冲
//!   代理（02 §7.4「stdin 需缓冲代理」注）；`Source::Volumes` 经
//!   [`crate::split::VolumeSetReader`] 装配后同样落临时文件物化。
//! - **`read_entry`（红线 2：乱序/重复/独立）**：两条路径——
//!   1. **直读**（绝大多数条目：非加密 + Stored/Deflated，2026-09-24 新增）：
//!      索引期已由 `ZipFile::data_start()` 备好数据区偏移（zip64/数据描述符由
//!      zip crate 解好）⇒ 另开句柄 seek 过去、按压缩长度围栏、按方法解码，
//!      CRC32 由 [`Crc32Checked`] 尾验（不符 → `Checksum 3001`）。天然 `Send`、
//!      天然支持乱序/重复。**为什么必须**：旧的纯 spool 实现在 5 GiB 单条目上
//!      实测让 `read_entry` 独占 2.7 s（进度停 0%，用户报「等很久才开始解压」），
//!      且要求临时盘放下整条内容（20 GB 条目 ⇒ 20 GB 临时空间）；
//!   2. **spool 两遍法**（其余：**加密条目**（zipcrypto 预验头 / AES 解密链）与
//!      Stored/Deflated 以外的编码（bzip2/zstd/deflate64））：zip crate 的
//!      `ZipFile` 借用 `&mut ZipArchive`、无法跨锁持有为 `Send` 流 → 首访条目时锁
//!      → `by_index_decrypt`/`by_index` → 流式解码 → 旁路 `Crc32` 尾验 → 落临时
//!      文件解锁；此后该条目所有读取重开 spool 顺序读。spool 目录随
//!      [`ZipReader`] drop 清理。
//! - **ZipCrypto（红线 3）**：`by_index_decrypt` 做 2 字节校验头预验（zip crate
//!   `ZipCryptoValidator`）；加密条目未带密码 → 立即 `Password 3101`；1/256 假
//!   阳性由读末 CRC 兜底 → `Checksum 3001`（禁乱码流）。
//! - **deflate64 读兼容**：方法 9 由 zip crate `deflate64` feature 自动解码；
//!   写永不 deflate64（02 §13.2）。
//! - **WinZip AES 读（T45，feature `aes`）**：method 99 条目的 0x9901 extra field
//!   解析与错误映射经 [`crate::aes`]；加解密与 HMAC 双验证由 zip crate
//!   `aes-crypto` 链路承载（红线 3 语义见 aes 模块文档）。
//! - **`extract` / `test`**：**直接流式解码**（不经 spool，TC-CONF-006 取消须在
//!   写盘过程中响应）；逐块取消检查点（每 `COPY_CHUNK_BYTES`）+ `Bytes` 进度；
//!   CRC 由旁路尾验承载（不符 → Checksum 3001，逐条目 fail-fast/best_effort）。
//! - **红线 4**：zip crate 解畸形数据可能 panic → [`crate::panic_boundary`] 包裹
//!   解码循环，panic 转 `Format{BadHeader}`。
//!
//! # 错误映射
//!
//! 统一经 [`crate::map_zip_err`]（T24 §4.3 表）。`open` 找不到 EOCD/截断时，若
//! 头为 `PK\x03\x04`（无目录产物）→ 交由 [`crate::salvage`] 恢复；salvage 失败
//! 才返回 `Format{Truncated}`（T24 §4.5）。salvage 仅对 `Source::File` 生效
//! （stdin/stream/volumes 一次性流无「失败产物文件」概念，报 Truncated）。

use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hzip_codec::crc::Crc32;
use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::consts::COPY_CHUNK_BYTES;
use hzip_format::{
    ArchiveReader, EntryAttrs, EntryInfo, ExtractCtx, ExtractReport, FormatCaps, FormatId,
    NameEncoding, PasswordRef, Selection, Source, parse_entry_name,
};

use crate::handler;
use crate::panic_boundary;
use crate::split::VolumeSetReader;

/// `Read + Seek` 对象安全联合（zip 底层）。
pub(crate) trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// zip crate 读取器底层：类型擦除 `Read + Seek + Send`。
type ZipSrc = Box<dyn ReadSeek + Send>;

/// 条目读取元数据（中央目录快照；`read_entry` 用）。
#[derive(Debug, Clone)]
pub(crate) struct ReadMeta {
    pub(crate) name: hzip_format::EntryName,
    pub(crate) is_dir: bool,
    pub(crate) is_symlink: bool,
    pub(crate) size_raw: u64,
    pub(crate) crc32: u32,
    pub(crate) encrypted: bool,
    /// 压缩方法（[`ZipReader::direct_reader`] 决定能否直读）。
    pub(crate) method: zip::CompressionMethod,
    /// 数据区起点（本地头之后；zip64 由 zip crate 解出）——直读 seek 目标。
    pub(crate) data_offset: u64,
    /// 压缩长度（数据区围栏；加密条目含加密开销）。
    pub(crate) size_packed: u64,
}

/// zip 读取器（`ArchiveReader`；`Send`：内部 `Arc<Mutex<ZipArchive>>`）。
pub(crate) struct ZipReader {
    format: FormatId,
    archive: Arc<Mutex<zip::ZipArchive<ZipSrc>>>,
    infos: Vec<EntryInfo>,
    metas: Vec<ReadMeta>,
    /// 条目 id → 已解码 spool（路径, 已校验长度）；`read_entry` 复用。
    spooled: Mutex<Vec<Option<(PathBuf, u64)>>>,
    /// spool 临时目录（drop 清理）。
    _spool_dir: tempfile::TempDir,
    /// 源档案路径（**直读**时另开句柄 seek 用；Stdin/Stream/Volumes 源指向
    /// `materialize` 落下的临时副本）。
    src_path: PathBuf,
    /// 非 File 源临时文件保活（stdin/stream/volumes 物化句柄 drop 前文件须在）。
    _src_keep: Option<tempfile::NamedTempFile>,
}

/// 物化后的源（File 句柄 + 保活临时文件）。
struct Materialized {
    path: PathBuf,
    file: std::fs::File,
    /// 非 File 源的 NamedTempFile 保活（供 reader 持有）。
    keep: Option<tempfile::NamedTempFile>,
}

/// 物化源：File 原样；Stdin/Stream/Volumes 落临时文件。
fn materialize(src: Source) -> Result<Materialized, HzipError> {
    match src {
        Source::File(p) => {
            let file = std::fs::File::open(&p).map_err(crate::map_io)?;
            Ok(Materialized {
                path: p,
                file,
                keep: None,
            })
        }
        Source::Stdin => {
            let mut ntf = tempfile::NamedTempFile::new().map_err(crate::map_io)?;
            io::copy(&mut io::stdin().lock(), ntf.as_file_mut()).map_err(crate::map_io)?;
            ntf.as_file_mut().flush().map_err(crate::map_io)?;
            let file = ntf.reopen().map_err(crate::map_io)?;
            let path = ntf.path().to_path_buf();
            Ok(Materialized {
                path,
                file,
                keep: Some(ntf),
            })
        }
        Source::Stream(s) => {
            let mut ntf = tempfile::NamedTempFile::new().map_err(crate::map_io)?;
            io::copy(&mut io::BufReader::new(s), ntf.as_file_mut()).map_err(crate::map_io)?;
            ntf.as_file_mut().flush().map_err(crate::map_io)?;
            let file = ntf.reopen().map_err(crate::map_io)?;
            let path = ntf.path().to_path_buf();
            Ok(Materialized {
                path,
                file,
                keep: Some(ntf),
            })
        }
        Source::Volumes { first, rest } => {
            let mut paths = vec![first];
            paths.extend(rest);
            let r = VolumeSetReader::open(paths).map_err(crate::map_io)?;
            let mut ntf = tempfile::NamedTempFile::new().map_err(crate::map_io)?;
            io::copy(&mut io::BufReader::new(r), ntf.as_file_mut()).map_err(crate::map_io)?;
            ntf.as_file_mut().flush().map_err(crate::map_io)?;
            let file = ntf.reopen().map_err(crate::map_io)?;
            let path = ntf.path().to_path_buf();
            Ok(Materialized {
                path,
                file,
                keep: Some(ntf),
            })
        }
    }
}

impl ZipReader {
    /// 打开（`Source` 分解见模块文档）。
    pub(crate) fn open(src: Source) -> Result<Box<dyn ArchiveReader>, HzipError> {
        let format = FormatId::from(handler::FORMAT_ID);
        let is_file_src = matches!(src, Source::File(_));
        let mut mat = materialize(src)?;
        let path = mat.path.clone();
        let keep = mat.keep.take();
        // 预读头 4B（salvage 判定；随后 seek 回 0）
        let mut head = [0u8; 4];
        let head_n = read_head(&mut mat.file, &mut head);
        let mut boxed: ZipSrc = Box::new(mat.file);
        boxed.seek(io::SeekFrom::Start(0)).map_err(crate::map_io)?;
        match zip::ZipArchive::new(boxed) {
            Ok(archive) => Self::from_archive(archive, keep, path),
            Err(e) => {
                let is_pk = head_n >= 4 && &head[..4] == b"PK\x03\x04";
                if is_file_src && is_pk {
                    // EOCD/中央目录缺失：头为本地文件头 → salvage 恢复读取
                    tracing::debug!(err = ?e, "zip 中央目录缺失，尝试 salvage");
                    return crate::salvage::SalvageReader::open(path);
                }
                Err(crate::map_zip_err(e, &format))
            }
        }
    }

    fn from_archive(
        mut archive: zip::ZipArchive<ZipSrc>,
        src_keep: Option<tempfile::NamedTempFile>,
        src_path: PathBuf,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        let format = FormatId::from(handler::FORMAT_ID);
        let n = archive.len();
        // 单遍法（02 §8.1 批量检测）：条目元数据与原始名样本在**同一趟**
        // `by_index_raw` 里取齐——该调用每次都 seek 解析一次本地头（zip crate 的
        // `by_index_raw` 内部即做本地头解析），旧两遍法把这份 IO 翻了一倍。样本到齐后批量检测一次，
        // 再对「UTF-8 flag 未置位」的条目回填名字与编码判定（E/F 分支）。
        let mut infos = Vec::with_capacity(n);
        let mut metas = Vec::with_capacity(n);
        // 非 UTF-8 条目的原始名样本（下标 = 条目 id；UTF-8 条目为 None）。
        let mut raws: Vec<Option<Vec<u8>>> = Vec::with_capacity(n);
        for i in 0..n {
            let f = panic_boundary::guard_ok(|| archive.by_index_raw(i))?
                .map_err(|e| crate::map_zip_err(e, &format))?;
            let raw = f.name_raw().to_vec();
            let is_utf8 = std::str::from_utf8(&raw).is_ok();
            // 名字解码：zip 9 起返回 `ZipResult`（声明编码与字节不符即失败）——
            // 失败视同「非 UTF-8 原始名」，一并交下方批量检测（E/F 分支）回填；
            // 此时占位为空名，必被覆盖、不外泄。
            let decoded = f.name().ok();
            let detect = !is_utf8 || decoded.is_none();
            raws.push(detect.then(|| raw.clone()));
            // UTF-8（flag 置位）直用 zip crate 解码名。
            let name = match decoded {
                Some(n) => name_of(&n),
                None => name_of(&String::from_utf8_lossy(&raw)),
            };
            let name_encoding = NameEncoding::Utf8;
            let m = ReadMeta {
                name: name.clone(),
                is_dir: f.is_dir(),
                is_symlink: f.is_symlink(),
                size_raw: if f.is_dir() { 0 } else { f.size() },
                crc32: f.crc32(),
                encrypted: f.encrypted(),
                method: f.compression(),
                // zip 9 起 `data_start()` 返回 `Option`（本地头已解析即 `Some`；
                // 旧版未设时同样回落 0——此处紧跟 `by_index_raw`，恒为 `Some`）。
                data_offset: f.data_start().unwrap_or(0),
                size_packed: f.compressed_size(),
            };
            let codec = codec_of(&f);
            // 加密算法（中央目录声明，零数据区 IO）：aes_mode 有值 = WinZip AES
            //（0x9901，细分密钥长度）；否则加密位 = 传统 ZipCrypto。注意 zip crate
            // 已把 method 99 替换成 real_method——`compression()` 的 Aes 变体
            // 不可作判据。
            let metadata = f.get_metadata();
            let encryption: Option<&'static str> = if !m.encrypted {
                None
            } else {
                // zip 9：AES 参数改由 AeX 扩展字段读出（`aes_settings()`，元组 =
                // (算法, 厂商版本)）；旧版是 `ZipFileData::aes_mode` 三元的字段。
                match metadata.aes_settings() {
                    Some((zip::AesMode::Aes128, _)) => Some("AES-128"),
                    Some((zip::AesMode::Aes192, _)) => Some("AES-192"),
                    Some((zip::AesMode::Aes256, _)) => Some("AES-256"),
                    // aes_mode 缺失但加密位已置（畸形头）→ 按传统算法保守标注
                    None => Some("ZipCrypto"),
                }
            };
            // DOS 属性位（system = Dos 时 external_attributes 低字节）：
            // 0x01 只读 / 0x02 隐藏 / 0x04 系统。Unix 条目走 mode 位语义（隐藏/
            // 系统为 Windows 专有，不推断）。
            use zip::read::HasZipMetadata;
            // `zip::System` 枚举未公开导出，DOS 语义改用等价判据：Unix 条目把
            // mode 放 external_attributes 高 16 位（zip crate `unix_mode()` 的
            // System::Unix 分支即取 `>> 16`），高 16 位为 0 ⇒ DOS 属性布局。
            let dos_attrs = if f.get_metadata().external_attributes >> 16 == 0 {
                f.get_metadata().external_attributes
            } else {
                0
            };
            let info = EntryInfo {
                id: i,
                name,
                is_dir: m.is_dir,
                is_symlink: m.is_symlink,
                symlink_target: if m.is_symlink {
                    Some(PathBuf::new())
                } else {
                    None
                },
                size_raw: m.size_raw,
                size_packed: f.compressed_size(),
                mtime: f.last_modified().and_then(crate::dos_time::from_dos),
                mode: f.unix_mode(),
                attributes: EntryAttrs {
                    executable: f.unix_mode().map(|m| m & 0o111 != 0),
                    read_only: dos_attrs & 0x01 != 0,
                    hidden: dos_attrs & 0x02 != 0,
                    system: dos_attrs & 0x04 != 0,
                },
                encrypted: m.encrypted,
                header_encrypted: false,
                // 目录条目无数据流 → CRC 恒 None（zip 目录头 CRC 域为 0 占位）
                crc32: if m.is_dir { None } else { Some(m.crc32) },
                codec,
                name_encoding,
                encryption,
                // zip 工具普遍不在头里写 deflate 级别（zip crate 读路径恒
                // None）——如实归 None，列显示「未知」而非猜测
                compression_level: None,
            };
            infos.push(info);
            metas.push(m);
        }
        // 批量检测（仅非 UTF-8 样本；低置信保留原始 + Suspect，02 §8.1 E 分支）
        let samples: Vec<&[u8]> = raws.iter().flatten().map(|v| v.as_slice()).collect();
        let batch = crate::encoding::detect_batch(&samples);
        // 回填未置位条目：原始字节（`raws[i]` = 条目 i）、`batch`、`classify` 与旧
        // 两遍法逐项相同 ⇒ 名字/编码判定逐位一致（差别只是少跑一趟本地头 IO）。
        for ((info, meta), raw) in infos.iter_mut().zip(metas.iter_mut()).zip(raws.iter()) {
            let Some(raw) = raw else {
                continue;
            };
            let (name_str, name_encoding) = crate::encoding::classify(raw, None, batch);
            let name = name_of(&name_str);
            meta.name = name.clone();
            info.name = name;
            info.name_encoding = name_encoding;
        }
        let _spool_dir = tempfile::tempdir().map_err(crate::map_io)?;
        Ok(Box::new(ZipReader {
            format,
            archive: Arc::new(Mutex::new(archive)),
            infos,
            metas,
            spooled: Mutex::new((0..n).map(|_| None).collect()),
            _spool_dir,
            src_path,
            _src_keep: src_keep,
        }))
    }

    fn meta(&self, id: usize) -> Result<&ReadMeta, HzipError> {
        self.metas.get(id).ok_or_else(|| HzipError::Format {
            format: self.format.clone(),
            kind: FormatErrorKind::BadHeader,
        })
    }

    /// **直读快路径**：另开档案句柄 → seek 到该条目数据区 → 按压缩长度围栏 →
    /// 按方法解码 → [`Crc32Checked`] 尾验。返回 `None` = 本条目不走直读（调用方
    /// 回落 spool 两遍法）。
    ///
    /// # 为什么需要（2026-09-24 用户实测）
    ///
    /// 原实现在 `read_entry` 里一律 spool：把整条**解码后**的内容先写进临时文件，
    /// 再把 spool 文件交给调用方。代价有两项，都在大条目上放大：
    ///
    /// 1. **解压前静默等待**：解压一个 5 GiB 单条目 zip，实测 `read_entry` 独占
    ///    **2.7 s**（进度停在 0%，用户报「等很久才开始实际的解压」；20 GB 的
    ///    `.qcow2` 条目按比例更久）；
    /// 2. **临时盘需求 = 条目全量**：20 GB 条目 ⇒ 20 GB 临时空间（与「磁盘满」
    ///    类故障同源，见 `hzip-core::services::space` 模块文档）。
    ///
    /// 直读把两项一起消掉：数据区偏移在索引期已由 `ZipFile::data_start()` 取到
    /// （zip64/数据描述符都由 zip crate 解好），解码器直接从文件流式产出。
    ///
    /// 仍走 spool 的两类：**加密条目**（zipcrypto 预验头 / AES 需 zip crate 的
    /// 解密链）与 Stored/Deflated 以外的编码（bzip2/zstd/deflate64——zipx 少见）。
    fn direct_reader(&self, meta: &ReadMeta) -> Result<Option<Box<dyn Read + Send>>, HzipError> {
        let open_at = || -> Result<std::fs::File, HzipError> {
            let mut f = std::fs::File::open(&self.src_path).map_err(crate::map_io)?;
            f.seek(io::SeekFrom::Start(meta.data_offset))
                .map_err(crate::map_io)?;
            Ok(f)
        };
        let inner: Box<dyn Read + Send> = match meta.method {
            zip::CompressionMethod::Stored => Box::new(open_at()?.take(meta.size_packed)),
            zip::CompressionMethod::Deflated => Box::new(hzip_codec::deflate::DeflateDecoder::new(
                open_at()?.take(meta.size_packed),
            )),
            _ => return Ok(None),
        };
        Ok(Some(Box::new(Crc32Checked::new(inner, meta))))
    }

    /// 解码条目为 spool 文件（首访建、此后复用）。返回 (路径, 解码长度)。
    fn ensure_spool(&self, id: usize) -> Result<(PathBuf, u64), HzipError> {
        {
            let g = self.spooled.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(Some(s)) = g.get(id) {
                return Ok(s.clone());
            }
        }
        let meta = self.meta(id)?.clone();
        let dir = self._spool_dir.path().to_path_buf();
        let sp_path = dir.join(format!("e{id}.bin"));

        let result: Result<u64, HzipError> = panic_boundary::guard(|| {
            let mut g = self.archive.lock().unwrap_or_else(|p| p.into_inner());
            let mut file = if meta.encrypted {
                return Err(HzipError::Password {
                    target: hzip_error::PasswordTarget::Entry,
                });
            } else {
                g.by_index(id)
                    .map_err(|e| crate::map_zip_err(e, &self.format))?
            };
            decode_spool(&mut file, &sp_path, &meta, &self.format)
        });
        match result {
            Ok(len) => {
                let mut g = self.spooled.lock().unwrap_or_else(|p| p.into_inner());
                g[id] = Some((sp_path.clone(), len));
                Ok((sp_path, len))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&sp_path);
                Err(e)
            }
        }
    }

    /// 有密码的解密 spool（`read_entry` 加密条目路径；zipcrypto 预验失败 → 3101）。
    fn ensure_spool_with_pw(&self, id: usize, pw: &str) -> Result<(PathBuf, u64), HzipError> {
        {
            let g = self.spooled.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(Some(s)) = g.get(id) {
                return Ok(s.clone());
            }
        }
        let meta = self.meta(id)?.clone();
        let dir = self._spool_dir.path().to_path_buf();
        let sp_path = dir.join(format!("e{id}.bin"));
        let pw_bytes = pw.as_bytes().to_vec();

        let result: Result<u64, HzipError> = panic_boundary::guard(|| {
            let mut g = self.archive.lock().unwrap_or_else(|p| p.into_inner());
            let mut file = g
                .by_index_decrypt(id, &pw_bytes)
                .map_err(|e| crate::map_zip_err(e, &self.format))?;
            decode_spool(&mut file, &sp_path, &meta, &self.format)
        });
        match result {
            Ok(len) => {
                let mut g = self.spooled.lock().unwrap_or_else(|p| p.into_inner());
                g[id] = Some((sp_path.clone(), len));
                Ok((sp_path, len))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&sp_path);
                Err(e)
            }
        }
    }

    /// 单条目流式解码到 `out`（extract/test 用；旁路 CRC 尾验；不落 spool）。
    fn stream_to(
        &self,
        id: usize,
        pw: Option<&str>,
        cancel: &hzip_progress::CancellationToken,
        progress: &hzip_format::ProgressBus,
        mut out: impl Write,
    ) -> Result<u64, HzipError> {
        // 只借用快照（`ReadMeta` 内含 `EntryName`，克隆整份 = 每条目一次多余拷贝）；
        // 需要 owned 名的地方（Checksum 错误）就地 `clone()`。
        let meta = self.meta(id)?;
        if meta.is_dir {
            return Ok(0);
        }
        let result: Result<u64, HzipError> = panic_boundary::guard(|| {
            let mut g = self.archive.lock().unwrap_or_else(|p| p.into_inner());
            let mut crc = Crc32::new();
            let mut file = match (meta.encrypted, pw) {
                (true, Some(p)) => g
                    .by_index_decrypt(id, p.as_bytes())
                    .map_err(|e| crate::map_zip_err(e, &self.format))?,
                (true, None) => {
                    return Err(HzipError::Password {
                        target: hzip_error::PasswordTarget::Entry,
                    });
                }
                (false, _) => g
                    .by_index(id)
                    .map_err(|e| crate::map_zip_err(e, &self.format))?,
            };
            let mut buf = vec![0u8; COPY_CHUNK_BYTES as usize];
            loop {
                if cancel.is_cancelled() {
                    return Err(HzipError::Cancelled);
                }
                let n = file.read(&mut buf).map_err(|e| {
                    map_read_err(e, &self.format, &meta.name, meta.crc32, crc.finalize())
                })?;
                if n == 0 {
                    break;
                }
                crc.update(&buf[..n]);
                out.write_all(&buf[..n]).map_err(crate::map_io)?;
                progress.send(hzip_format::ProgressEvent::Bytes { delta: n as u64 });
            }
            // `crc32 == 0` → 视为未提供（02 §8.3 红线2 唯一例外；WinZip AES/TPP
            // 按规范把中央目录 CRC 置 0，完整性由 AES-HMAC 承担）
            if meta.crc32 != 0 && crc.finalize() != meta.crc32 {
                return Err(HzipError::Checksum {
                    entry: meta.name.clone(),
                    expected: meta.crc32,
                    actual: crc.finalize(),
                });
            }
            out.flush().map_err(crate::map_io)?;
            Ok(crc.bytes())
        });
        result
    }
}

/// 读源前 4B（salvage 判定用；返回读到字节数）。
fn read_head(f: &mut std::fs::File, out: &mut [u8; 4]) -> usize {
    let _ = f.seek(io::SeekFrom::Start(0));
    f.read(out).unwrap_or_default()
}

/// 名称字符串（zip crate 已按 UTF-8/cp437 解码）→ EntryName（`\`→`/` 预转换）。
pub(crate) fn name_of(raw: &str) -> hzip_format::EntryName {
    let normalized = raw.replace('\\', "/");
    parse_entry_name(&normalized).unwrap_or_else(|_| hzip_format::EntryName::new("__invalid__"))
}

/// 压缩方法 → codec id（02 §6.3 zip 列；deflate64 读兼容）。zip crate 的
/// 方法变体随启用 feature 编译（Lzma/Xz/Ppmd 等 zipx 方法未启用 feature 时
/// 落入 `Unsupported(id)`）。
pub(crate) fn codec_of(f: &zip::read::ZipFile<'_, ZipSrc>) -> Option<hzip_codec::traits::CodecId> {
    match f.compression() {
        zip::CompressionMethod::Stored => Some("store".into()),
        zip::CompressionMethod::Deflated => Some("deflate".into()),
        zip::CompressionMethod::Deflate64 => Some("deflate64".into()),
        zip::CompressionMethod::Bzip2 => Some("bzip2".into()),
        zip::CompressionMethod::Zstd => Some("zstd".into()),
        zip::CompressionMethod::Aes => {
            // T45 AES 包封（feature aes）：real_method 经 0x9901 extra field 解析
            // （zip crate 已将 data.compression 换成 real_method，codec 由 aes 模块
            // 语义接管——此处仅做 extra field 结构健康检查，坏 → 3103 于读取时经
            // zip crate 报 UnsupportedArchive/InvalidArchive 映射）
            #[cfg(feature = "aes")]
            {
                let _ = f
                    .extra_data()
                    .and_then(|ef| crate::aes::parse_extra_field(&ef).ok());
            }
            #[cfg(not(feature = "aes"))]
            {
                let _ = &f;
            }
            None
        }
        #[allow(deprecated)]
        zip::CompressionMethod::Unsupported(_) => None,
        // zip crate 方法枚举非穷尽（feature 门控变体）：未启用 feature 的方法归 None
        _ => None,
    }
}

/// 把 `file`（zip 解码流）流式解码写 spool，旁路 CRC 尾验。
fn decode_spool(
    file: &mut zip::read::ZipFile<'_, ZipSrc>,
    sp_path: &Path,
    meta: &ReadMeta,
    format: &FormatId,
) -> Result<u64, HzipError> {
    let mut out = std::fs::File::create(sp_path).map_err(crate::map_io)?;
    let mut crc = Crc32::new();
    let mut buf = vec![0u8; COPY_CHUNK_BYTES as usize];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| map_read_err(e, format, &meta.name, meta.crc32, crc.finalize()))?;
        if n == 0 {
            break;
        }
        crc.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(crate::map_io)?;
    }
    out.flush().map_err(crate::map_io)?;
    // `crc32 == 0` → 视为未提供（02 §8.3；同 decode_copy：AE-2 存 0 由 HMAC 兜）
    if meta.crc32 != 0 && crc.finalize() != meta.crc32 {
        return Err(HzipError::Checksum {
            entry: meta.name.clone(),
            expected: meta.crc32,
            actual: crc.finalize(),
        });
    }
    Ok(crc.bytes())
}

/// 直读路径的 CRC32 尾验读取器（spool 路径 `decode_spool` 的流式等价物）。
///
/// 语义与 spool 路径逐条对齐：读到 EOF 时比对（`crc32 == 0` 视为未提供——
/// 02 §8.3 的 AE-2 例外，完整性由 AES-HMAC 兜），不符 → `HzipError::Checksum`
/// （3001）。**差异**：错误发生在**流交付之后**（直读是流式的），故以
/// `io::Error` 承载 `HzipError`，由 core 的拷贝循环取回原错误（
/// `hzip-core::services::extract` 的 `unattach_io`）——错误码与 best_effort
/// 语义因此与 spool 路径完全一致。
struct Crc32Checked<R> {
    inner: R,
    crc: Crc32,
    expected: u32,
    name: hzip_format::EntryName,
    /// 已做过尾验（EOF 只判一次；后续 read 直接返回 0）。
    done: bool,
}

impl<R: Read> Crc32Checked<R> {
    fn new(inner: R, meta: &ReadMeta) -> Self {
        Self {
            inner,
            crc: Crc32::new(),
            expected: meta.crc32,
            name: meta.name.clone(),
            done: false,
        }
    }
}

impl<R: Read> Read for Crc32Checked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.crc.update(&buf[..n]);
            return Ok(n);
        }
        if !self.done {
            self.done = true;
            let actual = self.crc.finalize();
            if self.expected != 0 && actual != self.expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    HzipError::Checksum {
                        entry: self.name.clone(),
                        expected: self.expected,
                        actual,
                    },
                ));
            }
        }
        Ok(0)
    }
}

/// 读 io 错误 → HzipError（CRC InvalidData → Checksum；截断 → Truncated）。
fn map_read_err(
    e: io::Error,
    format: &FormatId,
    entry: &hzip_format::EntryName,
    expected: u32,
    actual: u32,
) -> HzipError {
    match e.kind() {
        io::ErrorKind::InvalidData => {
            let m = e.to_string().to_ascii_lowercase();
            if m.contains("checksum") || m.contains("crc") {
                HzipError::Checksum {
                    entry: entry.clone(),
                    expected,
                    actual,
                }
            } else {
                HzipError::Format {
                    format: format.clone(),
                    kind: FormatErrorKind::BadHeader,
                }
            }
        }
        io::ErrorKind::UnexpectedEof => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::Truncated,
        },
        _ => crate::map_io(e),
    }
}

/// 条目密码获取（extract/test 共用；2026-09-15 用户报障回归锚点）：
/// **仅对加密条目**发起密码询问——未加密条目无条件询问会让 core PwRouter
/// 把它升级为弹窗请求，无人应答 120 s → 任务超时取消。未加密 None 直通
/// （`stream_to` 的 `(false, None)` 臂正常解压）。
fn entry_password(info: &EntryInfo, ctx: &ExtractCtx<'_>) -> Option<String> {
    if !info.encrypted {
        return None;
    }
    ctx.passwords
        .password(Some(&info.name))
        .map(|z| z.as_str().to_owned())
}

impl ArchiveReader for ZipReader {
    fn caps(&self) -> FormatCaps {
        handler::caps()
    }

    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError> {
        Ok(self.infos.clone())
    }

    fn read_entry(
        &self,
        id: usize,
        pw: PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError> {
        // 同 `stream_to`：借用快照，只有直读/ spool 分支才需要其标量字段。
        let meta = self.meta(id)?;
        if meta.is_dir {
            return Ok(Box::new(io::empty()));
        }
        // 直读快路径（非加密 + Stored/Deflated）：不落 spool——见 `direct_reader`。
        if !meta.encrypted
            && let Some(r) = self.direct_reader(meta)?
        {
            return Ok(r);
        }
        let (sp, len) = if meta.encrypted {
            match pw.as_str() {
                Some(p) => self.ensure_spool_with_pw(id, p)?,
                None => {
                    return Err(HzipError::Password {
                        target: hzip_error::PasswordTarget::Entry,
                    });
                }
            }
        } else {
            self.ensure_spool(id)?
        };
        let f = std::fs::File::open(&sp).map_err(crate::map_io)?;
        Ok(Box::new(io::BufReader::new(f).take(len)) as Box<dyn Read + Send>)
    }

    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError> {
        let infos = self.entries()?;
        let ids = resolve_ids(&infos, sel)?;
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
            // 普通条目/符号链接（链接内容即目标文本；落盘语义归 core 安全层）
            if let Some(parent) = out_path.parent()
                && !parent.as_os_str().is_empty()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                return Err(crate::map_io(e));
            }
            let pw = entry_password(info, ctx);
            let pw_ref = pw.as_deref();
            let result = std::fs::File::create(&out_path)
                .map_err(crate::map_io)
                .and_then(|f| self.stream_to(id, pw_ref, ctx.cancel, ctx.progress, f));
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

    fn test(&self, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError> {
        let infos = self.entries()?;
        let mut report = ExtractReport::default();
        for info in &infos {
            if ctx.cancel.is_cancelled() {
                return Err(HzipError::Cancelled);
            }
            if info.is_dir || info.is_symlink {
                report.entries_done += 1;
                continue;
            }
            let pw = entry_password(info, ctx);
            let pw_ref = pw.as_deref();
            let n = self.stream_to(info.id, pw_ref, ctx.cancel, ctx.progress, io::sink())?;
            report.entries_done += 1;
            report.bytes_written += n;
        }
        Ok(report)
    }
}

/// 选择解析（All / Ids / Names）。
pub(crate) fn resolve_ids(infos: &[EntryInfo], sel: &Selection) -> Result<Vec<usize>, HzipError> {
    match sel {
        Selection::All => Ok((0..infos.len()).collect()),
        Selection::Ids(ids) => {
            if ids.iter().any(|&i| i >= infos.len()) {
                return Err(HzipError::Format {
                    format: FormatId::from(handler::FORMAT_ID),
                    kind: FormatErrorKind::BadHeader,
                });
            }
            Ok(ids.clone())
        }
        Selection::Names(names) => Ok(infos
            .iter()
            .enumerate()
            .filter(|(_, i)| names.contains(&i.name))
            .map(|(i, _)| i)
            .collect()),
        _ => Err(HzipError::Unsupported {
            what: "未知 Selection 变体".into(),
        }),
    }
}

#[cfg(test)]
#[path = "tests/reader.rs"]
mod tests;
