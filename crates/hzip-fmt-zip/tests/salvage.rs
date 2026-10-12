//! 「无目录 zip」salvage 读取（02 §8.1；TC-CONF-008）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_format::{EntryMeta, FormatHandler, PasswordRef, Sink};

use common::{sha256_hex, tmp_dir};

fn find_sig(haystack: &[u8], sig: &[u8]) -> Option<usize> {
    haystack.windows(sig.len()).position(|w| w == sig)
}

/// 完整写档案后截去中央目录区（含 EOCD）→「无目录 zip」。
fn no_dir_zip(dir: &std::path::Path) -> std::path::PathBuf {
    let full = dir.join("full.zip");
    let entries: Vec<(String, Vec<u8>)> = vec![
        ("a.txt".into(), b"hello".to_vec()),
        (
            "b.bin".into(),
            (0..4000u32).map(|i| (i % 251) as u8).collect(),
        ),
        ("sub/c.dat".into(), vec![7u8; 800]),
    ];
    {
        let mut w = common::handler()
            .create_writer(Sink::File(full.clone()), &Default::default())
            .unwrap();
        for (name, data) in &entries {
            let meta = EntryMeta {
                name: name.as_str().into(),
                size_raw: Some(data.len() as u64),
                ..Default::default()
            };
            let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
            s.write_all(data).unwrap();
            s.finish().unwrap();
        }
        w.finish().unwrap();
    }
    let bytes = std::fs::read(&full).unwrap();
    let cd = find_sig(&bytes, b"PK\x01\x02").unwrap();
    let no_dir = dir.join("no-dir.zip");
    std::fs::write(&no_dir, &bytes[..cd]).unwrap();
    no_dir
}

#[test]
fn salvage_no_eocd() {
    let dir = tmp_dir("salvage");
    let p = no_dir_zip(&dir);
    let r = common::reader_of(&p);
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 3, "salvage 恢复 3 条目");
    assert_eq!(common::read_all(r.as_ref(), 0), b"hello");
    let want_b: Vec<u8> = (0..4000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(
        sha256_hex(&common::read_all(r.as_ref(), 1)),
        sha256_hex(&want_b)
    );
    assert_eq!(common::read_all(r.as_ref(), 2), vec![7u8; 800]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn salvage_wrong_content_detects_crc() {
    let dir = tmp_dir("salvage-crc");
    let p = no_dir_zip(&dir);
    let mut bytes = std::fs::read(&p).unwrap();
    // 定位第 2 个本地头（b.bin）数据区并翻转
    let mut count = 0;
    let mut off = 0;
    for (i, w) in bytes.windows(4).enumerate() {
        if w == b"PK\x03\x04" {
            count += 1;
            if count == 2 {
                off = i;
                break;
            }
        }
    }
    let fixed = 26; // 本地头固定区（不含 4B 签名）
    let nlen = u16::from_le_bytes([bytes[off + 26], bytes[off + 27]]) as usize;
    let elen = u16::from_le_bytes([bytes[off + 28], bytes[off + 29]]) as usize;
    let data_start = off + 4 + fixed + nlen + elen;
    bytes[data_start + 5] ^= 0xFF;
    std::fs::write(&p, &bytes).unwrap();

    let r = common::reader_of(&p);
    let mut rd = r.read_entry(1, PasswordRef::none()).unwrap();
    let mut buf = Vec::new();
    let res = std::io::Read::read_to_end(&mut rd, &mut buf);
    assert!(res.is_err(), "数据翻转后 salvage 读应报错");
    std::fs::remove_dir_all(&dir).ok();
}
