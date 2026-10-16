//! 磁盘镜像（`raw` + 真实 ext4 驱动）的**引擎级**集成测试（里程碑 M1）。
//!
//! 断言到**用户看到的形状**：分区即顶层目录、卷内目录树、按条目读文件——
//! 而不是停在「能打开」。qcow2/vmdk 只是换个容器，链路完全相同。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use hzip_format::PasswordRef;
use hzip_format_testkit::vol::{FixtureFs, raw_bare, raw_with_gpt, raw_with_mbr};
use tempfile::TempDir;

use common::engine;

fn write_temp(name: &str, bytes: &[u8]) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join(name);
    std::fs::write(&p, bytes).unwrap();
    (dir, p)
}

#[test]
fn raw_image_browses_partition_row_then_fs_tree() {
    let (_dir, path) = write_temp(
        "disk.img",
        &raw_with_mbr(
            FixtureFs::Ext4,
            &[
                ("/etc/fstab", b"LABEL=root /\n" as &[u8]),
                ("/dir/keep.txt", b"keep" as &[u8]),
            ],
        ),
    );
    let eng = engine(1, 1);
    let b = eng
        .browse(hzip_format::Source::File(path), &Default::default())
        .expect("打开 raw 镜像");

    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    // 分区行（MBR 无标签 ⇒ 稳定 ASCII 名 `Partition 1`）+ 卷内条目带前缀
    assert!(names.contains(&"Partition 1".to_string()), "{names:?}");
    assert!(names.contains(&"Partition 1/etc".to_string()), "{names:?}");
    assert!(
        names.contains(&"Partition 1/etc/fstab".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"Partition 1/dir/keep.txt".to_string()),
        "{names:?}"
    );

    // 树形：root → Partition 1 → { etc → fstab, dir → keep.txt }
    let tree = b.tree();
    let part = tree.children.get("Partition 1").expect("分区行在树里");
    let etc = part.children.get("etc").expect("etc 目录");
    assert!(etc.children.contains_key("fstab"), "etc/fstab 在树里");
    let dir = part.children.get("dir").expect("dir 目录");
    assert!(dir.children.contains_key("keep.txt"));
}

#[test]
fn entry_bytes_are_readable_through_the_engine() {
    let (_dir, path) = write_temp(
        "disk.img",
        &raw_with_mbr(
            FixtureFs::Ext4,
            &[("/etc/fstab", b"LABEL=root /\n" as &[u8])],
        ),
    );
    let eng = engine(1, 1);
    let b = eng
        .browse(hzip_format::Source::File(path), &Default::default())
        .unwrap();
    let info = b
        .entries()
        .iter()
        .find(|e| e.name.as_str().ends_with("etc/fstab"))
        .cloned()
        .expect("找到 /etc/fstab");
    let mut buf = Vec::new();
    std::io::Read::read_to_end(
        &mut b.read_entry(info.id, PasswordRef::none()).unwrap(),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf, b"LABEL=root /\n", "读出的字节要与写进去的一致");
}

#[test]
fn gpt_image_uses_the_partition_label_as_directory_name() {
    let (_dir, path) = write_temp("disk.vhd", &raw_with_gpt(FixtureFs::Ext4, &[("/x", b"y")]));
    let eng = engine(1, 1);
    let b = eng
        .browse(hzip_format::Source::File(path), &Default::default())
        .unwrap();
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.contains(&"hzip-fixture".to_string()),
        "GPT 分区名作目录名：{names:?}"
    );
    assert!(names.contains(&"hzip-fixture/x".to_string()), "{names:?}");
}

#[test]
fn bare_filesystem_image_shows_a_single_volume_row() {
    let (_dir, path) = write_temp(
        "rootfs.ext4",
        &raw_bare(FixtureFs::Ext4, &[("/marker", b"m")]),
    );
    let eng = engine(1, 1);
    let b = eng
        .browse(hzip_format::Source::File(path), &Default::default())
        .unwrap();
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    // 无分区表 ⇒ 顶层一行「卷」（ASCII 稳定名 `Volume`，界面按语言显示「卷」）
    assert!(names.contains(&"Volume".to_string()), "{names:?}");
    assert!(names.contains(&"Volume/marker".to_string()), "{names:?}");
}

#[test]
fn sniff_picks_raw_by_content_without_extension() {
    // 故意用无扩展名/无关联的文件名：判定必须走**内容探针**（否则只有扩展名回退）
    let (_dir, path) = write_temp(
        "mystery.bin",
        &raw_with_mbr(FixtureFs::Ext4, &[("/a", b"b")]),
    );
    let eng = engine(1, 1);
    let b = eng
        .browse(hzip_format::Source::File(path), &Default::default())
        .expect("内容像镜像 ⇒ 无扩展名也应能打开");
    assert!(b.entry_count() > 1);
}

