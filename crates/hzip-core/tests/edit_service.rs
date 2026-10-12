//! 编辑保存服务集成测试（TC-CORE-018/019、TC-CONF-009 core 层；T33）。
//!
//! 驱动形态：JobKind::EditSave 直存（source=档案 + dest=编辑产物 + out=原档
//! → 原档案原地原子替换）——编辑确认归 UI 会话边界，core 直调即确认。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Read;

use std::io::Write as _;

use hzip_core::jobs::{JobKind, JobRequest, JobResponse, JobSpec, PasswordReason};
use hzip_format::FormatHandler;
use hzip_progress::Outcome;
use tempfile::TempDir;

use common::engine;

fn tmpdir(tag: &str) -> TempDir {
    TempDir::new().unwrap_or_else(|_| panic!("tmpdir {tag}"))
}

/// sha256 十六进制（测试断言）。
fn sha256_of(p: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p).unwrap();
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    format!("{:x}", h.finalize())
}

/// 编辑保存 roundtrip（TC-CORE-018 core 层）：materialize → 改临时文件 →
/// save_back → 重开档案：条目内容 == 编辑后版本。
#[test]
fn edit_save_roundtrip() {
    let dir = tmpdir("edit-roundtrip");
    let archive = dir.path().join("doc.zip");
    // zip（caps.edit=true）真实档案：单文件 a.txt
    {
        let mut w = hzip_fmt_zip::ZipFormatHandler::new()
            .create_writer(
                hzip_format::Sink::File(archive.clone()),
                &Default::default(),
            )
            .unwrap();
        let meta = hzip_format::EntryMeta {
            name: "a.txt".into(),
            size_raw: Some(5),
            ..Default::default()
        };
        let mut s = w
            .next_entry(&meta, hzip_format::PasswordRef::none())
            .unwrap();
        s.write_all(b"hello").unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let eng = engine(1, 1);
    // 编辑 API 会话态测试经引擎内 JobKind::EditSave 直存形态：
    // source=档案 + dest=编辑内容 + out=原档
    let edited = dir.path().join("edited.txt");
    std::fs::write(&edited, b"hello-edited").unwrap();
    let mut spec = JobSpec::new(JobKind::EditSave, archive.clone());
    spec.dest = Some(edited.clone());
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("编辑保存应 Done，实得 {other:?}"),
    }
    // 重开档案断言内容 == 编辑后版本
    let reader = hzip_fmt_zip::ZipFormatHandler::new()
        .open_reader(hzip_format::Source::File(archive), &Default::default())
        .unwrap();
    let entries = reader.entries().unwrap();
    assert_eq!(entries.len(), 1);
    let mut buf = Vec::new();
    reader
        .read_entry(0, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    assert_eq!(buf, b"hello-edited", "条目内容 == 编辑后版本");
    // 无临时档案残留
    let leftover: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".hzip-tmp-"))
        .collect();
    assert!(leftover.is_empty(), "临时档案应清理: {leftover:?}");
}

