//! 组合/嵌套会话写回（T-IMG0）的**引擎级**集成测试。
//!
//! 覆盖三种链形状（`Codec` / `EntryReplace` / `TarRebuild`）、深度拒绝与
//! 失败原子性（TC-CONF-009：任一步失败 ⇒ 最外层字节不变）。
//!
//! 场景口径 = 用户实测（2026-10-16）：`.qcow2.gz` / `.qcow2.zip` 这类
//! 「映像被外层包住」的档案，在分区里改文件后要能写回外层。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

mod common;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use hzip_core::engine::Engine;
use hzip_core::jobs::{JobKind, JobSpec};
use hzip_core::services::{WriteBackChain, WriteBackLayer};
use hzip_format::{
    Change, ChangeSet, EntryMeta, EntryName, FormatHandler, PasswordRef, Sink, Source,
};
use hzip_format_testkit::vol::{FixtureFs, raw_with_mbr};
use hzip_progress::Outcome;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use common::engine;

/// 含 `/note.txt` 的 raw + 真实 ext4 镜像（驱动器夹具）字节。
fn image_with_note(text: &str) -> Vec<u8> {
    raw_with_mbr(FixtureFs::Ext4, &[("/note.txt", text.as_bytes())])
}

fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

fn sha256(path: &Path) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(std::fs::read(path).unwrap());
    h.finalize().to_vec()
}

/// 投影条目 id（与 GUI 行 id 同源：卷容器含分区行）。
fn entry_id(eng: &Engine, archive: &Path, name_suffix: &str) -> hzip_format::EntryId {
    let b = eng
        .browse(Source::File(archive.to_path_buf()), &Default::default())
        .unwrap();
    b.entries()
        .iter()
        .find(|e| e.name.as_str().ends_with(name_suffix))
        .unwrap_or_else(|| panic!("找不到条目 {name_suffix}"))
        .id
}

/// 读条目字节（引擎口径：分区行下的卷内条目）。
fn read_bytes(eng: &Engine, archive: &Path, name_suffix: &str) -> Vec<u8> {
    let b = eng
        .browse(Source::File(archive.to_path_buf()), &Default::default())
        .unwrap();
    let entries = b.entries();
    let info = entries
        .iter()
        .find(|e| e.name.as_str().ends_with(name_suffix))
        .unwrap_or_else(|| panic!("找不到条目 {name_suffix}"));
    let id = info.id;
    let mut buf = Vec::new();
    b.read_entry(id, PasswordRef::none())
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    buf
}

/// 造 zip（真实 ZipFormatHandler 写；条目原序）。
fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut w = hzip_fmt_zip::ZipFormatHandler::new()
        .create_writer(Sink::File(path.to_path_buf()), &Default::default())
        .unwrap();
    for (name, data) in entries {
        let meta = EntryMeta {
            name: EntryName::from(*name),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(data).unwrap();
        s.finish().unwrap();
    }
    w.finish().unwrap();
}

/// 读单文件编码档案（`.gz`）解出的字节。
fn read_gz_single(path: &Path) -> Vec<u8> {
    let r = hzip_fmt_single::gz()
        .open_reader(Source::File(path.to_path_buf()), &Default::default())
        .unwrap();
    let e = r.entries().unwrap().into_iter().next().unwrap();
    let mut buf = Vec::new();
    r.read_entry(e.id, PasswordRef::none())
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    buf
}

/// 把「内层映像」物化到临时档（GUI 会话做的那一步）。
fn materialize(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    write_file(&p, bytes);
    p
}

/// 编辑规格：`source` = 内层物化映像；`changes` = 卷内 Replace；可选写回链。
fn edit_spec(
    eng: &Engine,
    inner: &Path,
    new_note: &Path,
    chain: Option<WriteBackChain>,
) -> JobSpec {
    let note = entry_id(eng, inner, "note.txt");
    let mut spec = JobSpec::new(JobKind::EditBatch, inner.to_path_buf());
    spec.changes = Some(ChangeSet {
        changes: vec![Change::Replace {
            entry: note,
            src: new_note.to_path_buf(),
        }],
    });
    spec.write_back = chain;
    spec
}

/// 形状 1：`disk.img.gz`（纯编码层）——内层改完 ⇒ 外层 gz **全量重压**。
#[test]
fn shape1_edit_inside_gz_wrapped_volume_rewrites_the_outer_file() {
    let dir = TempDir::new().unwrap();
    let image = image_with_note("v1");
    let outer = dir.path().join("disk.img.gz");
    common::make_gz(&outer, &image);
    let inner = materialize(dir.path(), "materialized.img", &image);
    let new_note = dir.path().join("note.txt");
    write_file(&new_note, b"v2");

    let eng = engine(1, 1);
    let spec = edit_spec(
        &eng,
        &inner,
        &new_note,
        Some(WriteBackChain {
            outer: outer.clone(),
            layers: vec![WriteBackLayer::Codec { codec: "gz".into() }],
        }),
    );
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("写回应 Done，实得 {other:?}"),
    }

    // ① 外层仍是可解的 gz，解出来的映像里 /note.txt == v2
    let round = dir.path().join("round.img");
    write_file(&round, &read_gz_single(&outer));
    assert_eq!(read_bytes(&eng, &round, "note.txt"), b"v2");
    // ② 内层物化档自身也被改（同一次变更）
    assert_eq!(read_bytes(&eng, &inner, "note.txt"), b"v2");
}

