//! cpio 读取行为测试（02 §8.7 语义面：元数据 / 随机读 / 解压 / 完整性 / 坏档）。
//!
//! 素材由 `hzip_format_testkit::cpio` 直接拼字节（hzip 无 cpio 写侧），
//! 对齐口径已按 GNU cpio 2.14 实测校准。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::Path;

use hzip_error::{FormatErrorKind, HzipError};
use hzip_format::{ArchiveReader, FormatHandler, OpenOptions, PasswordRef, Selection, Source};
use hzip_format_testkit::cpio::{self, CpioVariant, EntrySpec};
use support::{Env, NoPasswordSource, names, read_all, tmp_dir, write_fixture};

fn handler() -> hzip_fmt_cpio::CpioHandler {
    hzip_fmt_cpio::CpioHandler::new()
}

fn open(path: &Path) -> Box<dyn ArchiveReader> {
    handler()
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .expect("打开 cpio")
}

/// 标准素材：目录 + 嵌套文件 + 符号链接 + 空文件 + 根自我描述条目。
fn fixture() -> Vec<u8> {
    cpio::build(
        CpioVariant::Newc,
        &[
            EntrySpec::dir(".").with_mtime(1_600_000_000),
            EntrySpec::dir("sub").with_mtime(1_600_000_001),
            EntrySpec::file("sub/a.txt", b"hello cpio\n").with_mtime(1_600_000_002),
            EntrySpec::file("empty.bin", b""),
            EntrySpec::symlink("link", b"sub/a.txt"),
            EntrySpec::file("mode755", b"#!/bin/sh\n").with_mode(0o100755),
            EntrySpec::file("readonly", b"ro").with_mode(0o100444),
        ],
    )
}

// ===================== 元数据 =====================
#[test]
fn entries_meta_matrix() {
    let dir = tmp_dir("meta");
    let p = write_fixture(&dir, "a.cpio", &fixture());
    let r = open(&p);
    let es = r.entries().expect("entries");
    // 根条目 `.` 被隐藏；其余 6 条按归档顺序
    assert_eq!(
        names(&*r),
        vec![
            "sub",
            "sub/a.txt",
            "empty.bin",
            "link",
            "mode755",
            "readonly"
        ],
        "`.` 不展示；目录/文件/链接名原样（`./` 前缀已由 parse_entry_name 剥除）"
    );
    let sub = &es[0];
    assert!(sub.is_dir && !sub.is_symlink);
    assert_eq!(sub.size_raw, 0, "目录不报数据量");
    assert_eq!(sub.mode, Some(0o755), "mode 取权限位");
    assert_eq!(
        sub.mtime.expect("mtime").unix_timestamp(),
        1_600_000_001,
        "unix 秒如实还原"
    );
    let f = &es[1];
    assert!(!f.is_dir && !f.is_symlink);
    assert_eq!(
        f.size_raw, 11,
        "文件大小 = 头 filesize（\"hello cpio\\n\"）"
    );
    assert!(f.size_packed > f.size_raw, "占用含头/名字/填充");
    assert_eq!(f.attributes.executable, Some(false));
    let link = &es[3];
    assert!(link.is_symlink, "mode 类型位 S_IFLNK");
    assert_eq!(
        link.symlink_target.as_deref(),
        Some(Path::new("sub/a.txt")),
        "链接目标取自数据区"
    );
    assert_eq!(es[4].attributes.executable, Some(true), "mode 755 → 可执行");
    assert!(es[5].attributes.read_only, "mode 444 → 只读");
    assert!(!es[1].attributes.read_only, "mode 644 → 非只读");
    assert!(es.iter().all(|e| !e.encrypted && !e.header_encrypted));
    assert!(es.iter().all(|e| e.codec.as_deref() == Some("store")));
    let _ = std::fs::remove_dir_all(&dir);
}

