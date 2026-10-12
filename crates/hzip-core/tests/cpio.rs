//! cpio 集成测试（02 §8.7）：裸 `.cpio` 四变体 + `.cpio.gz` 组合链的
//! browse / tree / 解压全链路。
//!
//! 素材由 `hzip_format_testkit::cpio` 拼字节（hzip 无 cpio 写侧）；压缩层由
//! flate2 造（与 `.tar.gz` 同构）。用户口径：「能打开 `.cpio.gz` 并展开出 cpio
//! 里的目录结构」——故每个用例都断言到**树形**与**落盘结果**，不停在「能打开」。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_core::jobs::JobSpec;
use hzip_format_testkit::cpio::{self, CpioVariant, EntrySpec};
use hzip_progress::Outcome;
use tempfile::TempDir;

use common::engine;

/// 一份带层级的素材：`init/`（目录）+ `init/bin/app`（可执行）+ `usr/lib/x.so`
/// + 符号链接 + 根自我描述条目 `.`（GNU `find . | cpio -o` 的产物形态）。
fn fixture(variant: CpioVariant, with_link: bool) -> Vec<u8> {
    let mut entries = vec![
        EntrySpec::dir("."),
        EntrySpec::dir("init"),
        EntrySpec::dir("init/bin"),
        EntrySpec::file("init/bin/app", b"#!/bin/sh\necho hi\n").with_mode(0o100755),
        EntrySpec::dir("usr"),
        EntrySpec::dir("usr/lib"),
        EntrySpec::file("usr/lib/x.so", vec![0x7Fu8; 4096]),
    ];
    if with_link {
        entries.push(EntrySpec::symlink("init/init", b"bin/app"));
    }
    cpio::build(variant, &entries)
}

/// gzip 压缩（组合链外层）。
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes).unwrap();
    enc.finish().unwrap()
}

/// 裸 `.cpio`：browse 出条目 + 目录树（目录结构是核心诉求）。
#[test]
fn cpio_bare_browse_builds_tree() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("init.cpio");
    std::fs::write(&p, fixture(CpioVariant::Newc, true)).unwrap();

    let eng = engine(1, 1);
    let b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    assert_eq!(
        b.entry_count(),
        7,
        "根条目 `.` 隐藏；其余 7 条（含隐式目录由显式目录条目表达）"
    );
    let tree = b.tree();
    // root → { init{ bin{ app } , init(链接) }, usr{ lib{ x.so } } }
    assert_eq!(tree.children.len(), 2, "root 两子：init/ + usr/");
    let init = tree.children.get("init").expect("init 目录");
    assert!(init.children.contains_key("bin"), "init/bin 目录");
    assert!(
        init.children.contains_key("init"),
        "init/init 链接条目在树里"
    );
    let bin = init.children.get("bin").expect("bin");
    assert!(bin.children.contains_key("app"));
    let usr = tree.children.get("usr").expect("usr");
    let lib = usr.children.get("lib").expect("lib");
    assert!(lib.children.contains_key("x.so"));
}

/// `.cpio.gz`：组合链（gz 解层 → cpio 内层）browse + 树。
#[test]
fn cpio_gz_composite_browse_builds_tree() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("initramfs.cpio.gz");
    std::fs::write(&p, gzip(&fixture(CpioVariant::Newc, true))).unwrap();

    let eng = engine(1, 1);
    let b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    assert_eq!(b.entry_count(), 7, "组合链透传 cpio 内层条目");
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(names.contains(&"init/bin/app".to_string()), "{names:?}");
    let tree = b.tree();
    assert!(
        tree.children
            .get("init")
            .and_then(|n| n.children.get("bin"))
            .is_some_and(|n| n.children.contains_key("app")),
        "树形层级：init/bin/app"
    );
}

