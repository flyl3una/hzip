//! 临时文件助手（写路径两遍法 / Stream 缓冲；本 crate 自管，随持有者 drop 清理）。
//!
//! 02 §8.2「先落临时文件再拷出」精神的 tar 侧应用（T22 §4.5 末段）：流式未知
//! 大小条目先经 [`Spool`] 落临时文件取实际大小，再写 tar 头；`Source::Stream`
//! （组合链内层）缓冲为临时文件后可随机访问。

use std::io::{self, Read, Write};

use hzip_error::HzipError;
use tempfile::NamedTempFile;

/// 临时文件前缀（孤儿识别/回收用：`NamedTempFile` 只在 drop 时删，
/// 进程被强杀会留下孤儿——实测一次测试中断累积 29.6 GiB 临时盘占用，
/// 直接导致用户打开大 `.cpio.gz` 时 ENOSPC。`hzip_core::services::space::sweep_stale_spools`
/// 启动时回收超龄孤儿）。
pub(crate) const SPOOL_PREFIX: &str = "hzip-spool-";

/// 一次性临时文件（drop 即删；持有期可 `reopen`/`path` 复用）。
pub(crate) struct Spool {
    inner: NamedTempFile,
}

impl Spool {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            inner: NamedTempFile::with_prefix_in(SPOOL_PREFIX, std::env::temp_dir())?,
        })
    }

    /// 临时文件路径（读方以只读方式另开）。
    pub(crate) fn path(&self) -> &std::path::Path {
        self.inner.path()
    }

    /// 以只读方式重开（读回缓冲数据；写句柄仍在位）。
    pub(crate) fn reopen(&self) -> io::Result<std::fs::File> {
        self.inner.reopen()
    }
}

impl Write for Spool {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.as_file_mut().write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.as_file_mut().flush()
    }
}

/// 把流完整缓冲进临时文件并 flush（调用方以 [`Spool`] 持有保活）。
///
/// 错误统一经 [`spool_error`] 补上临时目录上下文（磁盘满 → HZ-1002）。
pub(crate) fn spool_stream(mut src: impl Read) -> Result<Spool, HzipError> {
    let dir = std::env::temp_dir();
    let mut sp = Spool::new().map_err(|e| spool_error(e, &dir))?;
    io::copy(&mut src, &mut sp).map_err(|e| spool_error(e, &dir))?;
    sp.flush().map_err(|e| spool_error(e, &dir))?;
    Ok(sp)
}

/// spool 期 IO 错误 → 带临时目录上下文的统一错误。
///
/// 临时的盘写满（ENOSPC / ERROR_DISK_FULL）经 `HzipError::code` 归 HZ-1002；
/// `path` 让 `Display` 指出**是哪个目录**满了（用户据此清理或改 `TEMP`）。
pub(crate) fn spool_error(e: io::Error, dir: &std::path::Path) -> HzipError {
    HzipError::Io {
        source: e,
        path: Some(dir.to_path_buf()),
    }
}