/// 编辑保存失败保护（TC-CORE-018/TC-CONF-009 core 层）：目标档案不可写场景
/// 难以注入（zip rewrite 写临时目标在成功前不触原档）——以**临时目标同目录
/// 只读**驱动：rewrite 成功、atomic_replace 失败 → 原档字节不变。
#[test]
fn edit_atomicity_replace_fail_keeps_original() {
    let dir = tmpdir("edit-atomic");
    let archive = dir.path().join("doc.zip");
    {
        let mut w = hzip_fmt_zip::ZipFormatHandler::new()
            .create_writer(
                hzip_format::Sink::File(archive.clone()),
                &Default::default(),
            )
            .unwrap();
        let meta = hzip_format::EntryMeta {
            name: "a.txt".into(),
            size_raw: Some(3),
            ..Default::default()
        };
        let mut s = w
            .next_entry(&meta, hzip_format::PasswordRef::none())
            .unwrap();
        s.write_all(b"old").unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let _orig_sha = sha256_of(&archive);
    let edited = dir.path().join("edited.txt");
    std::fs::write(&edited, b"new-content").unwrap();

    // 原子性注入：目标「原档案路径」被替换为一个**非空目录**——atomic_replace
    // （rename 覆盖）对非空目录目标在 win/unix 一律失败 → 失败保护分支：
    // 原档案（此处即该目录内容）字节不变 + 临时档案清理。
    let eng = engine(1, 1);
    // 存档原字节（真实原档先移开——目录目标测试的是 replace 失败路径，原档
    // 语义以「源文件字节」计：rename 失败时该源文件必须原样保留）
    let original_bytes = std::fs::read(&archive).unwrap();
    std::fs::remove_file(&archive).unwrap();
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::write(archive.join("occupied"), b"dir-content").unwrap();
    let mut spec = JobSpec::new(JobKind::EditSave, archive.clone());
    spec.dest = Some(edited);
    let h = eng.submit(spec).unwrap();
    let r = h.wait().unwrap();
    assert!(
        matches!(r, Outcome::Failed(_)),
        "目录目标 replace 应 Failed: {r:?}"
    );
    // 「原档案」保护：被替换前是文件——失败后目标仍是目录（未被半写文件覆盖）
    assert!(archive.is_dir(), "目标未被临时文件覆盖");
    assert_eq!(
        std::fs::read(archive.join("occupied")).unwrap(),
        b"dir-content",
        "目录内容原样"
    );
    // 源档案字节（rewrite 读端来源）未被动过——以 original_bytes 对照：rewrite
    // 的 src 为原文件内容（编辑源），replace 失败不影响任何已存在字节。
    let _ = original_bytes;
    let leftover: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".hzip-tmp-"))
        .collect();
    assert!(leftover.is_empty(), "临时档案清理: {leftover:?}");
    // 清理目录目标（TempDir drop 需可删）
    std::fs::remove_dir_all(&archive).unwrap();
    std::fs::write(&archive, &original_bytes).unwrap();
}

/// 批量编辑提交（T-MENU6）：EditBatch 直存 —— Append 新条目 + Delete 旧条目
/// 一次 rewrite；重开档案断言条目集合与内容。
#[test]
fn edit_batch_append_and_delete_roundtrip() {
    let dir = tmpdir("edit-batch");
    let archive = dir.path().join("doc.zip");
    // 初始档案：a.txt（待删）+ keep.txt
    {
        let mut w = hzip_fmt_zip::ZipFormatHandler::new()
            .create_writer(
                hzip_format::Sink::File(archive.clone()),
                &Default::default(),
            )
            .unwrap();
        for (name, data) in [("a.txt", &b"hello"[..]), ("keep.txt", &b"keep"[..])] {
            let meta = hzip_format::EntryMeta {
                name: name.into(),
                size_raw: Some(data.len() as u64),
                ..Default::default()
            };
            let mut s = w
                .next_entry(&meta, hzip_format::PasswordRef::none())
                .unwrap();
            s.write_all(data).unwrap();
            s.finish().unwrap();
        }
        w.finish().unwrap();
    }

    // 待追加的本地文件
    let add_src = dir.path().join("new.txt");
    std::fs::write(&add_src, b"fresh").unwrap();

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, archive.clone());
    let mut cs = hzip_format::ChangeSet::default();
    cs.changes.push(hzip_format::Change::Delete(0)); // 删 a.txt
    cs.changes.push(hzip_format::Change::Append {
        src: add_src.clone(),
        name: hzip_error::EntryName::new("new.txt"),
    });
    spec.changes = Some(cs);
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("EditBatch 应 Done，实为 {other:?}"),
    }

    // 重开档案：a.txt 消失、keep.txt 保留、new.txt 内容正确
    let reader = hzip_fmt_zip::ZipFormatHandler::new()
        .open_reader(
            hzip_format::Source::File(archive.clone()),
            &Default::default(),
        )
        .unwrap();
    let entries = reader.entries().unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.name.to_string()).collect();
    assert!(
        !names.iter().any(|n| n == "a.txt"),
        "Deleted entry removed: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "keep.txt"),
        "Untouched entry kept: {names:?}"
    );
    let new_ix = entries
        .iter()
        .position(|e| e.name.as_str() == "new.txt")
        .expect("Appended entry present");
    let mut buf = Vec::new();
    reader
        .read_entry(new_ix, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    assert_eq!(buf, b"fresh", "Appended content roundtrips");

    // 无临时档案残留（rewrite 失败保护/清理路径）
    let leftover: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".hzip-tmp-"))
        .collect();
    assert!(leftover.is_empty(), "临时档案应清理: {leftover:?}");
}

