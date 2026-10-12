//! 格式嗅探（02 §7.4；Sniffed/Confidence 公开，魔数表内部）。
//!
//! 流程：读前 512 B → 魔数匹配（长魔数优先）→ 命中压缩流（gz/bz2/xz/zstd）则
//! 解一层递归嗅探内层 → 无命中查 ustar@257 → 扩展名回退（`Confidence::Low`）→
//! 全无 → `Err Format{BadSignature}`（错误码 2001；头部 hex 由调用方附 Display）。
//!
//! `Source::Stream` 为一次性流不可重放：对组合嗅探直接返回 `Err Unsupported`
//! （调用方应先对 File/Volumes/Stdin 完成嗅探再进入读取；02 §7.4 组合装配由
//! registry 从原始源重建链）。

use std::io::Read;

use hzip_error::HzipError;

use crate::FormatId;
use crate::consts::SNIFF_HEAD_BYTES;
use crate::handler::Source;

/// 嗅探结果（`layers` 外→内；tail 为最内层语义）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sniffed {
    /// 单格式直接命中
    Single(FormatId, Confidence),
    /// 组合：压缩层 + 尾语义
    Composite {
        /// 压缩层（外→内）
        layers: Vec<FormatId>,
        /// 尾容器（Raw = 单文件流语义，02 §7.5 规则③）
        tail: CompositeTail,
        /// 置信度
        confidence: Confidence,
    },
}

/// 组合尾语义。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositeTail {
    /// 容器格式（tar/zip/7z/…）
    Container(FormatId),
    /// 单文件流（`a.txt.gz.gz` 语义，剥多层后缀）
    Raw,
}

/// 置信度（Low = 扩展名回退）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// 高（魔数/结构命中）
    High,
    /// 低（扩展名回退）
    Low,
}

impl Sniffed {
    /// 展示用格式描述。
    pub fn describe(&self) -> String {
        match self {
            Sniffed::Single(f, _) => f.to_string(),
            Sniffed::Composite { layers, tail, .. } => {
                let mut s = layers
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join("+");
                match tail {
                    CompositeTail::Container(f) => s.push_str(&format!("+{f}")),
                    CompositeTail::Raw => s.push_str("+raw"),
                }
                s
            }
        }
    }
}

/// 魔数表条目。
struct MagicEntry {
    format: &'static str,
    magic: &'static [u8],
    /// 压缩流（命中后需解层递归）
    stream: bool,
}

/// 魔数表（长魔数优先——数组序即匹配序，手工保证 rar5 在 rar4 前）。
const MAGIC_TABLE: &[MagicEntry] = &[
    MagicEntry {
        format: "rar",
        magic: b"Rar!\x1a\x07\x01\x00",
        stream: false,
    },
    MagicEntry {
        format: "rar",
        magic: b"Rar!\x1a\x07\x00",
        stream: false,
    },
    MagicEntry {
        format: "7z",
        magic: b"7z\xbc\xaf\x27\x1c",
        stream: false,
    },
    MagicEntry {
        format: "zip",
        magic: b"PK\x03\x04",
        stream: false,
    },
    // cpio 三 ASCII 变体（02 §8.7）：newc/crc/odc **各 6 字节定长**，逐字节区分
    // （odc 的 `070707` 不是 newc/crc 的前缀，但仍按「先 newc/crc 后 odc」列出，
    // 免去将来加变体时的回头核对）
    MagicEntry {
        format: "cpio",
        magic: b"070701",
        stream: false,
    },
    MagicEntry {
        format: "cpio",
        magic: b"070702",
        stream: false,
    },
    MagicEntry {
        format: "cpio",
        magic: b"070707",
        stream: false,
    },
    MagicEntry {
        format: "xz",
        magic: b"\xfd7zXZ\x00",
        stream: true,
    },
    MagicEntry {
        format: "zst",
        magic: b"\x28\xb5\x2f\xfd",
        stream: true,
    },
    MagicEntry {
        format: "gz",
        magic: b"\x1f\x8b",
        stream: true,
    },
    MagicEntry {
        format: "bz2",
        magic: b"BZh",
        stream: true,
    }, // 需第 4 字节级别位
    // 旧二进制 cpio（`0x71C7` = 八进制 070707 的两种字序）。**2 字节魔数误命中面
    // 最大**，故排在表尾：其余长魔数先判，散落的 `c7 71` 开头的非 cpio 数据交
    // handler 的魔数预检二次拒绝（2001），用户可见结果与表内缺席时一致。
    MagicEntry {
        format: "cpio",
        magic: b"\xc7\x71",
        stream: false,
    },
    MagicEntry {
        format: "cpio",
        magic: b"\x71\xc7",
        stream: false,
    },
];

