//! deflate 封装（flate2，miniz_oxide 纯 Rust 后端；02 §6.3/§4.4）。
//!
//! 级别映射见 [`crate::level::deflate_level`]（zip deflate 列，`Store→0` 即 flate2
//! level 0 存储块）。deflate64 **只读**（zip crate 侧），本 crate 不提供 deflate64 编码
//! （02 §13.2：写路径永不用 deflate64）。deflate 并行发生在「档案内多条目」层
//! （02 §6.3 线程规则），本封装无内部多线程。

use std::io::{self, Read, Write};

use flate2::{
    Compression, read::DeflateDecoder as FlateDecoder, write::DeflateEncoder as FlateEncoder,
};

use crate::{
    counting::CountingWriter,
    crc::Crc32,
    level::{Level, deflate_level},
    traits::{CodecCaps, CodecId, CodecProvider, Decoder, DecoderStats, Encoder, EncoderStats},
};

/// deflate 编解码器门面（`id() == "deflate"`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct DeflateProvider;

impl CodecProvider for DeflateProvider {
    fn id(&self) -> CodecId {
        CodecId::from("deflate")
    }

    fn caps(&self) -> CodecCaps {
        CodecCaps {
            threaded_encode: false,
            threaded_decode: false,
            streaming: true,
        }
    }

    fn encode(&self, level: Level, _threads: u32) -> Box<dyn Encoder> {
        DeflateEncoder::boxed(Box::new(Vec::new()), level)
    }

    fn decode(&self, _threads: u32) -> Box<dyn Decoder> {
        Box::new(DeflateDecoder::new(io::empty()))
    }
}

/// deflate 编码器（`Write` 输入原始数据，压缩产物写入内部 sink `W`）。
#[derive(Debug)]
pub struct DeflateEncoder<W: Write> {
    inner: FlateEncoder<CountingWriter<W>>,
    crc: Crc32,
    bytes_in: u64,
}

impl<W: Write> DeflateEncoder<W> {
    /// 构造 deflate 编码器（`Store→0, Fastest→1, Fast→3, Normal→6, Maximum|Ultra→9`）。
    pub fn new(inner: W, level: Level) -> Self {
        Self {
            inner: FlateEncoder::new(
                CountingWriter::new(inner),
                Compression::new(deflate_level(level)),
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

impl DeflateEncoder<Box<dyn Write + Send>> {
    /// 以 trait 对象形式构造（sink 装箱）。
    pub fn boxed(inner: Box<dyn Write + Send>, level: Level) -> Box<dyn Encoder> {
        Box::new(Self::new(inner, level))
    }
}

impl<W: Write> Write for DeflateEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crc.update(buf);
        self.bytes_in += buf.len() as u64;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Encoder for DeflateEncoder<W> {
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

/// deflate 解码器（`Read` 输出原始数据；截断流返回 `io::Error`，不静默 EOF）。
#[derive(Debug)]
pub struct DeflateDecoder<R: Read> {
    inner: FlateDecoder<R>,
    crc: Crc32,
    bytes_out: u64,
}

impl<R: Read> DeflateDecoder<R> {
    /// 构造 deflate 解码器。
    pub fn new(inner: R) -> Self {
        Self {
            inner: FlateDecoder::new(inner),
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

impl<R: Read> Read for DeflateDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.crc.update(&buf[..n]);
        self.bytes_out += n as u64;
        Ok(n)
    }
}

impl<R: Read> Decoder for DeflateDecoder<R> {
    fn finish(self: Box<Self>) -> io::Result<DecoderStats> {
        let this = *self;
        Ok(DecoderStats {
            bytes_out: this.bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}
