//! GNU.sparse 区间提取 + 稀疏落盘（02 §8.4「按映射稀疏写文件」、T22 §3.3/§4.7）。
//!
//! 两种来源（T22 §3.3 mermaid）：
//!
//! - **GNU 旧式 `S` 头**：区间表在头内（首 4 项 + 扩展头）。tar crate 完整解析并
//!   把洞作为 `io::repeat(0)` 段内联进数据流——本层 **dense 直写即还原逻辑内容**，
//!   不另做区间（Windows 天然稀疏只作为优化，不影响正确性）。
//! - **GNU 1.0 / pax `GNU.sparse.offset/numbytes`**：区间表在 `x` pax 头；tar crate
//!   不处理此变体，数据流仅含区间内字节（无洞零）。由 [`crate::variant`] 兜底
//!   累积区间，经 [`write_sparse_pax`] 显式稀疏落盘。
//!
//! 落盘（[`write_sparse_pax`]）：逐区间 `seek(offset) → write`（区间外不写=洞）；
//! Windows 先 [`hzip_platform::fs_util::set_sparse`] 标记，失败降级密集写 +
//! tracing warning（字节正确性不变，T22 §7-4 不静默降级）。最后 `truncate` 到
//! 逻辑大小。

use std::io::{Read, Seek, SeekFrom, Write};

use hzip_error::{FormatErrorKind, HzipError};

use crate::variant;

/// 区间表合法性校验（T22 §3.3 `size 校验`）：升序、不重叠、末偏移 ≤ 逻辑大小。
/// 不符 → `Err Format{BadHeader}`（2004）。
pub(crate) fn validate(
    extents: &[(u64, u64)],
    logical_size: u64,
    format: &hzip_format::FormatId,
) -> Result<(), HzipError> {
    let mut prev_end = 0u64;
    for &(off, len) in extents {
        if len == 0 {
            continue;
        }
        if off < prev_end {
            return Err(bad_header(format, "稀疏区间重叠或乱序"));
        }
        let end = off
            .checked_add(len)
            .ok_or_else(|| bad_header(format, "稀疏区间溢出"))?;
        if end > logical_size {
            return Err(bad_header(format, "稀疏区间越出逻辑大小"));
        }
        prev_end = end;
    }
    Ok(())
}

fn bad_header(format: &hzip_format::FormatId, why: &str) -> HzipError {
    tracing::warn!(%why, "稀疏区间不合法");
    HzipError::Format {
        format: format.clone(),
        kind: FormatErrorKind::BadHeader,
    }
}

/// 旧式 `S` 头稀疏条目：tar crate 数据流已含洞零 → 普通 dense 写即可还原逻辑
/// 内容（reader 的 extract/read_entry 走 `entry_reader`）。本函数保留供单测
/// 直接校验区间语义（`dense_for_legacy_sparse` 对应 02 §3.3 的 E→F→G dense 分支）。
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn dense_for_legacy_sparse(
    extents: &[(u64, u64)],
    logical_size: u64,
    format: &hzip_format::FormatId,
) -> Result<(), HzipError> {
    validate(extents, logical_size, format)
}

/// GNU 1.0 / pax 稀疏写：数据流内区间字节紧密排列（无洞零）。
///
/// 逐区间读入并按 `offset` 落位写入；洞区成为物理洞（Windows 需先标记稀疏）。
/// 收尾 `truncate` 到逻辑大小。Windows `set_sparse` 失败 → warning + 密集写降级
/// （字节正确性不变，T22 §7-4）。
pub(crate) fn write_sparse_pax(
    file: &mut std::fs::File,
    extents: &[(u64, u64)],
    logical_size: u64,
    mut data: impl Read,
    format: &hzip_format::FormatId,
) -> Result<(), HzipError> {
    validate(extents, logical_size, format)?;

    #[cfg(windows)]
    {
        match hzip_platform::fs_util::set_sparse(file) {
            Ok(true) => {}
            Ok(false) => tracing::warn!("set_sparse 不支持（降级密集写）"),
            Err(e) => tracing::warn!(err = ?e, "set_sparse 失败（降级密集写）"),
        }
    }

    let mut buf = vec![0u8; 64 * 1024];
    for &(off, len) in extents {
        if len == 0 {
            continue;
        }
        file.seek(SeekFrom::Start(off))
            .map_err(|e| variant::map_err(e, format.clone()))?;
        let mut left = len;
        while left > 0 {
            let want = usize::try_from(left.min(buf.len() as u64))
                .map_err(|_| bad_header(format, "稀疏区间过长"))?;
            data.read_exact(&mut buf[..want])
                .map_err(|e| variant::map_err(e, format.clone()))?;
            file.write_all(&buf[..want])
                .map_err(|e| variant::map_err(e, format.clone()))?;
            left -= want as u64;
        }
    }
    // 数据流不应有区间外多余字节
    let mut extra = [0u8; 1];
    match data.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => return Err(bad_header(format, "稀疏数据流长度超区间和")),
        Err(e) => return Err(variant::map_err(e, format.clone())),
    }
    // 收尾：文件长度 = 逻辑大小
    file.set_len(logical_size)
        .map_err(|e| variant::map_err(e, format.clone()))?;
    Ok(())
}

#[cfg(test)]
#[path = "tests/sparse.rs"]
mod tests;