// ===================== 随机读（红线 2） =====================
#[test]
fn read_entry_is_random_and_repeatable() {
    let dir = tmp_dir("rand");
    let p = write_fixture(&dir, "a.cpio", &fixture());
    let r = open(&p);
    // 条目序（`.` 已隐藏）：0=sub 1=sub/a.txt 2=empty.bin 3=link 4=mode755 5=readonly
    let order = [1usize, 0, 1, 2, 3, 1];
    let mut seen: Vec<(usize, Vec<u8>)> = Vec::new();
    for &id in &order {
        let bytes = read_all(r.read_entry(id, PasswordRef::none()).expect("read_entry"));
        seen.push((id, bytes));
    }
    let by_id = |id: usize| -> &Vec<u8> {
        seen.iter()
            .find(|(i, _)| *i == id)
            .map(|(_, b)| b)
            .expect("该 id 读过")
    };
    assert_eq!(by_id(1).as_slice(), b"hello cpio\n");
    assert_eq!(by_id(2), &Vec::<u8>::new(), "空文件");
    assert_eq!(by_id(0), &Vec::<u8>::new(), "目录 → 空流");
    assert_eq!(by_id(3).as_slice(), b"sub/a.txt", "链接条目数据 = 目标路径");
    let repeats: Vec<&Vec<u8>> = seen
        .iter()
        .filter(|(i, _)| *i == 1)
        .map(|(_, b)| b)
        .collect();
    assert_eq!(repeats.len(), 3);
    assert!(repeats.windows(2).all(|w| w[0] == w[1]), "重复读一致");
    let _ = std::fs::remove_dir_all(&dir);
}