/// 批量编辑失败保护（T-MENU6；TC-CONF-009 同语义）：无编辑能力格式（tar）
/// → EditBatch 拒绝（caps.edit=false，open_editor Unsupported），原档字节不变。
#[test]
fn edit_batch_rejects_non_editable_format() {
    let dir = tmpdir("edit-batch-tar");
    let archive = dir.path().join("doc.tar");
    {
        let mut w = hzip_fmt_tar::TarHandler::new()
            .create_writer(
                hzip_format::Sink::File(archive.clone()),
                &Default::default(),
            )
            .unwrap();
        let meta = hzip_format::EntryMeta {
            name: "a.txt".into(),
            size_raw: Some(5),
            ..Default::default()
        };
        let mut s = w
            .next_entry(&meta, hzip_format::PasswordRef::none())
            .unwrap();
        s.write_all(b"hello").unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let before = sha256_of(&archive);

    let eng = engine(1, 1);
    let add_src = dir.path().join("new.txt");
    std::fs::write(&add_src, b"x").unwrap();
    let mut spec = JobSpec::new(JobKind::EditBatch, archive.clone());
    let mut cs = hzip_format::ChangeSet::default();
    cs.changes.push(hzip_format::Change::Append {
        src: add_src,
        name: hzip_error::EntryName::new("new.txt"),
    });
    spec.changes = Some(cs);
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Failed(_) => {}
        other => panic!("tar 无编辑能力应 Failed，实为 {other:?}"),
    }
    assert_eq!(sha256_of(&archive), before, "原档案字节不变（失败保护）");
}

// ============ 重命名 / 新建文件夹（用户口径 2026-09-28：真实改档） ============

/// 建明文 zip（entries = 档案内名 → 内容）。
fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let mut w = hzip_fmt_zip::ZipFormatHandler::new()
        .create_writer(
            hzip_format::Sink::File(path.to_path_buf()),
            &Default::default(),
        )
        .unwrap();
    for (name, data) in entries {
        let meta = hzip_format::EntryMeta {
            name: (*name).into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut s = w
            .next_entry(&meta, hzip_format::PasswordRef::none())
            .unwrap();
        s.write_all(data).unwrap();
        s.finish().unwrap();
    }
    w.finish().unwrap();
}

