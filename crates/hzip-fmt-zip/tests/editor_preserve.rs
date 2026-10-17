//! 编辑重写的**保真**语义（用户口径 2026-09-28：重命名/新建目录不得改动压缩包里
//! 其他信息——其他条目的压缩字节、权限、时间、加密状态与算法）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};

use hzip_format::{
    AesKey, Change, ChangeSet, CreateOptions, EncryptionRequest, EntryMeta, FormatHandler,
    OpenOptions, PasswordRef, Sink, Source, WriteCtx,
};

use common::{handler, tmp_dir};

/// WriteCtx 持有组件（与 editor.rs 同型）。
struct Ctx {
    cancel: hzip_progress::CancellationToken,
    bus: hzip_format::ProgressBus,
    opts: CreateOptions,
}
impl Ctx {
    fn new() -> Self {
        Self {
            cancel: Default::default(),
            bus: Default::default(),
            opts: CreateOptions::default(),
        }
    }
    fn wctx(&self) -> WriteCtx<'_> {
        WriteCtx {
            cancel: &self.cancel,
            progress: &self.bus,
            opts: &self.opts,
        }
    }
}

/// 写档案：`(名, 内容, 是否目录)`；`enc`/`pw` 非空则文件条目加密。
fn write_zip(
    path: &std::path::Path,
    entries: &[(&str, Option<&[u8]>, bool)],
    enc: Option<EncryptionRequest>,
    pw: Option<&str>,
) {
    let pwz = pw.map(|s| zeroize::Zeroizing::new(s.to_string()));
    let mut w = handler()
        .create_writer(
            Sink::File(path.to_path_buf()),
            &CreateOptions {
                encryption: enc,
                ..Default::default()
            },
        )
        .unwrap();
    for (name, data, is_dir) in entries {
        let meta = EntryMeta {
            name: (*name).into(),
            size_raw: data.map(|d| d.len() as u64),
            is_dir: *is_dir,
            // 权限位/时间：保真断言用（重写必须原样带过去）
            mode: Some(0o100_640),
            mtime: time::OffsetDateTime::from_unix_timestamp(1_700_000_000).ok(),
            ..Default::default()
        };
        let r = match (&pwz, is_dir) {
            (Some(p), false) => PasswordRef::new(p),
            _ => PasswordRef::none(),
        };
        let mut s = w.next_entry(&meta, r).unwrap();
        if let Some(d) = data {
            s.write_all(d).unwrap();
        }
        s.finish().unwrap();
    }
    w.finish().unwrap();
}

/// 提交变更集（源 → dst）。
fn edit(
    src: &std::path::Path,
    changes: Vec<Change>,
    dst: &std::path::Path,
    opts: &OpenOptions,
) -> hzip_format::WriteReport {
    let ctx = Ctx::new();
    let editor = handler()
        .open_editor(Source::File(src.to_path_buf()), opts)
        .unwrap();
    editor
        .rewrite(
            ChangeSet { changes },
            Sink::File(dst.to_path_buf()),
            &ctx.wctx(),
        )
        .unwrap()
}

/// 条目表（名 → (是否目录, 原始大小, 内容, 是否加密, 加密算法)）。
#[allow(clippy::type_complexity)]
fn entries(
    path: &std::path::Path,
    pw: Option<&str>,
) -> Vec<(String, bool, u64, Vec<u8>, bool, Option<&'static str>)> {
    let pwz = pw.map(|s| zeroize::Zeroizing::new(s.to_string()));
    let r = handler()
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .unwrap();
    let infos = r.entries().unwrap();
    let mut out = Vec::new();
    for info in &infos {
        let content = if info.is_dir {
            Vec::new()
        } else {
            let mut buf = Vec::new();
            let mut s = r
                .read_entry(
                    info.id,
                    match &pwz {
                        Some(p) => PasswordRef::new(p),
                        None => PasswordRef::none(),
                    },
                )
                .unwrap();
            s.read_to_end(&mut buf).unwrap();
            buf
        };
        out.push((
            info.name.as_str().to_string(),
            info.is_dir,
            info.size_raw,
            content,
            info.encrypted,
            info.encryption,
        ));
    }
    out
}

/// 条目在档案里的**原始（压缩+加密）字节**（逐字节保真断言用）。
fn raw_bytes(path: &std::path::Path, name: &str) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let f = archive.by_name(name).unwrap();
    // zip 9：`data_start()` 返回 `Option<u64>`（本地头已解析 ⇒ 恒 `Some`）
    let start = f.data_start().expect("本地头已解析，数据区偏移必有值");
    let len = f.compressed_size();
    drop(f);
    drop(archive);
    let mut all = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .read_to_end(&mut all)
        .unwrap();
    all[start as usize..(start + len) as usize].to_vec()
}