/// 形状 2：`v.zip` 里的 `disk.img`——只替换该条目，其它条目字节不动。
#[test]
fn shape2_edit_inside_zip_wrapped_volume_replaces_only_that_entry() {
    let dir = TempDir::new().unwrap();
    let image = image_with_note("v1");
    let outer = dir.path().join("v.zip");
    make_zip(&outer, &[("disk.img", &image), ("unrelated.bin", b"keep")]);
    let eng = engine(1, 1);
    let other_before = read_bytes(&eng, &outer, "unrelated.bin");
    let inner = materialize(dir.path(), "materialized.img", &image);
    let new_note = dir.path().join("note.txt");
    write_file(&new_note, b"v2");

    let zip_entry = entry_id(&eng, &outer, "disk.img");
    let spec = edit_spec(
        &eng,
        &inner,
        &new_note,
        Some(WriteBackChain {
            outer: outer.clone(),
            layers: vec![WriteBackLayer::EntryReplace {
                entry: zip_entry,
                entry_name: "disk.img".into(),
            }],
        }),
    );
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("写回应 Done，实得 {other:?}"),
    }

    // ① zip 里的 disk.img 已是编辑后的映像（解出来再读卷内文件）
    let extracted = dir.path().join("from_zip.img");
    write_file(&extracted, &read_bytes(&eng, &outer, "disk.img"));
    assert_eq!(
        std::fs::read(&extracted).unwrap().len(),
        std::fs::read(&inner).unwrap().len(),
        "条目字节 == 编辑后的内层映像"
    );
    assert_eq!(read_bytes(&eng, &extracted, "note.txt"), b"v2");
    // ② 同 zip 其它条目字节逐字不变；条目名集不变
    assert_eq!(
        read_bytes(&eng, &outer, "unrelated.bin"),
        other_before,
        "未命中条目必须无损保留"
    );
}

/// 形状 3：`disk.img.tar`（外层 tar 无条目编辑器）——整档重建 + 替换一条。
#[test]
fn tar_wrapped_volume_rebuilds_the_tar() {
    let dir = TempDir::new().unwrap();
    let image = image_with_note("v1");
    let outer = dir.path().join("disk.tar");
    common::make_tar(
        &outer,
        &[
            ("disk.img".to_string(), image.clone()),
            ("keep.txt".to_string(), b"keep".to_vec()),
        ],
    );
    let eng = engine(1, 1);
    let other_before = read_bytes(&eng, &outer, "keep.txt");
    let inner = materialize(dir.path(), "materialized.img", &image);
    let new_note = dir.path().join("note.txt");
    write_file(&new_note, b"v2");

    let spec = edit_spec(
        &eng,
        &inner,
        &new_note,
        Some(WriteBackChain {
            outer: outer.clone(),
            layers: vec![WriteBackLayer::TarRebuild {
                entry_name: "disk.img".into(),
            }],
        }),
    );
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("写回应 Done，实得 {other:?}"),
    }

    // 重建后的 tar：disk.img 更新、keep.txt 逐字不变、仍可整体浏览
    let extracted = dir.path().join("from_tar.img");
    write_file(&extracted, &read_bytes(&eng, &outer, "disk.img"));
    assert_eq!(read_bytes(&eng, &extracted, "note.txt"), b"v2");
    assert_eq!(read_bytes(&eng, &outer, "keep.txt"), other_before);
}

/// 容器套容器（深度 > 1）⇒ 如实拒绝（`vol.reject.nested-depth`），不静默降级。
#[test]
fn nested_depth_two_is_refused_with_a_reason() {
    let dir = TempDir::new().unwrap();
    let image = image_with_note("v1");
    let outer = dir.path().join("v.tar");
    common::make_tar(&outer, &[("disk.img".to_string(), image.clone())]);
    let inner = materialize(dir.path(), "materialized.img", &image);
    let new_note = dir.path().join("note.txt");
    write_file(&new_note, b"v2");

    let eng = engine(1, 1);
    let spec = edit_spec(
        &eng,
        &inner,
        &new_note,
        Some(WriteBackChain {
            outer: outer.clone(),
            layers: vec![
                WriteBackLayer::TarRebuild {
                    entry_name: "inner.zip".into(),
                },
                WriteBackLayer::EntryReplace {
                    entry: 0,
                    entry_name: "disk.img".into(),
                },
            ],
        }),
    );
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Failed(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("vol.reject.nested-depth"),
                "拒绝理由要可展示，实得 {msg}"
            );
        }
        other => panic!("深度 > 1 应拒绝，实得 {other:?}"),
    }
}

