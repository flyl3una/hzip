//! TC-EDIT-7Z：`ArchiveEditor::rewrite` 的改名 / 目录子树改名 / 新建目录 / 保真
//! （用户口径 2026-09-28）。全部以**真实 7z 自产档案**驱动（同 conformance.rs 策略）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use hzip_fmt_7z::SevenZipFormatHandler;
use hzip_format::{
    AesKey, ArchiveReader, Change, ChangeSet, CreateOptions, EncryptionRequest, EntryMeta,
    FormatHandler, OpenOptions, PasswordRef, Sink, Source, WriteCtx, WriteReport,
};
use time::OffsetDateTime;

/// 单条目写入描述（mtime 以 unix 秒给出）。
struct Spec {
    name: &'static str,
    data: &'static [u8],
    is_dir: bool,
    mode: Option<u32>,
    mtime: Option<i64>,
}

impl Spec {
    fn file(name: &'static str, data: &'static [u8], mode: u32, mtime: i64) -> Self {
        Self {
            name,
            data,
            is_dir: false,
            mode: Some(mode),
            mtime: Some(mtime),
        }
    }
    fn dir(name: &'static str, mode: u32, mtime: i64) -> Self {
        Self {
            name,
            data: b"",
            is_dir: true,
            mode: Some(mode),
            mtime: Some(mtime),
        }
    }
}

struct Ctx {
    dir: PathBuf,
    h: SevenZipFormatHandler,
}

impl Ctx {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "hzip-7z-edit-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            h: SevenZipFormatHandler::new(),
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn write_archive(&self, path: &Path, specs: &[Spec]) -> PathBuf {
        let mut w = self
            .h
            .create_writer(Sink::File(path.to_path_buf()), &CreateOptions::default())
            .unwrap();
        for s in specs {
            let meta = EntryMeta {
                name: s.name.into(),
                is_dir: s.is_dir,
                size_raw: Some(s.data.len() as u64),
                mode: s.mode,
                mtime: s
                    .mtime
                    .map(|secs| OffsetDateTime::from_unix_timestamp(secs).unwrap()),
                ..Default::default()
            };
            let mut sink = w.next_entry(&meta, PasswordRef::none()).unwrap();
            if !s.is_dir {
                sink.write_all(s.data).unwrap();
            }
            sink.finish().unwrap();
        }
        w.finish().unwrap();
        path.to_path_buf()
    }

    fn open(&self, path: &Path) -> Box<dyn ArchiveReader> {
        self.h
            .open_reader(Source::File(path.to_path_buf()), &Default::default())
            .unwrap()
    }

    fn rewrite(&self, src: &Path, dst: &Path, changes: Vec<Change>) -> WriteReport {
        let cancel = hzip_progress::CancellationToken::new();
        let bus = hzip_format::ProgressBus::new();
        let opts = CreateOptions::default();
        let ctx = WriteCtx {
            cancel: &cancel,
            progress: &bus,
            opts: &opts,
        };
        let editor = self
            .h
            .open_editor(Source::File(src.to_path_buf()), &Default::default())
            .unwrap();
        editor
            .rewrite(ChangeSet { changes }, Sink::File(dst.to_path_buf()), &ctx)
            .unwrap()
    }

    /// 带密码打开（加密档案读路径）。
    fn open_with_pw(&self, path: &Path, pw: &str) -> Box<dyn ArchiveReader> {
        self.h
            .open_reader(
                Source::File(path.to_path_buf()),
                &OpenOptions {
                    password: Some(zeroize::Zeroizing::new(pw.to_string())),
                    ..Default::default()
                },
            )
            .unwrap()
    }

