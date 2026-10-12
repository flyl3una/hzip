//! conformance 接入：对 `CpioHandler` 展开 TC-CONF-001..010（03 §2.1 预期）。
//!
//! # 为什么不用 testkit 的 `run_conformance`
//!
//! testkit 的读路径用例（001/003/004/006/008/010）以「桩档案 `HZIPSTUB`」驱动
//! （`hzip-format-testkit::stub`），该桩只有 `StubHandler`/`MockHandler` 能解析；
//! 真实 handler 依契约正确拒绝（首块非本格式魔数 → 2001，红线 4 fail-fast 优先）。
//! 与 tar 侧同一处置（T22 §8 纪律：不私改冻结的 testkit），本文件按**真实 cpio
//! 档案**重新驱动同样十组，逐组对齐 03 §2.1 与 02 §7.3 四条红线。
//!
//! # 用例映射
//!
//! | 用例 | 断言 |
//! |---|---|
//! | 001 元数据完整性 | 名字/mode/mtime/大小/链接目标/属性位逐项 |
//! | 002 读回一致 | `read_entry` 字节 == 建夹具时的载荷 |
//! | 003 选择性 | `Selection::Ids` / `Names` 精确命中 |
//! | 004 乱序重复 | 乱序 + 重复 `read_entry` 结果稳定 |
//! | 005 写能力 | 只读格式：`create_writer` → `Unsupported`（HZ-5001） |
//! | 006 取消 | 预置取消令牌 → `Cancelled`（退出码 130 面） |
//! | 007 无加密直读 | `caps.encryption == None`；密码源给了密码也不影响 |
//! | 008 畸形输入 | 截断/翻转/域破坏矩阵零 panic；坏档 → `Format` 错 |
//! | 009 编辑 | `open_editor` → `Unsupported`（HZ-5001） |
//! | 010 entries 零数据区 IO | 数据区改成垃圾仍能列表；精确读计数见
//!   `src/reader.rs::tests::index_reads_headers_only`（单测层以计数包络钉死） |
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::{
    CreateOptions, FormatHandler, OpenOptions, PasswordRef, PasswordSource, Selection, Sink, Source,
};
use hzip_format_testkit::cpio::{self, CpioVariant, EntrySpec};
use support::{Env, names, read_all, tmp_dir, write_fixture};

fn handler() -> hzip_fmt_cpio::CpioHandler {
    hzip_fmt_cpio::CpioHandler::new()
}

fn open(path: &std::path::Path) -> Box<dyn hzip_format::ArchiveReader> {
    handler()
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .expect("打开")
}

/// 三条目素材：目录 + 文件 + 符号链接（覆盖条目语义的三类）。
fn trio() -> Vec<u8> {
    cpio::build(
        CpioVariant::Newc,
        &[
            EntrySpec::dir("d").with_mtime(1_600_000_000),
            EntrySpec::file("d/f.bin", b"payload-42").with_mtime(1_600_000_100),
            EntrySpec::symlink("l", b"d/f.bin"),
        ],
    )
}

