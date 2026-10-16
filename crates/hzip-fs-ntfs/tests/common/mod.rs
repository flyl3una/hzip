//! `hzip-fs-ntfs` 集成测试的共用夹具（`tests/*.rs` 各自 `mod common;`）。
//!
//! 这里只放**测试用**的薄壳：设备构造、挂载、条目查询、本地源文件、以及两个
//! 「用上游写侧造状态」的小工具（WOF / EFS 标记、`$LogFile` 首页清零）。
//! 业务断言一律留在各自的 `tests/*.rs` 里。

#![allow(dead_code)]
#![allow(unused_imports)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::PathIo;
use fs_ntfs::{mft_io, write};
use hzip_format::{EntryId, EntryInfo, EntryName, Source};
use hzip_format_testkit::vol::MemDevice;
use hzip_progress::CancellationToken;
use hzip_vol::device::{Chain, WriteMode};
use hzip_vol::{BlockDevice, BlockRead as _, EntryTree, FsOptions, VolOptions, VolResult};

pub use hzip_format_testkit::vol::ntfs::{
    ntfs_image, ntfs_image_dirty, ntfs_image_hibernated, ntfs_image_pending_log,
};
pub use hzip_fs_ntfs::{NtfsDriver, NtfsVolume};
// 测试里要按 trait 调方法（`entries`/`read_at`/`apply`…），故把 trait 与常用类型
// 经 `use common::*` 带进作用域。
pub use hzip_vol::{EntryLimit, FsVolume};

/// 挂载一个内存盘上的 NTFS 卷（可写选项）。
pub fn device(bytes: Vec<u8>) -> Arc<dyn BlockDevice> {
    Arc::new(MemDevice::from_bytes(bytes))
}

/// 可写挂载选项（`replay_journal` 默认 false，与产品默认一致）。
pub fn writable() -> FsOptions {
    FsOptions {
        writable: true,
        ..FsOptions::default()
    }
}

/// 只读挂载选项。
pub fn read_only() -> FsOptions {
    FsOptions {
        writable: false,
        ..FsOptions::default()
    }
}

/// 可写挂载选项 + 显式允许重放日志。
pub fn writable_replaying() -> FsOptions {
    FsOptions {
        writable: true,
        replay_journal: true,
        ..FsOptions::default()
    }
}

/// 挂载（类型化入口，便于读诊断；`FsDriver::mount` 是同一个东西的装箱版）。
pub fn mount(dev: Arc<dyn BlockDevice>, opts: &FsOptions) -> VolResult<NtfsVolume> {
    NtfsDriver::new().mount_volume(dev, opts)
}

/// 一个从未取消的 token。
pub fn cancel() -> CancellationToken {
    CancellationToken::new()
}

/// 已取消的 token（取消路径用）。
pub fn cancelled() -> CancellationToken {
    let token = CancellationToken::new();
    token.cancel();
    token
}

/// 列全树（默认上限）。
pub fn entries(fs: &NtfsVolume) -> EntryTree {
    fs.entries(&EntryLimit::default(), &cancel())
        .expect("列目录失败")
}

/// 用给定上限列树。
pub fn entries_with(fs: &NtfsVolume, limit: &EntryLimit) -> EntryTree {
    fs.entries(limit, &cancel()).expect("列目录失败")
}

/// 条目名清单（排序后，便于断言）。
pub fn names(tree: &EntryTree) -> Vec<String> {
    let mut out: Vec<String> = tree
        .entries
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    out.sort();
    out
}

/// 按名字取 id（列不出就是夹具/驱动有 bug ⇒ 直接炸）。
pub fn id_of(fs: &NtfsVolume, name: &str) -> EntryId {
    let tree = entries(fs);
    tree.entries
        .iter()
        .find(|e| e.name.as_str() == name)
        .unwrap_or_else(|| panic!("条目 {name} 不在列表里：{:?}", names(&tree)))
        .id
}

/// 按名字取条目。
pub fn entry_of(fs: &NtfsVolume, name: &str) -> EntryInfo {
    let tree = entries(fs);
    tree.entries
        .iter()
        .find(|e| e.name.as_str() == name)
        .unwrap_or_else(|| panic!("条目 {name} 不在列表里：{:?}", names(&tree)))
        .clone()
}

/// 整读一个条目（分块，模拟惰性读者）。
pub fn read_all(fs: &NtfsVolume, id: EntryId, size: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 4096];
    let mut offset = 0u64;
    while offset < size {
        let n = fs.read_at(id, offset, &mut buf).expect("读失败");
        assert!(n > 0, "读到 EOF 却还没到 size（{offset}/{size}）");
        out.extend_from_slice(&buf[..n]);
        offset += n as u64;
    }
    out
}

/// 造一个本地文件（`Replace`/`Append` 的入参），返回路径；`TempDir` 由调用方持有。
pub fn local_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, data).expect("写本地文件失败");
    path
}

/// 造一个 `size` 字节的本地文件（内容可区分：递增字节）。
pub fn local_file_sized(dir: &Path, name: &str, size: usize) -> PathBuf {
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    local_file(dir, name, &data)
}

/// `EntryName` 的便捷构造（`Change::*` 的名字参数）。
pub fn name(text: &str) -> EntryName {
    EntryName::new(text)
}

/// 设备的当前字节（`MemDevice` 快照，用来验证「flush 前设备不变」）。
pub fn bytes_of(dev: &Arc<dyn BlockDevice>) -> Vec<u8> {
    let mut out = vec![0u8; usize::try_from(dev.size_bytes()).expect("设备超出 usize")];
    dev.read_at(0, &mut out).expect("读设备失败");
    out
}

