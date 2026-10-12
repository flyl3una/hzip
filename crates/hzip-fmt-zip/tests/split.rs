//! TC-ALG-015 分卷字节切分写入 + VolumeSetReader 读回。
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;

use hzip_format::{EntryMeta, FormatHandler, PasswordRef, Sink};

use common::{sha256_hex, tmp_dir};

/// 不可压缩伪随机数据（压缩率≈1，确保按原始字节分卷）。
fn pseudo_random(len: usize, seed: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x = seed as u32;
    for _ in 0..len {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        v.push((x >> 24) as u8);
    }
    v
}

/// Sink::Split 写 → 卷集读回一致；返回 prefix。
fn split_archive(prefix: std::path::PathBuf, vol_bytes: u64) -> std::path::PathBuf {
    // 单条大文件（500 KB 不可压缩 → zip ~500 KB），保证 vol < 总量必分卷
    let entries: Vec<(String, Vec<u8>)> = vec![("big.bin".into(), pseudo_random(500_000, 7))];
    let mut w = common::handler()
        .create_writer(
            Sink::Split {
                prefix: prefix.clone(),
                bytes_per_volume: vol_bytes,
            },
            &Default::default(),
        )
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

    let vols = split_files(&prefix);
    assert!(!vols.is_empty(), "应有分卷产物");

    let r = common::handler()
        .open_reader(
            hzip_format::Source::Volumes {
                first: vols[0].clone(),
                rest: vols[1..].to_vec(),
            },
            &Default::default(),
        )
        .unwrap();
    let infos = r.entries().unwrap();
    assert_eq!(infos.len(), 1, "分卷读回 1 条目");
    for (i, (_, data)) in entries.iter().enumerate() {
        let got = common::read_all(r.as_ref(), i);
        assert_eq!(sha256_hex(&got), sha256_hex(data), "条目 {i}");
    }
    prefix
}

/// 列分卷文件（排序）。
fn split_files(prefix: &std::path::Path) -> Vec<std::path::PathBuf> {
    let dir = prefix.parent().unwrap();
    let stem = prefix.file_stem().unwrap().to_string_lossy().into_owned();
    let final_ext = prefix
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            // {stem}.zNN 或 {stem}.{final_ext}
            if n == format!("{stem}.{final_ext}") {
                return true;
            }
            let body = n.strip_prefix(&format!("{stem}.z")).unwrap_or("");
            !body.is_empty() && body.chars().all(|c| c.is_ascii_digit())
        })
        .collect();
    files.sort();
    files
}

#[test]
fn split_write_volumes() {
    // 三档卷大小：< 单文件 / 介于半程 / ~总量，全部应分卷（总量 ~500KB）
    for (i, vol) in [16_384u64, 150_000, 490_000].iter().enumerate() {
        let dir = tmp_dir(&format!("split-{i}"));
        let prefix = dir.join("vols.zip");
        split_archive(prefix.clone(), *vol);
        let vols = split_files(&prefix);
        assert!(vols.len() >= 2, "多卷应 >=2（vol={vol}）");
        for v in &vols {
            assert!(std::fs::metadata(v).unwrap().len() > 0);
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn split_larger_than_total_single_file() {
    // 卷 > 总量 → 单 .zip 文件，读回一致
    let dir = tmp_dir("split-single");
    let prefix = dir.join("single-out.zip");
    split_archive(prefix.clone(), 10_000_000);
    let vols = split_files(&prefix);
    assert_eq!(vols.len(), 1, "大卷应只产单 .zip");
    std::fs::remove_dir_all(&dir).ok();
}