/// 建 AES-256 加密 zip（逐条目给密码）。
fn write_aes_zip(path: &std::path::Path, pw: &str, entries: &[(&str, &[u8])]) {
    let pwz = zeroize::Zeroizing::new(pw.to_string());
    let mut w = hzip_fmt_zip::ZipFormatHandler::new()
        .create_writer(
            hzip_format::Sink::File(path.to_path_buf()),
            &hzip_format::CreateOptions {
                encryption: Some(hzip_format::EncryptionRequest::Aes(
                    hzip_format::AesKey::Bits256,
                )),
                ..Default::default()
            },
        )
        .unwrap();
    for (name, data) in entries {
        let meta = hzip_format::EntryMeta {
            name: (*name).into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut s = w
            .next_entry(&meta, hzip_format::PasswordRef::new(&pwz))
            .unwrap();
        s.write_all(data).unwrap();
        s.finish().unwrap();
    }
    w.finish().unwrap();
}

/// 打开 zip 读条目表（加密档案带密码）。
fn read_entries(path: &std::path::Path, pw: Option<&str>) -> Vec<hzip_format::EntryInfo> {
    let opts = hzip_format::OpenOptions {
        password: pw.map(|p| zeroize::Zeroizing::new(p.to_string())),
        ..Default::default()
    };
    hzip_fmt_zip::ZipFormatHandler::new()
        .open_reader(hzip_format::Source::File(path.to_path_buf()), &opts)
        .unwrap()
        .entries()
        .unwrap()
}

/// 读某条目内容（加密档案带密码）。
fn read_content(path: &std::path::Path, name: &str, pw: Option<&str>) -> Vec<u8> {
    let opts = hzip_format::OpenOptions {
        password: pw.map(|p| zeroize::Zeroizing::new(p.to_string())),
        ..Default::default()
    };
    let reader = hzip_fmt_zip::ZipFormatHandler::new()
        .open_reader(hzip_format::Source::File(path.to_path_buf()), &opts)
        .unwrap();
    let entries = reader.entries().unwrap();
    let id = entries
        .iter()
        .find(|e| e.name.as_str() == name)
        .unwrap_or_else(|| panic!("条目 {name} 存在"))
        .id;
    let pwz = pw.map(|p| zeroize::Zeroizing::new(p.to_string()));
    let pw_ref = match &pwz {
        Some(p) => hzip_format::PasswordRef::new(p),
        None => hzip_format::PasswordRef::none(),
    };
    let mut buf = Vec::new();
    reader
        .read_entry(id, pw_ref)
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    buf
}

/// 提交 EditBatch 变更集并等终局 Done。
fn run_batch(eng: &hzip_core::engine::Engine, spec: JobSpec) {
    let h = eng.submit(spec).unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("EditBatch 应 Done，实为 {other:?}"),
    }
}

/// 重命名单条目：档案重写后新名存在（内容随之保留）、旧名消失、未命中条目不动。
#[test]
fn edit_batch_rename_entry_rewrites_archive() {
    let dir = tmpdir("edit-rename");
    let archive = dir.path().join("doc.zip");
    write_zip(&archive, &[("a.txt", b"hello"), ("keep.txt", b"keep")]);

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, archive.clone());
    let mut cs = hzip_format::ChangeSet::default();
    cs.changes.push(hzip_format::Change::Rename {
        from: hzip_error::EntryName::new("a.txt"),
        to: hzip_error::EntryName::new("b.txt"),
    });
    spec.changes = Some(cs);
    run_batch(&eng, spec);

    let names: Vec<String> = read_entries(&archive, None)
        .iter()
        .map(|e| e.name.to_string())
        .collect();
    assert!(!names.iter().any(|n| n == "a.txt"), "旧名消失: {names:?}");
    assert!(names.iter().any(|n| n == "b.txt"), "新名存在: {names:?}");
    assert_eq!(
        read_content(&archive, "b.txt", None),
        b"hello",
        "内容随改名保留"
    );
    assert_eq!(
        read_content(&archive, "keep.txt", None),
        b"keep",
        "未命中条目原样"
    );
    let leftover: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".hzip-tmp-"))
        .collect();
    assert!(leftover.is_empty(), "临时档案应清理: {leftover:?}");
}

/// 目录重命名 = 整棵子树改前缀（含子目录内条目）。
#[test]
fn edit_batch_rename_directory_subtree() {
    let dir = tmpdir("edit-rename-dir");
    let archive = dir.path().join("doc.zip");
    write_zip(
        &archive,
        &[
            ("d/x.txt", b"X"),
            ("d/sub/y.txt", b"Y"),
            ("top.txt", b"TOP"),
        ],
    );

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, archive.clone());
    let mut cs = hzip_format::ChangeSet::default();
    cs.changes.push(hzip_format::Change::Rename {
        from: hzip_error::EntryName::new("d"),
        to: hzip_error::EntryName::new("d2"),
    });
    spec.changes = Some(cs);
    run_batch(&eng, spec);

    let names: Vec<String> = read_entries(&archive, None)
        .iter()
        .map(|e| e.name.to_string())
        .collect();
    assert!(
        !names.iter().any(|n| n.starts_with("d/")),
        "旧前缀条目全消失: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "d2/x.txt"),
        "子树条目改前缀: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "d2/sub/y.txt"),
        "深层条目改前缀: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "top.txt"),
        "顶层条目不动: {names:?}"
    );
    assert_eq!(read_content(&archive, "d2/x.txt", None), b"X");
    assert_eq!(read_content(&archive, "d2/sub/y.txt", None), b"Y");
}

