use super::*;
use hzip_format::PasswordRef;
use hzip_format_testkit::cpio::{self, CpioVariant, EntrySpec};
use std::io::Read;

fn fixture_bytes() -> Vec<u8> {
    cpio::build(
        CpioVariant::Newc,
        &[
            EntrySpec::dir("sub"),
            EntrySpec::file("sub/a.txt", b"hello cpio"),
            EntrySpec::symlink("link", b"sub/a.txt"),
        ],
    )
}

#[test]
fn id_signature_and_caps() {
    let h = CpioHandler::new();
    assert_eq!(h.id().as_str(), "cpio");
    assert_eq!(h.signature().display, "cpio");
    let c = h.caps();
    assert!(c.read && !c.write && !c.append && !c.edit);
}

/// 只读格式的两个写入口恒 `Unsupported`（HZ-5001）。
#[test]
fn write_paths_are_unsupported() {
    let h = CpioHandler::new();
    let dst = Sink::File(std::env::temp_dir().join("hzip-cpio-never-written.cpio"));
    let e = h.create_writer(dst, &CreateOptions::default()).err();
    assert!(
        matches!(e, Some(HzipError::Unsupported { .. })),
        "create_writer 必须 Unsupported"
    );
    let e = h
        .open_editor(Source::File("x.cpio".into()), &OpenOptions::default())
        .err();
    assert!(
        matches!(e, Some(HzipError::Unsupported { .. })),
        "open_editor 必须 Unsupported"
    );
}

/// `Source::Stream`（组合链内层的形态）能开到条目——`.cpio.gz` 的关键一步。
#[test]
fn opens_from_stream_source() {
    let h = CpioHandler::new();
    let bytes = fixture_bytes();
    let r = h
        .open_reader(
            Source::Stream(Box::new(std::io::Cursor::new(bytes))),
            &OpenOptions::default(),
        )
        .expect("Stream 源打开");
    let entries = r.entries().expect("entries");
    assert_eq!(entries.len(), 3);
    let mut s = r.read_entry(1, PasswordRef::none()).expect("read_entry");
    let mut buf = String::new();
    s.read_to_string(&mut buf).expect("读数据");
    assert_eq!(buf, "hello cpio");
}

/// 坏魔数：首块既非四变体也非空 → 2001（不 panic、不误判）。
#[test]
fn bad_signature_is_rejected() {
    let h = CpioHandler::new();
    let e = h
        .open_reader(
            Source::Stream(Box::new(std::io::Cursor::new(b"not-a-cpio".to_vec()))),
            &OpenOptions::default(),
        )
        .err();
    assert!(matches!(
        e,
        Some(HzipError::Format {
            kind: hzip_error::FormatErrorKind::BadSignature,
            ..
        })
    ));
}

/// 空档（0 字节）= 空列表，不是错误（与 tar 侧同口径）。
#[test]
fn empty_input_lists_nothing() {
    let h = CpioHandler::new();
    let r = h
        .open_reader(
            Source::Stream(Box::new(std::io::Cursor::new(Vec::new()))),
            &OpenOptions::default(),
        )
        .expect("空档可打开");
    assert!(r.entries().expect("entries").is_empty());
}

/// 根条目 `.` 不进列表（GNU `find . | cpio -o` 会写它）。
#[test]
fn root_marker_is_hidden() {
    let h = CpioHandler::new();
    let bytes = cpio::build(
        CpioVariant::Newc,
        &[EntrySpec::dir("."), EntrySpec::file("a", b"x")],
    );
    let r = h
        .open_reader(
            Source::Stream(Box::new(std::io::Cursor::new(bytes))),
            &OpenOptions::default(),
        )
        .expect("打开");
    let names: Vec<String> = r
        .entries()
        .expect("entries")
        .into_iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["a"], "`.` 被隐藏");
    assert_eq!(
        r.read_entry(0, PasswordRef::none())
            .and_then(|mut s| {
                let mut b = Vec::new();
                s.read_to_end(&mut b).map_err(HzipError::from)?;
                Ok(b)
            })
            .expect("读 a"),
        b"x"
    );
}
