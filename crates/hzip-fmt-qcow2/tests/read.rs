//! qcow2 读侧：夹具自证 + 分区层 + **真实 ext4 驱动**的目录树与读文件 + 未分配簇。
//!
//! 镜像层是 qcow2、文件系统层是真驱动——与 `hzip-fmt-raw` 的端到端同一条链路，
//! 差别只在「打开设备」那一步（qcow2 多一层虚拟盘）。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use std::sync::Arc;

use common::{
    CLUSTER_SIZE, qcow2_image, qcow2_image_sparse, qcow2_image_with, raw_ext4_mbr, write_temp,
};
use hzip_fmt_qcow2::{Qcow2Handler, Qcow2Volume, volume::fs_options_for};
use hzip_format::{FormatHandler as _, OpenOptions, PasswordRef, Source};
use hzip_format_testkit::vol::MemDevice;
use hzip_fs_ext4::Ext4Driver;
use hzip_vol::registry::FsRegistry;
use hzip_vol::{BlockDevice, BlockRead as _, VolOptions, Volume as _};

/// 真驱动注册表（只有 ext4；NTFS/FAT 由各自的 crate 提供）。
fn registry() -> Arc<FsRegistry> {
    let mut reg = FsRegistry::new();
    reg.register(Box::new(Ext4Driver::new()));
    Arc::new(reg)
}

fn vol_opts(writable: bool) -> VolOptions {
    VolOptions {
        writable,
        fs: fs_options_for(writable),
        fs_registry: registry(),
        ..VolOptions::default()
    }
}

// ------------------------------------------------------------ 夹具自证

/// 夹具造出来的东西必须是**真 qcow2**：上游读取器能打开，且写进去的参数说得上话。
///
/// 这条是下面所有用例的地基——「手写夹具写成看起来对的假 qcow2」是 T-IMG7 点名的风险。
#[test]
fn fixture_is_a_real_qcow2_for_the_upstream_reader() {
    let raw = raw_ext4_mbr(&[("/a", b"b")]);
    let img = qcow2_image(&raw);
    let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::from_bytes(img));

    let reader = img_qcow2::Qcow2Reader::open_on_device(dev).expect("夹具必须能被上游打开");
    assert_eq!(reader.version(), 3);
    assert_eq!(reader.cluster_size(), CLUSTER_SIZE);
    assert_eq!(reader.virtual_size(), raw.len() as u64);
    assert!(!reader.has_backing());
    assert!(!reader.is_writable(), "只读打开的读取器不得自报可写");

    // 逐簇状态：整盘都是「已分配」（我们的 L2 项全给了数据簇）
    assert_eq!(
        reader.cluster_status_at(0).unwrap(),
        img_qcow2::ClusterStatus::Allocated
    );
    assert_eq!(
        reader.cluster_status_at(reader.virtual_size() - 1).unwrap(),
        img_qcow2::ClusterStatus::Allocated
    );
    // 读到的是夹具字节（MBR 签名）
    let mut head = [0u8; 512];
    hzip_vol::BlockRead::read_at(&reader, 0, &mut head).unwrap();
    assert_eq!(&head[510..512], &[0x55, 0xAA]);
}

#[test]
fn qemu_img_info_accepts_the_fixture_when_available() {
    // 开发机可选交叉校验（不进 CI 硬门）：没有 qemu-img 就跳过。
    if std::process::Command::new("qemu-img")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let raw = raw_ext4_mbr(&[("/a", b"b")]);
    let (_dir, path) = write_temp("oracle.qcow2", &qcow2_image(&raw));
    let out = std::process::Command::new("qemu-img")
        .args(["info", "--output=json"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "qemu-img 拒绝了夹具：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("\"format\": \"qcow2\""), "{text}");
}

// ------------------------------------------------------------ 端到端（MBR + ext4）

