//! 流 → 临时文件（**沿用仓库既有 spool 约定**）。
//!
//! # 为什么要 spool
//!
//! `hzip-format` 的契约里**没有定位抽象**：`Source::Stream` 是一次性、前向的
//! `Box<dyn Read + Send>`（见 `crates/hzip-format/src/handler.rs` 的 `Source`）。
//! 而镜像容器（qcow2 的 L1/L2 查表、vmdk 的 grain 目录、RAW 的分区探测与文件系统
//! 驱动）必须随机访问。既有三个 reader（zip / cpio / tar）的解法都是**自己 spool
//! 到临时文件**再随机访问——本模块就是卷层这一份，与它们同一约定。
//!
//! # 两条必须守住的约定
//!
//! 1. **文件名前缀 `hzip-spool-`**：进程被强杀会留下孤儿缓冲，
//!    `hzip-core::services::space::sweep_stale_spools()` 在启动时按该前缀回收超龄
//!    孤儿（该模块文档记着历史上 29.6 GiB 孤儿把系统盘挤爆的实测）。用别的前缀 =
//!    孤儿永远不被回收。
//! 2. **写盘错误要带上下文**：磁盘满（`HZ-1002`）在「打开」阶段就必须能被识别成
//!    「临时盘不够」而不是「档案损坏」——错误里因此带上临时目录路径。
//!
//! # 与配额的关系
//!
//! spool 只受**磁盘可用空间**与 [`SpoolPolicy::max_bytes`]（可选）约束，
//! **不**受 `QuotaLimits::MaxFileBytes`（32 GiB）约束——否则 40 GB 的镜像根本打不开。
//! 配额管的是「解出来的东西」，不是「为了读而缓冲的一份」。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use hzip_progress::CancellationToken;

use crate::SpoolPolicy;
use crate::error::{VolError, VolResult};

/// 孤儿回收识别前缀（与 `hzip-core::services::space::SPOOL_PREFIX` **同值**）。
pub const SPOOL_PREFIX: &str = "hzip-spool-";

/// spool 读缓冲大小（1 MiB：与仓库其它桥接循环一致）。
const CHUNK: usize = 1 << 20;

/// 已落盘的 spool 文件；`Drop` 即删除（崩溃残留由启动期孤儿回收兜底）。
#[derive(Debug)]
pub struct SpoolFile {
    file: tempfile::NamedTempFile,
    path: PathBuf,
    bytes: u64,
}

impl SpoolFile {
    /// 落盘路径（交给 [`crate::device::Chain`] 打开为块设备）。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 字节数（= 图像长度）。
    #[must_use]
    pub fn len(&self) -> u64 {
        self.bytes
    }

    /// 空 spool（零字节源；正常不该出现，但语义要明确）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }
}

/// 把 `src` 全部读进临时文件；`cancel` 命中即返回 [`VolError::Cancelled`]（临时文件随
/// `SpoolFile` 的丢弃而删除）。
pub fn spool_from_stream(
    src: &mut dyn Read,
    policy: &SpoolPolicy,
    cancel: Option<&CancellationToken>,
) -> VolResult<SpoolFile> {
    let mut file = tempfile::Builder::new()
        .prefix(policy.prefix)
        .tempfile_in(&policy.dir)
        .map_err(|e| {
            // 磁盘/权限问题在这里就会暴露：带上临时目录，让上层能说「临时盘不够」。
            tracing::warn!(dir = %policy.dir.display(), "spool 创建失败：{e}");
            VolError::Io(e)
        })?;
    let mut buf = vec![0u8; CHUNK];
    let mut bytes = 0u64;
    loop {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(VolError::Cancelled);
        }
        let n = src.read(&mut buf).map_err(VolError::Io)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if policy.max_bytes.is_some_and(|max| bytes > max) {
            return Err(VolError::Quota("vol.err.spool-limit"));
        }
        file.write_all(&buf[..n]).map_err(|e| {
            tracing::warn!(dir = %policy.dir.display(), "spool 写入失败（磁盘空间？）：{e}");
            VolError::Io(e)
        })?;
    }
    file.flush().map_err(VolError::Io)?;
    let path = file.path().to_path_buf();
    Ok(SpoolFile { file, path, bytes })
}

impl SpoolFile {
    /// 保活句柄（调试用；生产路径只需 [`Self::path`]）。
    #[must_use]
    pub fn handle(&self) -> &std::fs::File {
        self.file.as_file()
    }
}