#[test]
fn unknown_filesystem_still_opens_with_only_the_partition_row() {
    let disk = hzip_format_testkit::vol::mbr_only(&[(0x83, 2048, 4096)], 4 << 20);
    let (_dir, path) = write_temp("empty.img", &disk);
    let eng = engine(1, 1);
    let b = eng
        .browse(hzip_format::Source::File(path), &Default::default())
        .expect("未识别的卷也不该打不开（显示未知卷）");
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["Partition 1".to_string()], "{names:?}");
}

#[test]
fn engine_registers_volume_handlers_and_sniffs_them_by_magic() {
    let eng = engine(1, 1);
    let reg = eng.registry();
    // ① 已注册：raw（无魔数，靠内容探针）+ qcow2（大端魔数）——这正是「双击 .qcow2
    //    不再落到系统『打开方式』」的前提（未注册 → 嗅探失败 → 回落外部程序）。
    assert!(reg.handler("raw").is_some(), "raw handler 应已注册");
    assert!(reg.handler("qcow2").is_some(), "qcow2 handler 应已注册");
    assert!(reg.handler("vmdk").is_some(), "vmdk handler 应已注册");
    assert!(reg.handler("raw").unwrap().caps().read);
    assert_eq!(
        reg.handler("raw").unwrap().id().as_str(),
        "raw",
        "id 与扩展名表里的 container 值一致"
    );

    // ② 嗅探：qcow2 魔数（`QFI\xfb` + 大端版本 2/3）⇒ 交给 qcow2 handler
    let dir = TempDir::new().unwrap();
    let qcow = dir.path().join("disk.qcow2");
    let mut head = vec![0u8; 512];
    head[..4].copy_from_slice(&[0x51, 0x46, 0x49, 0xFB]); // "QFI\xfb"
    head[4..8].copy_from_slice(&3u32.to_be_bytes()); // 大端版本 3
    std::fs::write(&qcow, &head).unwrap();
    let sniffed = reg
        .sniff(&hzip_format::Source::File(qcow))
        .expect("qcow2 魔数应被认出");
    assert!(
        format!("{sniffed:?}").contains("qcow2"),
        "嗅探应交给 qcow2 handler：{sniffed:?}"
    );

    // vmdk：sparse 头魔数（小端 `KDMV`）同样应被嗅探表认领
    let vmdk = dir.path().join("disk.vmdk");
    let mut vhead = vec![0u8; 512];
    vhead[..4].copy_from_slice(b"KDMV");
    std::fs::write(&vmdk, &vhead).unwrap();
    let sniffed = reg
        .sniff(&hzip_format::Source::File(vmdk))
        .expect("vmdk 魔数应被认出");
    assert!(format!("{sniffed:?}").contains("vmdk"), "{sniffed:?}");

    // ③ 扩展名表认得出（文件关联/右键菜单/类型列同源）
    assert!(hzip_format::exts::container_handler("disk.qcow2").is_some());
    assert!(hzip_format::exts::container_handler("disk.vmdk").is_some());
    assert!(hzip_format::exts::container_handler("disk.raw").is_some());
    assert!(hzip_format::exts::container_handler("disk.img").is_some());
}

/// 组合形态：`*.img.gz`（外层 gz 编码层 → 内层磁盘镜像）。
///
/// 与用户口径的 `qcow2.gz` 同一机制，但有一个**已记录的差别**：qcow2/vmdk 有 4 字节魔数
/// ⇒ 嗅探能把内层认成「容器尾」直接下钻；`raw` 没有魔数（只能靠内容探针），所以 gz 层
/// 看到的是「单文件流」⇒ 顶层先给一行内层文件名（`disk.img`），**双击那一行**才按扩展名
/// 选 handler 进入镜像（`Browser::enter`）。本用例把这条路径钉死：先见行、再进层见分区树。
#[test]
fn gz_wrapped_raw_image_shows_inner_row_then_descends() {
    use std::io::Write as _;
    let inner = raw_with_mbr(
        FixtureFs::Ext4,
        &[("/etc/fstab", b"LABEL=root /\n" as &[u8])],
    );
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&inner).unwrap();
    let gz = enc.finish().unwrap();

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("disk.img.gz");
    std::fs::write(&path, &gz).unwrap();

    let eng = engine(1, 1);
    let mut b = eng
        .browse(hzip_format::Source::File(path), &Default::default())
        .expect("gz 包裹的镜像应可打开");
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["disk.img".to_string()],
        "内层镜像先作为一行出现：{names:?}"
    );

    // 双击那一行（引擎侧 = `enter`）：解层 → 内层 spool → 分区树
    let id = b.entries()[0].id;
    let inner_browser = b
        .enter(id, eng.registry(), PasswordRef::none())
        .expect("进入内层镜像");
    let inner_names: Vec<String> = inner_browser
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        inner_names.contains(&"Partition 1/etc/fstab".to_string()),
        "进层后应见到分区与卷内条目：{inner_names:?}"
    );
}

