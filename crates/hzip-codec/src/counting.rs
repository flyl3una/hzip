//! 私有工具：统计写字节数的 `Write` 包装器（压缩输出计账）。

use std::io::{self, Write};

/// 统计底层 sink 收到字节数的写包装器。
///
/// 各编码器以 `CountingWriter` 包裹输出端，`Encoder::finish` 时读取 `count`
/// 作为 `EncoderStats::bytes_out`（兼容 flate2/bzip2/xz/zstd 的内部缓冲，
/// 尾块刷新后计数精确）。
#[derive(Debug)]
pub(crate) struct CountingWriter<W: Write> {
    inner: W,
    count: u64,
}

impl<W: Write> CountingWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self { inner, count: 0 }
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    pub(crate) fn get_ref(&self) -> &W {
        &self.inner
    }

    pub(crate) fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// 取出底层 writer（含已写入的全部字节）。
    pub(crate) fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