/// tar ustar 魔数偏移（02 附录 B.2，与 hzip-fmt-tar 同值）。
pub const TAR_MAGIC_OFFSET: usize = 257;
const USTAR_MAGIC: &[u8; 5] = b"ustar";

/// zstd skippable 帧判定（`0x184D2A50` 小端 → `50 2A 4D 18`，末字节 `0x5X`）。
fn is_zstd_skippable(head: &[u8]) -> bool {
    head.len() >= 4
        && head[0] == 0x50
        && head[1] == 0x2A
        && head[2] == 0x4D
        && (head[3] & 0xF0) == 0x50
}

/// 读取前 512 B（返回实际填充长度，可能 < 512）。
///
/// # 缺陷修复背景（T32 授权例外改动）
///
/// 原实现循环读至 EOF（`n == 0` 才停）——但 `head` 上限 `SNIFF_HEAD_BYTES`，
/// `filled` 达上限后读**空切片**：gz/bz2/xz 解码器对空 buf 的 `read` 返回
/// `Ok(0)`（等价 EOF），而 `zstd::stream::read::Decoder` 对空 buf 返回
/// `Err("Operation made no progress ... output buffer being full")` → `.tar.zst`
/// 组合嗅探必败。嗅探读头本就不需要 EOF 语义——读满 `SNIFF_HEAD_BYTES` 即停
/// （`fill_buf` 语义），不足 512 才以 EOF 提前结束。
pub(crate) fn read_head_bytes(r: &mut dyn Read) -> std::io::Result<Vec<u8>> {
    let mut head = vec![0u8; SNIFF_HEAD_BYTES];
    let mut filled = 0usize;
    loop {
        if filled >= SNIFF_HEAD_BYTES {
            break; // 读满即止（zstd 空 buf read 报 Err；读头不须 EOF）
        }
        let n = r.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    head.truncate(filled);
    Ok(head)
}

/// 打开源为可重放读（File/Volumes 开文件）。
///
/// Stdin/Stream 为不可重放源：单层嗅探由 [`read_head_once`] 直接处理
/// （StdinLock 不 Send，不进重放链）；组合嗅探/装配对这些源返回 Unsupported
/// （stdin 组合场景由读取路径的缓冲代理处理，02 §7.4）。
fn open_replayable(src: &Source) -> std::io::Result<Option<Box<dyn Read + Send>>> {
    match src {
        Source::File(p) | Source::Volumes { first: p, .. } => {
            std::fs::File::open(p).map(|f| Some(Box::new(std::io::BufReader::new(f)) as _))
        }
        Source::Stdin | Source::Stream(_) => Ok(None),
    }
}

/// 一次性读头（Stdin 单层嗅探用；不可重放）。
fn read_head_once(src: &Source) -> std::io::Result<Vec<u8>> {
    match src {
        Source::Stdin => {
            let mut stdin = std::io::stdin().lock();
            read_head_bytes(&mut stdin)
        }
        // Stream 属一次性流：registry 嗅探前已是 File/Volumes/Stdin（组合装配
        // 内层 Stream 不再二次嗅探）。此处返回空以表达「不可非破坏嗅探」，
        // 由调用方在 sniff_impl 层转 Unsupported（见 open_replayable=None 分支）。
        Source::Stream(_) => Ok(Vec::new()),
        _ => unreachable!("read_head_once 仅用于 Stdin/Stream"),
    }
}

/// 魔数匹配（bz2 需 BZh + 第 4 字节级别位 '1'..'9'）。
fn match_magic(head: &[u8]) -> Option<&'static MagicEntry> {
    for entry in MAGIC_TABLE {
        if entry.format == "bz2" {
            if head.len() >= 4 && head.starts_with(b"BZh") && (b'1'..=b'9').contains(&head[3]) {
                return Some(entry);
            }
            continue;
        }
        if entry.magic.len() <= head.len() && head.starts_with(entry.magic) {
            return Some(entry);
        }
    }
    None
}