/// 失败原子性（TC-CONF-009）：链中途失败 ⇒ **最外层字节一个都不变**。
#[test]
fn failed_write_back_leaves_the_outer_file_untouched() {
    let dir = TempDir::new().unwrap();
    let image = image_with_note("v1");
    let outer = dir.path().join("v.zip");
    make_zip(&outer, &[("disk.img", &image), ("unrelated.bin", b"keep")]);
    let before = sha256(&outer);
    let inner = materialize(dir.path(), "materialized.img", &image);
    let new_note = dir.path().join("note.txt");
    write_file(&new_note, b"v2");

    let eng = engine(1, 1);
    // 最外层编码层用不存在的编码器 ⇒ 冒泡在**内层已改完**之后失败
    // （正好检验「内层改了、外层必须一字节不动」）
    let spec = edit_spec(
        &eng,
        &inner,
        &new_note,
        Some(WriteBackChain {
            outer: outer.clone(),
            layers: vec![WriteBackLayer::Codec {
                codec: "no-such-codec".into(),
            }],
        }),
    );
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Failed(e) => {
            assert!(
                e.to_string().contains("no-such-codec") || e.to_string().contains("未注册"),
                "失败理由应指向缺编码器，实得 {e}"
            );
        }
        other => panic!("缺编码器应 Failed，实得 {other:?}"),
    }
    assert_eq!(
        read_bytes(&eng, &inner, "note.txt"),
        b"v2",
        "内层确实已改（证明失败发生在冒泡阶段）"
    );
    assert_eq!(sha256(&outer), before, "失败时最外层字节必须不变");
    // 临时档不残留（同目录不得有 .hzip-tmp-*）
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("hzip-tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "临时档应清理干净，实留 {leftovers:?}");
}

/// 加密容器（用户实测 2026-10-19）：`qcow2.zip` 里改/删文件 ⇒ 外层重建必须
/// **带外层密码**去读其它加密条目，并按源算法**重新加密**写回——
/// 否则报「密码错误或者加密数据损坏」。
#[test]
fn write_back_into_an_encrypted_container_re_encrypts() {
    use hzip_format::{AesKey, EncryptionRequest};
    let dir = TempDir::new().unwrap();
    let pw = zeroize::Zeroizing::new("s3cret-pw".to_string());
    let image = image_with_note("v1");
    let outer = dir.path().join("v.zip");
    {
        // AES-256 加密 zip：disk.img（内层映像）+ keep.txt
        let opts = hzip_format::CreateOptions {
            encryption: Some(EncryptionRequest::Aes(AesKey::Bits256)),
            ..Default::default()
        };
        let mut w = hzip_fmt_zip::ZipFormatHandler::new()
            .create_writer(Sink::File(outer.clone()), &opts)
            .unwrap();
        let entries: [(&str, &[u8]); 2] = [("disk.img", &image), ("keep.txt", b"keep")];
        for (name, data) in entries {
            let meta = EntryMeta {
                name: EntryName::from(name),
                size_raw: Some(data.len() as u64),
                ..Default::default()
            };
            let mut s = w.next_entry(&meta, PasswordRef::new(&pw)).unwrap();
            s.write_all(data).unwrap();
            s.finish().unwrap();
        }
        w.finish().unwrap();
    }
    let eng = engine(1, 1);
    // 目录表是明文（AES zip 的中央目录不加密）⇒ 不带密码也能列条目
    let zip_entry = entry_id(&eng, &outer, "disk.img");
    let inner = materialize(dir.path(), "materialized.img", &image);
    let new_note = dir.path().join("note.txt");
    write_file(&new_note, b"v2");

    let mut spec = edit_spec(
        &eng,
        &inner,
        &new_note,
        Some(WriteBackChain {
            outer: outer.clone(),
            layers: vec![WriteBackLayer::EntryReplace {
                entry: zip_entry,
                entry_name: "disk.img".into(),
            }],
        }),
    );
    spec.outer_password = Some(pw.clone());
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("加密容器写回应 Done，实得 {other:?}"),
    }

    // ① 仍能用密码解出 disk.img，且内容 = 编辑后的映像（/note.txt == v2）
    let disk = read_entry_with_password(&outer, "disk.img", &pw);
    let extracted = dir.path().join("from_zip.img");
    write_file(&extracted, &disk);
    assert_eq!(read_bytes(&eng, &extracted, "note.txt"), b"v2");
    // ② 同档其它加密条目内容不变，且**仍可解**（重新加密没破坏它）
    assert_eq!(read_entry_with_password(&outer, "keep.txt", &pw), b"keep");
}

/// 用密码读加密档案的单条目字节。
fn read_entry_with_password(
    archive: &Path,
    name: &str,
    pw: &zeroize::Zeroizing<String>,
) -> Vec<u8> {
    let r = hzip_fmt_zip::ZipFormatHandler::new()
        .open_reader(
            Source::File(archive.to_path_buf()),
            &hzip_format::OpenOptions {
                password: Some(pw.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let entries = r.entries().unwrap();
    let e = entries
        .iter()
        .find(|e| e.name.as_str() == name)
        .unwrap_or_else(|| panic!("找不到条目 {name}"));
    let id = e.id;
    let mut buf = Vec::new();
    r.read_entry(id, PasswordRef::new(pw))
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    buf
}