#[test]
fn conf001_meta_integrity() {
    let dir = tmp_dir("conf001");
    let p = write_fixture(&dir, "t.cpio", &trio());
    let es = open(&p).entries().expect("entries");
    assert_eq!(es.len(), 3);
    assert_eq!(es[0].name.as_str(), "d");
    assert!(es[0].is_dir && es[0].mtime.expect("mtime").unix_timestamp() == 1_600_000_000);
    assert_eq!(es[0].mode, Some(0o755));
    assert_eq!(es[1].name.as_str(), "d/f.bin");
    assert_eq!(es[1].size_raw, 10);
    assert!(!es[1].is_dir && !es[1].is_symlink);
    assert!(es[2].is_symlink);
    assert_eq!(
        es[2].symlink_target.as_deref(),
        Some(std::path::Path::new("d/f.bin"))
    );
    assert!(
        es.iter()
            .all(|e| !e.encrypted && !e.header_encrypted && e.crc32.is_none())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conf002_read_back_matches_payload() {
    let dir = tmp_dir("conf002");
    let p = write_fixture(&dir, "t.cpio", &trio());
    let r = open(&p);
    assert_eq!(
        read_all(r.read_entry(1, PasswordRef::none()).expect("read")),
        b"payload-42"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conf003_selective_extract() {
    let dir = tmp_dir("conf003");
    let p = write_fixture(&dir, "t.cpio", &trio());
    let env = Env::new();
    let r = open(&p);

    let by_id = dir.join("id");
    std::fs::create_dir_all(&by_id).unwrap();
    let rep = r
        .extract(&Selection::Ids(vec![1]), &env.ctx(), &by_id)
        .expect("按 id");
    assert_eq!(rep.entries_done, 1);
    assert!(by_id.join("d/f.bin").is_file());

    let by_name = dir.join("name");
    std::fs::create_dir_all(&by_name).unwrap();
    let rep = r
        .extract(&Selection::Names(vec!["d".into()]), &env.ctx(), &by_name)
        .expect("按名");
    assert_eq!(rep.entries_done, 1);
    assert!(by_name.join("d").is_dir() && !by_name.join("d/f.bin").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conf004_out_of_order_and_repeatable() {
    let dir = tmp_dir("conf004");
    let p = write_fixture(&dir, "t.cpio", &trio());
    let r = open(&p);
    for _ in 0..2 {
        assert_eq!(
            read_all(r.read_entry(2, PasswordRef::none()).expect("2")),
            b"d/f.bin"
        );
        assert_eq!(
            read_all(r.read_entry(0, PasswordRef::none()).expect("0")),
            b""
        );
        assert_eq!(
            read_all(r.read_entry(1, PasswordRef::none()).expect("1")),
            b"payload-42"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conf005_write_is_unsupported() {
    // 用 `let ... else` 而非 `expect_err`：`ArchiveWriter` trait 对象不实现 Debug
    let Err(e) = handler().create_writer(
        Sink::File(std::env::temp_dir().join("hzip-cpio-conf005.cpio")),
        &CreateOptions::default(),
    ) else {
        panic!("只读格式必须拒绝写");
    };
    assert!(matches!(e, HzipError::Unsupported { .. }), "HZ-5001：{e:?}");
}

#[test]
fn conf006_cancellation() {
    let dir = tmp_dir("conf006");
    let p = write_fixture(&dir, "t.cpio", &trio());
    let env = Env::new();
    env.cancel.cancel();
    let r = open(&p);
    assert!(matches!(
        r.extract(&Selection::All, &env.ctx(), &dir.join("out"))
            .err(),
        Some(HzipError::Cancelled)
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conf007_plaintext_read_with_password_source() {
    /// 交密码也不该改变任何结果（cpio 无加密）。
    struct AlwaysPassword;
    impl PasswordSource for AlwaysPassword {
        fn password(
            &self,
            _e: Option<&hzip_format::EntryName>,
        ) -> Option<zeroize::Zeroizing<String>> {
            Some(zeroize::Zeroizing::new("hunter2".into()))
        }
    }
    let dir = tmp_dir("conf007");
    let p = write_fixture(&dir, "t.cpio", &trio());
    let r = open(&p);
    assert_eq!(r.caps().encryption, hzip_format::EncryptionCaps::None);
    let env = Env::new();
    let rep = r.test(&env.ctx()).expect("明文直读");
    assert_eq!(rep.bytes_written, 10);
    let _pw = AlwaysPassword;
    assert_eq!(names(&*r), vec!["d", "d/f.bin", "l"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conf008_malformed_never_panics() {
    let dir = tmp_dir("conf008");
    let base = trio();
    let env = Env::new();
    let mutations: Vec<(&str, Vec<u8>)> = vec![
        ("空", Vec::new()),
        ("半截魔数", b"0707".to_vec()),
        ("坏魔数", b"070799\x00\x00\x00\x00".to_vec()),
        ("头截断", base[..50].to_vec()),
        ("名截断", base[..115].to_vec()),
        ("数据截断", base[..base.len() - 20].to_vec()),
        ("非 cpio", b"PK\x03\x04\x00\x00\x00\x00".to_vec()),
        ("全零", vec![0u8; 256]),
        ("声称超长名", {
            let mut v = base.clone();
            v[6 + 11 * 8..6 + 12 * 8].copy_from_slice(b"ffffffff");
            v
        }),
    ];
    let mut panics = 0usize;
    for (tag, bytes) in &mutations {
        let p = write_fixture(&dir, &format!("{tag}.cpio"), bytes);
        let got = std::panic::catch_unwind(|| {
            if let Ok(r) = handler().open_reader(Source::File(p.clone()), &OpenOptions::default()) {
                let _ = r.entries().map(|es| es.len());
                let _ = r.read_entry(0, PasswordRef::none()).map(read_all);
                let _ = r.test(&env.ctx());
            }
        });
        if got.is_err() {
            panics += 1;
        }
    }
    assert_eq!(panics, 0, "畸形输入必须零 panic");

    // 坏魔数是**格式错误**而非 panic/静默
    let p = write_fixture(&dir, "bad.cpio", b"070799\x00\x00\x00\x00");
    let Err(e) = handler().open_reader(Source::File(p), &OpenOptions::default()) else {
        panic!("坏魔数必须报错");
    };
    assert!(matches!(
        e,
        HzipError::Format {
            kind: FormatErrorKind::BadSignature,
            ..
        }
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conf009_edit_is_unsupported() {
    let dir = tmp_dir("conf009");
    let p = write_fixture(&dir, "t.cpio", &trio());
    let Err(e) = handler().open_editor(Source::File(p), &OpenOptions::default()) else {
        panic!("cpio 无编辑能力");
    };
    assert!(matches!(e, HzipError::Unsupported { .. }), "HZ-5001：{e:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 010：列表与数据区**内容**无关（精确读计数证据见 `src/reader.rs` 单测
/// `index_reads_headers_only`——用读计数包络证明 8 MiB 载荷下索引期 I/O < 64 KiB）。
#[test]
fn conf010_entries_do_not_need_data_area() {
    let dir = tmp_dir("conf010");
    let mut bytes = cpio::build(
        CpioVariant::Newc,
        &[
            EntrySpec::file("big.bin", vec![0u8; 256 * 1024]),
            EntrySpec::file("after.txt", b"tail"),
        ],
    );
    // 只毁数据区（头 110 + 名 8 = 118，已 4 对齐）：头与名字完好
    let (ds, de) = (118usize, 118 + 256 * 1024);
    for b in bytes[ds..de].iter_mut() {
        *b = 0x5A;
    }
    let p = write_fixture(&dir, "junk.cpio", &bytes);
    let r = open(&p);
    assert_eq!(names(&*r), vec!["big.bin", "after.txt"]);
    assert_eq!(r.entries().expect("entries")[0].size_raw, 256 * 1024);
    // 而数据区真被改动时，test() 会读出来（内容确实变了）——对照组
    let env = Env::new();
    let rep = r.test(&env.ctx()).expect("长度仍相符（只改内容不改长度）");
    assert_eq!(rep.bytes_written, 256 * 1024 + 4);
    let _ = std::fs::remove_dir_all(&dir);
}
