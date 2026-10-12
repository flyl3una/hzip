//! TC-CONF-008 CRC 校验 + sink drop 校验链（02 §6.2 双通道）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};

use hzip_error::HzipError;
use hzip_format::{EntryMeta, FormatHandler, PasswordRef, Sink};

use common::tmp_dir;

fn find_sig(haystack: &[u8], sig: &[u8]) -> Option<usize> {
    haystack.windows(sig.len()).position(|w| w == sig)
}

#[test]
fn crc_detect_corruption() {
    // 中央目录 CRC 字段改 1 位 → 读侧 EOF 校验 Checksum 3001（非 panic）
    let dir = tmp_dir("crc");
    let p = common::write_archive(&[(
        "data.bin".into(),
        (0..2000u32).map(|i| (i % 251) as u8).collect(),
    )]);
    let bytes = std::fs::read(&p).unwrap();
    let cd = find_sig(&bytes, b"PK\x01\x02").unwrap();
    let mut corrupt = bytes.clone();
    let crc_off = cd + 16; // 中央目录头 CRC32 字段
    corrupt[crc_off] ^= 0x01;

    let bad = dir.join("corrupt.zip");
    std::fs::write(&bad, &corrupt).unwrap();
    let r = common::reader_of(&bad);
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 1);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut buf = Vec::new();
        r.read_entry(0, PasswordRef::none()).and_then(|mut rd| {
            rd.read_to_end(&mut buf)
                .map(|_| ())
                .map_err(HzipError::from)
        })
    }));
    assert!(outcome.is_ok(), "CRC 不符不得 panic");
    let err = outcome.unwrap().err().unwrap();
    assert_eq!(err.code(), 3001, "应报 Checksum 3001");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}

#[test]
fn sink_drop_crc_chain() {
    // EntrySink 写入 N 字节：zip crate 中央目录 CRC == 旁路 == 源
    let dir = tmp_dir("sinkcrc");
    let src: Vec<u8> = (0..5000u32).map(|i| (i % 253) as u8).collect();
    let want_crc = crc32_of(&src);
    let p = dir.join("ok.zip");
    {
        let mut w = common::handler()
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: "a.bin".into(),
            size_raw: Some(src.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(&src).unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let r = common::reader_of(&p);
    let infos = r.entries().unwrap();
    assert_eq!(
        infos[0].crc32,
        Some(want_crc),
        "zip crate 中央目录 CRC 应等于源 crc32"
    );
    std::fs::remove_dir_all(&dir).ok();
}

fn crc32_of(data: &[u8]) -> u32 {
    let mut c = hzip_codec::crc::Crc32::new();
    c.update(data);
    c.finalize()
}
