//! 写入契约（02 §7.3/§6.2；契约冻结）。
//!
//! [`EntrySink`] 为 fmt 实现侧共享的单条目写入句柄：CRC 增量累计、取消检查点、
//! 进度 `Bytes` 事件、drop 保底收尾。

use std::io::{self, Write};
use std::path::PathBuf;

use hzip_error::HzipError;
use hzip_progress::{CancellationToken, ProgressBus};

use crate::CreateOptions;
use crate::password::PasswordRef;
use crate::types::{EntryInfo, EntryMeta};

/// 写入上下文（02 §7.3；rewrite 的执行参数）。
pub struct WriteCtx<'a> {
    /// 取消令牌
    pub cancel: &'a CancellationToken,
    /// 进度总线
    pub progress: &'a ProgressBus,
    /// 写入选项（重压缩级别/线程/加密）
    pub opts: &'a CreateOptions,
}

/// 写入报告（`volume_paths`：分卷产物清单，`caps.split`）。数据类。
#[derive(Debug, Default)]
pub struct WriteReport {
    /// 写入条目数
    pub entries_written: usize,
    /// 输入字节
    pub bytes_in: u64,
    /// 输出字节
    pub bytes_out: u64,
    /// 分卷产物路径
    pub volume_paths: Vec<PathBuf>,
}

/// 档案写入器契约（02 §7.3 冻结签名）。
pub trait ArchiveWriter: Send {
    /// 声明下一条目元数据并取得写入句柄（`caps` 不满足的请求 →
    /// `Err Unsupported`，如加密/追加）
    fn next_entry(
        &mut self,
        meta: &EntryMeta,
        pw: PasswordRef<'_>,
    ) -> Result<EntrySink<'_>, HzipError>;
    /// 收尾（写尾块/中央目录；分卷产物路径入报告）
    fn finish(self: Box<Self>) -> Result<WriteReport, HzipError>;
}

/// fmt 实现侧的单条目收尾钩子（契约支撑类型，同等冻结）。
pub trait EntrySinkBackend: Write + Send {
    /// 条目数据写毕时调用恰好一次：写数据描述符/尾块/CRC 汇总，
    /// 校验预声明 size（不符 → `Err Format{BadHeader}`）。
    fn finish_entry(&mut self) -> Result<(), HzipError>;
}

pub use crate::consts::COPY_CHUNK_BYTES;

/// 单条目写入句柄（fmt 实现构造入口见 [`EntrySink::from_backend`]）。
pub struct EntrySink<'a> {
    backend: Box<dyn EntrySinkBackend + 'a>,
    meta: &'a EntryMeta,
    cancel: &'a CancellationToken,
    progress: &'a ProgressBus,
    bytes: u64,
    crc: hzip_codec::crc::Crc32,
    finished: bool,
}

impl<'a> EntrySink<'a> {
    /// fmt 实现构造入口：以 backend 包装单条目写入（02 §7.3）。
    pub fn from_backend(
        backend: Box<dyn EntrySinkBackend + 'a>,
        meta: &'a EntryMeta,
        cancel: &'a CancellationToken,
        progress: &'a ProgressBus,
    ) -> Self {
        Self {
            backend,
            meta,
            cancel,
            progress,
            bytes: 0,
            crc: hzip_codec::crc::Crc32::new(),
            finished: false,
        }
    }

    /// 显式收尾（推荐）：调用 backend `finish_entry` 恰一次。
    /// 预声明 size 与实际不符 → `Err Format{BadHeader}`。
    pub fn finish(mut self) -> Result<(), HzipError> {
        self.finish_inner()
    }

    /// 已写字节数。
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// 当前 CRC32（T12 codec 流式增量）。
    pub fn crc32(&self) -> u32 {
        self.crc.finalize()
    }

    fn finish_inner(&mut self) -> Result<(), HzipError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if let Some(declared) = self.meta.size_raw
            && self.bytes != declared
        {
            return Err(HzipError::Format {
                format: crate::FormatId::from("?"),
                kind: hzip_error::FormatErrorKind::BadHeader,
            });
        }
        self.backend.finish_entry()
    }
}

impl Drop for EntrySink<'_> {
    /// drop 为保底收尾：错误经 tracing 记录不上抛（drop 中不得 panic，红线 4）。
    fn drop(&mut self) {
        if !self.finished
            && let Err(e) = self.backend.finish_entry()
        {
            tracing::warn!(err = ?e, "EntrySink::drop 保底收尾失败");
        }
    }
}

impl Write for EntrySink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // 取消检查点（每块一次，02 §4.2 <5ns）。
        // 注意：不能返回 ErrorKind::Interrupted——`Write::write_all` 会对
        // Interrupted 自动重试，取消将陷入死循环；以 Other 表达「中止」。
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let n = self.backend.write(buf)?;
        if n > 0 {
            self.bytes += n as u64;
            self.crc.update(&buf[..n]);
            self.progress
                .send(crate::ProgressEvent::Bytes { delta: n as u64 });
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.backend.flush()
    }
}

/// 供 fmt 实现判断 `entries()` 类型形状的辅助：条目列表与写入报告同构。
pub type EntryList = Vec<EntryInfo>;

#[cfg(test)]
#[path = "tests/writer.rs"]
mod tests;
