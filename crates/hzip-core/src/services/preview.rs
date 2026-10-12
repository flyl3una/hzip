//! 预览服务（02 §9.10；T34）。
//!
//! [`preview`] 按需流截断读取单条目：文本（chardetng 检测转 UTF-8，默认
//! 256 KB）/ 图片（原始字节透传 + 魔数探测类型，默认 8 MB，**不解码**）/
//! HEX（原始字节，默认 4 KB）。上限经 `max_bytes` 参数优先于配置默认。
//!
//! 边界铁律（02 §9.10）：本模块不做图像解码（core 不依赖图像库）；魔数
//! 查表（16 字节内）≠ 解码。外部打开走 T11 `open` + TempArea 物化（T33
//! edit 侧能力），本模块不实现。

use std::io::Read;

use hzip_error::HzipError;
use hzip_format::types::Encoding;
use hzip_format::{ArchiveReader, EntryId};

/// 预览模式。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    /// 文本（chardetng → UTF-8）
    Text,
    /// 图片（原始字节 + 魔数探测，不解码）
    Image,
    /// HEX（原始字节 + 截断）
    Hex,
    /// 自动判定（NUL/控制字符占比 + 图片魔数）
    Auto,
}

/// 图片类型（魔数探测结果；GUI 选解码器用）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Bmp,
    WebP,
}

/// 文本默认上限（B.1 `preview.max_text_kb`）。
pub const DEFAULT_MAX_TEXT: u64 = 256 * 1024;
/// 图片默认上限（B.1 `preview.max_image_mb`）。
pub const DEFAULT_MAX_IMAGE: u64 = 8 * 1024 * 1024;
/// HEX 默认上限（B.1 `preview.max_hex_kb`）。
pub const DEFAULT_MAX_HEX: u64 = 4 * 1024;

/// 预览结果。
pub struct PreviewData {
    /// 实际生效模式（Auto 判定结果）
    pub mode: PreviewMode,
    /// 截断后的原始/转码后字节
    pub bytes: Vec<u8>,
    /// 读满上限（档案条目更大）
    pub truncated: bool,
    /// 条目完整大小
    pub total_size: u64,
    /// Text 模式：检出编码（转 UTF-8 后的报告值）
    pub encoding: Option<Encoding>,
    /// Text 低置信/转码退化标记
    pub suspect: bool,
    /// Image 模式：魔数探测
    pub image_kind: Option<ImageKind>,
}

impl PreviewData {
    fn new(mode: PreviewMode, bytes: Vec<u8>, truncated: bool, total_size: u64) -> Self {
        Self {
            mode,
            bytes,
            truncated,
            total_size,
            encoding: None,
            suspect: false,
            image_kind: None,
        }
    }
}

/// 预览单条目（截断流；上限 = max_bytes 参数优先于模式默认）。
///
/// `pw`：会话密码（加密条目预览用；`PasswordRef::none()` = 未加密/未缓存——
/// 加密条目会以 `HzipError::Password`（3101/3102）失败，由调用方过密码门）。
pub fn preview(
    reader: &dyn ArchiveReader,
    entry_id: EntryId,
    mode: PreviewMode,
    max_bytes: Option<u64>,
    pw: hzip_format::PasswordRef<'_>,
) -> Result<PreviewData, HzipError> {
    let info = match reader.entries()?.into_iter().find(|e| e.id == entry_id) {
        Some(e) => e,
        None => {
            return Err(HzipError::Unsupported {
                what: format!("hz-5003 no-entry-{entry_id}").into(),
            });
        }
    };
    let total_size = info.size_raw;
    let mode = match mode {
        PreviewMode::Auto => auto_mode(&info, reader, entry_id, pw)?,
        m => m,
    };
    let limit = max_bytes.unwrap_or(match mode {
        PreviewMode::Text => DEFAULT_MAX_TEXT,
        PreviewMode::Image => DEFAULT_MAX_IMAGE,
        PreviewMode::Hex => DEFAULT_MAX_HEX,
        PreviewMode::Auto => DEFAULT_MAX_HEX, // unreachable（Auto 已解析）
    });
    let stream = reader.read_entry(entry_id, pw)?;
    let mut buf = Vec::with_capacity(limit as usize);
    let mut take = stream.take(limit + 1); // +1 探测截断
    take.read_to_end(&mut buf).map_err(|e| HzipError::Io {
        source: e,
        path: None,
    })?;
    let truncated =
        buf.len() as u64 > limit || (buf.len() as u64) < total_size && total_size > limit;
    if buf.len() as u64 > limit {
        buf.truncate(limit as usize);
    }
    match mode {
        PreviewMode::Text => text_preview(buf, truncated, total_size),
        PreviewMode::Image => {
            let kind = detect_image_kind(&buf);
            let mut d = PreviewData::new(PreviewMode::Image, buf, truncated, total_size);
            d.image_kind = kind;
            Ok(d)
        }
        PreviewMode::Hex => Ok(PreviewData::new(
            PreviewMode::Hex,
            buf,
            truncated,
            total_size,
        )),
        PreviewMode::Auto => unreachable!("Auto resolved"),
    }
}