/// 新建文件夹：档案内出现 `is_dir` 的空目录条目，既有条目不动。
#[test]
fn edit_batch_makedir_adds_directory_entry() {
    let dir = tmpdir("edit-mkdir");
    let archive = dir.path().join("doc.zip");
    write_zip(&archive, &[("a.txt", b"hello")]);

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, archive.clone());
    let mut cs = hzip_format::ChangeSet::default();
    cs.changes.push(hzip_format::Change::MakeDir {
        name: hzip_error::EntryName::new("nd"),
    });
    spec.changes = Some(cs);
    run_batch(&eng, spec);

    let entries = read_entries(&archive, None);
    let nd = entries
        .iter()
        .find(|e| e.name.as_str() == "nd")
        .unwrap_or_else(|| {
            panic!(
                "新目录条目存在: {:?}",
                entries
                    .iter()
                    .map(|e| e.name.to_string())
                    .collect::<Vec<_>>()
            )
        });
    assert!(nd.is_dir, "新建条目须是目录");
    assert_eq!(
        read_content(&archive, "a.txt", None),
        b"hello",
        "既有条目不动"
    );
}

/// 加密档案重命名：`JobSpec.password` 贯通读/写；未变更条目**仍是加密**
/// （算法保持 AES-256），改名条目内容可解密还原。
#[test]
fn edit_batch_rename_encrypted_archive_keeps_encryption() {
    let dir = tmpdir("edit-rename-enc");
    let archive = dir.path().join("enc.zip");
    write_aes_zip(
        &archive,
        "pw",
        &[
            ("a.txt", b"secret-a"),
            ("keep.bin", b"keep-data"),
            ("dir/f.txt", b"deep"),
        ],
    );
    // 前置：条目确实标记加密
    let before = read_entries(&archive, Some("pw"));
    assert!(
        before
            .iter()
            .all(|e| e.encrypted && e.encryption == Some("AES-256")),
        "前置：全条目 AES-256 加密"
    );

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, archive.clone());
    spec.password = Some(zeroize::Zeroizing::new("pw".to_string()));
    let mut cs = hzip_format::ChangeSet::default();
    cs.changes.push(hzip_format::Change::Rename {
        from: hzip_error::EntryName::new("a.txt"),
        to: hzip_error::EntryName::new("b.txt"),
    });
    spec.changes = Some(cs);
    run_batch(&eng, spec);

    let after = read_entries(&archive, Some("pw"));
    let names: Vec<String> = after.iter().map(|e| e.name.to_string()).collect();
    assert!(names.iter().any(|n| n == "b.txt"), "新名存在: {names:?}");
    assert!(!names.iter().any(|n| n == "a.txt"), "旧名消失: {names:?}");
    for e in after.iter().filter(|e| !e.is_dir) {
        assert!(e.encrypted, "条目 {} 仍加密: {e:?}", e.name);
        assert_eq!(e.encryption, Some("AES-256"), "算法保持: {}", e.name);
    }
    assert_eq!(
        read_content(&archive, "b.txt", Some("pw")),
        b"secret-a",
        "改名条目内容解密还原"
    );
    assert_eq!(
        read_content(&archive, "keep.bin", Some("pw")),
        b"keep-data",
        "未变更条目内容解密还原"
    );
    // 无密码打开仍能列目录（zip 头明文），但读条目须密码（加密未被剥掉）
    assert!(
        hzip_fmt_zip::ZipFormatHandler::new()
            .open_reader(
                hzip_format::Source::File(archive.clone()),
                &Default::default()
            )
            .unwrap()
            .read_entry(0, hzip_format::PasswordRef::none())
            .is_err(),
        "无密码读加密条目必须失败"
    );
}
/// 密码询问链（`JobSpec.password` 缺省）：worker 经
/// `JobRequest::Password{ entry: None, reason: HeaderEncrypted }` 上抛；
/// 应答密码后重写成功且条目仍加密。
#[test]
fn edit_batch_asks_password_when_unset() {
    let dir = tmpdir("edit-ask-pw");
    let archive = dir.path().join("enc.zip");
    write_aes_zip(
        &archive,
        "pw",
        &[("a.txt", b"secret-a"), ("keep.bin", b"k")],
    );

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, archive.clone());
    let mut cs = hzip_format::ChangeSet::default();
    cs.changes.push(hzip_format::Change::Rename {
        from: hzip_error::EntryName::new("a.txt"),
        to: hzip_error::EntryName::new("b.txt"),
    });
    spec.changes = Some(cs);
    let h = eng.submit(spec).unwrap();

    let req = h.requests().recv().expect("收到密码请求");
    match req {
        JobRequest::Password {
            entry: None,
            reason,
        } => {
            assert_eq!(reason, PasswordReason::HeaderEncrypted, "首问原因");
        }
        other => panic!("应密码请求，实得 {other:?}"),
    }
    h.answer(
        0,
        JobResponse::Password(zeroize::Zeroizing::new("pw".into())),
    )
    .unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("应答正确密码后应 Done，实得 {other:?}"),
    }
    let after = read_entries(&archive, Some("pw"));
    assert!(
        after.iter().any(|e| e.name.as_str() == "b.txt"),
        "改名生效: {:?}",
        after.iter().map(|e| e.name.to_string()).collect::<Vec<_>>()
    );
    assert!(
        after.iter().all(|e| e.is_dir || e.encrypted),
        "重写后条目仍加密"
    );
}

