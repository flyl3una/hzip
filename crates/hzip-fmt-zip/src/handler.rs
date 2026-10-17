//! zip handler：caps / signature / 三入口（02 §7.3、§8.1；T24 §4.2）。
//!
//! caps 声明与 T24 §1 逐字对照：
//!
//! | T24 §1 | [`FormatCaps`] 字段 |
//! |---|---|
//! | read / write / append / edit | 全 `true` |
//! | encryption=ZipCrypto（读） | `encryption: EncryptionCaps::ZipCrypto` |
//! | split=true | `split: true` |
//! | solid / comment = false | `solid: false`、`comment: false` |
//!
//! signature（02 §7.4）：魔数 `PK\x03\x04`（本地头）+ `PK\x05\x06`（EOCD 空档），
//! 见 [`SIGNATURE`]。

use hzip_error::HzipError;
use hzip_format::{
    AesKey, ArchiveEditor, ArchiveReader, ArchiveWriter, CreateOptions, EncryptionCaps,
    EncryptionRequest, FormatCaps, FormatHandler, FormatId, OpenOptions, Signature, Sink, Source,
};

use crate::editor::ZipEditor;
use crate::reader::ZipReader;
use crate::writer::ZipWriterImpl;

/// zip 格式 ID（02 §7.2）。
pub const FORMAT_ID: &str = "zip";

/// 签名：魔数 `PK\x03\x04`（本地文件头，02 §7.4 表）。
pub static SIGNATURE: Signature = Signature {
    magic: b"PK\x03\x04",
    offset: 0,
    display: "zip",
    extensions: &["zip"],
};

/// 能力矩阵常量（T24 §1；见模块文档对照表）。
pub(crate) fn caps() -> FormatCaps {
    FormatCaps {
        read: true,
        write: true,
        append: true,
        edit: true,
        encryption: EncryptionCaps::ZipCrypto,
        split: true,
        solid: false,
        comment: false,
        // zip 唯一写侧编码器 = Deflate（store 是「级别」轴语义，见 CodecMethod rustdoc）
        methods: &[hzip_codec::traits::CodecMethod::Deflate],
        // 写侧加密：ZipCrypto（unstable API，锁 zip =9.0.4）+ WinZip AES 三档
        write_encryption: &[
            EncryptionRequest::ZipCrypto,
            EncryptionRequest::Aes(AesKey::Bits128),
            EncryptionRequest::Aes(AesKey::Bits192),
            EncryptionRequest::Aes(AesKey::Bits256),
        ],
    }
}

/// zip 处理器（02 §8.1；`Send + Sync`，无内部状态）。
#[derive(Debug, Default, Clone, Copy)]
pub struct ZipFormatHandler;

impl ZipFormatHandler {
    /// 便捷构造（与 `Default` 同义；handler 无状态）。
    pub fn new() -> Self {
        Self
    }
}

impl FormatHandler for ZipFormatHandler {
    fn id(&self) -> FormatId {
        FormatId::from(FORMAT_ID)
    }

    fn signature(&self) -> &'static Signature {
        &SIGNATURE
    }

    fn caps(&self) -> FormatCaps {
        caps()
    }

    fn open_reader(
        &self,
        src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        ZipReader::open(src)
    }

    fn create_writer(
        &self,
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        if let Some(req) = opts.encryption
            && !caps().write_encryption.contains(&req)
        {
            return Err(HzipError::Unsupported {
                what: format!("zip 不支持该写入加密方案：{req:?}（HZ-5001）").into(),
            });
        }
        ZipWriterImpl::create(dst, opts)
    }

    fn open_editor(
        &self,
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        ZipEditor::open(src, opts)
    }
}

/// zip 格式 ID（内部便捷）。
pub(crate) fn fmt() -> FormatId {
    FormatId::from(FORMAT_ID)
}

#[cfg(test)]
#[path = "tests/handler.rs"]
mod tests;