/// ustar 判定（偏移 257 起 5 字节 "ustar"）。
fn is_ustar(head: &[u8]) -> bool {
    head.len() >= TAR_MAGIC_OFFSET + 5
        && &head[TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + 5] == USTAR_MAGIC
}

/// 扩展名回退（`.zipx`/`.zip`/`.7z`/`.rar`/`.cpio`）。
fn match_extension(src: &Source) -> Option<FormatId> {
    let path = match src {
        Source::File(p) | Source::Volumes { first: p, .. } => p,
        _ => return None,
    };
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let known: &[(&str, &str)] = &[
        ("zip", "zip"),
        ("7z", "7z"),
        ("rar", "rar"),
        // cpio 的四变体魔数都在偏移 0，正常路径不会走到扩展名回退；列出 `.cpio`
        // 是为了「内容被截到不足 6 字节」时仍报一个带格式名的错（而非 unknown）
        ("cpio", "cpio"),
    ];
    for (fmt, ext) in known {
        if name.ends_with(&format!(".{ext}")) || name == *ext {
            return Some(FormatId::from(*fmt));
        }
    }
    if name.ends_with(".zipx") {
        return Some(FormatId::from("zip"));
    }
    None
}

/// 包装解层解码器（feature 门控；02 §7.5 要点④：gzip 容器帧不在 codec 抽象内）。
///
/// 公开面：**嵌套容器下钻**（`Browser::enter` 遇到 `.tar.gz`/`.tgz` 这类组合条目时
/// 先解压缩层再交给容器 handler）与本 crate 的组合嗅探共用同一份解码器构造 ——
/// 各编解码库的 feature 门控知识只在这里维护一次。
pub fn wrap_layer_decoder(
    format: &str,
    r: Box<dyn Read + Send>,
) -> std::io::Result<Box<dyn Read + Send>> {
    match format {
        #[cfg(feature = "deflate")]
        "gz" => Ok(Box::new(flate2::read::GzDecoder::new(r))),
        #[cfg(feature = "bzip2")]
        "bz2" => Ok(Box::new(bzip2::read::BzDecoder::new(r))),
        #[cfg(feature = "xz")]
        "xz" => Ok(Box::new(xz2::read::XzDecoder::new(r))),
        #[cfg(feature = "zstd")]
        "zst" => Ok(Box::new(zstd::stream::read::Decoder::new(r)?)),
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("嗅探到未启用的压缩层 {other}"),
        )),
    }
}

