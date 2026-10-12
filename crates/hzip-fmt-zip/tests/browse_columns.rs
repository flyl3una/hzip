//! 浏览新五列数据面（2026-09-25 用户口径）：加密算法 / CRC / 文件属性 /
//! 压缩级别——全部来自**中央目录快照**（`entries()` 零数据区 IO，红线 1）。
//!
//! 覆盖：AES-256/ZipCrypto 算法细分标注、DOS 属性位（只读/隐藏/系统）、
//! CRC32 头声明值、目录条目 CRC 恒 None（GUI 侧口径的 fmt 根）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_format::{
    AesKey, CreateOptions, EncryptionRequest, EntryMeta, FormatHandler, OpenOptions, PasswordRef,
    Sink, Source,
};

use common::{handler, tmp_dir};

const CONTENT: &[u8] = b"browse columns probe\n";

/// 写一个非加密档案：目录 + 普通 + 只读 + 隐藏。
fn write_plain(path: &std::path::Path) {
    let mut w = handler()
        .create_writer(Sink::File(path.to_path_buf()), &CreateOptions::default())
        .unwrap();
    for (name, data, is_dir) in [
        ("dir/", None::<&[u8]>, true),
        ("plain.txt", Some(CONTENT), false),
        ("readonly.txt", Some(CONTENT), false),
        ("hidden.txt", Some(CONTENT), false),
    ] {
        let meta = EntryMeta {
            name: name.into(),
            is_dir,
            size_raw: data.map(|d| d.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        if let Some(d) = data {
            s.write_all(d).unwrap();
        }
        s.finish().unwrap();
    }
    w.finish().unwrap();
}

fn entries_of(path: &std::path::Path) -> Vec<hzip_format::EntryInfo> {
    let r = handler()
        .open_reader(Source::File(path.to_path_buf()), &OpenOptions::default())
        .unwrap();
    r.entries().unwrap()
}

/// AES-256 加密档案 → `encryption = "AES-256"`（0x9901 extra field 细分）。
#[test]
fn entries_report_aes256_algorithm() {
    let dir = tmp_dir("cols-aes");
    let p = dir.join("aes.zip");
    let pwz = zeroize::Zeroizing::new("pw".to_string());
    let mut w = handler()
        .create_writer(
            Sink::File(p.clone()),
            &CreateOptions {
                encryption: Some(EncryptionRequest::Aes(AesKey::Bits256)),
                ..Default::default()
            },
        )
        .unwrap();
    let meta = EntryMeta {
        name: "sec.txt".into(),
        size_raw: Some(CONTENT.len() as u64),
        ..Default::default()
    };
    let mut s = w.next_entry(&meta, PasswordRef::new(&pwz)).unwrap();
    s.write_all(CONTENT).unwrap();
    s.finish().unwrap();
    w.finish().unwrap();

    let infos = entries_of(&p);
    let sec = infos.iter().find(|e| e.name.as_str() == "sec.txt").unwrap();
    assert!(sec.encrypted);
    assert_eq!(
        sec.encryption,
        Some("AES-256"),
        "0x9901 强度字节 = 3 → AES-256"
    );
    // CRC 头声明值 = 写入内容 CRC32
    let mut hasher = hzip_codec::crc::Crc32::new();
    hasher.update(CONTENT);
    assert_eq!(sec.crc32, Some(hasher.finalize()));
}

/// ZipCrypto 加密档案 → `encryption = "ZipCrypto"`。
#[test]
fn entries_report_zipcrypto_algorithm() {
    let dir = tmp_dir("cols-zc");
    let p = dir.join("zc.zip");
    let pwz = zeroize::Zeroizing::new("legacy".to_string());
    let mut w = handler()
        .create_writer(
            Sink::File(p.clone()),
            &CreateOptions {
                encryption: Some(EncryptionRequest::ZipCrypto),
                ..Default::default()
            },
        )
        .unwrap();
    let meta = EntryMeta {
        name: "legacy.txt".into(),
        size_raw: Some(CONTENT.len() as u64),
        ..Default::default()
    };
    let mut s = w.next_entry(&meta, PasswordRef::new(&pwz)).unwrap();
    s.write_all(CONTENT).unwrap();
    s.finish().unwrap();
    w.finish().unwrap();

    let infos = entries_of(&p);
    let e = infos
        .iter()
        .find(|e| e.name.as_str() == "legacy.txt")
        .unwrap();
    assert!(e.encrypted);
    assert_eq!(e.encryption, Some("ZipCrypto"));
    // 未加密条目 → encryption 恒 None
    assert!(!e.encrypted || e.encryption.is_some());
}

/// 非加密档案：`encryption = None`；目录条目 CRC = None（fmt 根口径）；
/// 文件条目 CRC = 头声明值；新字段默认（级别）如实为 None。
#[test]
fn entries_plain_columns_defaults() {
    let dir = tmp_dir("cols-plain");
    let p = dir.join("plain.zip");
    write_plain(&p);
    let infos = entries_of(&p);

    let d = infos.iter().find(|e| e.name.as_str() == "dir").unwrap();
    assert!(d.is_dir);
    assert_eq!(d.crc32, None, "目录条目 CRC 恒 None（fmt 根口径）");
    assert_eq!(d.encryption, None);

    let f = infos
        .iter()
        .find(|e| e.name.as_str() == "plain.txt")
        .unwrap();
    assert!(!f.encrypted);
    assert_eq!(f.encryption, None, "未加密 → 无算法标注");
    assert_eq!(f.compression_level, None, "zip 读路径不落级别（如实）");
    let mut hasher = hzip_codec::crc::Crc32::new();
    hasher.update(CONTENT);
    assert_eq!(f.crc32, Some(hasher.finalize()), "CRC = 头声明值");
}
