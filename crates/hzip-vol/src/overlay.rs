//! 覆盖设备：**读穿透 + 写暂存**，提交前原设备一个字节都不动。
//!
//! # 为什么需要它（写模式 B 的全部实现）
//!
//! 用户裁定 ②：改动默认先暂存，提交时才写回原文件。做法是在真设备**下面**垫一层
//! 覆盖设备（`FileDevice → OverlayDevice → Qcow2Reader/…`），于是：
//!
//! - 上游（镜像容器与文件系统驱动）看到的仍是一个可随机读写的块设备，代码零改动；
//! - 所有写落在溢出文件（同长度的**稀疏**临时文件）并记脏块；
//! - 提交 = 只把脏块回放到真文件；取消/崩溃 = 丢弃溢出文件，原文件字节不变。
//!
//! # 两个刻意的设计
//!
//! | 决策 | 理由 |
//! |---|---|
//! | 读路径**不做写时拷贝**：脏块从溢出文件读，未脏块直接穿透到 base | 少一次整块抄写；且「哪些块脏」这一份状态就是回放的依据，不需要第二份 |
//! | 块粒度 64 KiB | ≥ qcow2 默认簇与 VMDK grain 上限 ⇒ 回放不产生碎片；同时把脏块表的规模压在「改动量 / 64 KiB」 |
//!
//! # 已定的边界
//!
//! - **不改容量**：`set_len` 恒错（本期不做镜像 resize）；
//! - **回放幂等**：同一覆盖层反复 `replay_into` 结果一致（core 的密码重试会重复提交）；
//! - **回放只碰脏块**：目标设备的其它字节不受影响（有专测）。

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Mutex;

use fs_core::BlockRead as _;
use hzip_progress::CancellationToken;

use crate::error::{VolError, VolResult};

/// 覆盖层的块粒度（64 KiB；见模块文档）。
pub const OVERLAY_BLOCK: u64 = 64 * 1024;

/// 覆盖设备：`base` 只读打开，写入落 `spill`（稀疏临时文件）并记脏。
pub struct OverlayDevice {
    base: std::sync::Arc<dyn fs_core::BlockRead>,
    spill: std::fs::File,
    dirty: Mutex<BTreeSet<u64>>,
    block: u64,
}

impl std::fmt::Debug for OverlayDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayDevice")
            .field("size", &self.size_bytes())
            .field("block", &self.block)
            .field("dirty_blocks", &self.dirty_blocks())
            .finish()
    }
}

impl OverlayDevice {
    /// 在 `base` 之上建覆盖层；`spill` 是承载改动的文件（调用方负责生命周期与清理）。
    #[must_use]
    pub fn new(
        base: std::sync::Arc<dyn fs_core::BlockRead>,
        spill: std::fs::File,
        block: u64,
    ) -> Self {
        debug_assert!(block > 0 && block.is_power_of_two(), "块粒度应为 2 的幂");
        Self {
            base,
            spill,
            dirty: Mutex::new(BTreeSet::new()),
            block,
        }
    }

    /// 脏块数（进度与日志用）。
    #[must_use]
    pub fn dirty_blocks(&self) -> u64 {
        self.locked().len() as u64
    }

    /// 脏字节数（= 脏块数 × 块粒度；上界，实际改动可能更少）。
    #[must_use]
    pub fn dirty_bytes(&self) -> u64 {
        self.dirty_blocks() * self.block
    }