// ---------------------------------------------------------------- 上游造状态

/// 把 `bytes` 落到临时文件上跑一段**基于路径**的上游操作，再读回字节。
///
/// 上游有一批「构造特定磁盘状态」的工具只吃路径（`update_mft_record`、
/// `write_reparse_point`、`fsck::set_dirty`…），而我们的被测设备在内存里。
/// 这个壳就是两者之间的桥：**只在测试里用**。
pub fn patch_image(bytes: Vec<u8>, patch: impl FnOnce(&Path)) -> Vec<u8> {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("ntfs-fixture.img");
    std::fs::write(&path, &bytes).expect("落盘夹具");
    patch(&path);
    std::fs::read(&path).expect("读回夹具")
}

/// 给 `file` 打上「WOF（Compact OS）压缩」标记：`IO_REPARSE_TAG_WOF` 重解析点。
///
/// 上游会**拒读**这种文件的 `$DATA`（内容其实在 `WofCompressedData` 流里），
/// 正是驱动必须提前拒、并给出稳定键的那一档。
pub fn mark_wof(bytes: Vec<u8>, file: &str) -> Vec<u8> {
    patch_image(bytes, |path| {
        write::write_reparse_point(
            path,
            file,
            fs_ntfs::record_build::reparse_tag::WOF,
            &[0u8; 16],
        )
        .expect("上游写 WOF 重解析点失败");
    })
}

/// 给 `file` 打上「EFS 加密」标记：`$STANDARD_INFORMATION` 的属性位与无名 `$DATA` 的
/// `ENCRYPTED` 标志一起置位（真实 EFS 文件两者都有；上游没有「造 EFS 文件」的公开
/// 写接口 ⇒ 测试自己改位，改完的记录走上游 `update_mft_record` 的 fixup 路径）。
pub fn mark_efs(bytes: Vec<u8>, file: &str) -> Vec<u8> {
    patch_image(bytes, |path| {
        let rec = write::resolve_path_to_record_number(path, file).expect("解析文件失败");
        mft_io::update_mft_record(path, rec, |record| {
            let si = attr_io::find_attribute(record, AttrType::StandardInformation, None)
                .ok_or_else(|| fs_ntfs::error::Error::io("没有 $STANDARD_INFORMATION"))?;
            let at = si.attr_offset + si.resident_value_offset.unwrap_or(0) as usize + 0x20;
            let flags =
                u32::from_le_bytes(record[at..at + 4].try_into().unwrap_or([0; 4])) | 0x4000; // FILE_ATTRIBUTE_ENCRYPTED
            record[at..at + 4].copy_from_slice(&flags.to_le_bytes());

            let data = attr_io::find_attribute(record, AttrType::Data, None)
                .ok_or_else(|| fs_ntfs::error::Error::io("没有无名 $DATA"))?;
            let at = data.attr_offset + attr_io::attr_off::FLAGS;
            let flags =
                u16::from_le_bytes(record[at..at + 2].try_into().unwrap_or([0; 2])) | 0x4000; // ENCRYPTED
            record[at..at + 2].copy_from_slice(&flags.to_le_bytes());
            Ok(())
        })
        .expect("上游改 MFT 记录失败");
    })
}

/// 把 `$LogFile` 首页清成零 ⇒ 上游读到 `LogfileState::Pending`（日志可能含事务）。
pub fn zero_logfile_head(bytes: Vec<u8>) -> Vec<u8> {
    patch_image(bytes, |path| {
        let mut io = PathIo::open_rw(path).expect("打开夹具 rw");
        let rec = fs_ntfs::read::resolve_path(&mut io, "/$LogFile").expect("$LogFile");
        let (params, record) = mft_io::read_mft_record_io(&mut io, rec).expect("读 $LogFile 记录");
        let loc = attr_io::find_attribute(&record, AttrType::Data, None).expect("$LogFile:$DATA");
        let pairs = loc.non_resident_mapping_pairs_offset.expect("非常驻") as usize;
        let runs = fs_ntfs::data_runs::decode_runs(
            &record[loc.attr_offset + pairs..loc.attr_offset + loc.attr_length],
        )
        .expect("解 run list");
        let first = runs.first().expect("run list 非空");
        let lcn = first.lcn.expect("首个 run 是实簇");
        let page = usize::try_from(first.length * params.cluster_size)
            .expect("长度")
            .min(4096);
        fs_ntfs::block_io::BlockIo::write_all_at(
            &mut io,
            lcn * params.cluster_size,
            &vec![0u8; page],
        )
        .expect("清日志首页");
        fs_ntfs::block_io::BlockIo::sync(&mut io).expect("落盘");
    })
}

/// 只读设备链：`Chain`（`WriteMode::ReadOnly`）产出的 `Arc<dyn BlockDevice>`，
/// `is_writable() == false`。
///
/// 返回 `Chain` 是要**保持 spool 临时件活着**——只拿设备不管链，临时件会被删掉。
/// 用产品自己的只读链（而不是测试里手搓一个只读壳）是因为 `fs_core` 的设备类型
/// 不是本 crate 的依赖，而 `Chain` 是。
pub fn read_only_chain(bytes: Vec<u8>) -> (Chain, Arc<dyn BlockDevice>) {
    let source = Source::Stream(Box::new(std::io::Cursor::new(bytes)));
    let chain = Chain::open(source, WriteMode::ReadOnly, &VolOptions::default(), None)
        .expect("只读设备链打开失败");
    let dev = chain.device();
    (chain, dev)
}
