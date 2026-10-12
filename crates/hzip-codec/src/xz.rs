//! xz 封装（xz2 / liblzma；02 §6.3/§4.5）。
//!
//! 级别映射见 [`crate::level::xz_preset`] / [`crate::level::xz_extreme`]；`Store→0` 合法。
//! **多线程仅编码方向**：`MtStreamBuilder` 按 LZMA2 block 并行压缩（xz 容器不变，
//! GNU `xz -T` 兼容）；解码端 `XzDecoder` 单线程，`threads` 参数仅记录于 `caps`
//! 供上层调度（xz2 解码多线程需 block index 预扫描，暂不做）。
//!
//! `Level::Ultra` = preset 9 + extreme（9e，经 liblzma `LZMA_PRESET_EXTREME` 位透传）。
//! block_size 不显式设置（0）→ liblzma 按字典 3×dict 推定（02 §4.5 默认语义）。
//! 多流（`.xz` 串联）在 fmt-single 层处理（02 §8.5），本层单流。
//!
//! 内存预算：`Ultra` preset 解码 window 峰值风险 ~512 MB（TC-PERF-005 相关），
//! fmt/core 层负责预算决策；本层不静默降级（任务卡 §7.5）。

use std::io::{self, Read, Write};

use xz2::{read::XzDecoder, stream::MtStreamBuilder, write::XzEncoder as XzWriter};

use crate::{
    counting::CountingWriter,
    crc::Crc32,
    level::{Level, xz_extreme, xz_preset},
    traits::{CodecCaps, CodecId, CodecProvider, Decoder, DecoderStats, Encoder, EncoderStats},
};

/// liblzma `LZMA_PRESET_EXTREME` 位（`1u << 31`，即 9e 的 e）。
const XZ_EXTREME_BIT: u32 = 1 << 31;

/// 计算 MtStreamBuilder 的 preset 值：preset 数字 + 可选的 extreme 位。
fn xz_preset_value(level: Level) -> u32 {
    xz_preset(level) | if xz_extreme(level) { XZ_EXTREME_BIT } else { 0 }
}

/// xz 编解码器门面（`id() == "xz"`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct XzProvider;

impl CodecProvider for XzProvider {
    fn id(&self) -> CodecId {
        CodecId::from("xz")
    }

    fn caps(&self) -> CodecCaps {
        CodecCaps {
            threaded_encode: true,
            threaded_decode: false,
            streaming: true,
        }
    }

    fn encode(&self, level: Level, threads: u32) -> Box<dyn Encoder> {
        // 内存型工厂路径：sink 用 Vec；流式管线请用 XzEncoder::new_mt 指定实际 sink。
        match XzEncoder::new_mt(Vec::new(), level, threads) {
            Ok(e) => Box::new(e),
            Err(e) => Box::new(crate::traits::failed_encoder(e)),
        }
    }

    fn decode(&self, _threads: u32) -> Box<dyn Decoder> {
        Box::new(XzDecoderWrapper::new(io::empty()))
    }
}

/// xz 编码器（`Write` 输入原始数据，压缩产物写入内部 sink `W`；多线程）。
pub struct XzEncoder<W: Write> {
    inner: XzWriter<CountingWriter<W>>,
    crc: Crc32,
    bytes_in: u64,
}

impl<W: Write> XzEncoder<W> {
    /// 构造 xz 多线程编码器（`threads` 经 `clamp_threads`；`Store→0, ... Ultra→9e`）。
    ///
    /// `threads == 0` 由 liblzma 视为 auto（全部可用核）。
    pub fn new_mt(inner: W, level: Level, threads: u32) -> io::Result<Self> {
        let mut b = MtStreamBuilder::new();
        b.threads(crate::level::clamp_threads(threads))
            .timeout_ms(u32::MAX)
            .preset(xz_preset_value(level));
        let stream = b.encoder()?;
        Ok(Self {
            inner: XzWriter::new_stream(CountingWriter::new(inner), stream),
            crc: Crc32::new(),
            bytes_in: 0,
        })
    }

    /// 底层 sink 引用。
    pub fn get_ref(&self) -> &W {
        self.inner.get_ref().get_ref()
    }

    /// 底层 sink 可变引用。
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.get_mut().get_mut()
    }

    /// 结束编码并取出底层 sink（写 footer；等价 `finish` 的冲刷，但不返回统计）。
    pub fn into_inner(self) -> io::Result<W> {
        Ok(self.inner.finish()?.into_inner())
    }
}

impl XzEncoder<Box<dyn Write + Send>> {
    /// 以 trait 对象形式构造（sink 装箱）。
    pub fn boxed(
        inner: Box<dyn Write + Send>,
        level: Level,
        threads: u32,
    ) -> io::Result<Box<dyn Encoder>> {
        Ok(Box::new(Self::new_mt(inner, level, threads)?))
    }
}

impl<W: Write> Write for XzEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crc.update(buf);
        self.bytes_in += buf.len() as u64;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Encoder for XzEncoder<W> {
    fn finish(self: Box<Self>) -> io::Result<EncoderStats> {
        let mut this = *self;
        this.inner.try_finish()?;
        let bytes_out = this.inner.get_ref().count();
        Ok(EncoderStats {
            bytes_in: this.bytes_in,
            bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}

/// xz 解码器（`Read` 输出原始数据；截断流/坏 check 返回 `io::Error`，不静默 EOF）。
pub struct XzDecoderWrapper<R: Read> {
    inner: XzDecoder<R>,
    crc: Crc32,
    bytes_out: u64,
}

impl<R: Read> XzDecoderWrapper<R> {
    /// 构造 xz 解码器（单线程；footer 与 check 由 liblzma 校验）。
    pub fn new(inner: R) -> Self {
        Self {
            inner: XzDecoder::new(inner),
            crc: Crc32::new(),
            bytes_out: 0,
        }
    }

    /// 底层源引用。
    pub fn get_ref(&self) -> &R {
        self.inner.get_ref()
    }

    /// 底层源可变引用。
    pub fn get_mut(&mut self) -> &mut R {
        self.inner.get_mut()
    }
}

impl<R: Read> Read for XzDecoderWrapper<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.crc.update(&buf[..n]);
        self.bytes_out += n as u64;
        Ok(n)
    }
}

impl<R: Read> Decoder for XzDecoderWrapper<R> {
    fn finish(self: Box<Self>) -> io::Result<DecoderStats> {
        let this = *self;
        Ok(DecoderStats {
            bytes_out: this.bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}
