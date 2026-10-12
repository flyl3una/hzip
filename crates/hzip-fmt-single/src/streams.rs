//! 单文件流的读取基础设施：头预读（不吞字节）+ 解码包装（02 §8.5 / T23 §4.5）。
//!
//! # 问题
//!
//! `entries()` 需要读取流**头**（gzip mtime/FNAME、魔数确认），而读取契约要求
//! `entries()` 不消费数据区（红线 1）；`read_entry(0)` 返回的必须是**从头开始**的
//! 完整解码流。对可重放源（File/Volumes）可重开文件解决；对不可重放源
//! （Stdin/Stream）则由 [`ReplayReader`] 把「预读阶段消费的字节」暂存并在解码阶段
//! 重放，保证字节不丢、不重复、不乱序。
//!
//! # 解码包装
//!
//! - **gz**：`flate2::read::MultiGzDecoder`（内建 multi-member 串联；任一成员尾
//!   CRC32/ISIZE 失败报错，不静默 EOF）——02 §8.5「多成员 gzip 读取端串联全部
//!   成员」的最短实现（T23 §4.5-1）；
//! - **bz2**：`bzip2::read::MultiBzDecoder`（内建成员串联 + 每块 CRC 校验；
//!   Wikipedia 等即 bzip2 multi-stream 产物）；
//! - **xz**：`xz2::read::XzDecoder::new_multi_decoder`（concatenated stream 支持；
//!   T23 §4.5-3 首选，坏第二流报 `InvalidData`）；
//! - **zst**：`zstd::stream::read::Decoder`（默认拼接帧直至 EOF，并自动跳过
//!   skippable 帧——含 hzip 私有 mtime 帧；坏帧/截断报错）。
//!
//! 底层库报错统一经 `io::Error` 透传：`UnexpectedEof` → 截断（HZ-2002），其余
//! 数据损坏 → `Format{BadHeader}`/`Checksum` 由调用方 [`crate::map_io`] 细化。

use std::io::Read;

/// 不可重放源预读缓冲（头解析阶段消费的字节在解码阶段重放）。
///
/// 语义等价「带前缀的流」：`capture` 期间从 `inner` 读入的字节追加进 `buf`；
/// `capture` 关闭后按「先 `buf` 后 `inner`」的顺序继续供出。任何从本读取器
/// 消费过的字节都只被看到一次，且顺序稳定。
pub(crate) struct ReplayReader {
    inner: Box<dyn Read + Send>,
    buf: Vec<u8>,
    pos: usize,
    capturing: bool,
}

impl ReplayReader {
    /// 包装底层流（初始非捕获态：直接透传）。
    pub(crate) fn new(inner: Box<dyn Read + Send>) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            pos: 0,
            capturing: false,
        }
    }

    /// 打开捕获态：此后 `read` 读到的底层字节会追加进预读缓冲（供解码重放）。
    pub(crate) fn start_capture(&mut self) {
        self.capturing = true;
    }

    /// 关闭捕获态（预读阶段结束；已捕获字节在后续读取时重放）。
    pub(crate) fn stop_capture(&mut self) {
        self.capturing = false;
    }
}

impl Read for ReplayReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        // 先供出预读缓冲中尚未重放的字节
        if self.pos < self.buf.len() {
            let n = std::cmp::min(out.len(), self.buf.len() - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        // 缓冲已放空；捕获态下新读入的字节记录进 buf 供后续重放
        if self.capturing {
            let n = self.inner.read(out)?;
            if n > 0 {
                self.buf.extend_from_slice(&out[..n]);
            }
            return Ok(n);
        }
        self.inner.read(out)
    }
}

#[cfg(test)]
#[path = "tests/streams.rs"]
mod tests;
