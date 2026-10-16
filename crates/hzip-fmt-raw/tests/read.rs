//! `raw` 容器的端到端测试：探针 + 分区层 + **真实 ext4 驱动**的目录树与读文件。
//!
//! 这是「双击磁盘镜像自动挂载、分区当目录显示」这条用户需求的最小可验形态
//! （镜像层用 raw，文件系统层用真驱动；qcow2/vmdk 只是换个容器）。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::sync::Arc;

use hzip_fmt_raw::{PROBE_HEAD_BYTES, RawHandler, raw_probe};
use hzip_format::{FormatHandler as _, OpenOptions, PasswordRef, Source};
use hzip_format_testkit::vol::{FixtureFs, MemDevice, raw_bare, raw_with_gpt, raw_with_mbr};
use hzip_fs_ext4::Ext4Driver;
use hzip_vol::BlockRead as _;
use hzip_vol::registry::FsRegistry;

/// 真驱动注册表（只有 ext4；NTFS/FAT 由各自的 crate 提供）。
fn registry() -> Arc<FsRegistry> {
    let mut reg = FsRegistry::new();
    reg.register(Box::new(Ext4Driver::new()));
    Arc::new(reg)
}

fn write_temp(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(name);
    std::fs::write(&p, bytes).unwrap();
    (dir, p)
}

// ------------------------------------------------------------------ 探针

#[test]
fn probe_recognises_images_and_ignores_junk() {
    // 强证据：MBR / GPT / 裸 ext4
    let mbr_img = raw_with_mbr(FixtureFs::Ext4, &[("/a", b"b")]);
    assert!(raw_probe(&mbr_img[..PROBE_HEAD_BYTES]).is_some());
    let gpt_img = raw_with_gpt(FixtureFs::Ext4, &[("/a", b"b")]);
    assert!(raw_probe(&gpt_img[..PROBE_HEAD_BYTES]).is_some());
    let bare = raw_bare(FixtureFs::Ext4, &[("/a", b"b")]);
    assert!(
        raw_probe(&bare[..PROBE_HEAD_BYTES]).is_some(),
        "裸 ext4 也要认出来"
    );

    // 反例：全零 / 纯文本 / 头长不足
    assert!(raw_probe(&[0u8; 2048]).is_none(), "全零不得认");
    assert!(raw_probe(b"this is just a text file, not a disk image at all").is_none());
    assert!(raw_probe(&[0x55, 0xAA]).is_none(), "头长不足 ⇒ 不猜");
}

#[test]
fn probe_rejects_mbr_signature_without_plausible_entries() {
    let mut head = [0u8; 2048];
    head[510..512].copy_from_slice(&[0x55, 0xAA]); // 只有签名、表项全零
    assert!(
        raw_probe(&head).is_none(),
        "55AA 但无有效分区表 ⇒ 不足以认定"
    );
}

// ------------------------------------------------------- 端到端（MBR + ext4）

#[test]
fn mbr_image_lists_partition_row_and_fs_tree() {
    let (_dir, path) = write_temp(
        "disk.img",
        &raw_with_mbr(
            FixtureFs::Ext4,
            &[
                ("/etc/fstab", b"LABEL=root /\n"),
                ("/dir/keep.txt", b"keep"),
            ],
        ),
    );
    let handler = RawHandler::new(registry());
    let reader = handler
        .open_reader(Source::File(path), &OpenOptions::default())
        .expect("打开 raw 镜像");

    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    // 无 label 的 MBR 分区 ⇒ 稳定 ASCII 名 `Partition 1`
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

    // 不变量：`info.id` 必须与它在列表里的下标一致（`read_entry` 正是按 id 取）
    for (i, e) in reader.entries().unwrap().iter().enumerate() {
        assert_eq!(e.id, i, "投影必须把 id 重编号成下标：{}", e.name.as_str());
    }

    // 元数据：label 缺失、fs 是 ext4（供界面显示分区图标/类型/本地化名）
    let rows = reader.volume_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Partition 1");
    assert_eq!(
        rows[0].label, None,
        "MBR 分区无标签 ⇒ 界面按语言显示「分区 N」"
    );
    assert_eq!(rows[0].fs.as_deref(), Some("ext4"));

    // 读文件（随机 + 重复）
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

#[test]
fn bare_filesystem_image_has_no_partition_layer() {
    let (_dir, path) = write_temp(
        "rootfs.ext4",
        &raw_bare(FixtureFs::Ext4, &[("/marker", b"m")]),
    );
    let handler = RawHandler::new(registry());
    let reader = handler
        .open_reader(Source::File(path), &OpenOptions::default())
        .unwrap();
    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    // 无分区表 ⇒ 顶层是一行「卷」（ASCII 稳定的 `Volume`，界面按语言显示「卷」），
    // 该卷的目录树挂在它下面——与会话顶层「总是卷列表」的形状一致（便于统一 UI）。
    assert!(names.contains(&"Volume".to_string()), "{names:?}");
    assert!(names.contains(&"Volume/marker".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.starts_with("Partition")),
        "无表 ⇒ 不应出现分区行：{names:?}"
    );
    let rows = reader.volume_rows();
    assert_eq!(rows.len(), 1, "整卷也作为一行报给界面");
    assert_eq!(rows[0].label, None, "整卷没有分区标签");
    assert_eq!(rows[0].fs.as_deref(), Some("ext4"));
}

#[test]
fn gpt_image_reports_the_partition_label() {
    let (_dir, path) = write_temp("disk.vhd", &raw_with_gpt(FixtureFs::Ext4, &[("/x", b"y")]));
    let handler = RawHandler::new(registry());
    let reader = handler
        .open_reader(Source::File(path), &OpenOptions::default())
        .unwrap();
    let rows = reader.volume_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].label.as_deref(),
        Some("hzip-fixture"),
        "GPT 分区名作 label"
    );
    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.contains(&"hzip-fixture/x".to_string()),
        "顶层目录名用 label：{names:?}"
    );
}

