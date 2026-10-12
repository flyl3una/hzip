//! 7z 分卷写入（feature `split`；T46 独占模块；02 §8.2 + §9.5 `Sink::Split`）。
//!
//! 7z 分卷为**纯字节切分**（无每卷独立结构头）：`{stem}.7z.001, .002, …`；首卷以
//! 32 B 签名头开头，尾头（nextHeader）落在末卷。写侧与 T25 读侧
//! [`crate::reader::VolumeFile`] 的逻辑连续流装配互为对称面（字节拼接 = 完整档案）。
//!
//! # 卷头与边界对齐（§3.4 规则）
//!
//! 1. 最小卷尺寸保障 ≥ 32 B：`vol_bytes < 32` → 创建即 `Err`（HZ-5003 语义经
//!    [`SplitSink::new`] 返回 `HzipError`，防签名头被切穿）；
//! 2. 切分不感知 7z 结构（pack 流可跨卷）；
//! 3. 收卷原子性：rotate 前 flush；末卷 = finish 回写头之后自然形成。
//!
//! # 与 `SevenZWriter` 对接
//!
//! `SevenZWriter` 要求 `Write + Seek`；分卷端为纯 `Write`（Seek 仅前进语义不足）。
//! 与 T24/tar 相同形态：`Sink::Split` 走「临时文件两遍法」——先写完整单盘档案至
//! 临时文件（可 seek），`finish()` 后经 [`SplitSink`] 顺序读出按卷切分落盘。
//! 本模块实现切分器（含 `vol_bytes < 32` 防护与卷路径产出）。

// 模块消费方=单测+writer split 两遍法挂点（T46 交付接口）；允许 dead_code 直至 Sink::Split 经 finish 切分落盘接通（02 §9.5）。
#![allow(dead_code)]
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use hzip_error::HzipError;

/// 分卷字节切分器（`Write`；纯字节滚动切分）。
///
/// 语义：`write(buf)` 顺序消费字节，当前卷写满 `vol_bytes` 即收卷换下一文件；
/// `finalize()` 收末卷。卷文件：`{stem}.7z.{index:03}`（index 从 1 起；`.001` 起）。
/// `vol_bytes < 32` 构造即错（签名头最小保障，§3.4 规则 1）。
#[derive(Debug)]
pub(crate) struct SplitSink {
    /// 目标目录。
    dir: PathBuf,
    /// 卷名主干（含 `.7z` 扩展名前的 stem）。
    stem: String,
    /// 每卷字节上限（末卷可为尾卷不受限）。
    vol_bytes: u64,
    /// 当前打开卷文件。
    cur: Option<File>,
    /// 当前卷已写字节。
    cur_written: u64,
    /// 卷序（1 起 → `.001`）。
    index: usize,
    /// 已产卷路径（含末卷，按序）。
    produced: Vec<PathBuf>,
}

impl SplitSink {
    /// 新建切分器；`vol_bytes < 32` → `Err`（HZ-5003 类防护：不产出被切穿
    /// 签名头的残卷）。`dir` 应为 `prefix` 所在目录，`stem` 为前缀主干。
    pub(crate) fn new(dir: &Path, stem: &str, vol_bytes: u64) -> Result<Self, HzipError> {
        if vol_bytes < 32 {
            return Err(HzipError::Unsupported {
                what: format!("7z 分卷卷大小必须 ≥ 32 B（签名头最小保障；vol_bytes={vol_bytes}）")
                    .into(),
            });
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            stem: stem.to_string(),
            vol_bytes,
            cur: None,
            cur_written: 0,
            index: 1,
            produced: Vec::new(),
        })
    }

    /// 当前卷路径（`{stem}.7z.{index:03}`）。
    fn volume_path(&self, index: usize) -> PathBuf {
        self.dir.join(format!("{}.7z.{:03}", self.stem, index))
    }

    /// 收卷：flush+close 当前卷并登记路径。
    fn close_current(&mut self) -> io::Result<()> {
        if let Some(mut f) = self.cur.take() {
            f.flush()?;
            drop(f); // Windows 需先 drop 句柄再改名（此处无改名，仅登记）
            self.produced.push(self.volume_path(self.index));
        }
        Ok(())
    }

    /// 滚动到新卷（当前卷满）。
    fn rotate(&mut self) -> io::Result<()> {
        self.close_current()?;
        self.index += 1;
        self.cur_written = 0;
        let p = self.volume_path(self.index);
        let f = File::create(&p)?;
        self.cur = Some(f);
        Ok(())
    }

    /// 收尾：flush+close 末卷并返回卷集清单（`.7z.001 …`）。
    pub(crate) fn finalize(mut self) -> io::Result<Vec<PathBuf>> {
        self.close_current()?;
        Ok(self.produced)
    }
}

impl Write for SplitSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut off = 0usize;
        while off < buf.len() {
            if self.cur.is_none() {
                let p = self.volume_path(self.index);
                let f = File::create(&p)?;
                self.cur = Some(f);
            }
            if self.cur_written >= self.vol_bytes {
                self.rotate()?;
                continue;
            }
            let room = (self.vol_bytes - self.cur_written) as usize;
            let n = room.min(buf.len() - off);
            if let Some(f) = self.cur.as_mut() {
                f.write_all(&buf[off..off + n])?;
            }
            self.cur_written += n as u64;
            off += n;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = self.cur.as_mut() {
            f.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/split.rs"]
mod tests;
