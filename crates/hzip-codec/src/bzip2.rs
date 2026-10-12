//! bzip2 封装（libbz2；02 §6.3/§4.7）。
//!
//! 级别映射见 [`crate::level::bzip2_level`]（block 列 1–9；`Store` 不可用，见下）。
//! 02 §6.3「—」列：bzip2 无 Store 级，`Level::Store` 时档案层走
//! [`crate::store`] 恒等路径，本封装不接受该组合。无内部多线程
//! （并行在 core 多条目层）。

use std::io::{self, Read, Write};

use bzip2::{Compression as BzCompression, read::BzDecoder, write::BzEncoder};

use crate::{
    counting::CountingWriter,
    crc::Crc32,
    level::{Level, bzip2_level},
    traits::{CodecCaps, CodecId, CodecProvider, Decoder, DecoderStats, Encoder, EncoderStats},
};

/// bzip2 编解码器门面（`id() == "bzip2"`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Bzip2Provider;

impl CodecProvider for Bzip2Provider {
    fn id(&self) -> CodecId {
        CodecId::from("bzip2")
    }

    fn caps(&self) -> CodecCaps {
        CodecCaps {
            threaded_encode: false,
            threaded_decode: false,
            streaming: true,
        }
    }

    fn encode(&self, level: Level, _threads: u32) -> Box<dyn Encoder> {
        // Store 级对 bzip2 不可用；工厂路由本不应进入。防御：降为 Fastest 级。
        let level = match level {
            Level::Store => Level::Fastest,
            l => l,
        };
        Bzip2Encoder::boxed(Box::new(Vec::new()), level)
    }

    fn decode(&self, _threads: u32) -> Box<dyn Decoder> {
        Box::new(Bzip2Decoder::new(io::empty()))
    }
}

/// bzip2 编码器（`Write` 输入原始数据，压缩产物写入内部 sink `W`）。
pub struct Bzip2Encoder<W: Write> {
    inner: BzEncoder<CountingWriter<W>>,
    crc: Crc32,
    bytes_in: u64,
}

impl<W: Write> Bzip2Encoder<W> {
    /// 构造 bzip2 编码器（`Fastest→1, Fast→3, Normal→6, Maximum|Ultra→9`）。
    ///
    /// `Level::Store` 不可用（02 §6.3「—」列）；传入时按 Fastest 处理。
    pub fn new(inner: W, level: Level) -> Self {
        let level = match level {
            Level::Store => Level::Fastest,
            l => l,
        };
        Self {
            inner: BzEncoder::new(
                CountingWriter::new(inner),
                BzCompression::new(bzip2_level(level)),
            ),
            crc: Crc32::new(),
            bytes_in: 0,
        }
    }

    /// 底层 sink 引用。
    pub fn get_ref(&self) -> &W {
        self.inner.get_ref().get_ref()
    }

    /// 底层 sink 可变引用。
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.get_mut().get_mut()
    }

    /// 结束编码并取出底层 sink（等价 `finish` 的冲刷，但不返回统计）。
    pub fn into_inner(self) -> io::Result<W> {
        Ok(self.inner.finish()?.into_inner())
    }
}

impl Bzip2Encoder<Box<dyn Write + Send>> {
    /// 以 trait 对象形式构造（sink 装箱）。
    pub fn boxed(inner: Box<dyn Write + Send>, level: Level) -> Box<dyn Encoder> {
        Box::new(Self::new(inner, level))
    }
}

impl<W: Write> Write for Bzip2Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crc.update(buf);
        self.bytes_in += buf.len() as u64;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Encoder for Bzip2Encoder<W> {
    fn finish(self: Box<Self>) -> io::Result<EncoderStats> {
        let mut this = *self;
        this.inner.try_finish()?;
        let bytes_out = this.inner.total_out();
        Ok(EncoderStats {
            bytes_in: this.bytes_in,
            bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}

/// bzip2 解码器（`Read` 输出原始数据；截断流返回 `io::Error`，不静默 EOF）。
pub struct Bzip2Decoder<R: Read> {
    inner: BzDecoder<R>,
    crc: Crc32,
    bytes_out: u64,
}

impl<R: Read> Bzip2Decoder<R> {
    /// 构造 bzip2 解码器。
    pub fn new(inner: R) -> Self {
        Self {
            inner: BzDecoder::new(inner),
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

impl<R: Read> Read for Bzip2Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.crc.update(&buf[..n]);
        self.bytes_out += n as u64;
        Ok(n)
    }
}

impl<R: Read> Decoder for Bzip2Decoder<R> {
    fn finish(self: Box<Self>) -> io::Result<DecoderStats> {
        let this = *self;
        Ok(DecoderStats {
            bytes_out: this.bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}
