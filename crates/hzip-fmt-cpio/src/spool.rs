//! 临时文件助手（`Source::Stream` 缓冲；本 crate 自管，随持有者 drop 清理）。
//!
//! 组合链内层（`.cpio.gz` 等经 registry 装配后）给到容器 handler 的是**一次性
//! 流**：cpio 的索引必须能 seek 跳过数据区（红线 1「entries() 不读数据区」），
//! 故先整体缓冲为临时文件再按路径随机访问（与 tar 侧同策略，02 §7.5）。
//!
//! # 为什么文件名带前缀、错误要带目录（2026-09-24 用户实测）
//!
//! 用户打开一个 940 MB 的 `.cpio.gz`：等了十来秒、进度窗一闪，最后只看到
//! 「磁盘」类报错。根因是**临时盘写满**（ENOSPC）——缓冲区大小 = 解压后的
//! 内层容器（此处 896 MiB），而 `NamedTempFile` 的删除只在 drop 时执行，
//! 进程被强杀/崩溃会**留下孤儿**（实测某次测试中断留下 8 个、共 29.6 GiB）。
//! 因此：
//!
//! 1. 文件前缀 `hzip-spool-`——孤儿可被识别（跨 crate 同名，
//!    `hzip_core::services::space::sweep_stale_spools` 启动时回收超龄孤儿）；
//! 2. 写盘错误一律补上**临时目录路径**（[`spool_error`]）：`Display` 变
//!    「IO 错误: … (C:\…\Temp)」，错误码经 `io_code` 归 1002（磁盘空间不足，
//!    修复指引 FreeSpaceOrChangeTarget），不再是裸「os error 112」。
//!
//! 提前失败（不等整层解压完）由 core 的开档预检负责：组合链声明大小可知时
//! 先比可用空间（`hzip_core::services::space::precheck_open_temp_space`）。

use std::io::{self, Read, Write};

use hzip_error::HzipError;
use tempfile::NamedTempFile;

/// 临时文件前缀（孤儿识别/回收用，见模块文档）。
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
