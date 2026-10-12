//! 裸 tar 追加：尾零块截断法（02 §8.4 caps.append、T22 §4.6）。
//!
//! # 流程
//!
//! ```text
//! append(src_path, new_entries)：
//!   以 Read+Write 打开档案
//!   seek 到 EOF - 10240（尾扫描窗口，GNU tar 兼容）
//!   向前扫描定位「连续 ≥ TRAILER_BLOCKS(2) 个零块的首块」→ 截断点 truncate
//!   （10240 窗口内找不到合法结尾 → Err Format{BadHeader}：档案损坏或非裸 tar）
//!   追加新条目（writer 路径，§4.2）+ finish 写新尾零块
//! ```
//!
//! 边界：`Sink::Stream` / 组合链 → `Err Unsupported`（§3.4，压缩组合走 core 转换
//! 管线）；追加后旧条目字节不变（TC-CONF-005 断言：旧数据区零改动，仅截断
//! 旧尾块 + 新增记录 + 新尾块）。

use std::io::{Read, Seek, SeekFrom};

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::FormatId;

use crate::consts::{APPEND_TAIL_WINDOW, BLOCK_BYTES, TRAILER_BLOCKS};
use crate::variant;

/// 定位合法档案尾（连续 ≥ TRAILER_BLOCKS 全零块序列的首块），返回截断点。
///
/// - EOF < 512 → 空档/过短 → `Err Format{BadHeader}`（非裸 tar）。
/// - 扫描窗口 = `[max(0, eof-10240) 对齐到块, eof)`；窗口内找不到 → `Err 2004`。
pub(crate) fn find_truncate_point(
    file: &mut std::fs::File,
    format: &FormatId,
) -> Result<u64, HzipError> {
    let eof = file
        .seek(SeekFrom::End(0))
        .map_err(|e| variant::map_err(e, format.clone()))?;
    if eof < BLOCK_BYTES as u64 {
        return Err(HzipError::Format {
            format: format.clone(),
            kind: FormatErrorKind::BadHeader,
        });
    }
    // 窗口起点（块对齐）
    let win_start_off = eof.saturating_sub(APPEND_TAIL_WINDOW as u64);
    let win_start = win_start_off - (win_start_off % BLOCK_BYTES as u64);
    let win_len = (eof - win_start) as usize;
    let mut buf = vec![0u8; win_len];
    file.seek(SeekFrom::Start(win_start))
        .map_err(|e| variant::map_err(e, format.clone()))?;
    file.read_exact(&mut buf)
        .map_err(|e| variant::map_err(e, format.clone()))?;
    let nblocks = buf.len() / BLOCK_BYTES;
    let mut run = 0usize;
    for i in (0..nblocks).rev() {
        let blk = &buf[i * BLOCK_BYTES..(i + 1) * BLOCK_BYTES];
        if blk.iter().all(|&b| b == 0) {
            run += 1;
        } else if run >= TRAILER_BLOCKS {
            // i 是最后一个非零块：截断点 = win_start + (i+1)*512
            return Ok(win_start + (i + 1) as u64 * BLOCK_BYTES as u64);
        } else {
            run = 0;
        }
    }
    if run >= TRAILER_BLOCKS {
        // 整个窗口都是零块（超长尾零区）→ 截断到窗口起点（保守：重写该区）
        return Ok(win_start);
    }
    Err(HzipError::Format {
        format: format.clone(),
        kind: FormatErrorKind::BadHeader,
    })
}
