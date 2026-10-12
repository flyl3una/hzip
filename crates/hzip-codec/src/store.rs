//! 恒等（store）codec：`id() == "store"`。
//!
//! 02 §6.3「—」列语义的落点：zstd/bzip2 在 `Level::Store` 下不可用，档案层改走
//! 本 provider 路由——原样复制字节，仅计账 `bytes_in/bytes_out` 与 CRC32。

use std::io::{self, Read, Write};

use crate::{
    crc::Crc32,
    level::Level,
    traits::{CodecCaps, CodecId, CodecProvider, Decoder, DecoderStats, Encoder, EncoderStats},
};

/// store 编解码器门面（`id() == "store"`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct StoreCodec;

impl CodecProvider for StoreCodec {
    fn id(&self) -> CodecId {
        CodecId::from("store")
    }

    fn caps(&self) -> CodecCaps {
        CodecCaps {
            threaded_encode: false,
            threaded_decode: false,
            streaming: true,
        }
    }

    fn encode(&self, _level: Level, _threads: u32) -> Box<dyn Encoder> {
        StoreEncoder::<Box<dyn Write + Send>>::boxed(Box::new(Vec::new()))
    }

    fn decode(&self, _threads: u32) -> Box<dyn Decoder> {
        Box::new(StoreDecoder::new(io::empty()))
    }
}

/// 恒等编码器（`Write` 输入原样透传到 sink）。
#[derive(Debug)]
pub struct StoreEncoder<W: Write> {
    inner: W,
    crc: Crc32,
    bytes_in: u64,
}

impl<W: Write> StoreEncoder<W> {
    /// 构造恒等编码器。
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            crc: Crc32::new(),
            bytes_in: 0,
        }
    }

    /// 底层 sink 引用。
    pub fn get_ref(&self) -> &W {
        &self.inner
    }

    /// 底层 sink 可变引用。
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// 取出底层 sink（store 无需冲刷）。
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl StoreEncoder<Box<dyn Write + Send>> {
    /// 以 trait 对象形式构造（sink 装箱）。
    pub fn boxed(inner: Box<dyn Write + Send>) -> Box<dyn Encoder> {
        Box::new(Self::new(inner))
    }
}

impl<W: Write> Write for StoreEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crc.update(buf);
        self.bytes_in += buf.len() as u64;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Encoder for StoreEncoder<W> {
    fn finish(self: Box<Self>) -> io::Result<EncoderStats> {
        let this = *self;
        Ok(EncoderStats {
            bytes_in: this.bytes_in,
            bytes_out: this.bytes_in,
            crc32: this.crc.finalize(),
        })
    }
}

/// 恒等解码器（`Read` 输入原样透传）。
#[derive(Debug)]
pub struct StoreDecoder<R: Read> {
    inner: R,
    crc: Crc32,
    bytes_out: u64,
}

impl<R: Read> StoreDecoder<R> {
    /// 构造恒等解码器。
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            crc: Crc32::new(),
            bytes_out: 0,
        }
    }

    /// 底层源引用。
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// 底层源可变引用。
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }
}

impl<R: Read> Read for StoreDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.crc.update(&buf[..n]);
        self.bytes_out += n as u64;
        Ok(n)
    }
}

impl<R: Read> Decoder for StoreDecoder<R> {
    fn finish(self: Box<Self>) -> io::Result<DecoderStats> {
        let this = *self;
        Ok(DecoderStats {
            bytes_out: this.bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}
