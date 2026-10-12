//! `SingleHandler`：单文件流四格式（gz/bz2/xz/zst）的参数化工厂（02 §8.5）。
//!
//! 四种格式共享同一 caps（read/write、无 append/edit/encryption/split/solid/
//! comment），差异仅在 id / 魔数 / 扩展名 / 容器读写包装（feature 门控）。

use hzip_error::HzipError;
use hzip_format::{ArchiveEditor, ArchiveReader, ArchiveWriter};
use hzip_format::{CreateOptions, FormatCaps, FormatHandler, OpenOptions, Signature, Sink, Source};

use crate::reader::SingleReader;
use crate::writer::SingleWriter;

/// 格式能力（02 §8.5 + TC-CONF 映射；T23 §3.4）。
///
/// 四变体共享**同一份**矩阵，唯一差异是写侧压缩方式——
/// 由 [`SingleHandler::caps`] 以 `methods: self.fmt.methods()` 覆写本常量的空值。
pub(crate) const SINGLE_CAPS: FormatCaps = FormatCaps {
    read: true,
    write: true,
    append: false,
    edit: false,
    encryption: hzip_format::EncryptionCaps::None,
    split: false,
    solid: false,
    comment: false,
    methods: &[],
    // 单流编码器（gz/bz2/xz/zst）：格式无加密概念，写入不可加密
    write_encryption: &[],
};

/// 单文件流 handler（id/魔数/扩展名参数化）（02 §8.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SingleHandler {
    pub(crate) fmt: SingleFmt,
}

/// 格式参数（feature 门控的静态描述）。
///
/// `#[cfg(not(any(...)))] Placeholder` 变体仅用于**零格式 feature** 裁剪下让
/// 枚举可穷尽（02 §7-7：`--no-default-features` 下 handler 集为空、crate 仍编译）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SingleFmt {
    #[cfg(feature = "gz")]
    Gz,
    #[cfg(feature = "bz2")]
    Bz2,
    #[cfg(feature = "xz")]
    Xz,
    #[cfg(feature = "zst")]
    Zst,
    #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
    Placeholder,
}

impl SingleFmt {
    /// 格式 id（"gz"/"bz2"/"xz"/"zst"）。
    pub(crate) fn id(&self) -> &'static str {
        match self {
            #[cfg(feature = "gz")]
            SingleFmt::Gz => "gz",
            #[cfg(feature = "bz2")]
            SingleFmt::Bz2 => "bz2",
            #[cfg(feature = "xz")]
            SingleFmt::Xz => "xz",
            #[cfg(feature = "zst")]
            SingleFmt::Zst => "zst",
            #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
            SingleFmt::Placeholder => "none",
        }
    }

    /// 扩展名回退表。
    pub(crate) fn extensions(&self) -> &'static [&'static str] {
        match self {
            #[cfg(feature = "gz")]
            SingleFmt::Gz => &["gz"],
            #[cfg(feature = "bz2")]
            SingleFmt::Bz2 => &["bz2"],
            #[cfg(feature = "xz")]
            SingleFmt::Xz => &["xz"],
            #[cfg(feature = "zst")]
            SingleFmt::Zst => &["zst"],
            #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
            SingleFmt::Placeholder => &[],
        }
    }

    /// 写侧可选压缩方式（**单候选**：容器规格即编码器规格，用户不可选）。
    ///
    /// UI 上表现为锁定值（`Select` 禁用 + 「该格式固定使用 X」说明）。
    pub(crate) fn methods(&self) -> &'static [hzip_codec::traits::CodecMethod] {
        use hzip_codec::traits::CodecMethod;
        match self {
            #[cfg(feature = "gz")]
            SingleFmt::Gz => &[CodecMethod::Deflate],
            #[cfg(feature = "bz2")]
            SingleFmt::Bz2 => &[CodecMethod::BZip2],
            #[cfg(feature = "xz")]
            SingleFmt::Xz => &[CodecMethod::Lzma2],
            #[cfg(feature = "zst")]
            SingleFmt::Zst => &[CodecMethod::Zstd],
            #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
            SingleFmt::Placeholder => &[],
        }
    }
}

