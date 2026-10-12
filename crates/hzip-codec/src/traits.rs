//! 编解码抽象：`CodecProvider` / `Encoder` / `Decoder` 与 `CodecId`（02 §6.5，API 冻结）。

use std::fmt;
use std::io::{self, Read, Write};

use crate::level::Level;

/// 编解码器标识（02 §6.5）：`"store" | "deflate" | "bzip2" | "xz" | "zstd" | "lzma" | "lzma2" | "bcj" | "bcj2" ...`。
///
/// 本 crate 工厂 [`provider`] 实际返回 `store` + 已编译 feature 对应 codec；
/// `lzma`/`lzma2`/`bcj`/`bcj2` 等为上层（fmt-7z）装配所需预留的标识字面量。
pub type CodecId = smol_str::SmolStr;

/// 可选压缩方式（写侧「用哪个编码器」的**闭合枚举**）。
///
/// [`CodecId`] 是开放字符串标识（含 `store`/`bcj` 等内部用途），本枚举只列
/// **用户可选**的压缩方式——即写侧界面能暴露的那几个；经
/// [`CodecMethod::codec_id`] 桥到 [`CodecId`] 供诊断/路由。
///
/// # 与 [`Level`] 正交
///
/// 「方式」选**编码器**，「级别」选**该编码器压多狠**。「不压缩」（存储）由级别轴
/// 承担（[`Level::Store`] → zip 写 `Stored`、lzma2 取 [`crate::level::Mode::Copy`]），
/// 故本枚举**不含 Store**——避免同一语义出现两个入口（7-Zip 的 method 列表里
/// 有 `Copy`，hzip 不跟）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodecMethod {
    /// Deflate（zip / gzip 容器的原生编码）
    Deflate,
    /// BZip2
    BZip2,
    /// LZMA2（7z / xz 容器的原生编码）
    Lzma2,
    /// Zstandard
    Zstd,
}

impl CodecMethod {
    /// 稳定标识（配置落盘值 / UI 白名单 value / 诊断；**永不本地化**）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deflate => "deflate",
            Self::BZip2 => "bzip2",
            Self::Lzma2 => "lzma2",
            Self::Zstd => "zstd",
        }
    }

    /// 标识 → 枚举（严格匹配 canonical 标识；未知值 `None`——调用方回落格式默认）。
    ///
    /// 不做别名宽容（`lzma` ≠ `lzma2`、`zst` ≠ `zstd`）：配置写坏时应回落默认，
    /// 而不是静默选到另一个编码器。
    ///
    /// 命名与 [`CodecMethod::codec_id`] 配对。**故意不实现 `std::str::FromStr`**：
    /// 本函数对未知标识返回 `None`（「回落默认」语义），而 `FromStr` 的约定是
    /// `Err`（「解析失败」）——两者语义不同，硬套 trait 会让调用方误以为失败即错误。
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "deflate" => Some(Self::Deflate),
            "bzip2" => Some(Self::BZip2),
            "lzma2" => Some(Self::Lzma2),
            "zstd" => Some(Self::Zstd),
            _ => None,
        }
    }

    /// 桥到开放标识 [`CodecId`]（诊断/路由用）。
    pub fn codec_id(self) -> CodecId {
        self.as_str().into()
    }

    /// 人读名（诊断/日志；界面展示走 i18n 表 `opt.method.*`，不用本方法）。
    pub fn display(self) -> &'static str {
        match self {
            Self::Deflate => "Deflate",
            Self::BZip2 => "BZip2",
            Self::Lzma2 => "LZMA2",
            Self::Zstd => "Zstd",
        }
    }
}

impl fmt::Display for CodecMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 编解码器能力（02 §6.5）：线程性、流式性。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecCaps {
    /// 编码方向是否支持库内部多线程（lzma2/zstd 是；deflate/bzip2 否——其并行在档案多条目层）
    pub threaded_encode: bool,
    /// 解码方向是否支持库内部多线程（本层均否）
    pub threaded_decode: bool,
    /// 是否流式（本层全部为流式包装器）
    pub streaming: bool,
}

/// 编码统计（02 §6.5；`crc32` 为原始输入字节的流式 CRC，EntrySink drop 时校验）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderStats {
    /// 输入原始字节数
    pub bytes_in: u64,
    /// 输出压缩字节数
    pub bytes_out: u64,
    /// 输入字节的 CRC32
    pub crc32: u32,
}

/// 解码统计（02 §6.5）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderStats {
    /// 输出原始字节数
    pub bytes_out: u64,
    /// 输出字节的 CRC32
    pub crc32: u32,
}