// ===================== 解压 =====================
#[test]
fn extract_writes_tree_with_mtime() {
    let dir = tmp_dir("extract");
    let p = write_fixture(&dir, "a.cpio", &fixture());
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).expect("dest");
    let env = Env::new();
    let r = open(&p);
    let rep = r
        .extract(&Selection::All, &env.ctx(), &dest)
        .expect("extract");
    assert_eq!(rep.entries_done, 6, "6 个条目");
    assert_eq!(
        std::fs::read(dest.join("sub/a.txt")).expect("读回"),
        b"hello cpio\n"
    );
    assert!(dest.join("sub").is_dir());
    assert!(dest.join("empty.bin").is_file());
    assert_eq!(
        std::fs::read(dest.join("link")).expect("链接条目落盘"),
        b"sub/a.txt"
    );
    let mt = std::fs::metadata(dest.join("sub/a.txt"))
        .expect("元数据")
        .modified()
        .expect("mtime");
    let secs = mt
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix 纪元")
        .as_secs();
    assert_eq!(secs, 1_600_000_002, "mtime 已落到磁盘");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn selection_by_ids_and_names_and_bounds() {
    let dir = tmp_dir("sel");
    let p = write_fixture(&dir, "a.cpio", &fixture());
    let env = Env::new();
    let r = open(&p);

    let dest = dir.join("by-ids");
    std::fs::create_dir_all(&dest).expect("dest");
    let rep = r
        .extract(&Selection::Ids(vec![1, 2]), &env.ctx(), &dest)
        .expect("按 id 解压");
    assert_eq!(rep.entries_done, 2);
    assert_eq!(
        std::fs::read(dest.join("sub/a.txt")).expect("读回"),
        b"hello cpio\n"
    );

    let dest2 = dir.join("by-names");
    std::fs::create_dir_all(&dest2).expect("dest");
    let rep = r
        .extract(
            &Selection::Names(vec!["empty.bin".into()]),
            &env.ctx(),
            &dest2,
        )
        .expect("按名解压");
    assert_eq!(rep.entries_done, 1);
    assert!(dest2.join("empty.bin").exists());

    // 越界 id：报错不 panic
    let e = r
        .extract(&Selection::Ids(vec![99]), &env.ctx(), &dest2)
        .expect_err("越界必须报错");
    assert!(matches!(
        e,
        HzipError::Format {
            kind: FormatErrorKind::BadHeader,
            ..
        }
    ));
    assert!(
        r.read_entry(99, PasswordRef::none()).is_err(),
        "read_entry 越界同样报错"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_reports_payload_bytes() {
    let dir = tmp_dir("test");
    let p = write_fixture(&dir, "a.cpio", &fixture());
    let env = Env::new();
    let r = open(&p);
    let rep = r.test(&env.ctx()).expect("完整性测试");
    assert_eq!(rep.entries_done, 6, "含目录与链接（不读其数据）");
    // a.txt 11 + empty.bin 0 + mode755 10 + readonly 2（目录与链接不贡献载荷）
    assert_eq!(rep.bytes_written, 11 + 10 + 2, "文件载荷字节合计");
    let _ = std::fs::remove_dir_all(&dir);
}

/// crc 变体的 `check` 是**数据字节和**：改一个字节必须报 HZ-3001。
#[test]
fn crc_variant_detects_corrupt_payload() {
    let dir = tmp_dir("crc");
    let mut bytes = cpio::build(CpioVariant::Crc, &[EntrySpec::file("a.txt", b"hello")]);
    let p = write_fixture(&dir, "ok.cpio", &bytes);
    let env = Env::new();
    open(&p).test(&env.ctx()).expect("原档校验通过");

    // 数据区起点：头 110 + 名 6（"a.txt"+NUL）= 116（已 4 对齐，无填充）
    bytes[116] ^= 0xFF;
    let bad = write_fixture(&dir, "bad.cpio", &bytes);
    let e = open(&bad).test(&env.ctx()).expect_err("必须报校验失败");
    match e {
        HzipError::Checksum {
            expected,
            actual,
            entry,
        } => {
            assert_eq!(entry.as_str(), "a.txt");
            // "hello" = 104+101+108+108+111 = 532；首字节 ^0xFF 后 104 → 151
            assert_eq!(expected, 532, "头里的 check = 原数据字节和");
            assert_eq!(actual, 532 - 104 + 151, "实测和随数据改变");
        }
        other => panic!("期望 Checksum，实得 {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 五变体都能索引 + 读回同一份载荷（对齐/字序差异全在这条路径上）。
#[test]
fn all_variants_read_the_same_payload() {
    let dir = tmp_dir("variants");
    let specs = [
        EntrySpec::dir("d"),
        EntrySpec::file("d/x.bin", b"0123456789"),
        EntrySpec::file("d/y.bin", b"z"),
    ];
    for (v, tag) in [
        (CpioVariant::Newc, "newc"),
        (CpioVariant::Crc, "crc"),
        (CpioVariant::Odc, "odc"),
        (CpioVariant::BinLe, "bin-le"),
        (CpioVariant::BinBe, "bin-be"),
    ] {
        let p = write_fixture(&dir, &format!("{tag}.cpio"), &cpio::build(v, &specs));
        let r = open(&p);
        assert_eq!(names(&*r), vec!["d", "d/x.bin", "d/y.bin"], "{tag} 条目名");
        assert_eq!(
            read_all(r.read_entry(1, PasswordRef::none()).expect("read_entry")),
            b"0123456789",
            "{tag} 载荷"
        );
        let env = Env::new();
        let rep = r.test(&env.ctx()).expect("完整性测试");
        assert_eq!(rep.bytes_written, 11, "{tag} 载荷字节合计");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ===================== 坏档与容错 =====================
#[test]
fn truncated_records_are_format_errors() {
    let dir = tmp_dir("bad");
    let env = Env::new();

    // 头被截断（只有 110 字节头，无名字）→ 2002
    let p = write_fixture(
        &dir,
        "hdr.cpio",
        &cpio::header_only(CpioVariant::Newc, "abc", 10),
    );
    let r = open(&p);
    let e = r.entries().expect_err("必须报错");
    assert!(
        matches!(
            e,
            HzipError::Format {
                kind: FormatErrorKind::Truncated,
                ..
            }
        ),
        "头后名字缺失 → 2002：{e:?}"
    );
    let _ = env;

    // 数据区被截断（头 + 名字齐、数据缺）→ 索引 OK（只跳数据区），test() 报 2002
    let p2 = write_fixture(
        &dir,
        "data.cpio",
        &cpio::header_and_name(CpioVariant::Newc, "abc", 10),
    );
    let r2 = open(&p2);
    let es = r2.entries().expect("索引不需要数据区");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].size_raw, 10, "声明大小如实上报");
    assert!(
        r2.test(&env.ctx()).is_err(),
        "test() 发现数据不足 → 2002（不静默半条）"
    );

    // namesize = 0 → 2004
    let mut zero_ns = cpio::header_only(CpioVariant::Newc, "abc", 0);
    // 把 namesize（偏移 6+11*8..6+12*8）改成 0
    zero_ns[6 + 11 * 8..6 + 12 * 8].copy_from_slice(b"00000000");
    let p3 = write_fixture(&dir, "ns0.cpio", &zero_ns);
    assert!(matches!(
        open(&p3).entries().err(),
        Some(HzipError::Format {
            kind: FormatErrorKind::BadHeader,
            ..
        })
    ));

    // namesize 超上限（内存炸弹）→ 2004，且不分配
    let mut huge_ns = cpio::header_only(CpioVariant::Newc, "abc", 0);
    huge_ns[6 + 11 * 8..6 + 12 * 8].copy_from_slice(b"7fffffff");
    let p4 = write_fixture(&dir, "ns-huge.cpio", &huge_ns);
    assert!(matches!(
        open(&p4).entries().err(),
        Some(HzipError::Format {
            kind: FormatErrorKind::BadHeader,
            ..
        })
    ));

    // 变体混用（newc 记录后接 odc 记录）→ 2004
    let mut mixed = cpio::build_without_trailer(CpioVariant::Newc, &[EntrySpec::file("a", b"x")]);
    mixed.extend_from_slice(&cpio::build_without_trailer(
        CpioVariant::Odc,
        &[EntrySpec::file("b", b"y")],
    ));
    let p5 = write_fixture(&dir, "mixed.cpio", &mixed);
    let e5 = open(&p5).entries().expect_err("变体混用必须报错");
    assert!(matches!(
        e5,
        HzipError::Format {
            kind: FormatErrorKind::BadHeader,
            ..
        }
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 缺尾条目：记录边界处 EOF 容错收尾（真实世界存在被截掉的归档尾部）。
#[test]
fn missing_trailer_still_lists_entries() {
    let dir = tmp_dir("notail");
    let bytes = cpio::build_without_trailer(
        CpioVariant::Newc,
        &[EntrySpec::file("a", b"1"), EntrySpec::file("b", b"22")],
    );
    let p = write_fixture(&dir, "notail.cpio", &bytes);
    let r = open(&p);
    assert_eq!(names(&*r), vec!["a", "b"]);
    assert_eq!(
        read_all(r.read_entry(1, PasswordRef::none()).expect("read_entry")),
        b"22"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 尾部 512 零块（GNU cpio `-B` 形态）：零块即结束，不多报条目。
#[test]
fn zero_padded_block_ends_archive() {
    let dir = tmp_dir("blocked");
    let bytes = cpio::build_blocked(CpioVariant::Newc, &[EntrySpec::file("a", b"x")]);
    let p = write_fixture(&dir, "blocked.cpio", &bytes);
    assert_eq!(names(&*open(&p)), vec!["a"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 数据区内容与索引无关（entries 不读数据区）：把数据区改成垃圾仍可列表。
#[test]
fn entries_ignore_data_area_content() {
    let dir = tmp_dir("nocare");
    let mut bytes = cpio::build(
        CpioVariant::Newc,
        &[
            EntrySpec::file("big", vec![0u8; 64 * 1024]),
            EntrySpec::file("t", b"x"),
        ],
    );
    // 数据区 = 头 110 + 名 4（"big"+NUL）= 114 → 补 2 → [116, 116+64KiB)；
    // 只改这一段（**不得**越界到下一个头，否则改的是头不是数据区）
    let (ds, de) = (116usize, 116 + 64 * 1024);
    for b in bytes[ds..de].iter_mut() {
        *b = 0xFF;
    }
    let p = write_fixture(&dir, "junk.cpio", &bytes);
    let r = open(&p);
    let es = r.entries().expect("索引与数据区内容无关");
    assert_eq!(es.len(), 2);
    assert_eq!(es[0].size_raw, 64 * 1024);
    assert_eq!(names(&*r), vec!["big", "t"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 红线 4：畸形输入不 panic（截断 / 位翻转 / 域破坏的定长矩阵）。
#[test]
fn malformed_inputs_never_panic() {
    let dir = tmp_dir("fuzz");
    let base = fixture();
    let env = Env::new();
    let mut cases: Vec<Vec<u8>> = Vec::new();
    // 截断矩阵（含记录内与记录边界）
    for cut in [
        0usize,
        1,
        5,
        6,
        109,
        110,
        111,
        118,
        130,
        base.len() / 2,
        base.len() - 1,
    ] {
        cases.push(base[..cut.min(base.len())].to_vec());
    }
    // 位翻转矩阵（头部区域与名字区域）
    for off in [0usize, 3, 5, 6, 50, 95, 109, 110, 112, 116] {
        let mut v = base.clone();
        v[off] ^= 0xFF;
        cases.push(v);
    }
    // 变体魔数混改
    for m in [
        b"070702".as_slice(),
        b"070707",
        &[0xC7, 0x71, 0x00, 0x00, 0x00, 0x00],
        &[b'P', b'K', 0x03, 0x04, 0x00, 0x00],
        b"000000",
    ] {
        let mut v = base.clone();
        v[..m.len()].copy_from_slice(m);
        cases.push(v);
    }
    // 超长名字字段（不改动实际字节 → 名字读取越界）
    let mut v = base.clone();
    v[6 + 11 * 8..6 + 12 * 8].copy_from_slice(b"000fffff");
    cases.push(v);

    let mut panics = 0usize;
    for (i, bytes) in cases.iter().enumerate() {
        let p = write_fixture(&dir, &format!("case{i}.cpio"), bytes);
        let outcome = std::panic::catch_unwind(|| {
            let Ok(r) = handler().open_reader(Source::File(p.clone()), &OpenOptions::default())
            else {
                return;
            };
            let _ = r.entries();
            for id in [0usize, 1, 5] {
                let _ = r.read_entry(id, PasswordRef::none()).map(read_all);
            }
            let _ = r.test(&env.ctx());
            let dest = p.parent().expect("parent").join(format!("out{i}"));
            let _ = r.extract(&Selection::All, &env.ctx(), &dest);
            let _ = std::fs::remove_dir_all(&dest);
        });
        if outcome.is_err() {
            panics += 1;
            eprintln!("case {i} panicked（{} 字节）", bytes.len());
        }
    }
    assert_eq!(panics, 0, "{} 个畸形样本必须零 panic", cases.len());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 无加密格式：给了密码源也不影响读取（红线 3 的负向面）。
#[test]
fn encryption_caps_none_and_password_is_ignored() {
    let dir = tmp_dir("pw");
    let p = write_fixture(&dir, "a.cpio", &fixture());
    let r = open(&p);
    assert_eq!(
        r.caps().encryption,
        hzip_format::EncryptionCaps::None,
        "cpio 无加密概念"
    );
    let _pw = NoPasswordSource;
    assert_eq!(
        read_all(r.read_entry(1, PasswordRef::none()).expect("read_entry")),
        b"hello cpio\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 取消：预置取消令牌 → `Cancelled`（README 与 05 §退出码契约的 130 面）。
#[test]
fn cancellation_is_honoured() {
    let dir = tmp_dir("cancel");
    let p = write_fixture(&dir, "a.cpio", &fixture());
    let env = Env::new();
    env.cancel.cancel();
    let r = open(&p);
    assert!(matches!(
        r.extract(&Selection::All, &env.ctx(), &dir.join("out"))
            .err(),
        Some(HzipError::Cancelled)
    ));
    assert!(matches!(
        r.test(&env.ctx()).err(),
        Some(HzipError::Cancelled)
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
