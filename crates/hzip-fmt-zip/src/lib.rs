//! hzip-fmt-zip：zip 格式的 `FormatHandler`（02 §8.1 基础版；T24）。
//!
//! # 职责（02 §8.1 / T24 任务卡 §1）
//!
//! - **store / deflate 读写** + **deflate64 读兼容**（写永不 deflate64，02 §13.2）；
//! - **追加**：`ZipWriter::new_append` 直接改中央目录（02 §8.1 writer 职责行），
//!   旧条目本地头与数据字节完全不变（TC-CONF-005）；
//! - **CRC 流式校验链**：读侧 EOF 增量校验 + 写侧 `EntrySink` 双通道 drop 校验
//!   （02 §6.2 旁路语义）；
//! - **「无目录 zip」salvage 读取**（中途失败产物，02 §8.1 写入路径要点末句）；
//! - **分卷字节切分**（输出端 `.zNN… + .zip`，02 §8.1 / §9.5）与读侧
//!   `Source::Volumes` 卷集装配；
//! - **ZipCrypto 传统解密读**（2 字节密码校验头预验，02 §8.1 流程 C/D 分支；
//!   写加密方向归 T45，本任务不含）。
//!
//! # caps 声明（T24 §1 / 02 §7.2 逐字对照）
//!
//! `read=true, write=true, append=true, edit=true, split=true, solid=false,
//! comment=false, encryption=ZipCrypto`（加密读方向 ZipCrypto，02 §8.1）。
//!
//! # 模块结构（02 §8.1 表，排除 aes/encoding/zipx——T45 独占）
//!
//! | 子模块 | 职责 | 出处 |
//! |---|---|---|
//! | [`handler`] | `ZipFormatHandler`：caps / signature / 三入口 | 02 §7.3 |
//! | [`reader`] | 基于 zip crate 的 `ArchiveReader`（含 ZipCrypto 读、deflate64 读） | 02 §8.1 |
//! | [`writer`] | 创建/追加（中央目录直改）、`EntrySink` drop 校验链 | 02 §8.1 / §6.2 |
//! | [`editor`] | `ArchiveEditor::rewrite`（流式重写） | 02 §7.3 / §9.9 |
//! | [`salvage`] | 无中央目录 zip 的本地头顺序扫描恢复 | 02 §8.1 写入路径要点 |
//! | [`split`] | `SplitSink`（分卷字节切分）+ `VolumeSetReader` | 02 §8.1 / §9.5 |
//! | [`panic_boundary`] | 契约红线 4「任何输入不 panic」的 catch_unwind 包裹 | 02 §7.3 |
//!
//! **T45 独占（本 crate 不创建文件）**：`aes`（WinZip AES）、`encoding`
//! （chardetng 编码修复）、`zipx`（方法 ID 12/93/14 解码路由）。基础版文件名
//! 编码判定：UTF-8 flag 置位 → `NameEncoding::Utf8`，否则 `Unknown`
//! （02 §8.1 编码修复流程的 T45 前占位，rustdoc 注明）。
//!
//! # 错误映射（§4.3 表统一入口 [`error::map_zip_err`]）
//!
//! | zip crate 错误 | HzipError | HZ 码 |
//! |---|---|---|
//! | `ZipError::Io` | `Io { source, path }` | 1xxx |
//! | 魔数不匹配（InvalidArchive 含 signature 语义） | `Format{BadSignature}` | 2001 |
//! | 找不到 EOCD / 截断 | 先走 salvage（[`salvage`]），salvage 亦失败 → `Format{Truncated}` | 2002 |
//! | 未知压缩方法 | `Format{UnknownMethod(id)}` | 2003 |
//! | 其余 InvalidArchive | `Format{BadHeader}` | 2004 |
//! | InvalidPassword | `Password{Entry}` | 3101 |
//! | 读末 CRC 不符（io InvalidData） | `Checksum{entry, expected, actual}` | 3001 |
//! | UnsupportedArchive（如 ppmd 方法） | `Unsupported` | 5001 |
//!
//! # 契约红线（02 §7.3，TC-CONF-001..010）
//!
//! 1. `entries()` 不读数据区——`ZipArchive::new` 一次读入中央目录，缓存于
//!    [`reader::ZipReader`]；2. `read_entry` 任意 id 乱序/重复可读（按数据区偏移
//!    重新定位）；3. 错误密码返回 `Password`（ZipCrypto 校验头预验 + CRC 兜底，
//!    禁乱码流）；4. 任何输入不 panic（[`panic_boundary`] + 错误映射兜底）。
//!
//! # 依赖红线
//!
//! `hzip-fmt-zip → hzip-format / hzip-codec / hzip-error`（+hzip-progress/time/
//! tracing/zip，02 §2.1）；与其他 fmt crate 零依赖。zip crate 版本锁定在根
//! `[workspace.dependencies]`（05 §1.2 纪律，本任务未改根 Cargo.toml——zip 已在
//! 根声明，feature 并集满足本卡需求，见 `Cargo.toml` 注释）。
//!
//! # 未实现项（T45 独占，明确标注不静默砍）
//!
//! - WinZip AES 写方向与 AES 逻辑（`src/aes.rs` 属 T45）；
//! - 文件名非 UTF-8 编码检测修复（chardetng，`src/encoding.rs` 属 T45）；
//! - zipx 方法（bzip2/zstd/LZMA）解码路由（`src/zipx.rs` 属 T45）。基础版对
//!   非 deflate/deflate64/store 方法按 `Unsupported`/`UnknownMethod` 明确报错。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![forbid(unsafe_code)]

mod dos_time;
pub mod editor;
pub mod handler;
mod panic_boundary;
pub mod reader;
mod salvage;

/// 条目「made by」系统字节：**恒定** [`zip::System::Unix`]（不要改成按宿主推导）。
///
/// zip 9 起这个字段默认按**构建宿主**取（`cfg!(windows) ⇒ System::Dos`），而 4.x 恒定
/// `System::Unix`。这粒字节牵动两处，都不允许随构建机漂移：
/// 1. **读侧 `unix_mode()`**：`System::Dos` 会让 zip 9 走合成分支（`get_unix_mode`）——
///    丢弃 external_attributes 高位里我们刚写进去的 unix 模式、改报 `0o664`，于是
///    「写进去 → 读回来」的权限位自相矛盾（真机实测：写 0o640 读回 0o664）；
/// 2. **别的工具看这份档案**：Linux `unzip`/`bsdtar` 按 DOS 口径落地（0o664），
///    而不是档案里记着的模式。
///
/// 另：同一份档案在 Windows 与 Linux 构建机上必须逐字节相同（04 §10 可复现构建口径）。
pub(crate) const MADE_BY: zip::System = zip::System::Unix;

mod split;
pub mod writer;

/// T45 AES 子模块（feature `aes` 门控；02 §8.1）。
#[cfg(feature = "aes")]
pub mod aes;
/// T45 编码修复子模块（feature 无关；02 §8.1）。
pub mod encoding;
/// T45 zipx 方法路由子模块（feature `zipx` 门控；02 §8.1）。
#[cfg(feature = "zipx")]
pub mod zipx;

pub use handler::ZipFormatHandler;

/// 内部错误映射模块（`map_zip_err`；见 crate 文档错误映射表）。
mod error;
pub(crate) use error::map_io;
pub(crate) use error::map_zip_err;
