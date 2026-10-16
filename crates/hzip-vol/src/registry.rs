//! 文件系统驱动注册表：`FsKind` → [`FsDriver`]。
//!
//! # 为什么注册表在 `hzip-vol` 而驱动在别的 crate
//!
//! `hzip-vol` **不依赖任何 `hzip-fs-*`**（依赖倒置：驱动依赖它、它不依赖驱动）。
//! 驱动由 core 在启动时注册（与 `register_builtin_handlers` 同一手法）：
//!
//! ```ignore
//! let mut fs = FsRegistry::new();
//! fs.register(Box::new(hzip_fs_ext4::Ext4Driver::new()));   // feature 门控
//! ```
//!
//! 于是「装哪几个文件系统」是 **feature 与注册**决定的，不是编译期写死的；发行版
//! 裁掉某个驱动时，对应的镜像只会退化成「未知卷（只读）」，而不是打不开整档。
//!
//! # 挂载前**复核**头部（不只看嗅探结论）
//!
//! 分区嗅探读的是「分区起始的一小段窗口」，与驱动真正需要的超级块偏移可能不同
//! （ext4 的超级块在 1024、NTFS/FAT 在 0）。故 `mount_best` 会先把头部 4 KiB 读出来，
//! 让驱动用自己的 [`FsDriver::supports`] 复核一次——**两份独立证据都点头才挂载**。

use std::sync::Arc;

use crate::error::VolResult;
use crate::{BlockDevice, BlockRead as _, FsDriver, FsKind, FsOptions, FsVolume, VolError};

/// 复核窗口（4 KiB：覆盖 ext4 超级块、FAT/NTFS 引导扇区、GPT 保护性 MBR 之后的区域）。
pub const HEAD_BYTES: usize = 4096;

/// 文件系统驱动注册表。
#[derive(Default)]
pub struct FsRegistry {
    drivers: Vec<Box<dyn FsDriver>>,
}

impl std::fmt::Debug for FsRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsRegistry")
            .field(
                "drivers",
                &self.drivers.iter().map(|d| d.id()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl FsRegistry {
    /// 空注册表（测试与裁剪构建用；真实运行由 core 注册）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个驱动（注册序即复核顺序）。
    pub fn register(&mut self, driver: Box<dyn FsDriver>) {
        tracing::debug!(id = driver.id(), "注册文件系统驱动");
        self.drivers.push(driver);
    }

    /// 已注册的驱动 id（诊断与 UI 用）。
    #[must_use]
    pub fn driver_ids(&self) -> Vec<&'static str> {
        self.drivers.iter().map(|d| d.id()).collect()
    }

    /// 挑出能接这个卷的驱动（`kind` 是嗅探结论，`head` 复核由驱动做）。
    #[must_use]
    pub fn driver_for(&self, kind: FsKind, head: &[u8]) -> Option<&dyn FsDriver> {
        self.drivers
            .iter()
            .find(|d| d.supports(kind, head))
            .map(std::convert::AsRef::as_ref)
    }

    /// 读设备头部（最多 [`HEAD_BYTES`]）。
    pub fn read_head(dev: &Arc<dyn BlockDevice>) -> VolResult<Vec<u8>> {
        let size = dev.size_bytes();
        let len = HEAD_BYTES.min(usize::try_from(size).unwrap_or(HEAD_BYTES));
        let mut head = vec![0u8; len];
        if len > 0 {
            dev.read_at(0, &mut head)?;
        }
        Ok(head)
    }

    /// 按嗅探结论挂载；`Ok(None)` = 没有驱动接这个卷（上层显示「未知卷」，只读）。
    ///
    /// 驱动挂载失败**不是**「未知卷」：那是真错误（损坏/不支持的特性），
    /// 如实上抛，让用户看到原因而不是一个空树。
    pub fn mount_best(
        &self,
        dev: &Arc<dyn BlockDevice>,
        kind: FsKind,
        opts: &FsOptions,
    ) -> VolResult<Option<Box<dyn FsVolume>>> {
        let head = Self::read_head(dev)?;
        let Some(driver) = self.driver_for(kind, &head) else {
            tracing::debug!(?kind, "无驱动接这个卷 ⇒ 未知卷（只读）");
            return Ok(None);
        };
        tracing::debug!(id = driver.id(), ?kind, "挂载文件系统");
        driver.mount(dev.clone(), opts).map(Some)
    }

    /// 便捷：无注册表时的统一拒绝（测试与裁剪构建）。
    pub fn no_driver_error(kind: FsKind) -> VolError {
        tracing::debug!(?kind, "无可用文件系统驱动");
        VolError::ReadOnly(crate::ReadOnlyReason::FsUnknown)
    }
}
