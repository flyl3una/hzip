//! TC-X-012 deflate64 读兼容（黄金样本 7-Zip Deflate64 生成；写永不 deflate64）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_codec::level::Level;
use hzip_format::{CreateOptions, EntryMeta, FormatHandler, PasswordRef, Sink};

// 黄金样本（7-Zip 24.07 `-mm=Deflate64 -mx=9`）：
//   deflate64-7z.zip 内条目 d64sample.bin（方法 9）
//   deflate64-7z.plain.bin：原始内容
const D64_ZIP: &[u8] = include_bytes!("fixtures/deflate64-7z.zip");
const D64_PLAIN: &[u8] = include_bytes!("fixtures/deflate64-7z.plain.bin");

fn write_fixture(bytes: &[u8]) -> std::path::PathBuf {
    let dir = common::tmp_dir("d64");
    let p = dir.join("d64.zip");
    std::fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn deflate64_read_compat() {
    let p = write_fixture(D64_ZIP);
    let h = common::handler();
    let r = h
        .open_reader(hzip_format::Source::File(p.clone()), &Default::default())
        .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(
        infos[0].codec.as_deref(),
        Some("deflate64"),
        "codec 应标 deflate64"
    );
    let got = common::read_all(r.as_ref(), 0);
    assert_eq!(got, D64_PLAIN, "deflate64 样本解压一致");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn deflate64_no_write_path() {
    // 写入永不 deflate64（02 §13.2）：level 映射最高 Ultra → deflate 9，非方法 9
    let dir = common::tmp_dir("d64w");
    let p = dir.join("out.zip");
    let mut w = common::handler()
        .create_writer(
            Sink::File(p.clone()),
            &CreateOptions {
                level: Level::Ultra,
                ..Default::default()
            },
        )
        .unwrap();
    let meta = EntryMeta {
        name: "a.txt".into(),
        size_raw: Some(3),
        ..Default::default()
    };
    let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
    s.write_all(b"abc").unwrap();
    s.finish().unwrap();
    w.finish().unwrap();
    let infos = common::entries(&p);
    assert_eq!(infos[0].codec.as_deref(), Some("deflate"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}
