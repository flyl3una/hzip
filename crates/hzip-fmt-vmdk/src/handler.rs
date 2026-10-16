//! `VmdkHandler`：把 VMDK 镜像接进 hzip 的格式契约。
//!
//! 与 [`hzip_fmt_raw::RawHandler`] 同构——差别只在「打开设备」那一步：
//! raw 的整文件**就是**块设备，而 vmdk 的块设备是**上游 `VmdkReader` 解释出的
//! 虚拟磁盘**（[`crate::volume::VmdkVolume`] 负责这一步）。分区层、文件系统驱动、
//! 条目投影、`ChangeSet` 全部原样复用。
//!
//! [`hzip_fmt_raw::RawHandler`]: https://docs.rs/hzip-fmt-raw

use std::sync::Arc;

use hzip_error::HzipError;
use hzip_format::{
    ArchiveReader, Confidence, CreateOptions, FormatCaps, FormatHandler, FormatId, OpenOptions,
    Signature, Sink, Source,
};
use hzip_progress::CancellationToken;
use hzip_vol::device::{Chain, WriteMode};
use hzip_vol::reader::VolumeReader;
use hzip_vol::registry::FsRegistry;
use hzip_vol::{EntryLimit, VolOptions, VolumeHandler};

use crate::probe::{PROBE_HEAD_BYTES, vmdk_probe};
use crate::volume::{VmdkVolume, fs_options_for};

/// VMDK 镜像 handler。
///
/// `Clone` 廉价（只含一个 `Arc`）：`open_editor` 需要把一个**自有**的 handler 交给
/// 编辑器（编辑器可能活到会话结束，不能借用 `&self`）。
#[derive(Clone)]
pub struct VmdkHandler {
    fs_registry: Arc<FsRegistry>,
}

impl VmdkHandler {
    /// 构造：文件系统驱动注册表由 core 在启动时组装后传入（`hzip-vol` 不依赖任何驱动）。
    #[must_use]
    pub fn new(fs_registry: Arc<FsRegistry>) -> Self {
        Self { fs_registry }
    }
}

impl VolumeHandler for VmdkHandler {
    fn id(&self) -> &'static str {
        "vmdk"
    }

    fn open(
        &self,
        dev: Arc<dyn hzip_vol::BlockDevice>,
        opts: &VolOptions,
    ) -> hzip_vol::error::VolResult<Box<dyn hzip_vol::Volume>> {
        Ok(Box::new(VmdkVolume::new(dev, opts.clone())?))
    }
}

impl FormatHandler for VmdkHandler {
    fn id(&self) -> FormatId {
        FormatId::from("vmdk")
    }

    fn signature(&self) -> &'static Signature {
        static S: Signature = Signature {
            // VMDK 稀疏 extent 头魔数（**小端** `KDMV`，与 qcow2 的大端相反）；
            // flat 布局没有魔数 ⇒ 靠扩展名回退进来后由描述符判定并拒绝。
            magic: b"KDMV",
            offset: 0,
            display: "VMware VMDK disk image",
            extensions: &["vmdk"],
        };
        &S
    }

    fn caps(&self) -> FormatCaps {
        FormatCaps {
            read: true,
            // 写侧：`ChangeSet` 语义由卷内文件系统驱动承担；镜像层只允许
            // `monolithicSparse`（`streamOptimized` 等只读）
            edit: true,
            ..Default::default()
        }
    }

    fn probe(&self, head: &[u8]) -> Option<Confidence> {
        vmdk_probe(head)
    }

    fn probe_head_bytes(&self) -> usize {
        PROBE_HEAD_BYTES
    }

    fn supports_in_place_edit(&self) -> bool {
        // 源是常规文件时，设备链可就地写（提交只回放脏块）；组合链由 core 的会话判定否决
        true
    }

    fn open_reader(
        &self,
        src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError> {
        let writable = could_be_writable(&src);
        let vol_opts = VolOptions {
            writable,
            fs: fs_options_for(writable),
            fs_registry: self.fs_registry.clone(),
            ..VolOptions::default()
        };
        // 浏览一律走**只读设备链**：打开一个档案不该写它一个字节。
        let chain =
            Chain::open(src, WriteMode::ReadOnly, &vol_opts, None).map_err(HzipError::from)?;
        let vol = VmdkVolume::new(chain.device(), vol_opts).map_err(HzipError::from)?;
        let reader = VolumeReader::build(
            Box::new(vol),
            &EntryLimit::default(),
            // 契约里 `open_reader` 没有取消令牌；列树期间的取消由上层超时/关闭会话兜底。
            &CancellationToken::new(),
        )
        .map_err(HzipError::from)?;
        Ok(Box::new(reader))
    }

    fn create_writer(
        &self,
        _dst: Sink,
        _opts: &CreateOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveWriter>, HzipError> {
        // 不产出镜像：从零造 VMDK（头 + 描述符 + 目录 + grain）是另一个特性，
        // 本期只做「打开既有镜像」。
        Err(HzipError::Unsupported {
            what: "vol.unsupported.create-image".into(),
        })
    }

    fn open_editor(
        &self,
        src: Source,
        _opts: &OpenOptions,
    ) -> Result<Box<dyn hzip_format::ArchiveEditor>, HzipError> {
        let opts = VolOptions {
            writable: true,
            fs: fs_options_for(true),
            fs_registry: self.fs_registry.clone(),
            ..VolOptions::default()
        };
        let editor = hzip_vol::editor::VolumeEditor::open(self.clone(), src, opts)
            .map_err(HzipError::from)?;
        Ok(Box::new(editor))
    }
}

/// 源是否**可能**可写：常规文件且未置只读位。
///
/// 只做 `metadata` 判断，不真打开 r/w（浏览不该改动文件的时间戳/锁）。
/// 流源（组合链/档案条目）一律按不可写报告：能否写回取决于外层能否重建，
/// 那是 core 的**会话判定**（`chain_editable`），不是单个 handler 的事。
fn could_be_writable(src: &Source) -> bool {
    match src {
        Source::File(p) => std::fs::metadata(p)
            .map(|m| m.is_file() && !m.permissions().readonly())
            .unwrap_or(false),
        _ => false,
    }
}