/// 嗅探主入口。
pub(crate) fn sniff_impl(src: &Source) -> Result<Sniffed, HzipError> {
    use hzip_error::FormatErrorKind;

    let bad_signature = |what: &str| HzipError::Format {
        format: FormatId::from(what),
        kind: FormatErrorKind::BadSignature,
    };

    // 不可重放源：Stdin 一次性读头可单层嗅探；Stream 属一次性流不支持嗅探
    // （组合装配内层 Stream 由 registry 在打开时构造，不二次嗅探）。
    let mut replayable = open_replayable(src).map_err(crate::map_io)?;
    let head = match replayable.as_deref_mut() {
        Some(r) => read_head_bytes(r).map_err(crate::map_io)?,
        None => match src {
            Source::Stdin => read_head_once(src).map_err(crate::map_io)?,
            Source::Stream(_) => {
                return Err(HzipError::Unsupported {
                    what: "Stream 源不支持嗅探（一次性流不可重放）".into(),
                });
            }
            _ => unreachable!(),
        },
    };
    if head.is_empty() {
        return Err(bad_signature("empty"));
    }

    if !is_zstd_skippable(&head)
        && let Some(entry) = match_magic(&head)
    {
        if entry.stream {
            if replayable.is_none() {
                return Err(HzipError::Unsupported {
                    what: "组合嗅探需要可重放源（File/Volumes）".into(),
                });
            }
            return sniff_composite_replay(src, vec![FormatId::from(entry.format)], 1);
        }
        return Ok(Sniffed::Single(
            FormatId::from(entry.format),
            Confidence::High,
        ));
    }
    if is_ustar(&head) {
        return Ok(Sniffed::Single(FormatId::from("tar"), Confidence::High));
    }
    if let Some(f) = match_extension(src) {
        return Ok(Sniffed::Single(f, Confidence::Low));
    }
    Err(bad_signature("unknown"))
}

/// 压缩层递归嗅探：每次从原始源**重建解码链**读内层头（嗅探只读头部、开销一次性）。
fn sniff_composite_replay(
    src: &Source,
    layers: Vec<FormatId>,
    depth: u32,
) -> Result<Sniffed, HzipError> {
    use hzip_error::FormatErrorKind;

    let bad_layer = |f: &str| HzipError::Format {
        format: FormatId::from(f),
        kind: FormatErrorKind::BadHeader,
    };

    // 重建链：从原始源逐层 wrap（外→内）
    let mut chain: Box<dyn Read + Send> = match open_replayable(src).map_err(crate::map_io)? {
        Some(r) => r,
        None => {
            return Err(HzipError::Unsupported {
                what: "stream-sniff".into(),
            });
        }
    };
    for f in &layers {
        chain = wrap_layer_decoder(f.as_str(), chain).map_err(|_| bad_layer(f.as_str()))?;
    }
    let head = read_head_bytes(&mut chain).map_err(crate::map_io)?;

    if head.is_empty() {
        // 压缩流解完为空 → 视作 Raw 单文件流（空档案）
        if layers.len() == 1 {
            return Ok(Sniffed::Single(layers[0].clone(), Confidence::High));
        }
        return Ok(Sniffed::Composite {
            layers,
            tail: CompositeTail::Raw,
            confidence: Confidence::High,
        });
    }

    if !is_zstd_skippable(&head)
        && let Some(inner) = match_magic(&head)
    {
        if inner.stream {
            if depth + 1 > crate::consts::COMPOSITE_DEPTH_MAX {
                return Err(HzipError::Quota {
                    quota: hzip_error::QuotaKind::MaxEntries, // HZ-4504
                });
            }
            let mut layers = layers;
            layers.push(FormatId::from(inner.format));
            return sniff_composite_replay(src, layers, depth + 1);
        }
        return Ok(Sniffed::Composite {
            layers,
            tail: CompositeTail::Container(FormatId::from(inner.format)),
            confidence: Confidence::High,
        });
    }
    if is_ustar(&head) {
        return Ok(Sniffed::Composite {
            layers,
            tail: CompositeTail::Container(FormatId::from("tar")),
            confidence: Confidence::High,
        });
    }
    if layers.len() == 1 {
        return Ok(Sniffed::Single(layers[0].clone(), Confidence::High));
    }
    Ok(Sniffed::Composite {
        layers,
        tail: CompositeTail::Raw,
        confidence: Confidence::High,
    })
}