    /// 以可选密码 open_editor 后重写（`None` = 不带密码，用于负断言）。
    fn rewrite_with_pw(
        &self,
        src: &Path,
        dst: &Path,
        changes: Vec<Change>,
        pw: Option<&str>,
    ) -> Result<WriteReport, hzip_error::HzipError> {
        let cancel = hzip_progress::CancellationToken::new();
        let bus = hzip_format::ProgressBus::new();
        let opts = CreateOptions::default();
        let ctx = WriteCtx {
            cancel: &cancel,
            progress: &bus,
            opts: &opts,
        };
        let open = OpenOptions {
            password: pw.map(|p| zeroize::Zeroizing::new(p.to_string())),
            ..Default::default()
        };
        let editor = self.h.open_editor(Source::File(src.to_path_buf()), &open)?;
        editor.rewrite(ChangeSet { changes }, Sink::File(dst.to_path_buf()), &ctx)
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read_entry(r: &dyn ArchiveReader, id: usize) -> Vec<u8> {
    let mut b = Vec::new();
    r.read_entry(id, PasswordRef::none())
        .unwrap()
        .read_to_end(&mut b)
        .unwrap();
    b
}

fn names(r: &dyn ArchiveReader) -> Vec<String> {
    r.entries()
        .unwrap()
        .iter()
        .map(|i| i.name.as_str().to_string())
        .collect()
}

fn id_of(r: &dyn ArchiveReader, name: &str) -> usize {
    r.entries()
        .unwrap()
        .iter()
        .position(|i| i.name.as_str() == name)
        .unwrap_or_else(|| panic!("条目 {name} 不存在"))
}

const X: &[u8] = b"AAAA-content-X";
const Y: &[u8] = b"BBBB-content-Y";
const T_A: i64 = 1_700_000_000;
const T_B: i64 = 1_699_500_000;
const T_D: i64 = 1_699_000_000;

/// 基础档案：`a.txt`(X) + 目录 `d` + `d/b.txt`(Y)。
fn base(ctx: &Ctx, name: &str) -> PathBuf {
    ctx.write_archive(
        &ctx.file(name),
        &[
            Spec::file("a.txt", X, 0o644, T_A),
            Spec::dir("d", 0o755, T_D),
            Spec::file("d/b.txt", Y, 0o600, T_B),
        ],
    )
}

/// (a) 普通 7z：`a.txt` → `c.txt`（内容一致，`a.txt` 消失，`d/b.txt` 不受影响）；
/// (d) 保真：未变更条目的 mode/mtime 与源一致。
#[test]
fn rename_file_preserves_others() {
    let c = Ctx::new("rename-file");
    let src = base(&c, "in.7z");
    let dst = c.file("out.7z");

    let rep = c.rewrite(
        &src,
        &dst,
        vec![Change::Rename {
            from: "a.txt".into(),
            to: "c.txt".into(),
        }],
    );
    assert_eq!(rep.entries_written, 3, "改名不应增减条目");

    let out = c.open(&dst);
    assert_eq!(names(out.as_ref()), vec!["c.txt", "d", "d/b.txt"]);
    assert!(!names(out.as_ref()).iter().any(|n| n == "a.txt"));
    // 只改名：内容字节一致
    assert_eq!(read_entry(out.as_ref(), id_of(out.as_ref(), "c.txt")), X);
    // 其它条目内容不变
    assert_eq!(read_entry(out.as_ref(), id_of(out.as_ref(), "d/b.txt")), Y);

    // 保真：未变更条目 `d/b.txt` 的 mode/mtime 与源档案一致
    let src_reader = c.open(&src);
    let src_infos = src_reader.entries().unwrap();
    let out_infos = out.entries().unwrap();
    let s_b = src_infos
        .iter()
        .find(|i| i.name.as_str() == "d/b.txt")
        .unwrap();
    let o_b = out_infos
        .iter()
        .find(|i| i.name.as_str() == "d/b.txt")
        .unwrap();
    assert_eq!(o_b.mode, s_b.mode, "mode 保真");
    assert_eq!(o_b.mtime, s_b.mtime, "mtime 保真");
    assert_eq!(o_b.mode, Some(0o600));
    assert_eq!(
        o_b.mtime,
        Some(OffsetDateTime::from_unix_timestamp(T_B).unwrap())
    );
    // 目录条目同样是元数据保真
    let s_d = src_infos.iter().find(|i| i.name.as_str() == "d").unwrap();
    let o_d = out_infos.iter().find(|i| i.name.as_str() == "d").unwrap();
    assert_eq!(o_d.mode, s_d.mode);
    assert_eq!(o_d.mtime, s_d.mtime);
    assert!(o_d.is_dir);
}

/// (b) 目录子树改名：`d` → `d2`（目录条目本身 + 前缀子树一起改）。
#[test]
fn rename_directory_subtree() {
    let c = Ctx::new("rename-dir");
    let src = base(&c, "in.7z");
    let dst = c.file("out.7z");

    let rep = c.rewrite(
        &src,
        &dst,
        vec![Change::Rename {
            from: "d".into(),
            to: "d2".into(),
        }],
    );
    assert_eq!(rep.entries_written, 3);

    let out = c.open(&dst);
    assert_eq!(names(out.as_ref()), vec!["a.txt", "d2", "d2/b.txt"]);
    assert_eq!(read_entry(out.as_ref(), id_of(out.as_ref(), "d2/b.txt")), Y);
    let infos = out.entries().unwrap();
    let d2 = infos.iter().find(|i| i.name.as_str() == "d2").unwrap();
    assert!(d2.is_dir, "目录条目本身也应改名并保持目录性");
    // 内容与元数据不因改名而变
    assert_eq!(d2.mode, Some(0o755));
    assert_eq!(read_entry(out.as_ref(), id_of(out.as_ref(), "a.txt")), X);
}

/// (c) `MakeDir{name}` → 产物多一个 `is_dir` 空目录条目，原条目不受影响。
#[test]
fn make_dir_appends_directory_entry() {
    let c = Ctx::new("mkdir");
    let src = base(&c, "in.7z");
    let dst = c.file("out.7z");

    let rep = c.rewrite(
        &src,
        &dst,
        vec![Change::MakeDir {
            name: "newdir".into(),
        }],
    );
    assert_eq!(rep.entries_written, 4);

    let out = c.open(&dst);
    assert_eq!(names(out.as_ref()), vec!["a.txt", "d", "d/b.txt", "newdir"]);
    let infos = out.entries().unwrap();
    let nd = infos.iter().find(|i| i.name.as_str() == "newdir").unwrap();
    assert!(nd.is_dir, "新建条目必须是目录");
    assert_eq!(nd.size_raw, 0);
    assert!(nd.mtime.is_some(), "新建目录时间戳取执行时刻");
    assert_eq!(read_entry(out.as_ref(), id_of(out.as_ref(), "a.txt")), X);
}

/// 改名引用不存在的条目名 → `Format{BadHeader}`（同 id 越界语义）。
#[test]
fn rename_missing_name_is_bad_header() {
    let c = Ctx::new("rename-missing");
    let src = base(&c, "in.7z");
    let dst = c.file("out.7z");

    let cancel = hzip_progress::CancellationToken::new();
    let bus = hzip_format::ProgressBus::new();
    let opts = CreateOptions::default();
    let ctx = WriteCtx {
        cancel: &cancel,
        progress: &bus,
        opts: &opts,
    };
    let editor =
        c.h.open_editor(Source::File(src.clone()), &Default::default())
            .unwrap();
    let res = editor.rewrite(
        ChangeSet {
            changes: vec![Change::Rename {
                from: "nope.txt".into(),
                to: "x.txt".into(),
            }],
        },
        Sink::File(dst),
        &ctx,
    );
    assert!(
        matches!(res.as_ref(), Err(e) if e.code() == 2004),
        "未命中的改名应 BadHeader(2004)，实得 {res:?}"
    );
}

// ---- 加密档案重写（Change 项 3/4：加密状态与密码通道）----

const PW: &str = "hzip-7z-enc-pw";

/// 写加密 7z（数据加密、文件名明文）：目录 `d` + `d/top.txt`(X)。
fn write_encrypted(ctx: &Ctx, path: &Path) -> PathBuf {
    let pw = zeroize::Zeroizing::new(PW.to_string());
    let mut w = ctx
        .h
        .create_writer(
            Sink::File(path.to_path_buf()),
            &CreateOptions {
                encryption: Some(EncryptionRequest::Aes(AesKey::Bits256)),
                encrypt_names: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    for (name, data, is_dir) in [("d", b"".as_slice(), true), ("d/top.txt", X, false)] {
        let meta = EntryMeta {
            name: name.into(),
            is_dir,
            size_raw: Some(data.len() as u64),
            mode: Some(if is_dir { 0o755 } else { 0o644 }),
            ..Default::default()
        };
        let mut sink = w
            .next_entry(
                &meta,
                if is_dir {
                    PasswordRef::none()
                } else {
                    PasswordRef::new(&pw)
                },
            )
            .unwrap();
        if !is_dir {
            sink.write_all(data).unwrap();
        }
        sink.finish().unwrap();
    }
    w.finish().unwrap();
    path.to_path_buf()
}

/// 带密码读条目内容。
fn read_entry_pw(r: &dyn ArchiveReader, id: usize, pw: &str) -> Vec<u8> {
    let pw = zeroize::Zeroizing::new(pw.to_string());
    let mut b = Vec::new();
    r.read_entry(id, PasswordRef::new(&pw))
        .unwrap()
        .read_to_end(&mut b)
        .unwrap();
    b
}

/// 加密档案重写：改名生效、内容一致、产物**仍加密**（无密码读内容报错）；
/// 无密码重写加密档案 → 明确 `Password` 错（不静默产明文档）。
#[test]
fn rewrite_encrypted_archive_keeps_encryption() {
    let c = Ctx::new("enc-rewrite");
    let src = write_encrypted(&c, &c.file("enc.7z"));
    let dst = c.file("out.7z");

    let changes = vec![Change::Rename {
        from: "d/top.txt".into(),
        to: "d/renamed.txt".into(),
    }];

    let missing = c.rewrite_with_pw(&src, &c.file("bad.7z"), changes.clone(), None);
    assert!(
        matches!(missing, Err(hzip_error::HzipError::Password { .. })),
        "无密码重写加密档案应报 Password 错，实得 {missing:?}"
    );

    let rep = c.rewrite_with_pw(&src, &dst, changes, Some(PW)).unwrap();
    assert_eq!(rep.entries_written, 2);

    let out = c.open_with_pw(&dst, PW);
    assert_eq!(names(out.as_ref()), vec!["d", "d/renamed.txt"]);
    let infos = out.entries().unwrap();
    let renamed = infos
        .iter()
        .find(|i| i.name.as_str() == "d/renamed.txt")
        .unwrap();
    assert!(renamed.encrypted, "重写后加密条目必须仍加密");
    assert_eq!(renamed.encryption, Some("AES-256"));
    assert_eq!(
        read_entry_pw(out.as_ref(), id_of(out.as_ref(), "d/renamed.txt"), PW),
        X
    );

    // 内容确实加密（而非仅标志位）：无密码读条目内容报错
    let plain = c.open(&dst);
    let id = id_of(plain.as_ref(), "d/renamed.txt");
    let mut buf = Vec::new();
    let res = plain
        .read_entry(id, PasswordRef::none())
        .and_then(|mut s| s.read_to_end(&mut buf).map_err(hzip_error::HzipError::from));
    assert!(res.is_err(), "无密码读取加密条目应报错");
}
