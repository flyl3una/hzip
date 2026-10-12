//! 分卷字节切分（02 §8.1 写入路径要点 + §9.5 `Sink::Split`；T24 §4.8）。
//!
//! 写入端为**纯字节流切分，不感知 zip 结构**：卷满即收卷换文件；中央目录可跨卷
//! （EOCD 允许落在最后一卷内任意偏移）。命名沿用 Info-ZIP / 7-Zip 分卷约定：
//! `{stem}.z01, {stem}.z02, …, {stem}.zip`（`.zip` 为最后一卷，含中央目录）。
//! 卷数 > 99 时扩展为 `.zNNN` 三位（Info-ZIP 兼容，T24 §4.8）。
//!
//! # 实现形态（与 T24 §4.8 图的差异声明）
//!
//! 任务卡 §4.8 为 `SplitSink` 绘制了 `Write + Seek`（Seek 仅前进）——但 zip 写入器
//! 在 `finish_file` 阶段需**回退 seek** 更新本地头 CRC/尺寸（`update_local_file_header`
//! seek 到 `header_start`），纯流切分端无法承载回退。故本 crate 的 `Sink::Split`
//! 采用「临时文件两遍法」形态：`ZipWriter` 先写完整单盘 zip 到临时文件（可回退
//! seek），`finish()` 后经 [`Splitter`] 顺序读出、按卷字节切分落盘（`split.rs` 只
//! 提供纯 `Write` 切分器，无 Seek——T24 §4.8 的 Seek 语义由临时文件承载，
//! rustdoc 注明性能与语义：`caps.split` 对外不变，两遍法的临时文件在 drop 清理）。
//!
//! 读侧：[`VolumeSetReader`] 把分卷文件按序装配为逻辑连续流（zip crate 视角为单盘
//! 档案）。hzip 自产分卷为「单盘 zip 字节切分」语义，EOCD 磁盘字段不做改写（T24
//! §7-4：外部工具交叉兼容归 TC-ALG-015/TC-X-005，发现需改写则挂跟踪表决策）。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 分卷切分器（`Write`；字节级滚动切分）。
///
/// 语义：`write(buf)` 顺序消费字节，当前卷写满 [`Self::bytes_per_volume`] 字节即
/// 收卷换下一文件；`finalize()` 收最后一卷并把末卷原子重命名为最终 `.zip`。
/// 卷文件：`{stem}.z{NN:02}`（`NN` 从 1 起；`NN > 99` 时三位宽）。
pub(crate) struct Splitter {
    /// 目标目录
    dir: PathBuf,
    /// 卷名主干（不含 `.zNN` 尾缀；末卷重命名目标为 `{dir}/{stem}`）
    stem: String,
    /// 最终 `.zip` 完整路径（prefix 原样，含扩展名）
    final_path: PathBuf,
    /// 每卷字节上限（最后卷可为尾卷不受限，可不足）
    bytes_per_volume: u64,
    /// 当前打开卷文件
    cur: Option<File>,
    /// 当前卷已写字节
    cur_written: u64,
    /// 卷序（1 起 → `.z01`）
    index: usize,
    /// 已产卷路径（按序，不含末卷重命名后的 final）
    produced: Vec<PathBuf>,
}

impl Splitter {
    /// 新建切分器（写往 `final_path` 同目录，卷前缀为其 stem）。
    pub(crate) fn new(final_path: PathBuf, bytes_per_volume: u64) -> io::Result<Self> {
        let dir = final_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let stem = final_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".into());
        Ok(Self {
            dir,
            stem,
            final_path,
            bytes_per_volume: bytes_per_volume.max(1),
            cur: None,
            cur_written: 0,
            index: 1,
            produced: Vec::new(),
        })
    }

    /// 当前卷路径（`.zNN` 命名）。
    fn volume_path(&self, index: usize) -> PathBuf {
        let width = if index > 99 { 3 } else { 2 };
        self.dir
            .join(format!("{}.z{:0width$}", self.stem, index, width = width))
    }

    /// 收卷：flush+close 当前卷并登记路径。
    fn close_current(&mut self) -> io::Result<()> {
        if let Some(mut f) = self.cur.take() {
            f.flush()?;
            // Windows 需先 drop 句柄才能重命名；此处返回时已 take，函数结束 drop。
            self.produced.push(self.volume_path(self.index));
        }
        Ok(())
    }

    /// 滚动到新卷（当前卷满或首卷）。
    fn rotate(&mut self) -> io::Result<()> {
        self.close_current()?;
        self.index += 1;
        self.cur_written = 0;
        let p = self.volume_path(self.index);
        let f = File::create(&p)?;
        self.cur = Some(f);
        Ok(())
    }

