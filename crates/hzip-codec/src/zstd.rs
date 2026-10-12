//! zstd 封装（libzstd，zstd-safe；02 §6.3/§4.6）。
//!
//! 级别映射见 [`crate::level::zstd_level`]（`Store→0` 不可用，见下）。**Store 级 zstd
//! 不可用**（02 §6.3「—」列），档案层走 [`crate::store`] 恒等路径。
//!
//! 线程：`CParameter::NbWorkers(n)`（zstd job 切分并行压缩，frame 格式不变，与单线程
//! 产物字节可不同但均合法——断言只做可解性，任务卡 §7.7）。标准帧含魔数（默认，
//! 无需显式设置）；帧尾写 xxhash64 校验（`CParameter::ChecksumFlag(true)`，解码端依赖）。
//! `--long` 等高级参数本层不启用。
//!
//! 内存预算：`Ultra`(22) 隐含 windowLog 27（128 MB window，解码内存 ~500 MB 峰值风险，
//! TC-PERF-005 相关），fmt/core 层负责预算决策（任务卡 §7.5）。

use std::io::{self, Read, Write};

use zstd::stream::{
    raw::{CParameter, Encoder as RawEncoder},
    read::Decoder as ZstdDecoder,
    write::Encoder as ZstdEncoder,
};

use crate::{
    counting::CountingWriter,
    crc::Crc32,
    level::{Level, clamp_threads, zstd_level},
    traits::{CodecCaps, CodecId, CodecProvider, Decoder, DecoderStats, Encoder, EncoderStats},
};

/// zstd 编解码器门面（`id() == "zstd"`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct ZstdProvider;

impl CodecProvider for ZstdProvider {
    fn id(&self) -> CodecId {
        CodecId::from("zstd")
    }

    fn caps(&self) -> CodecCaps {
        CodecCaps {
            threaded_encode: true,
            threaded_decode: false,
            streaming: true,
        }
    }

    fn encode(&self, level: Level, threads: u32) -> Box<dyn Encoder> {
        let level = match level {
            Level::Store => Level::Fastest,
            l => l,
        };
        match ZstdEncoderWrapper::new(Vec::new(), level, threads) {
            Ok(e) => Box::new(e),
            Err(e) => Box::new(crate::traits::failed_encoder(e)),
        }
    }

    fn decode(&self, _threads: u32) -> Box<dyn Decoder> {
        match ZstdDecoderWrapper::new(io::empty()) {
            Ok(d) => Box::new(d),
            Err(e) => Box::new(crate::traits::failed_decoder(e)),
        }
    }
}

/// zstd 编码器（`Write` 输入原始数据，压缩产物写入内部 sink `W`；多线程）。
pub struct ZstdEncoderWrapper<W: Write> {
    inner: ZstdEncoder<'static, CountingWriter<W>>,
    crc: Crc32,
    bytes_in: u64,
}

impl<W: Write> ZstdEncoderWrapper<W> {
    /// 构造 zstd 编码器（`Store` 不可用，防御按 Fastest；`threads` 经 `clamp_threads`）。
    ///
    /// 产物为标准 zstd 帧（含魔数 + xxhash64 帧尾校验，`Decoder::finish` 依赖）。
    pub fn new(inner: W, level: Level, threads: u32) -> io::Result<Self> {
        let level = match level {
            Level::Store => Level::Fastest,
            l => l,
        };
        let mut raw = RawEncoder::new(zstd_level(level))?;
        raw.set_parameter(CParameter::ChecksumFlag(true))?;
        raw.set_parameter(CParameter::NbWorkers(clamp_threads(threads)))?;
        let inner = ZstdEncoder::with_encoder(CountingWriter::new(inner), raw);
        Ok(Self {
            inner,
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

    /// 结束编码并取出底层 sink（写帧尾；等价 `finish` 的冲刷，但不返回统计）。
    pub fn into_inner(self) -> io::Result<W> {
        let cw = self.inner.try_finish().map_err(|(_, e)| e)?;
        Ok(cw.into_inner())
    }
}

impl ZstdEncoderWrapper<Box<dyn Write + Send>> {
    /// 以 trait 对象形式构造（sink 装箱）。
    pub fn boxed(
        inner: Box<dyn Write + Send>,
        level: Level,
        threads: u32,
    ) -> io::Result<Box<dyn Encoder>> {
        Ok(Box::new(Self::new(inner, level, threads)?))
    }
}

impl<W: Write> Write for ZstdEncoderWrapper<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crc.update(buf);
        self.bytes_in += buf.len() as u64;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Encoder for ZstdEncoderWrapper<W> {
    fn finish(self: Box<Self>) -> io::Result<EncoderStats> {
        let mut this = *self;
        this.inner.do_finish()?;
        let bytes_out = this.inner.get_ref().count();
        Ok(EncoderStats {
            bytes_in: this.bytes_in,
            bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}

/// zstd 解码器（`Read` 输出原始数据；截断流/帧校验错误由 libzstd 报 `io::Error`）。
pub struct ZstdDecoderWrapper<R: Read> {
    inner: ZstdDecoder<'static, std::io::BufReader<R>>,
    crc: Crc32,
    bytes_out: u64,
}

impl<R: Read> ZstdDecoderWrapper<R> {
    /// 构造 zstd 解码器（单线程；读到帧尾后校验 xxhash64）。
    pub fn new(inner: R) -> io::Result<Self> {
        Ok(Self {
            inner: ZstdDecoder::new(inner)?,
            crc: Crc32::new(),
            bytes_out: 0,
        })
    }

    /// 底层源引用。
    pub fn get_ref(&self) -> &R {
        self.inner.get_ref().get_ref()
    }

    /// 底层源可变引用。
    pub fn get_mut(&mut self) -> &mut R {
        self.inner.get_mut().get_mut()
    }
}

impl<R: Read> Read for ZstdDecoderWrapper<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.crc.update(&buf[..n]);
        self.bytes_out += n as u64;
        Ok(n)
    }
}

impl<R: Read> Decoder for ZstdDecoderWrapper<R> {
    fn finish(self: Box<Self>) -> io::Result<DecoderStats> {
        let this = *self;
        Ok(DecoderStats {
            bytes_out: this.bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}