/// Text 模式：chardetng 检测 → UTF-8 转码（低置信 lossy + suspect）。
fn text_preview(buf: Vec<u8>, truncated: bool, total_size: u64) -> Result<PreviewData, HzipError> {
    let mut d = PreviewData::new(PreviewMode::Text, buf, truncated, total_size);
    let raw = std::mem::take(&mut d.bytes);
    // chardetng 单样本检测
    let mut det = chardetng::EncodingDetector::new();
    det.feed(&raw, true);
    let (enc, assess) = det.guess_assess(None, true);
    let has_non_ascii = raw.iter().any(|&b| b >= 0x80);
    if has_non_ascii
        && assess
        && let Some(hz) = Encoding::from_charset(enc.name())
        && let Some(e) = encoding_rs::Encoding::for_label(enc.name().as_bytes())
    {
        let (cow, _, had_errors) = e.decode(&raw);
        if !had_errors {
            d.bytes = cow.into_owned().into_bytes();
            d.encoding = Some(hz);
            return Ok(d);
        }
    }
    // 低置信/失败 → lossy UTF-8 + suspect
    let (cow, _, _) = encoding_rs::UTF_8.decode(&raw);
    d.bytes = cow.into_owned().into_bytes();
    d.suspect = true;
    Ok(d)
}

/// Auto 判定：前 4KB 统计 NUL/控制字符 + 图片魔数。
fn auto_mode(
    _info: &hzip_format::EntryInfo,
    reader: &dyn ArchiveReader,
    entry_id: EntryId,
    pw: hzip_format::PasswordRef<'_>,
) -> Result<PreviewMode, HzipError> {
    // 先看魔数（读前 16 字节）
    let mut stream = reader.read_entry(entry_id, pw)?;
    let mut head = [0u8; 16];
    let mut filled = 0;
    while filled < 16 {
        let n = stream
            .read(&mut head[filled..])
            .map_err(|e| HzipError::Io {
                source: e,
                path: None,
            })?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    if detect_image_kind(&head[..filled]).is_some() {
        return Ok(PreviewMode::Image);
    }
    // 文本启发：控制字符占比低
    let sample = &head[..filled];
    if sample.is_empty() {
        return Ok(PreviewMode::Text);
    }
    let ctrl = sample
        .iter()
        .filter(|&&b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r')
        .count();
    let nul = sample.iter().filter(|&&b| b == 0).count();
    if ctrl == 0 && nul == 0 {
        Ok(PreviewMode::Text)
    } else if nul > 0 {
        Ok(PreviewMode::Hex)
    } else {
        // 控制字符但少 → 文本（二进制多 NUL）
        Ok(PreviewMode::Text)
    }
}

/// 图片魔数探测（16 字节内查表；不解码）。
fn detect_image_kind(head: &[u8]) -> Option<ImageKind> {
    if head.len() >= 8 && head[0..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        Some(ImageKind::Png)
    } else if head.len() >= 3 && head[0..3] == [0xFF, 0xD8, 0xFF] {
        Some(ImageKind::Jpeg)
    } else if head.len() >= 4 && &head[0..4] == b"GIF8" {
        Some(ImageKind::Gif)
    } else if head.len() >= 2 && &head[0..2] == b"BM" {
        Some(ImageKind::Bmp)
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some(ImageKind::WebP)
    } else {
        None
    }
}
