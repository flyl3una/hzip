//! TC-ALG-001 级别 roundtrip / TC-CONF-002 / TC-CONF-010 / 长名 / 空档案 / 目录。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_codec::level::Level;
use hzip_format::{CreateOptions, EntryMeta, FormatHandler, PasswordRef, Sink, Source};

use common::{handler, read_entry_file, sha256_hex, tmp_dir, write_archive};

fn sample_entries() -> Vec<(String, Vec<u8>)> {
    vec![
        ("hello.txt".into(), b"hello world\n".to_vec()),
        ("empty.dat".into(), Vec::new()),
        (
            "bin/data.bin".into(),
            (0..256u32).map(|i| (i % 251) as u8).collect(),
        ),
        ("docs/name-cn.txt".into(), "中文内容".as_bytes().to_vec()),
        ("a/b/c/d/deep.txt".into(), b"deep".to_vec()),
        ("zeros.bin".into(), vec![0u8; 65536]),
    ]
}

#[test]
fn roundtrip_store_deflate_levels() {
    // 六级别全部 roundtrip 一致（TC-ALG-001：全零样本压缩率随级别单调不减）
    let dir = tmp_dir("levels");
    let samples = sample_entries();
    let mut sizes = Vec::new();
    let all_zero = vec![0u8; 131072];
    for (i, level) in [
        Level::Store,
        Level::Fastest,
        Level::Fast,
        Level::Normal,
        Level::Maximum,
        Level::Ultra,
    ]
    .iter()
    .enumerate()
    {
        let p = dir.join(format!("l{i}.zip"));
        let mut w = handler()
            .create_writer(
                Sink::File(p.clone()),
                &CreateOptions {
                    level: *level,
                    ..Default::default()
                },
            )
            .unwrap();
        for (name, data) in &samples {
            let meta = EntryMeta {
                name: name.as_str().into(),
                size_raw: Some(data.len() as u64),
                ..Default::default()
            };
            let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
            s.write_all(data).unwrap();
            s.finish().unwrap();
        }
        let meta = EntryMeta {
            name: "allzero.bin".into(),
            size_raw: Some(all_zero.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(&all_zero).unwrap();
        s.finish().unwrap();
        w.finish().unwrap();

        for (name, data) in &samples {
            let got = read_zip_entry(&p, name);
            assert_eq!(sha256_hex(&got), sha256_hex(data), "{level:?} {name}");
        }
        let z = read_zip_entry(&p, "allzero.bin");
        assert_eq!(z.len(), all_zero.len());
        sizes.push(std::fs::metadata(&p).unwrap().len());
    }
    // deflate 各级全零样本为主 → zip 文件大小应单调不增（1..5）
    for w in sizes.windows(2) {
        assert!(
            w[1] <= w[0] + 64,
            "deflate 级别压缩率应单调不减（zip 大小 {sizes:?}）"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

fn read_zip_entry(p: &std::path::Path, name: &str) -> Vec<u8> {
    let r = handler()
        .open_reader(Source::File(p.to_path_buf()), &Default::default())
        .unwrap();
    let infos = r.entries().unwrap();
    let id = infos
        .iter()
        .find(|i| i.name.as_str() == name)
        .map(|i| i.id)
        .unwrap();
    read_entry_file(p, id)
}

#[test]
fn empty_archive() {
    let dir = tmp_dir("empty");
    let p = dir.join("empty.zip");
    let w = handler()
        .create_writer(Sink::File(p.clone()), &Default::default())
        .unwrap();
    w.finish().unwrap();
    let infos = common::entries(&p);
    assert_eq!(infos.len(), 0, "空档 entries=0（TC-ALG-010）");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn long_name_zip64_roundtrip() {
    // TC-ALG-013 fmt 半场：259/260+ 中文/emoji/前后空格 roundtrip 保真
    let dir = tmp_dir("longname");
    let names = [
        "x".repeat(259),
        format!("{}/{}", "d".repeat(150), "f".repeat(150)),
        "中文名/深目录/文件-文件.txt".to_string(),
        "emoji-🦊🐕.txt".to_string(),
        " 前后空格.txt".to_string(),
    ];
    for (i, name) in names.iter().enumerate() {
        let data = format!("content-{i}").into_bytes();
        let p = dir.join(format!("ln{i}.zip"));
        let mut w = handler()
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: name.as_str().into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(&data).unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
        let r = handler()
            .open_reader(Source::File(p.clone()), &Default::default())
            .unwrap();
        let infos = r.entries().unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name.as_str(), name, "长名 {i} 读回一致");
        let got = read_entry_file(&p, 0);
        assert_eq!(got, data);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dir_and_meta() {
    let dir = tmp_dir("meta");
    let p = dir.join("meta.zip");
    let mut w = handler()
        .create_writer(Sink::File(p.clone()), &Default::default())
        .unwrap();
    let meta = EntryMeta {
        name: "sub".into(),
        is_dir: true,
        mode: Some(0o755),
        ..Default::default()
    };
    let s = w.next_entry(&meta, PasswordRef::none()).unwrap();
    s.finish().unwrap();
    let meta = EntryMeta {
        name: "sub/f.txt".into(),
        size_raw: Some(3),
        mode: Some(0o600),
        ..Default::default()
    };
    let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
    s.write_all(b"abc").unwrap();
    s.finish().unwrap();
    w.finish().unwrap();

    let infos = common::entries(&p);
    assert_eq!(infos.len(), 2);
    assert!(infos[0].is_dir);
    assert_eq!(infos[0].size_raw, 0);
    assert!(!infos[1].is_dir);
    assert_eq!(infos[1].name.as_str(), "sub/f.txt");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn zip_self_roundtrip_full() {
    // TC-CONF-002：写 → 读（extract）逐文件 sha
    let dir = tmp_dir("conf002");
    let p = write_archive(&sample_entries());
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let pw = common::NoPwSource;
    let env = common::TestEnv::new(&pw);
    let r = common::reader_of(&p);
    let report = r
        .extract(&hzip_format::Selection::All, &env.ctx(), &dest)
        .unwrap();
    assert!(report.failures.is_empty());
    for (name, data) in &sample_entries() {
        let on_disk = std::fs::read(dest.join(name)).unwrap();
        assert_eq!(sha256_hex(&on_disk), sha256_hex(data), "{name}");
    }
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(p.parent().unwrap()).ok();
}