/// gzip 单文件流 handler 工厂（02 §8.5；feature 门控）。
#[cfg(feature = "gz")]
pub fn gz() -> SingleHandler {
    SingleHandler { fmt: SingleFmt::Gz }
}

/// bzip2 单文件流 handler 工厂（02 §8.5；feature 门控）。
#[cfg(feature = "bz2")]
pub fn bz2() -> SingleHandler {
    SingleHandler {
        fmt: SingleFmt::Bz2,
    }
}

/// xz 单文件流 handler 工厂（02 §8.5；feature 门控）。
#[cfg(feature = "xz")]
pub fn xz() -> SingleHandler {
    SingleHandler { fmt: SingleFmt::Xz }
}

/// zstd 单文件流 handler 工厂（02 §8.5；feature 门控）。
#[cfg(feature = "zst")]
pub fn zst() -> SingleHandler {
    SingleHandler {
        fmt: SingleFmt::Zst,
    }
}

impl SingleHandler {
    #[cfg(feature = "gz")]
    pub fn gz() -> Self {
        gz()
    }
    #[cfg(feature = "bz2")]
    pub fn bz2() -> Self {
        bz2()
    }
    #[cfg(feature = "xz")]
    pub fn xz() -> Self {
        xz()
    }
    #[cfg(feature = "zst")]
    pub fn zst() -> Self {
        zst()
    }
}

impl FormatHandler for SingleHandler {
    fn id(&self) -> hzip_format::FormatId {
        hzip_format::FormatId::from(self.fmt.id())
    }

    fn signature(&self) -> &'static Signature {
        // 静态签名：魔数/扩展名随格式不同——需独立 static。用匹配返回。
        match self.fmt {
            #[cfg(feature = "gz")]
            SingleFmt::Gz => &Signature {
                magic: &[0x1f, 0x8b],
                offset: 0,
                display: "gzip",
                extensions: &["gz"],
            },
            #[cfg(feature = "bz2")]
            SingleFmt::Bz2 => &Signature {
                magic: b"BZh",
                offset: 0,
                display: "bzip2",
                extensions: &["bz2"],
            },
            #[cfg(feature = "xz")]
            SingleFmt::Xz => &Signature {
                magic: b"\xfd7zXZ\x00",
                offset: 0,
                display: "xz",
                extensions: &["xz"],
            },
            #[cfg(feature = "zst")]
            SingleFmt::Zst => &Signature {
                magic: &[0x28, 0xb5, 0x2f, 0xfd],
                offset: 0,
                display: "zstd",
                extensions: &["zst"],
            },
            #[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
            SingleFmt::Placeholder => &Signature {
                magic: &[],
                offset: 0,
                display: "none",
                extensions: &[],
            },
        }
    }

    fn caps(&self) -> FormatCaps {
        // 四变体共享 SINGLE_CAPS，唯一差异 = 写侧压缩方式（单候选）
        FormatCaps {
            methods: self.fmt.methods(),
            ..SINGLE_CAPS
        }
    }

    fn open_reader(
        &self,
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        SingleReader::open(self.fmt, src, opts).map(|r| Box::new(r) as Box<dyn ArchiveReader>)
    }

    fn create_writer(
        &self,
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        SingleWriter::create(self.fmt, dst, opts).map(|w| Box::new(w) as Box<dyn ArchiveWriter>)
    }

    fn open_editor(
        &self,
        _src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        Err(HzipError::Unsupported {
            what: format!(
                "{} 无编辑能力（caps.edit=false，TC-CONF-009）",
                self.fmt.id()
            )
            .into(),
        })
    }
}

#[cfg(test)]
#[path = "tests/handler.rs"]
mod tests;