    /// 只把脏块回放到 `target`；**幂等**（反复调用结果一致）。
    ///
    /// `cancel` 每块检查一次：命中即返回 [`VolError::Cancelled`]，已回放的块留在
    /// 目标上（调用方据此如实报告「部分提交」——我们不假装回滚得掉）。
    pub fn replay_into(
        &self,
        target: &dyn fs_core::BlockDevice,
        cancel: Option<&CancellationToken>,
    ) -> VolResult<u64> {
        let blocks: Vec<u64> = self.locked().iter().copied().collect();
        let mut buf = vec![0u8; self.block.min(self.size_bytes()) as usize];
        let mut done = 0u64;
        for b in blocks {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(VolError::Cancelled);
            }
            let off = b * self.block;
            let len = self.block.min(self.size_bytes().saturating_sub(off)) as usize;
            if len == 0 {
                continue;
            }
            // 读穿透：脏块从溢出文件取（这正是本地读路径的语义）
            fs_core::BlockRead::read_at(self, off, &mut buf[..len])?;
            target.write_at(off, &buf[..len])?;
            done += 1;
        }
        target.flush()?;
        Ok(done)
    }

    /// 整流量拷（源是流 / 需重写外层时用）：把「当前视图」逐块写进 `w`。
    ///
    /// 与 [`Self::replay_into`] 的差别：目标是一条**只能前向写**的流，故没有
    /// 「只写脏块」的可能——这也是组合链（`.gz` 包裹）提交必然全量重压的根源。
    pub fn stream_out(
        &self,
        w: &mut dyn Write,
        cancel: Option<&CancellationToken>,
    ) -> VolResult<u64> {
        let total = self.size_bytes();
        let mut buf = vec![0u8; self.block.min(total) as usize];
        let mut off = 0u64;
        while off < total {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(VolError::Cancelled);
            }
            let len = self.block.min(total - off) as usize;
            fs_core::BlockRead::read_at(self, off, &mut buf[..len])?;
            w.write_all(&buf[..len])?;
            off += len as u64;
        }
        w.flush()?;
        Ok(off)
    }

    /// 丢弃全部改动（回到「源 + 空覆盖」）；core 的重试语义要求可重置。
    pub fn reset(&self) -> VolResult<()> {
        self.locked().clear();
        // 溢出文件不必清零：脏块表决定哪些块可读，未标记的块永远穿透到 base。
        Ok(())
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, BTreeSet<u64>> {
        // 中毒只可能来自本模块自身的 panic（持锁期间不跑用户回调）⇒ 恢复数据比炸掉好。
        self.dirty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 把 `offset..offset+buf.len()` 涉及的所有块标脏，并把字节写进溢出文件。
    ///
    /// # 为什么要拷贝未触及的区间（写时拷贝）
    ///
    /// 块粒度（64 KiB）远大于典型写入（文件系统一次写 4 KiB）。若只把本次负载写进
    /// 溢出文件，那么「脏块」里没被写到的部分在溢出文件里是**洞**——后续任何整块读
    /// （文件系统驱动遍地都是）会读到零而不是 base 的原内容，直接把文件系统读坏。
    ///
    /// 故首次把一个块变脏时，先把该块内**不属于本次写**的区间从 base 抄进溢出文件
    /// （两端各一段），使溢出文件里的每个脏块都是**完整块像**。只抄未触及的区间而不是
    /// 整块，是为了让连续大写的放大为 0（两端区间为空 ⇒ 不产生额外 IO）。
    fn touch(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let size = self.size_bytes();
        let write_end = offset + buf.len() as u64;
        let first = offset / self.block;
        let last = (write_end - 1) / self.block;
        for b in first..=last {
            let blk_off = b * self.block;
            let blk_len = self.block.min(size.saturating_sub(blk_off));
            if blk_len == 0 {
                continue;
            }
            let newly = self.locked().insert(b);
            if !newly {
                continue; // 已经是脏块：块像早已完整，无需再抄
            }
            let blk_end = blk_off + blk_len;
            let w_start = offset.max(blk_off);
            let w_end = write_end.min(blk_end);
            // 前段 [blk_off, w_start) 与后段 [w_end, blk_end)：从 base 补齐
            for (from, to) in [(blk_off, w_start), (w_end, blk_end)] {
                if from >= to {
                    continue;
                }
                let mut tmp = vec![0u8; (to - from) as usize];
                self.base.read_at(from, &mut tmp)?;
                write_all_at(&self.spill, from, &tmp).map_err(fs_core::Error::Io)?;
            }
        }
        write_all_at(&self.spill, offset, buf).map_err(fs_core::Error::Io)
    }
}

impl fs_core::BlockRead for OverlayDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        let size = self.size_bytes();
        if offset
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > size)
        {
            return Err(fs_core::Error::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size,
            });
        }
        if buf.is_empty() {
            return Ok(());
        }
        // 按块分流：跨块的读可能一部分来自溢出文件、一部分穿透到 base。
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let block = pos / self.block;
            let block_end = (block + 1) * self.block;
            let take = ((block_end - pos) as usize).min(buf.len() - done);
            let is_dirty = self.locked().contains(&block);
            let dst = &mut buf[done..done + take];
            if is_dirty {
                read_exact_at(&self.spill, pos, dst)?;
            } else {
                self.base.read_at(pos, dst)?;
            }
            done += take;
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.base.size_bytes()
    }
}

impl fs_core::BlockDevice for OverlayDevice {
    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()> {
        let size = self.size_bytes();
        if offset
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > size)
        {
            return Err(fs_core::Error::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size,
            });
        }
        self.touch(offset, buf)
    }

    fn flush(&self) -> fs_core::Result<()> {
        self.spill.sync_all().map_err(fs_core::Error::Io)
    }

    fn is_writable(&self) -> bool {
        true
    }

    fn set_len(&self, _new_len: u64) -> fs_core::Result<()> {
        // 本期不做镜像容量变更：拒绝比「悄悄改了、读侧还不知道」安全。
        //
        // 代价（2026-10-16 vmdk 代理实测）：上游 `VmdkReader` 分配新 grain 靠加长宿主文件
        // ⇒ **需要新分配的写入**在覆盖写模式下会以本错误干净失败（`vol.unsupported.resize`），
        // 就地写正常。用户可切「就地写」或「另存为」。若要支持：溢出文件长大 + 逻辑长度
        // 跟着长，并在 `Chain::commit` 里对「目标比源短」先 `set_len`（独立一步，见 T-IMG6）。
        Err(fs_core::Error::Custom(
            "vol.unsupported.resize：卷容量不可变".to_string(),
        ))
    }

    fn can_grow(&self) -> bool {
        false
    }
}

/// 位置写（不移动文件游标）；`std::os::unix::fs::FileExt` / `windows::FileExt` 各一份。
#[cfg(unix)]
fn write_all_at(file: &std::fs::File, mut offset: u64, mut buf: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !buf.is_empty() {
        match file.write_at(buf, offset) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "写零字节",
                ));
            }
            Ok(n) => {
                buf = &buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &std::fs::File, mut offset: u64, mut buf: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, offset) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "写零字节",
                ));
            }
            Ok(n) => {
                buf = &buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &std::fs::File, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !buf.is_empty() {
        match file.read_at(buf, offset) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "溢出文件短读（块的其余部分是稀疏零）",
                ));
            }
            Ok(n) => {
                let tmp = buf;
                buf = &mut tmp[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &std::fs::File, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "溢出文件短读（块的其余部分是稀疏零）",
                ));
            }
            Ok(n) => {
                let tmp = buf;
                buf = &mut tmp[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
