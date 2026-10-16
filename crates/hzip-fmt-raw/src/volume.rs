//! `raw` 卷：整文件就是一个块设备，内部分区/文件系统全靠下游探测。

use std::sync::Arc;

use hzip_vol::error::VolResult;
use hzip_vol::partition::{PartitionProbe, open_region, probe_partitions, sniff_whole_device};
use hzip_vol::{
    BlockDevice, FsOptions, FsVolume, Geometry, PartitionId, PartitionView, VolCaps, VolOptions,
    Volume,
};

/// raw 镜像的卷视图（探测结果在构造时缓存一次）。
pub struct RawVolume {
    dev: Arc<dyn BlockDevice>,
    probe: PartitionProbe,
    opts: VolOptions,
}

impl std::fmt::Debug for RawVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawVolume")
            .field("size", &self.dev.size_bytes())
            .field("table", &self.probe.table)
            .field("partitions", &self.probe.partitions.len())
            .finish()
    }
}

impl RawVolume {
    /// 打开卷：探一次分区表（挂在构造上，避免 `partitions()` 被反复调用时重扫）。
    pub fn new(dev: Arc<dyn BlockDevice>, opts: VolOptions) -> VolResult<Self> {
        let probe = probe_partitions(&dev)?;
        Ok(Self { dev, probe, opts })
    }

    /// 探测结果（诊断用）。
    #[must_use]
    pub fn probe(&self) -> &PartitionProbe {
        &self.probe
    }
}

impl Volume for RawVolume {
    fn geometry(&self) -> Geometry {
        Geometry {
            virtual_size: self.dev.size_bytes(),
            sector_size: 512,
            table: self.probe.table,
            // raw 没有稀疏元数据：占用即容量（qcow2/vmdk 才可能远小于虚拟容量）
            is_sparse: false,
            has_backing: false,
        }
    }

    fn partitions(&self) -> VolResult<Vec<PartitionView>> {
        Ok(self.probe.partitions.clone())
    }

    fn open_fs(&self, part: Option<PartitionId>) -> VolResult<Option<Box<dyn FsVolume>>> {
        let region = open_region(&self.dev, &self.probe, part)?;
        // 文件系统种类：有表 ⇒ 该分区嗅探结果；无表（整卷）⇒ 现场按头部判
        let kind = match part {
            Some(i) => self.probe.partitions.get(i).and_then(|p| p.fs),
            None => sniff_whole_device(&region)?,
        };
        let Some(kind) = kind else {
            return Ok(None); // 未识别 ⇒ 上层显示「未知卷」（只读），不是错误
        };
        self.opts
            .fs_registry
            .mount_best(&region, kind, &self.opts.fs)
    }

    fn caps(&self) -> VolCaps {
        let writable = self.dev.is_writable();
        VolCaps {
            writable,
            // raw 的设备末端由设备链决定：源是常规文件 ⇒ 提交可只回放脏块
            in_place: writable,
            read_only_reason: (!writable).then_some(hzip_vol::ReadOnlyReason::DeviceReadOnly),
            warnings: self.probe.warnings.clone(),
        }
    }

    fn flush(&mut self) -> VolResult<()> {
        use hzip_vol::BlockDevice as _;
        self.dev.flush().map_err(Into::into)
    }
}

/// 挂载选项：浏览路径按「源是否可写的常规文件」决定**报给驱动的可写性**
/// （驱动据此报告真实 `FsCaps`；真正写入由 `open_editor` 的写模式设备链负责）。
#[must_use]
pub fn fs_options_for(writable: bool) -> FsOptions {
    FsOptions {
        writable,
        // 浏览永不重放脏日志（重放 = 改档案，必须用户显式同意；见 T-IMG5 §2.2）
        replay_journal: false,
        cache_blocks: 0,
    }
}
