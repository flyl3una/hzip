//! 7z handler：`FormatHandler` 实现、`Signature` 与 caps（02 §8.2 / T25 §4.1 handler 行）。
//!
//! caps 声明（02 §7.2，逐字对照 T25 §1）：
//!
//! | 字段 | 值 | 依据 |
//! |---|---|---|
//! | `read` | true | 头解析 + folder 解码 |
//! | `write` | true | 逐条目 lzma2 写入 |
//! | `append` | false | 7z 无追加语义 → `create_writer(append=true)` `Err Unsupported` HZ-5001 |
//! | `edit` | true | 流式重写（rewrite） |
//! | `encryption` | `None` | AES 归 T46 |
//! | `split` | false | 写侧分卷归 T46 |
//! | `solid` | false | 逐条目 folder（solid 归 T46） |
//! | `comment` | false | 注释未启用 |

use hzip_error::HzipError;
use hzip_format::{
    AesKey, ArchiveEditor, ArchiveReader, ArchiveWriter, CreateOptions, EncryptionCaps,
    EncryptionRequest, FormatCaps, FormatHandler, FormatId, OpenOptions, Signature, Sink, Source,
};

use crate::editor::SevenZipEditor;
use crate::reader::SevenZipReader;
use crate::writer::SevenZipWriter;

/// 7z 签名：魔数 `7z\xBC\xAF\x27\x1C`（02 §7.4；7z 格式规范签名头首 6 字节）。
pub static SIGNATURE: Signature = Signature {
    magic: b"7z\xBC\xAF\x27\x1C",
    offset: 0,
    display: "7z",
    extensions: &["7z"],
};

/// 能力矩阵（02 §8.2 / T25 §1 逐字对照，见模块文档）。
///
/// **能力如实声明**：`encryption=AesHeader`（头加密读写已交付，`aes` feature 门控）；
/// `split` / `solid` **恒为 false**——两个 feature 仍门控各自模块的编译，但 writer
/// 尚未接入（写侧分卷归 T46、固实块打包归 T46），能力不得先行声明（09 §15.7 / 02 §7.2）。
pub(crate) fn caps() -> FormatCaps {
    // `cfg!` 而非 `#[cfg]` 块：本函数在**非默认 feature 组合**（如合规构建
    // `--no-default-features --features default-formats`）下也要干净编译——
    // `#[cfg]` + `let mut c` 的写法会在唯一可变点（aes）被裁掉时触发 `unused_mut`。
    let aes = cfg!(feature = "aes");
    // 两条「能力不得先行声明」的说明（与 `solid` 的 09 §15.7 超声明修正同款）：
    // * `split` 恒为 false：`split` feature 仍门控 `split.rs`（`SplitSink`）的编译，
    //   但 **writer 尚未接入**——`writer.rs` 对 `Sink::Split` 直接 `Err Unsupported`
    //   （HZ-5001，写侧分卷归 T46）。声明成 true 会让调用方以为可用：CLI 实测
    //   `hzip c -f 7z --split 100m` 一路走到 writer 才报错，GUI 也会在 7z 上显示一个
    //   必然失败的「分卷」控件。接通后改回 `#[cfg(feature = "split")] { c.split = true; }`。
    // * `solid` 恒为 false：`solid.rs` 已编译但 writer 未接固实块打包，配套断言
    //   `solid_request_is_rejected` 守住「显式拒绝，而非接受后静默忽略」。
    FormatCaps {
        read: true,
        write: true,
        append: false,
        edit: true,
        // aes 关 → None（feature 回落口径，02 §7.2）
        encryption: if aes {
            EncryptionCaps::AesHeader
        } else {
            EncryptionCaps::None
        },
        split: false,
        solid: false,
        comment: false,
        // 7z 唯一写侧编码器 = LZMA2（sevenz-rust 装配点见 writer.rs；多编码器为后续扩展）
        methods: &[hzip_codec::traits::CodecMethod::Lzma2],
        write_encryption: if aes {
            &[EncryptionRequest::Aes(AesKey::Bits256)]
        } else {
            &[]
        },
    }
}

/// 7z handler（`Send + Sync`，无内部状态）。
#[derive(Debug, Default, Clone, Copy)]
pub struct SevenZipFormatHandler;

impl SevenZipFormatHandler {
    /// 便捷构造（与 `Default` 同义；handler 无状态）。
    pub fn new() -> Self {
        Self
    }
}

impl FormatHandler for SevenZipFormatHandler {
    fn id(&self) -> FormatId {
        FormatId::from(crate::FORMAT_ID)
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
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        SevenZipReader::open(src, opts)
    }

    fn create_writer(
        &self,
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError> {
        // 加密方案按 caps.write_encryption 校验（aes feature 裁剪 → 集合为空 →
        // 全拒，即原「需 aes feature」语义；方案不匹配如 zipcrypto-on-7z 同在此拒绝）。
        if let Some(req) = opts.encryption
            && !caps().write_encryption.contains(&req)
        {
            return Err(HzipError::Unsupported {
                what: format!("7z 不支持该写入加密方案：{req:?}（仅 AES-256，HZ-5001）").into(),
            });
        }
        if opts.append {
            return Err(HzipError::Unsupported {
                what: "7z 无追加语义（caps.append=false，02 §8.2；TC-CONF-005）".into(),
            });
        }
        if opts.solid {
            // **超声明修正（09 §15.7）**：`caps.solid` 此前在 feature 开启时为 true，
            // 但 writer 从未消费 `opts.solid`——`solid.rs` 的块模型（默认 64 MiB）
            // 只被其自身测试引用，条目仍逐条经 `push_archive_entry`（= 一条目一
            // folder）。也就是说「固实」过去是个**静默空操作**。
            //
            // 现在改为：能力如实声明为 false（见 `caps()`），且**显式拒绝**该请求，
            // 而不是接受后忽略——接受而忽略会让用户以为已固实，压缩率却与不勾一样。
            // 接通路径：改用 `SevenzWriter::push_archive_entries`（sevenz-rust 的
            // 固实入口）把整块作为**一个 folder** 压缩，并保留目录条目的
            // `has_stream=false` 语义（见 solid.rs rustdoc）。
            return Err(HzipError::Unsupported {
                what: "7z 固实打包尚未接通（solid.rs 块模型已就绪，writer 仍逐条目推送；\
                       caps.solid=false）"
                    .into(),
            });
        }
        if opts.comment.is_some() {
            return Err(HzipError::Unsupported {
                what: "7z 注释未启用（caps.comment=false）".into(),
            });
        }
        SevenZipWriter::create(dst, opts)
    }

    fn open_editor(
        &self,
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError> {
        SevenZipEditor::open(src, opts)
    }
}

#[cfg(test)]
#[path = "tests/handler.rs"]
mod tests;
