//! 内存块设备：所有 vol 测试共用的 base / 差异对照盘。
//!
//! 实现 [`fs_core::BlockRead`] + [`fs_core::BlockDevice`]（二者都是
//! `Send + Sync`），所以既能直接喂给分区/容器层（`Arc<dyn fs_core::BlockDevice>`），
//! 也能经 `fs_ext4::fs_core_bridge::CoreDevice` 包一层喂给 ext4 驱动。
//!
//! # 语义（冻结点，vol 全线依赖）
//!
//! - **越界即错**：读/写超出容量返回 [`fs_core::Error::OutOfBounds`]，**不 panic、
//!   不静默截断**。零长度读写在 `offset <= len` 上合法（空区间），`offset > len`
//!   仍算越界。
//! - **`read_at` 必须填满 `buf`**：不可满足即 `Err`，没有"短读返回前缀"的路径。
//! - `is_writable()` 恒为 `true`（夹具要能被格式化/写文件）；`flush()` 无操作成功。
//! - `set_len()` 恒 `Err`：**卷容量在构造时固定**，这是内存盘的语义（不是
//!   "只读设备"），故返回 [`fs_core::Error::Custom`] 而非 `ReadOnly`；
//!   `can_grow()` 保持 trait 默认的 `false`，调用方按能力位判断，别猜错误码。
//!
//! # 为什么用 `Arc<Mutex<..>>`
//!
//! `Clone` 是**共享底层**的句柄克隆：先 `MemDevice::new(n)` 造盘，`clone()` 一份
//! 交给 `Arc<dyn BlockDevice>` 去挂载，写完文件后原句柄 `to_bytes()` 就能拿到
//! 上游写进去的字节——不需要把设备从 `Arc` 里"取回来"。要独立副本则用
//! [`MemDevice::to_bytes`] + [`MemDevice::from_bytes`]。

use std::sync::{Arc, Mutex, MutexGuard};

use fs_core::{BlockDevice, BlockRead};

/// 内存块设备：一块定长字节缓冲区，读写都走 [`Mutex`]。
///
/// 构造见 [`MemDevice::new`]（定长清零）与 [`MemDevice::from_bytes`]（接管现有
/// 字节）；`Clone` 共享同一底层缓冲区，见模块文档。
#[derive(Clone, Debug, Default)]
pub struct MemDevice {
    data: Arc<Mutex<Vec<u8>>>,
}

impl MemDevice {
    /// 造一块 `len` 字节全零的内存盘。
    ///
    /// `len` 超出本机 `usize` 上限（32 位目标是 4 GiB）时直接 panic：静默截断会
    /// 让"容量"这个契约值失真，而调用方拿不到任何信号。
    #[must_use]
    pub fn new(len: u64) -> Self {
        let len = match usize::try_from(len) {
            Ok(n) => n,
            Err(_) => panic!("MemDevice::new: 容量 {len} 超出本机 usize 上限"),
        };
        Self {
            data: Arc::new(Mutex::new(vec![0u8; len])),
        }
    }

    /// 接管 `v` 的字节作为盘内容（长度即容量）。
    #[must_use]
    pub fn from_bytes(v: Vec<u8>) -> Self {
        Self {
            data: Arc::new(Mutex::new(v)),
        }
    }

    /// 复制出当前全部字节（独立副本；见模块文档）。
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.guard().clone()
    }

    /// 取锁。中毒才 panic：持锁期间不跑任何用户回调，中毒只可能来自本模块自身
    /// 的中断，此时返回假数据（如容量 0）比炸测试更糟。越界仍走 `Err`，见模块文档。
    fn guard(&self) -> MutexGuard<'_, Vec<u8>> {
        match self.data.lock() {
            Ok(g) => g,
            Err(_) => panic!("MemDevice: 内部锁中毒（持锁线程已 panic）"),
        }
    }
}

impl BlockRead for MemDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        let data = self.guard();
        let range = span(offset, buf.len(), data.len())?;
        buf.copy_from_slice(&data[range]);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.guard().len() as u64
    }
}

impl BlockDevice for MemDevice {
    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()> {
        let mut data = self.guard();
        let size = data.len();
        let range = span(offset, buf.len(), size)?;
        data[range].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&self) -> fs_core::Result<()> {
        // 内存盘没有易失写缓存：写入即生效，flush 无事可做但必须成功。
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }

    fn set_len(&self, new_len: u64) -> fs_core::Result<()> {
        // 容量不可变：这是内存盘的语义，不是"只读"。`can_grow()` 保持默认 false，
        // 调用方应按能力位判断（见 rust-fs-core 的 `BlockDevice::set_len` 文档）。
        Err(fs_core::Error::Custom(format!(
            "MemDevice: 卷容量不可变（构造时固定，拒绝 set_len({new_len})）"
        )))
    }
}

/// `[offset, offset + len)` 落在 `[0, size]` 内的带界检查（空区间允许贴尾）。
fn span(offset: u64, len: usize, size: usize) -> fs_core::Result<std::ops::Range<usize>> {
    let oob = || fs_core::Error::OutOfBounds {
        offset,
        len: len as u64,
        size: size as u64,
    };
    let start = usize::try_from(offset).map_err(|_| oob())?;
    let end = start.checked_add(len).ok_or_else(oob)?;
    if end > size {
        return Err(oob());
    }
    Ok(start..end)
}
