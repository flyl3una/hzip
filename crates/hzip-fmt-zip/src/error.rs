//! zip crate → HzipError 错误映射（02 §8.1 读路径 / T24 §4.3 表）。
//!
//! 映射表见 crate 文档；本模块是唯一入口，供 reader / salvage / writer 复用。

use std::io::{self, ErrorKind};

use hzip_error::{EntryName, FormatErrorKind, HzipError, PasswordTarget};
use hzip_format::FormatId;
use zip::result::ZipError;

/// `zip::result::ZipError` → `HzipError`（§4.3 表逐行；未知分支兜底 BadHeader
/// + 原始串附注，02 §3.3「错误码语义对齐附录 A」）。
pub(crate) fn map_zip_err(e: ZipError, format: &FormatId) -> HzipError {
    match e {
        ZipError::Io(ioe) => {
            // 读末 CRC 不符：zip crate 以 io::ErrorKind::InvalidData 报出（crc32.rs
            // 的 invalid_checksum），语义是校验失败 → Checksum 3001（T24 §4.3 表）。
            if ioe.kind() == ErrorKind::InvalidData {
                let msg = ioe.to_string();
                if msg.contains("checksum") || msg.contains("Invalid checksum") {
                    return HzipError::Checksum {
                        entry: EntryName::new(""),
                        expected: 0,
                        actual: 0,
                    };
                }
                return HzipError::Format {
                    format: format.clone(),
                    kind: FormatErrorKind::BadHeader,
                };
            }
            if ioe.kind() == ErrorKind::UnexpectedEof {
                return HzipError::Format {
                    format: format.clone(),
                    kind: FormatErrorKind::Truncated,
                };
            }
            HzipError::Io {
                source: ioe,
                path: None,
            }
        }
        ZipError::InvalidPassword => HzipError::Password {
            target: PasswordTarget::Entry,
        },
        ZipError::FileNotFound => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        },
        ZipError::UnsupportedArchive(msg) => HzipError::Unsupported {
            what: format!("zip 档案不支持: {msg}").into(),
        },
        ZipError::InvalidArchive(msg) => map_invalid_archive(msg.into_owned(), format),
        _ => HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        },
    }
}

/// InvalidArchive 消息 → Format 细类（§4.3 表第 2..5 行）。
///
/// zip crate 对坏档案的错误消息语义分散（`invalid!` 宏产生的各类文案）；
/// 归并策略（与 02 §8.1 错误映射精神一致）：
///
/// - 消息含 PK 魔数相关关键词 / 首个本地头语义 → BadSignature 2001（由
///   `Signature` 预检把守，此处仅兜底）；
/// - 消息含 `EOCD` / `Central Directory` / `CDFH` / `offset` / `footer`（找不到
///   目录或截断）→ Truncated 2002（可 salvage 的由调用方先 salvage）；
/// - 压缩方法（`Compression method` / `PPMd` 等 zipx）语义放行给
///   `UnsupportedArchive`（已在上一分支命中）；`InvalidPassword` 已单列。
/// - 其余 → BadHeader 2004（原始消息经 display 附注，供诊断）。
fn map_invalid_archive(msg: String, format: &FormatId) -> HzipError {
    let m = msg.to_ascii_lowercase();
    let truncated = [
        "eocd",
        "central directory",
        "cdfh",
        "footer",
        "end of central",
        "multi-disk",
        "truncat",
        "unexpected end",
        "too large",
    ];
    if truncated.iter().any(|k| m.contains(k)) {
        return HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::Truncated,
        };
    }
    if m.contains("signature") || m.contains("magic") || m.contains("local header") {
        return HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadSignature,
        };
    }
    HzipError::Format {
        format: format.clone(),
        kind: FormatErrorKind::BadHeader,
    }
}

/// io::Error → HzipError（经 `From`，附加路径 None；供 writer/editor 兜底）。
pub(crate) fn map_io(e: io::Error) -> HzipError {
    HzipError::from(e)
}
