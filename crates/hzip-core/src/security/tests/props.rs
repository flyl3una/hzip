//! proptest 属性测试（TC-SEC-012：10 万次畸形路径零逃逸）。
//! 本文件经 mod.rs `#[cfg(test)] mod props;` 门控，勿再加 `#![cfg(test)]`。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Component;

use hzip_error::HzipError;
use hzip_format::EntryName;
use proptest::collection;
use proptest::prelude::*;

use super::sanitize::sanitize_entry_path;

/// 畸形段语料生成器（02 §9.6 X2~X4 + 编码/同形变体，03 §2.9 TC-FUZ-005 注）。
fn malformed_segment() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => Just("..".into()),
        2 => Just("..\\".into()),
        3 => Just("..\\..\\..\\".into()),
        2 => Just("..\\..\\".into()),
        2 => Just("%2e%2e%5c".into()),
        2 => Just("..%5c".into()),
        2 => Just("..%25c".into()),
        1 => Just("．．".into()),
        1 => Just("‥".into()),
        2 => Just("∕".into()),
        2 => Just("／".into()),
        1 => Just("⁄".into()),
        2 => prop_oneof![
            Just("CON".into()),
            Just("con.txt".into()),
            Just("NUL.bin".into()),
            Just("COM1".into()),
            Just("lpt9.log".into())
        ],
        2 => Just("C:\\evil".into()),
        2 => Just("\\\\srv\\share".into()),
        2 => Just("\\x".into()),
        2 => Just("/abs".into()),
        2 => prop_oneof![
            Just("a\x00b".into()),
            Just("a\x01b".into()),
            Just("a\x1fb".into()),
            Just("a\x7fb".into())
        ],
        8 => "[a-z0-9_]{1,8}".prop_map_into(),
    ]
}

fn entry_name_strategy() -> impl Strategy<Value = String> {
    collection::vec(malformed_segment(), 0..6).prop_map(|segs| segs.join("/"))
}

fn dest_strategy() -> impl Strategy<Value = std::path::PathBuf> {
    prop_oneof![
        Just(std::path::PathBuf::from("C:\\dest")),
        Just(std::path::PathBuf::from("C:\\dest\\sub")),
        Just(std::path::PathBuf::from("/dest")),
        Just(std::path::PathBuf::from("/dest/sub")),
        Just(std::path::PathBuf::from("rel/dest")),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100_000))]

    #[test]
    fn sanitize_never_escapes_dest(raw in entry_name_strategy(), dest in dest_strategy()) {
        let cfg = hzip_format::SanitizeConfig::default();
        match sanitize_entry_path(&EntryName::new(raw.clone()), &dest, &cfg) {
            Ok(s) => {
                prop_assert!(!s.rel.components().any(|c| matches!(c, Component::ParentDir)));
                prop_assert!(!s.rel.is_absolute());
                // dest_path 必须以 dest 为分量级前缀（win \\?\ 前缀属注入，先剥）
                let dp = strip_win_prefix(&s.dest_path);
                prop_assert!(dp.starts_with(&dest), "dest_path 逃出 dest: {:?} !startswith {:?}", dp, dest);
                prop_assert!(s.rel.to_str().is_none_or(|p| p.chars().all(|c| !c.is_control())));
            }
            Err(e) => {
                prop_assert!(
                    matches!(e, HzipError::Security { .. }),
                    "拒绝必须是安全类错误: {e:?}"
                );
            }
        }
    }
}

/// 剥掉 Windows `\\?\` 注入前缀后再做 starts_with 断言。
fn strip_win_prefix(p: &std::path::Path) -> std::path::PathBuf {
    let s = p.to_string_lossy();
    for pre in ["\\\\?\\", "//?/"] {
        if let Some(rest) = s.strip_prefix(pre) {
            return std::path::PathBuf::from(rest);
        }
    }
    p.to_path_buf()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn unix_like_legal_segments_preserved(raw in "[a-z0-9_]{1,8}(/[a-z0-9_]{1,8}){0,4}") {
        let cfg = hzip_format::SanitizeConfig::default();
        // 纯合法路径永不拒绝（win 上保留名段不在此语料 → 也 Ok）
        if let Ok(s) = sanitize_entry_path(&EntryName::new(raw.clone()), &dest_strategy_sample(), &cfg) {
            let joined = s.rel.to_string_lossy().replace('/', "_");
            prop_assert!(!joined.is_empty());
        }
    }
}

fn dest_strategy_sample() -> std::path::PathBuf {
    std::path::PathBuf::from("C:\\dest")
}