/// **编辑保存真的落到卷里**（T-IMG9 的最后一环，core 级端到端）。
///
/// 驱动形态与 GUI 一致：`JobKind::EditBatch` + **档案级**变更集（名字带分区行前缀）。
/// 断言到用户看得见的结果，而不是停在「调用成功」：
///
/// 1. 新条目读得回、旧条目还在（内容也对）；
/// 2. **走的是就地写**：不产出整档副本，故目录里不留 `.hzip-tmp-` 临时档
///    （`rewrite` 路径必然造一个同卷临时档再原子替换）。
///
/// 路径选择由 `rewrite_and_replace` 判定（单格式 + handler 声明可就地编辑 ⇒ 就地写），
/// 而本用例正是那条判定的现实验证：`.raw` 是单格式卷容器。
#[test]
fn edit_batch_inside_a_volume_writes_in_place() {
    use hzip_core::jobs::{JobKind, JobSpec};
    use hzip_format::{Change, ChangeSet};
    use hzip_progress::Outcome;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("disk.img");
    std::fs::write(
        &path,
        raw_with_mbr(FixtureFs::Ext4, &[("/a.txt", b"old" as &[u8])]),
    )
    .unwrap();
    let new_content = dir.path().join("new.txt");
    std::fs::write(&new_content, b"NEW-CONTENT").unwrap();

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, path.clone());
    spec.changes = Some(ChangeSet {
        changes: vec![Change::Append {
            src: new_content,
            name: "Partition 1/new.txt".into(),
        }],
    });
    let h = eng.submit(spec).unwrap();
    let stats = match h.wait().unwrap() {
        Outcome::Done(stats) => stats,
        other => panic!("卷内编辑保存应 Done，实得 {other:?}"),
    };
    // ① **就地写**的判据：写出的字节数只含脏块，而 `rewrite` 路径必然写出**整档**
    //    （镜像 ≥ 8 MiB）。用这条把两条路径区分开——仅看「有没有临时档残留」是
    //    区分不了的（rewrite 成功后临时档会被 rename 掉，同样不留残留）。
    let image_bytes = std::fs::metadata(&path).unwrap().len();
    assert!(
        stats.bytes_done > 0 && stats.bytes_done < image_bytes / 4,
        "就地写应只报脏块字节（{} < {} / 4），实得 {} —— 若报整档说明走了 rewrite 路径",
        image_bytes / 4,
        image_bytes,
        stats.bytes_done
    );

    // ② 读回：新条目在、旧条目与内容都在
    let b = eng
        .browse(hzip_format::Source::File(path.clone()), &Default::default())
        .expect("提交后仍能打开");
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.contains(&"Partition 1/new.txt".to_string()),
        "新条目应在：{names:?}"
    );
    assert!(
        names.contains(&"Partition 1/a.txt".to_string()),
        "旧条目应原样保留：{names:?}"
    );
    let id = b
        .entries()
        .iter()
        .find(|e| e.name.as_str() == "Partition 1/new.txt")
        .map(|e| e.id)
        .unwrap();
    let mut buf = Vec::new();
    use std::io::Read as _;
    b.read_entry(id, PasswordRef::none())
        .expect("读新条目")
        .read_to_end(&mut buf)
        .unwrap();
    assert_eq!(buf, b"NEW-CONTENT", "新条目内容");

    // ③ 就地写：不留 `rewrite` 路径的临时档
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".hzip-tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "就地写不该留临时档：{leftovers:?}");
}

/// **卷内改名**（GUI 最常用的编辑操作）端到端：`.raw` + 真 ext4 驱动 + 写侧路由 + 就地写。
///
/// 夹具故意用**裸叶名**形态的 `to`（`Change::Rename` 的文档契约：驱动按
/// `parent(from) + '/' + to` 拼），而 GUI 传的是完整路径形态——两种都必须能过
/// （路由对两种形态的处理见 `hzip-vol/src/editor.rs::route_change`）。
#[test]
fn edit_batch_renames_a_file_inside_a_volume() {
    use hzip_core::jobs::{JobKind, JobSpec};
    use hzip_format::{Change, ChangeSet};
    use hzip_progress::Outcome;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("disk.img");
    std::fs::write(
        &path,
        raw_with_mbr(FixtureFs::Ext4, &[("/a.txt", b"payload" as &[u8])]),
    )
    .unwrap();

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, path.clone());
    spec.changes = Some(ChangeSet {
        changes: vec![Change::Rename {
            from: "Partition 1/a.txt".into(),
            to: "b.txt".into(), // 裸叶名
        }],
    });
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("卷内改名应 Done，实得 {other:?}"),
    }

    let b = eng
        .browse(hzip_format::Source::File(path.clone()), &Default::default())
        .expect("提交后仍能打开");
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.contains(&"Partition 1/b.txt".to_string()),
        "改名后的条目应在：{names:?}"
    );
    assert!(
        !names.contains(&"Partition 1/a.txt".to_string()),
        "旧名字应消失：{names:?}"
    );
}