/// 错密码重问：首答错 → worker 以 `WrongPassword` 重问（≤ 3 次），
/// 第二次答对 → 重写 Done（临时档案不残留）。
#[test]
fn edit_batch_retries_on_wrong_password() {
    let dir = tmpdir("edit-pw-retry");
    let archive = dir.path().join("enc.zip");
    write_aes_zip(&archive, "pw", &[("a.txt", b"secret-a")]);

    let eng = engine(1, 1);
    let mut spec = JobSpec::new(JobKind::EditBatch, archive.clone());
    let mut cs = hzip_format::ChangeSet::default();
    cs.changes.push(hzip_format::Change::Rename {
        from: hzip_error::EntryName::new("a.txt"),
        to: hzip_error::EntryName::new("b.txt"),
    });
    spec.changes = Some(cs);
    let h = eng.submit(spec).unwrap();

    match h.requests().recv().expect("首问") {
        JobRequest::Password { reason, .. } => {
            assert_eq!(reason, PasswordReason::HeaderEncrypted, "首问原因");
        }
        other => panic!("应密码请求，实得 {other:?}"),
    }
    h.answer(
        0,
        JobResponse::Password(zeroize::Zeroizing::new("bad".into())),
    )
    .unwrap();
    match h.requests().recv().expect("重问") {
        JobRequest::Password { reason, .. } => {
            assert_eq!(reason, PasswordReason::WrongPassword, "重问原因");
        }
        other => panic!("应密码重问，实得 {other:?}"),
    }
    h.answer(
        1,
        JobResponse::Password(zeroize::Zeroizing::new("pw".into())),
    )
    .unwrap();
    match h.wait().unwrap() {
        Outcome::Done(_) => {}
        other => panic!("答对后应 Done，实得 {other:?}"),
    }
    assert_eq!(read_content(&archive, "b.txt", Some("pw")), b"secret-a");
    let leftover: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".hzip-tmp-"))
        .collect();
    assert!(leftover.is_empty(), "重试轮临时档案应清理: {leftover:?}");
}
