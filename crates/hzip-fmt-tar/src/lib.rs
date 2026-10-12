//! hzip-fmt-tar：tar 格式的 `FormatHandler`（02 §8.4 三变体兼容）。
//!
//! # 职责（02 §8.4）
//!
//! - **三变体兼容读取**：GNU（长名/稀疏扩展）/ POSIX ustar / pax（扩展头属性
//!   合并）以及无魔数 v7 老档（checksum 合法即读）；
//! - **pax 头覆盖**：`path`/`mtime`/`linkpath` 按 POSIX 覆盖 ustar 头字段；
//! - **GNU.sparse.\*** 稀疏条目按映射稀疏写文件（`hzip-platform::fs_util`
//!   联动，02 §5）；
//! - **caps**：`write=true, append=仅裸 tar, edit=false`（压缩组合一律走 core
//!   转换管线，T22 §3.4）。
//!
//! # caps 声明（T22 §3.4 / 02 §8.4 逐字对照）
//!
//! | 操作 | caps | 行为 |
//! |---|---|---|
//! | read | true | 三变体全兼容 |
//! | write | true | GNU 头基线；长名自动 GNU `L`/pax 扩展（`append_data`，TC-ALG-013） |
//! | append | 仅裸 tar | `Sink::File` → 尾零块截断法（模块 `append`，T22 §4.6）；`Sink::Stream`/`Split`/`Stdout` → `Err Unsupported` HZ-5001 |
//! | edit | false | [`TarHandler::open_editor`] 恒 `Err Unsupported` HZ-5001 |
//!
//! # 错误映射（`variant::map_err` 统一）
//!
//! | io 错误 | HzipError | 码 |
//! |---|---|---|
//! | `UnexpectedEof` / 截断 | `Format{Truncated}` | 2002 |
//! | 坏 checksum / 坏头 / GNU.sparse 半套记录 | `Format{BadHeader}` | 2004 |
//! | 坏魔数（首块非三变体） | `Format{BadSignature}` | 2001 |
//!
//! 出处标注：02 §7.3 契约 trait、02 §8.4 语义、T22 实施要点。模块结构（T22
//! §4.1）：`caps`/`reader`/`variant`/`sparse`/`writer`/`append`/`consts`。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod append;
mod caps;
mod consts;
mod reader;
mod sparse;
mod spool;
mod variant;
mod writer;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveEditor, ArchiveReader, ArchiveWriter, CreateOptions, FormatCaps, FormatHandler,
    FormatId, OpenOptions, Sink, Source,
};

/// tar 格式 ID（02 §7.2；re-export 自模块 `caps`）。
pub use caps::FORMAT_ID;

/// tar 处理器（02 §8.4；`Send + Sync`，无内部状态）。
#[derive(Debug, Default, Clone, Copy)]
pub struct TarHandler;

impl TarHandler {
    /// 便捷构造（与 `Default` 同义；handler 无状态）。
    pub fn new() -> Self {
        Self
    }
}

impl FormatHandler for TarHandler {
    fn id(&self) -> FormatId {
        FormatId::from(FORMAT_ID)
    }

    fn signature(&self) -> &'static hzip_format::Signature {
        &caps::SIGNATURE
    }

    fn caps(&self) -> FormatCaps {
        caps::caps()
    }

    fn open_reader(
        &self,
        src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        let format = FormatId::from(FORMAT_ID);
        match src {
            Source::File(p) => reader::TarReader::from_path(p, format),
            Source::Stdin => {
                let sp = spool::spool_stream(std::io::stdin().lock())?;
                reader::TarReader::from_path_spooled(sp.path().to_path_buf(), sp)
            }
            Source::Stream(s) => {
                let sp = spool::spool_stream(s)?;
                reader::TarReader::from_path_spooled(sp.path().to_path_buf(), sp)
            }
            Source::Volumes { .. } => Err(HzipError::Unsupported {
                what: "tar 无分卷（caps.split=false，02 §8.4）".into(),
            }),
        }
    }

    fn create_writer(
        &self,
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        if opts.encryption.is_some() {
            return Err(HzipError::Unsupported {
                what: "tar 无加密（02 §8.4：encryption=None）".into(),
            });
        }
        if opts.append {
            writer::TarWriter::append(FormatId::from(FORMAT_ID), dst)
        } else {
            writer::TarWriter::create(FormatId::from(FORMAT_ID), dst)
        }
    }

    fn open_editor(
        &self,
        _src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        Err(HzipError::Unsupported {
            what: "tar 无编辑能力（caps.edit=false，02 §8.4；编辑走 core 转换管线）".into(),
        })
    }
}

/// 供 reader 的 `from_path_spooled` 用的格式 ID 便捷。
pub(crate) fn format_id() -> FormatId {
    FormatId::from(FORMAT_ID)
}

#[cfg(test)]
#[path = "tests/lib_rs.rs"]
mod tests;