#[test]
fn lists_partition_row_and_fs_tree_through_real_ext4() {
    let raw = raw_ext4_mbr(&[
        ("/etc/fstab", b"LABEL=root /\n"),
        ("/dir/keep.txt", b"keep"),
    ]);
    let (_dir, path) = write_temp("disk.qcow2", &qcow2_image(&raw));

    let handler = Qcow2Handler::new(registry());
    let reader = handler
        .open_reader(Source::File(path), &OpenOptions::default())
        .expect("打开 qcow2 镜像");

    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.contains(&"Partition 1".to_string()),
        "分区行应在：{names:?}"
    );
    assert!(
        names.contains(&"Partition 1/etc/fstab".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"Partition 1/dir/keep.txt".to_string()),
        "{names:?}"
    );

    // 元数据：无 label（MBR 分区没有标签概念）、fs 是 ext4
    let rows = reader.volume_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Partition 1");
    assert_eq!(rows[0].label, None);
    assert_eq!(rows[0].fs.as_deref(), Some("ext4"));

    // 读文件
    let entries = reader.entries().unwrap();
    let id = entries
        .iter()
        .position(|e| e.name.as_str().ends_with("etc/fstab"))
        .unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(
        &mut reader.read_entry(id, PasswordRef::none()).unwrap(),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf, b"LABEL=root /\n");
}

// ------------------------------------------------------------ 稀疏 / 未分配簇

#[test]
fn unallocated_clusters_read_back_as_zeros() {
    let raw = raw_ext4_mbr(&[("/a", b"b")]);
    // 末尾追加 8 个**未分配**虚拟簇：虚拟盘比 raw 大，多出来的部分读回 0
    let img = qcow2_image_with(&raw, &[], 8, 0);
    let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::from_bytes(img));

    let vol = Qcow2Volume::open(dev, vol_opts(false)).unwrap();
    let geom = vol.geometry();
    assert_eq!(geom.virtual_size, raw.len() as u64 + 8 * CLUSTER_SIZE);
    assert!(geom.is_sparse, "qcow2 恒为稀疏容器");
    assert!(!geom.has_backing);

    let vdev = vol.device();
    let tail = raw.len() as u64;
    let mut buf = vec![0xFFu8; 8192];
    vdev.read_at(tail, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0), "未分配簇必须读回 0");

    // 对照：已分配区域读回夹具自己的字节（不是全 0，也不是全 FF）
    let mut head = [0u8; 512];
    vdev.read_at(0, &mut head).unwrap();
    assert_eq!(&head[510..512], &[0x55, 0xAA]);
}

#[test]
fn fixture_holes_are_reported_unallocated_and_read_as_zeros() {
    let raw = raw_ext4_mbr(&[("/a", b"b")]);
    // 最后两个（虚拟）簇：ext4 的镜像尾巴，mkfs 之后一直是 0（下面先自证这一点，
    // 否则「洞读回 0」这条断言会因为原内容本来就是 0 而变得没有信息量）。
    let clusters = raw.len() as u64 / CLUSTER_SIZE;
    let tail = ((clusters - 2) * CLUSTER_SIZE) as usize;
    assert!(
        raw[tail..].iter().all(|&b| b == 0),
        "夹具前提：8 MiB ext4 镜像的最后两个簇应为 0"
    );
    let img = qcow2_image_sparse(&raw, &[clusters - 2, clusters - 1]);
    let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::from_bytes(img));

    let reader = img_qcow2::Qcow2Reader::open_on_device(dev).unwrap();
    assert_eq!(
        reader
            .cluster_status_at((clusters - 1) * CLUSTER_SIZE)
            .unwrap(),
        img_qcow2::ClusterStatus::Unallocated
    );
    let mut buf = vec![0xEEu8; 4096];
    hzip_vol::BlockRead::read_at(&reader, (clusters - 1) * CLUSTER_SIZE, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn registry_without_drivers_yields_unknown_volume_not_error() {
    let raw = raw_ext4_mbr(&[("/a", b"b")]);
    let img = qcow2_image(&raw);
    let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::from_bytes(img));
    let opts = VolOptions {
        writable: false,
        fs: fs_options_for(false),
        fs_registry: Arc::new(FsRegistry::new()), // 无驱动
        ..VolOptions::default()
    };
    let vol = Qcow2Volume::open(dev, opts).expect("没有驱动也不该打不开");
    // 分区行照给，文件系统未知 ⇒ open_fs 返回 None（上层显示「未知卷」）
    assert_eq!(vol.partitions().unwrap().len(), 1);
    assert!(vol.open_fs(Some(0)).unwrap().is_none());
}
