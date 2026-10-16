//! `fatfs` 的设备适配：`Arc<dyn BlockDevice>` → 可读/可写/可定位的流。
//!
//! # 为什么需要这一层
//!
//! `fatfs` 只认 `Read + Write + Seek`（0.3.6 的 [`fatfs::ReadWriteSeek`] 就是这三者的
//! 空标记 trait + 一揽子 impl，故**实现三者即等于实现了它**）；而我们的原始设备是
//! [`hzip_vol::BlockDevice`]——按偏移的整块读写、容量构造时固定。本模块是二者之间
//! **唯一**的适配点：文件系统的其余代码一行都看不到设备接口。
//!
//! # 三条不变量
//!
//! | 不变量 | 做法 |
//! |---|---|
//! | 容量不可变 | 读写一律夹在 `[0, size_bytes]`；写越界 ⇒ `WriteZero`（不静默截断、不 panic）。`fatfs` 没有 `set_len`/`truncate` 入口，故「卷容量不可变」在接口层天然成立 |
//! | 读必须填满 | 先按设备容量夹取再调 `read_at`（`BlockRead` 的契约是「不可满足即 `Err`」），不把短读伪装成成功 |
//! | 位置从 0 起 | [`fatfs::FileSystem::new`] 有 `debug_assert!(disk.seek(Current(0)) == 0)`，故 [`BlockDeviceSeek::new`] 从 0 开始 |
//!
//! # 独占
//!
//! `fatfs` 假设设备是**独占**的（同一设备上挂两个 `FileSystem` 会互相踩 FAT 表/目录项）。
//! 本适配器为此不提供任何共享入口：一个 [`BlockDeviceSeek`] 只属于一个 `FileSystem`，
//! 后者由 [`crate::volume::FatVolume`] 用 `Mutex` 串行化。

use std::fmt;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use hzip_vol::{BlockDevice, BlockRead as _, VolError};

/// 把块设备包成 `fatfs` 要的形状（见模块文档）。
pub struct BlockDeviceSeek {
    /// 底层设备（`Arc` 克隆：设备链的末端可能是覆盖层/缓存层）。
    dev: Arc<dyn BlockDevice>,
    /// 当前位置（字节）。
    pos: u64,
}

impl BlockDeviceSeek {
    /// 包一个设备；初始位置 0（`fatfs` 的挂载前提）。
    #[must_use]
    pub fn new(dev: Arc<dyn BlockDevice>) -> Self {
        Self { dev, pos: 0 }
    }

    /// 底层设备（落盘、能力查询用）。
    #[must_use]
    pub fn device(&self) -> &Arc<dyn BlockDevice> {
        &self.dev
    }
}

impl fmt::Debug for BlockDeviceSeek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Arc<dyn BlockDevice>` 不是 `Debug`：只报位置与容量这两个标量。
        f.debug_struct("BlockDeviceSeek")
            .field("pos", &self.pos)
            .field("size", &self.dev.size_bytes())
            .finish()
    }
}

impl Read for BlockDeviceSeek {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let size = self.dev.size_bytes();
        let left = size.saturating_sub(self.pos);
        if left == 0 {
            return Ok(0); // 卷尾：EOF（不是错误——`fatfs` 靠它判目录结束）
        }
        let n = clamp_to_buf(buf.len(), left);
        // 设备错误经 `VolError` 折进 `io::Error::source`：`volume.rs::from_io` 会把它解回来，
        // 于是「只读 / 结构损坏」这些语义类别不丢（`io::ErrorKind` 表达不了它们）。
        self.dev
            .read_at(self.pos, &mut buf[..n])
            .map_err(|e| io::Error::other(VolError::from(e)))?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for BlockDeviceSeek {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.dev.is_writable() {
            // 只读设备：`fatfs` 的任何写路径都不该走到这儿（能力位已把写侧关掉）。
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                VolError::ReadOnly(hzip_vol::ReadOnlyReason::DeviceReadOnly).to_string(),
            ));
        }
        let size = self.dev.size_bytes();
        let left = size.saturating_sub(self.pos);
        if left == 0 {
            // 卷容量不可变 + 已到卷尾：写不进去就**报错**，绝不假装写成功。
            return Err(io::Error::new(
                ErrorKind::WriteZero,
                format!(
                    "写越界：卷容量 {size} 字节不可变，位置 {} 已在卷尾",
                    self.pos
                ),
            ));
        }
        let n = clamp_to_buf(buf.len(), left);
        self.dev
            .write_at(self.pos, &buf[..n])
            .map_err(|e| io::Error::other(VolError::from(e)))?;
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        // `fatfs` 在 `File`/`Dir` 的 flush 里会调到这里 ⇒ 一路透到覆盖层/缓存层。
        self.dev
            .flush()
            .map_err(|e| io::Error::other(VolError::from(e)))
    }
}

impl Seek for BlockDeviceSeek {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let size = self.dev.size_bytes();
        let target: i128 = match from {
            SeekFrom::Start(x) => i128::from(x),
            SeekFrom::Current(x) => i128::from(self.pos) + i128::from(x),
            SeekFrom::End(x) => i128::from(size) + i128::from(x),
        };
        if target < 0 || target > i128::from(size) {
            // 越界即错（不夹取）：坏镜像里的越界簇号要变成错误，而不是被悄悄改成 0。
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!("定位越界：{target} 不在 [0, {size}]"),
            ));
        }
        self.pos = u64::try_from(target)
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, format!("定位越界：{target}")))?;
        Ok(self.pos)
    }
}

/// `left`（剩余容量）夹进 `usize`，再与缓冲长度取小。
fn clamp_to_buf(want: usize, left: u64) -> usize {
    let left = usize::try_from(left).unwrap_or(usize::MAX);
    want.min(left)
}