#[test]
fn rename_file_preserves_untouched_entries_byte_exact() {
    let dir = tmp_dir("editor-preserve");
    let src = dir.join("src.zip");
    write_zip(
        &src,
        &[
            ("a.txt", Some(b"alpha"), false),
            ("d/b.bin", Some(&[0x11u8; 500]), false),
            ("d/c.txt", Some(b"charlie"), false),
        ],
        None,
        None,
    );
    let dst = dir.join("dst.zip");
    let report = edit(
        &src,
        vec![Change::Rename {
            from: "a.txt".into(),
            to: "z.txt".into(),
        }],
        &dst,
        &OpenOptions::default(),
    );
    assert_eq!(report.entries_written, 3);

    let out = entries(&dst, None);
    let names: Vec<&str> = out.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["z.txt", "d/b.bin", "d/c.txt"], "改名 + 原序保持");
    assert_eq!(out[0].3, b"alpha", "改名条目内容不变");
    assert_eq!(out[1].3, vec![0x11u8; 500]);
    assert_eq!(out[2].3, b"charlie");
    // 权限/时间原样带过
    let r = handler()
        .open_reader(Source::File(dst.clone()), &OpenOptions::default())
        .unwrap();
    for info in r.entries().unwrap() {
        assert_eq!(info.mode, Some(0o100_640), "{} 权限位保持", info.name);
        assert_eq!(
            info.mtime.map(|t| t.unix_timestamp()),
            Some(1_700_000_000),
            "{} 时间保持",
            info.name
        );
    }
    // 未命中条目：压缩数据**逐字节**未动（原样拷贝路径的判据）
    for name in ["d/b.bin", "d/c.txt"] {
        assert_eq!(
            raw_bytes(&src, name),
            raw_bytes(&dst, name),
            "{name} 压缩字节应原样拷贝"
        );
    }
    // 旧名不得残留
    let dfile = std::fs::File::open(&dst).unwrap();
    let mut darch = zip::ZipArchive::new(dfile).unwrap();
    assert!(darch.by_name("a.txt").is_err(), "改名后旧名不应残留");
}

#[test]
fn rename_dir_moves_whole_subtree() {
    let dir = tmp_dir("editor-preserve-dir");
    let src = dir.join("src.zip");
    write_zip(
        &src,
        &[
            ("d/", None, true),
            ("d/b.txt", Some(b"bee"), false),
            ("d/sub/", None, true),
            ("d/sub/c.txt", Some(b"cee"), false),
            ("outside.txt", Some(b"out"), false),
        ],
        None,
        None,
    );
    let dst = dir.join("dst.zip");
    edit(
        &src,
        vec![Change::Rename {
            from: "d".into(),
            to: "d2".into(),
        }],
        &dst,
        &OpenOptions::default(),
    );
    let out = entries(&dst, None);
    let names: Vec<(&str, bool)> = out.iter().map(|e| (e.0.as_str(), e.1)).collect();
    assert_eq!(
        names,
        [
            ("d2", true),
            ("d2/b.txt", false),
            ("d2/sub", true),
            ("d2/sub/c.txt", false),
            ("outside.txt", false)
        ],
        "目录改名 = 整棵子树改前缀"
    );
    assert_eq!(out[1].3, b"bee");
    assert_eq!(out[3].3, b"cee");
    assert_eq!(out[4].3, b"out");
    // 前缀相似但不属子树的名字不得被改（`d2x` 不是 `d/` 的后代）
    let src2 = dir.join("src2.zip");
    write_zip(
        &src2,
        &[
            ("d/", None, true),
            ("d/x.txt", Some(b"x"), false),
            ("dd/y.txt", Some(b"y"), false),
        ],
        None,
        None,
    );
    let dst2 = dir.join("dst2.zip");
    edit(
        &src2,
        vec![Change::Rename {
            from: "d".into(),
            to: "e".into(),
        }],
        &dst2,
        &OpenOptions::default(),
    );
    let names2: Vec<String> = entries(&dst2, None).into_iter().map(|e| e.0).collect();
    assert_eq!(names2, ["e", "e/x.txt", "dd/y.txt"]);
}

#[test]
fn make_dir_appends_empty_directory() {
    let dir = tmp_dir("editor-preserve-mkdir");
    let src = dir.join("src.zip");
    write_zip(&src, &[("a.txt", Some(b"alpha"), false)], None, None);
    let dst = dir.join("dst.zip");
    let report = edit(
        &src,
        vec![
            Change::MakeDir {
                name: "新建目录".into(),
            },
            Change::MakeDir {
                name: "子/深层".into(),
            },
        ],
        &dst,
        &OpenOptions::default(),
    );
    assert_eq!(report.entries_written, 3);
    let out = entries(&dst, None);
    let names: Vec<(&str, bool, u64)> = out.iter().map(|e| (e.0.as_str(), e.1, e.2)).collect();
    assert_eq!(
        names,
        [
            ("a.txt", false, 5),
            ("新建目录", true, 0),
            ("子/深层", true, 0)
        ]
    );
    // 既有条目压缩字节未动
    assert_eq!(raw_bytes(&src, "a.txt"), raw_bytes(&dst, "a.txt"));
}

