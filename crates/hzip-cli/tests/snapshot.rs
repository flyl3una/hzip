//! CLI 帮助/错误快照（TC-CLI-013；insta）。
//!
//! 快照断言帮助文本与典型错误输出稳定。`--help` 输出含版本号与用法文本——
//! 以 insta 字符串快照锁定（`cargo insta` 非必需：`INSTA_UPDATE=always` 或
//! `cargo test` 的 `.snap.new` 差异提示均可维护）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use assert_cmd::prelude::*;
use std::process::Command;

fn hzip() -> Command {
    Command::cargo_bin("hzip").unwrap()
}

/// 归一化 clap 用法行中的程序名：其取值是 `argv[0]`，Windows 产物为 `hzip.exe`、
/// unix 为 `hzip`。快照若原样锁定就会把平台差异钉成断言（Windows 录的快照在
/// macOS/Linux 上必失败）——本函数把两者统一为逻辑名 `hzip`，使同一份快照在
/// 三平台都成立（03 §1.3 规则 1：平台差异点写成对照，而非各存一份）。
fn normalize(text: &str) -> String {
    text.replace("hzip.exe", "hzip")
}

#[test]
fn help_snapshot_top() {
    let out = hzip().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = normalize(&String::from_utf8_lossy(&out.stdout));
    insta::assert_snapshot!("help_top", text);
}

#[test]
fn help_snapshot_compress() {
    let out = hzip().args(["c", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = normalize(&String::from_utf8_lossy(&out.stdout));
    insta::assert_snapshot!("help_compress", text);
}

#[test]
fn err_snapshot_bad_format() {
    let out = hzip()
        .args(["c", "-f", "bogus", "-o", "x.zip", "in"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = normalize(&String::from_utf8_lossy(&out.stderr));
    insta::assert_snapshot!("err_bad_format", text);
}
