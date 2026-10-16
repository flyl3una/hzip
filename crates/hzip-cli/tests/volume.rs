//! 磁盘镜像（T-IMG）CLI 端到端：`l` 列出分区层与卷内树、`x` 提取、`t` 自检。
//!
//! 素材由 `hzip_format_testkit::vol` 现场造（真 MBR/GPT + 真 ext4 卷），
//! 跑的是**真实二进制**（`cargo_bin("hzip")`），走的是与 GUI 同一条 core 服务链。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use assert_cmd::Command;
use hzip_format_testkit::vol::{FixtureFs, raw_bare, raw_with_mbr};
use tempfile::TempDir;

fn write_img(name: &str, bytes: &[u8]) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join(name);
    std::fs::write(&p, bytes).unwrap();
    (dir, p)
}

/// `hzip l` 的完整 stdout（**不要按列切**：分区行名形如 `Partition 1`，带空格，
/// 「取末列」会把它截成 `1`）。
fn list_stdout(img: &std::path::Path) -> String {
    let out = Command::cargo_bin("hzip")
        .unwrap()
        .arg("l")
        .arg(img)
        .output()
        .unwrap();
    assert!(out.status.success(), "l 应成功：{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// MBR + ext4：分区行 + 卷内条目（**用户口径**：分区当目录显示、名字用 label 否则「N」）。
#[test]
fn tc_cli_volume_list_shows_partition_layer_and_fs_tree() {
    let (_dir, img) = write_img(
        "disk.img",
        &raw_with_mbr(
            FixtureFs::Ext4,
            &[
                ("/etc/fstab", b"LABEL=root /\n" as &[u8]),
                ("/dir/keep.txt", b"keep" as &[u8]),
            ],
        ),
    );
    let out = list_stdout(&img);
    assert!(out.contains("Partition 1"), "分区行应在列表里：\n{out}");
    assert!(
        out.contains("Partition 1/etc/fstab"),
        "卷内条目带分区前缀：\n{out}"
    );
    assert!(out.contains("Partition 1/dir/keep.txt"), "\n{out}");
    assert!(out.contains("lost+found"), "ext4 根目录条目也在：\n{out}");
}

/// `hzip x`：按条目提取（路径含分区前缀），字节与写入一致。
#[test]
fn tc_cli_volume_extract_writes_bytes() {
    let (_dir, img) = write_img(
        "disk.img",
        &raw_with_mbr(
            FixtureFs::Ext4,
            &[("/etc/fstab", b"LABEL=root /\n" as &[u8])],
        ),
    );
    let dest = TempDir::new().unwrap();
    Command::cargo_bin("hzip")
        .unwrap()
        .arg("x")
        .arg(&img)
        .arg("-o")
        .arg(dest.path())
        .assert()
        .success();
    assert_eq!(
        std::fs::read(dest.path().join("Partition 1/etc/fstab")).unwrap(),
        b"LABEL=root /\n"
    );
}

/// 无分区表的裸文件系统镜像：顶层一行「卷」（ASCII 稳定名 `Volume`，界面按语言显示「卷」）。
#[test]
fn tc_cli_volume_bare_image_shows_single_volume_row() {
    let (_dir, img) = write_img(
        "rootfs.ext4",
        &raw_bare(FixtureFs::Ext4, &[("/marker", b"m")]),
    );
    let out = list_stdout(&img);
    assert!(out.contains("Volume"), "整卷行应在：\n{out}");
    assert!(out.contains("Volume/marker"), "卷内条目带整卷前缀：\n{out}");
}

/// `hzip t`：卷的「测试」= 文件系统自检（ext4 走上游 audit）⇒ 干净卷退出 0。
#[test]
fn tc_cli_volume_test_runs_audit() {
    let (_dir, img) = write_img(
        "disk.img",
        &raw_with_mbr(FixtureFs::Ext4, &[("/a", b"b" as &[u8])]),
    );
    Command::cargo_bin("hzip")
        .unwrap()
        .arg("t")
        .arg(&img)
        .assert()
        .success();
}