    /// 收尾：关末卷并原子重命名为最终 `.zip`（最后一卷含中央目录与 EOCD）。
    ///
    /// 返回卷集清单（读侧按序拼接顺序：`.z01 … .zNN + .zip`）。单卷产物
    /// （未发生 rotate）同样经重命名得到不含 `.zNN` 的单文件。
    pub(crate) fn finalize(mut self) -> io::Result<Vec<PathBuf>> {
        let mut volumes = std::mem::take(&mut self.produced);
        let last_volume_path = if let Some(mut f) = self.cur.take() {
            f.flush()?;
            drop(f);
            self.volume_path(self.index)
        } else {
            self.volume_path(self.index)
        };
        if last_volume_path.exists() {
            std::fs::rename(&last_volume_path, &self.final_path).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "分卷末卷重命名失败 {} -> {}: {e}",
                        last_volume_path.display(),
                        self.final_path.display()
                    ),
                )
            })?;
        } else {
            File::create(&self.final_path)?;
        }
        volumes.push(self.final_path.clone());
        Ok(volumes)
    }
}

impl Write for Splitter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut off = 0usize;
        while off < buf.len() {
            if self.cur.is_none() {
                let p = self.volume_path(self.index);
                let f = File::create(&p)?;
                self.cur = Some(f);
            }
            if self.cur_written >= self.bytes_per_volume {
                self.rotate()?;
                continue;
            }
            let room = (self.bytes_per_volume - self.cur_written) as usize;
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

/// 分卷逻辑连续流装配（`Source::Volumes` → 读侧）。
///
/// 把卷文件按序视为一个连续的 `Read + Seek`（zip crate 视角单盘档案）；缺失卷在
/// 构造时校验 → `Err`（HZ-2005 归调用方契约；此处以 io 错误承载）。`Seek` 在全
/// 卷集范围内定位（跨卷 seek 顺序快进）。
pub(crate) struct VolumeSetReader {
    paths: Vec<PathBuf>,
    /// 当前已打开卷下标
    cur_idx: usize,
    /// 当前卷文件
    cur: File,
    /// 各卷起点在逻辑流中的偏移
    bounds: Vec<u64>,
    total: u64,
    /// 逻辑流当前位置
    pos: u64,
}

impl VolumeSetReader {
    /// 依序打开卷文件并校验存在（缺失 → `io::ErrorKind::NotFound` 语义上抛）。
    pub(crate) fn open(paths: Vec<PathBuf>) -> io::Result<Self> {
        if paths.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "空卷集"));
        }
        let mut bounds = Vec::with_capacity(paths.len());
        let mut total = 0u64;
        let mut cur: Option<File> = None;
        for (i, p) in paths.iter().enumerate() {
            bounds.push(total);
            let f = File::open(p).map_err(|e| {
                io::Error::new(e.kind(), format!("分卷缺失/不可读: {}", p.display()))
            })?;
            total += f.metadata()?.len();
            if i == 0 {
                cur = Some(f);
            }
        }
        let cur = cur.ok_or_else(|| io::Error::other("卷集无首卷"))?;
        Ok(Self {
            paths,
            cur_idx: 0,
            cur,
            bounds,
            total,
            pos: 0,
        })
    }

    #[cfg(test)]
    fn total_len(&self) -> u64 {
        self.total
    }

    /// 定位逻辑偏移：切换当前卷（顺序前进）。
    fn seek_to(&mut self, target: u64) -> io::Result<()> {
        // 找 target 所在卷：bounds 单调递增，末卷界外归末卷
        let mut idx = self.bounds.len() - 1;
        for (i, &b) in self.bounds.iter().enumerate() {
            if target < b {
                idx = i.saturating_sub(1);
                break;
            }
        }
        if idx != self.cur_idx {
            self.cur = File::open(&self.paths[idx])?;
            self.cur_idx = idx;
        }
        self.cur.seek(SeekFrom::Start(target - self.bounds[idx]))?;
        self.pos = target;
        Ok(())
    }

    fn read_more(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // 尝试当前卷内读取；到卷尾则切下一卷
        let n = self.cur.read(buf)?;
        if n > 0 {
            return Ok(n);
        }
        if self.cur_idx + 1 < self.paths.len() {
            self.cur_idx += 1;
            self.cur = File::open(&self.paths[self.cur_idx])?;
            let r = self.cur.read(buf)?;
            Ok(r)
        } else {
            Ok(0)
        }
    }
}

impl Read for VolumeSetReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total {
            return Ok(0);
        }
        let n = self.read_more(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for VolumeSetReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let base = match pos {
            SeekFrom::Start(o) => o,
            SeekFrom::End(o) => self
                .total
                .checked_add_signed(o)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek 越界"))?,
            SeekFrom::Current(o) => self
                .pos
                .checked_add_signed(o)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek 越界"))?,
        };
        let base = base.min(self.total);
        self.seek_to(base)?;
        Ok(self.pos)
    }
}

#[cfg(test)]
#[path = "tests/split.rs"]
mod tests;