/// `.cpio.gz` 端到端解压：走完整管道（sniff → 组合装配 → 预检 → 落盘）。
#[test]
fn cpio_gz_extract_end_to_end() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("payload.cpio.gz");
    // 素材不含符号链接：core 的链接策略（默认恢复链接）在无开发者模式的 Windows
    // 上必然失败，与本用例要证的「cpio 组合链能解出目录结构」无关；链接条目在
    // 浏览用例（树里出现 init/init）与 fmt 侧解析用例中另证。
    std::fs::write(&p, gzip(&fixture(CpioVariant::Newc, false))).unwrap();
    let dest = dir.path().join("out");

    let opts = hzip_core::services::extract::ExtractOptions {
        dest_root: Some(dest.clone()),
        conflict: hzip_format::ConflictPolicy::Overwrite,
        ..Default::default()
    };
    let eng = engine(1, 1);
    let h = eng.submit(JobSpec::extract_job(p, opts)).unwrap();
    let outcome = h.wait().unwrap();
    let Outcome::Done(stats) = &outcome else {
        panic!("解压应完成，实得 {outcome:?}");
    };
    assert_eq!(stats.entries_done, 6, "6 个条目（不含隐藏的根条目）");
    assert_eq!(
        std::fs::read(dest.join("init/bin/app")).unwrap(),
        b"#!/bin/sh\necho hi\n"
    );
    assert!(dest.join("init/bin").is_dir() && dest.join("usr/lib").is_dir());
    assert_eq!(
        std::fs::read(dest.join("usr/lib/x.so")).unwrap().len(),
        4096
    );
}

/// 四变体都经嗅探识别并列出（魔数表 + 各变体解析路径的端到端覆盖）。
#[test]
fn all_cpio_variants_open_and_list() {
    let dir = TempDir::new().unwrap();
    for (variant, tag) in [
        (CpioVariant::Newc, "newc"),
        (CpioVariant::Crc, "crc"),
        (CpioVariant::Odc, "odc"),
        (CpioVariant::BinLe, "bin-le"),
        (CpioVariant::BinBe, "bin-be"),
    ] {
        for (suffix, bytes) in [
            ("cpio", fixture(variant, true)),
            ("cpio.gz", gzip(&fixture(variant, true))),
        ] {
            let p = dir.path().join(format!("{tag}.{suffix}"));
            std::fs::write(&p, bytes).unwrap();
            let b = engine(1, 1)
                .browse(hzip_format::Source::File(p.clone()), &Default::default())
                .unwrap_or_else(|e| panic!("{tag}.{suffix} 打开失败：{e:?}"));
            assert_eq!(b.entry_count(), 7, "{tag}.{suffix} 条目数");
        }
    }
}

/// 尾块补齐（GNU cpio `-B` / bsdtar 的 512 零块）不产生多余条目。
#[test]
fn blocked_cpio_lists_without_noise() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("blocked.cpio");
    std::fs::write(
        &p,
        cpio::build_blocked(
            CpioVariant::Newc,
            &[EntrySpec::dir("d"), EntrySpec::file("d/a", b"x")],
        ),
    )
    .unwrap();
    let b = engine(1, 1)
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["d", "d/a"], "零填充块不产条目");
}

/// 坏 cpio：报格式错而不是 panic（用户可见为「不支持的格式/档案损坏」）。
#[test]
fn corrupt_cpio_reports_format_error() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("broken.cpio");
    let mut bytes = fixture(CpioVariant::Newc, true);
    // 切在**记录内部**（头/名字中途）：记录边界处的 EOF 是容错收尾（不报错），
    // 只有记录内部截断才判 2002——故本用例切到头中途（50 < 头长 110）
    bytes.truncate(50);
    std::fs::write(&p, bytes).unwrap();
    let Err(e) = engine(1, 1).browse(hzip_format::Source::File(p), &Default::default()) else {
        panic!("截断档案必须报错");
    };
    let msg = format!("{e}");
    assert!(
        msg.contains("截断") || msg.contains("2002") || msg.contains("格式"),
        "错误信息应指明截断/格式：{msg}"
    );
}