// --------------------------------------------------- 只读策略（脏卷/未知卷）

#[test]
fn dirty_volume_is_reported_read_only_with_reason() {
    let (_dir, path) = write_temp(
        "dirty.img",
        &raw_with_mbr(FixtureFs::Ext4Dirty, &[("/a", b"b")]),
    );
    let handler = RawHandler::new(registry());
    let reader = handler
        .open_reader(Source::File(path), &OpenOptions::default())
        .unwrap();
    // 列表照常给（脏卷可读），但编辑能力关闭且理由明确
    assert!(!reader.caps().edit, "脏卷不得广告可编辑");
    // 理由（`JournalPending`）由驱动自己的用例断言（`hzip-fs-ext4/tests`）；
    // 此处只守「能力关闭」这一可观察面（理由→界面 tooltip 的接线属 T-IMG10）。
    let entries = reader.entries().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.name.as_str().contains("Partition 1"))
    );
}

#[test]
fn unknown_filesystem_shows_a_row_but_no_children() {
    // 造一个只有分区表、分区内全零的镜像
    let disk = hzip_format_testkit::vol::mbr_only(&[(0x83, 2048, 4096)], 4 << 20);
    let (_dir, path) = write_temp("empty.img", &disk);
    let handler = RawHandler::new(registry());
    let reader = handler
        .open_reader(Source::File(path), &OpenOptions::default())
        .unwrap();
    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["Partition 1".to_string()],
        "未识别卷只给分区行：{names:?}"
    );
    assert_eq!(reader.volume_rows()[0].fs, None);
}

#[test]
fn registry_without_drivers_yields_unknown_volume_not_error() {
    let (_dir, path) = write_temp("rootfs.ext4", &raw_bare(FixtureFs::Ext4, &[("/a", b"b")]));
    let handler = RawHandler::new(Arc::new(FsRegistry::new())); // 无驱动
    let reader = handler
        .open_reader(Source::File(path), &OpenOptions::default())
        .expect("没有驱动也不该打不开：显示为未知卷即可");
    assert_eq!(reader.entries().unwrap().len(), 1, "只有整卷那一行");
}

#[test]
fn stream_source_is_spooled_and_browsable() {
    let bytes = raw_with_mbr(FixtureFs::Ext4, &[("/from-stream", b"s")]);
    let handler = RawHandler::new(registry());
    let reader = handler
        .open_reader(
            Source::Stream(Box::new(std::io::Cursor::new(bytes))),
            &OpenOptions::default(),
        )
        .expect("流源（组合链/档案内条目的形态）应可浏览");
    let names: Vec<String> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with("from-stream")),
        "{names:?}"
    );
}

#[test]
fn create_writer_is_refused_by_name() {
    let handler = RawHandler::new(registry());
    let err = match handler.create_writer(
        hzip_format::Sink::File(std::path::PathBuf::from("x.img")),
        &Default::default(),
    ) {
        Ok(_) => panic!("raw 不产出镜像，应拒绝"),
        Err(e) => e,
    };
    assert!(
        matches!(err, hzip_error::HzipError::Unsupported { ref what } if what == "vol.unsupported.create-image"),
        "{err:?}"
    );
}

#[test]
fn mem_device_fixture_is_usable_from_this_crate() {
    // 保留一次对夹具 `MemDevice` 的引用：其它用例用它做内存镜像，这里确保导出可用
    let dev = MemDevice::new(4096);
    assert_eq!(dev.size_bytes(), 4096);
}