/// 编解码器门面（02 §6.5，冻结级签名）。
///
/// 调用方通过 [`provider`] 或各 codec 模块的具体类型取得实例；以具体泛型适配器
/// （如 [`crate::deflate::DeflateEncoder`]）构造流式管线，`provider` 工厂的
/// `encode/decode` 返回内存型适配器（用于路由探测/能力查询，具体用法见各模块 rustdoc）。
pub trait CodecProvider: Send + Sync {
    /// 编解码器标识。
    fn id(&self) -> CodecId;
    /// 线程性、流式性能力。
    fn caps(&self) -> CodecCaps;
    /// 构造编码器（02 §6.5；`level` 为统一级别，`threads` 经 [`crate::level::clamp_threads`]）。
    fn encode(&self, level: Level, threads: u32) -> Box<dyn Encoder>;
    /// 构造解码器（02 §6.5；`threads` 仅记录于 `caps` 供上层调度，见各模块 rustdoc）。
    fn decode(&self, threads: u32) -> Box<dyn Decoder>;
}

/// 编码器（02 §6.5）：输出端。
///
/// 输出端（即写压缩字节的 sink 在构造时给定）：本 trait 的 `Write` 输入为**原始数据**，
/// 压缩产物写入内部 sink；`finish` 冲刷内部缓冲、写尾块并返回统计，之后底层 sink
/// 处于完整流末尾（截断/坏数据在写入或 `finish` 阶段以 `io::Error` 报出）。
pub trait Encoder: Write {
    /// 结束编码：冲刷内部缓冲、写尾块、返回统计（02 §6.5）。
    fn finish(self: Box<Self>) -> io::Result<EncoderStats>;
}

/// 解码器（02 §6.5）：输入端。
///
/// 输入端（压缩字节源在构造时给定）：本 trait 的 `Read` 输出**原始数据**。
///
/// 收尾契约（TC-CONF-008 codec 层贡献）：调用方须读至 EOF；**截断流 / 坏校验
/// 必须报 `Err`（`io::Error`），不得静默 EOF**。本层各实现（deflate/bzip2/xz/zstd）
/// 在 `read` 阶段即透传底层库的截断/校验错误；`finish` 在未发生错误时返回统计。
pub trait Decoder: Read {
    /// 结束解码：返回统计（02 §6.5；底层校验错误在 `read` 阶段已报出）。
    fn finish(self: Box<Self>) -> io::Result<DecoderStats>;
}

/// 工厂（02 §6.5）：按 `CodecId` 取 provider（feature 门控）。
///
/// `"store"` 恒返回恒等 codec；`deflate`/`bzip2`/`xz`/`zstd` 依对应 feature
/// 是否编译返回。未编译/不支持的 id 返回 `None`（上层映射为 Unsupported）。
pub fn provider(id: &str) -> Option<Box<dyn CodecProvider>> {
    match id {
        "store" => Some(Box::new(crate::store::StoreCodec)),
        #[cfg(feature = "deflate")]
        "deflate" => Some(Box::new(crate::deflate::DeflateProvider)),
        #[cfg(feature = "bzip2")]
        "bzip2" => Some(Box::new(crate::bzip2::Bzip2Provider)),
        #[cfg(feature = "xz")]
        "xz" => Some(Box::new(crate::xz::XzProvider)),
        #[cfg(feature = "zstd")]
        "zstd" => Some(Box::new(crate::zstd::ZstdProvider)),
        _ => None,
    }
}

/// 当前编译环境下可用的 codec id 列表（`store` 恒在，其余随 feature 裁剪）。
///
/// 供 fmt 层路由探测与参数化测试枚举。
pub fn supported_ids() -> Vec<CodecId> {
    let mut ids = vec![CodecId::from("store")];
    #[cfg(feature = "deflate")]
    ids.push(CodecId::from("deflate"));
    #[cfg(feature = "bzip2")]
    ids.push(CodecId::from("bzip2"));
    #[cfg(feature = "xz")]
    ids.push(CodecId::from("xz"));
    #[cfg(feature = "zstd")]
    ids.push(CodecId::from("zstd"));
    ids
}

/// 工厂路径构造失败时的占位编码器：`finish` 重放构造错误，保证工厂返回的
/// trait 对象不会在写入时 panic 穿越（任务卡 §7.4）。
#[derive(Debug)]
pub(crate) struct FailedEncoder {
    err: io::Error,
}

impl Write for FailedEncoder {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(self.err.kind(), "编码器未就绪（构造失败）"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(self.err.kind(), "编码器未就绪（构造失败）"))
    }
}

impl Encoder for FailedEncoder {
    fn finish(self: Box<Self>) -> io::Result<EncoderStats> {
        Err(self.err)
    }
}

/// 工厂路径构造失败时的占位解码器：`read` 重放构造错误（不 panic 穿越）。
#[derive(Debug)]
pub(crate) struct FailedDecoder {
    err: io::Error,
}

impl Read for FailedDecoder {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(self.err.kind(), "解码器未就绪（构造失败）"))
    }
}

impl Decoder for FailedDecoder {
    fn finish(self: Box<Self>) -> io::Result<DecoderStats> {
        Err(self.err)
    }
}

pub(crate) fn failed_encoder(err: io::Error) -> FailedEncoder {
    FailedEncoder { err }
}

pub(crate) fn failed_decoder(err: io::Error) -> FailedDecoder {
    FailedDecoder { err }
}

#[cfg(test)]
#[path = "tests/traits.rs"]
mod tests;