#[test]
fn rename_in_encrypted_archive_keeps_encryption() {
    let dir = tmp_dir("editor-preserve-enc");
    let src = dir.join("src.zip");
    let pw = "hzip-pw-123";
    write_zip(
        &src,
        &[
            ("dir/", None, true),
            ("dir/a.txt", Some(b"secret-content"), false),
            ("b.bin", Some(&[0x22u8; 64]), false),
        ],
        Some(EncryptionRequest::Aes(AesKey::Bits256)),
        Some(pw),
    );
    let dst = dir.join("dst.zip");
    edit(
        &src,
        vec![Change::Rename {
            from: "b.bin".into(),
            to: "renamed.bin".into(),
        }],
        &dst,
        &OpenOptions {
            password: Some(zeroize::Zeroizing::new(pw.to_string())),
            ..Default::default()
        },
    );
    let out = entries(&dst, Some(pw));
    let names: Vec<&str> = out.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["dir", "dir/a.txt", "renamed.bin"]);
    // 未改动条目：内容可读 + 仍加密 + 算法保持 AES-256
    assert_eq!(out[1].3, b"secret-content");
    assert!(out[1].4, "未改动条目必须仍加密");
    assert_eq!(out[1].5, Some("AES-256"), "加密算法必须沿用");
    assert_eq!(out[2].3, vec![0x22u8; 64]);
    assert!(out[2].4, "被改名条目仍加密");
    // 无密码读被拒（加密语义未破）
    let r = handler()
        .open_reader(Source::File(dst.clone()), &OpenOptions::default())
        .unwrap();
    assert!(
        r.read_entry(1, PasswordRef::none()).is_err(),
        "无密码读取必须失败"
    );
}

#[test]
fn non_utf8_names_fall_back_to_streaming_path() {
    // 非 UTF-8 名（gp 位不置 UTF-8 时的原始字节）→ 原样拷贝路径不适用（重写会改
    // 名字节），回退流式重写：改名仍须生效、其他条目内容不变。
    let dir = tmp_dir("editor-preserve-nonutf8");
    let src = dir.join("src.zip");
    write_zip(
        &src,
        &[
            ("a.txt", Some(b"alpha"), false),
            ("q.txt", Some(b"legacy"), false),
        ],
        None,
        None,
    );
    // 把 `q.txt` 的名字字节就地改成非 UTF-8（`0xE9.txt`，等长 → 偏移不变；
    // 本地头 + 中央目录两处都要改）
    {
        let mut bytes = std::fs::read(&src).unwrap();
        let needle = b"q.txt";
        let repl = b"\xE9.txt";
        let mut hits = 0usize;
        for i in 0..bytes.len().saturating_sub(needle.len()) {
            if &bytes[i..i + needle.len()] == needle {
                bytes[i..i + needle.len()].copy_from_slice(repl);
                hits += 1;
            }
        }
        assert_eq!(hits, 2, "本地头 + 中央目录各一处");
        std::fs::write(&src, &bytes).unwrap();
    }
    let dst = dir.join("dst.zip");
    edit(
        &src,
        vec![Change::Rename {
            from: "a.txt".into(),
            to: "z.txt".into(),
        }],
        &dst,
        &OpenOptions::default(),
    );
    let out = entries(&dst, None);
    assert!(
        out.iter().any(|e| e.0 == "z.txt" && e.3 == b"alpha"),
        "改名必须生效（流式回退路径）: {out:?}"
    );
    assert!(
        out.iter().any(|e| e.3 == b"legacy"),
        "非 UTF-8 名条目内容不得丢: {out:?}"
    );
}

#[test]
fn rename_of_missing_name_is_an_error() {
    // 改名目标不存在时**必须报错**：静默「成功」会让用户看到「任务完成但什么
    // 都没改」（用户口径 2026-09-28：保存要生效，不许无声无息）。
    let dir = tmp_dir("editor-preserve-miss");
    let src = dir.join("src.zip");
    write_zip(&src, &[("a.txt", Some(b"alpha"), false)], None, None);
    let dst = dir.join("dst.zip");
    let ctx = Ctx::new();
    let editor = handler()
        .open_editor(Source::File(src.clone()), &OpenOptions::default())
        .unwrap();
    let r = editor.rewrite(
        ChangeSet {
            changes: vec![Change::Rename {
                from: "nope.txt".into(),
                to: "x.txt".into(),
            }],
        },
        Sink::File(dst),
        &ctx.wctx(),
    );
    assert!(r.is_err(), "不存在的改名目标必须报错，实得 {r:?}");
    // 子树前缀相似但不同名也不得误判命中
    let dst2 = dir.join("dst2.zip");
    let editor = handler()
        .open_editor(Source::File(src.clone()), &OpenOptions::default())
        .unwrap();
    let r2 = editor.rewrite(
        ChangeSet {
            changes: vec![Change::Rename {
                from: "a.tx".into(),
                to: "b.tx".into(),
            }],
        },
        Sink::File(dst2),
        &ctx.wctx(),
    );
    assert!(r2.is_err(), "前缀相似名不得算命中");
}
