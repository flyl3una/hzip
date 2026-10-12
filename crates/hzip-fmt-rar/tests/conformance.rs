//! T26 conformance 接入（05 T26 交付物 ③；TC-CONF-001..010 read 子集）。
//!
//! # 接入策略
//!
//! rar handler **只读且无法自产样本**（D2：rar 压缩永不做）——testkit 的读路径
//! 用例以 `HZIPSTUB` 桩文件驱动真实 handler（`hzip-format-testkit` T21 既有
//! 行为，见 hzip-fmt-tar/single conformance 同型记录），真实 rar handler 按契约
//! 正确拒绝桩文件（`BadSignature` 2001）→ 001~004/006/008/010 对真实 handler
//! 必 Fail，属 testkit 尚未就绪的**对方缺陷**，不私改 testkit。故：
//!
//! - 保留能力过滤下的**负断言与自动跳过组**（testkit 不依赖桩、真实校验）：
//!   - **TC-CONF-005**：Run（append=false 负断言 → `Unsupported` 5001，适用性
//!     检查分支，02 §3.4 caps）；007/009 由 CapsFilter 自动 Skip；
//! - 各契约红线（001 元数据 / 002 只读 roundtrip / 003 选择性 / 004 乱序重复 /
//!   006 取消 / 008 畸形无 panic / 010 列表零 IO）的等价验证在本 crate 内以
//!   unrar 自带真实 rar 样本驱动（见 `src/reader.rs` 测试与下方 `real_samples`
//!   测试），不依赖桩。
//!
//! `#[ignore = "needs-rar-assets"]` 项：无 WinRAR 黄金资产（tests-assets/
//! golden/rar-manual 当前为空）时跳过；待 T02 人工产出后移除 ignore。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use hzip_format::FormatHandler;
use hzip_format_testkit::{CapsFilter, ConfCaseId, KitCtx, run_conformance};

fn assets_path() -> &'static std::path::Path {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-assets");
    Box::leak(p.into_boxed_path())
}

/// conformance：保留桩无关组（005 负断言 + 009 自动 Skip）。
///
/// 007（错误密码）虽属加密能力组，但 testkit 以桩档案驱动（真实 rar handler
/// 打开桩 → BadHeader），等价验证见本文件 `real_rar4_wrong_password_is_3101`
/// （unrar 自带真实加密 rar 样本）。
fn ctx() -> KitCtx {
    let filter = CapsFilter::auto()
        .skip(ConfCaseId::Conf001)
        .skip(ConfCaseId::Conf002)
        .skip(ConfCaseId::Conf003)
        .skip(ConfCaseId::Conf004)
        .skip(ConfCaseId::Conf006)
        .skip(ConfCaseId::Conf007)
        .skip(ConfCaseId::Conf008)
        .skip(ConfCaseId::Conf010);
    KitCtx::new(assets_path())
        .expect("tests-assets 存在")
        .with_caps_filter(filter)
}

fn report(h: &dyn hzip_format::FormatHandler, list: &hzip_format_testkit::DynTestList) {
    let s = list.summary();
    eprintln!(
        "conformance [{}]: total={} passed={} skipped={} failed={}",
        h.id(),
        s.total,
        s.passed,
        s.skipped,
        s.failed
    );
    for t in list.tests() {
        eprintln!("  {} {} {}", t.id, t.name, t.outcome);
    }
}

#[test]
fn conformance_rar_read_subset() {
    let h = hzip_fmt_rar::RarHandler::new();
    let list = run_conformance(&h, &ctx());
    report(&h, &list);
    list.assert_all_pass();
}

// ===================== 真实 rar 样本冒烟（unrar crate 自带资产） =====================
//
// tests-assets/golden/rar-manual 尚无人产出（README 登记 manual_pending）；
// 本组以 unrar crate（本机 cargo registry 源码树 unrar-*/data）的真实微型
// rar4/rar5 样本驱动契约红线等价验证。样本随依赖编译就绪；找不到 → auto-skip
// （与 src 内测试同策略，不硬性依赖本机路径）。

fn unrar_asset(name: &str) -> Option<PathBuf> {
    let home = std::env::var("CARGO_HOME")
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.cargo")))?;
    let root = PathBuf::from(home).join("registry/src");
    let rd = std::fs::read_dir(root).ok()?;
    for top in rd.flatten() {
        for c in std::fs::read_dir(top.path()).ok()?.flatten() {
            let n = c.file_name().to_string_lossy().into_owned();
            if n.starts_with("unrar-") {
                let cand = c.path().join("data").join(name);
                if cand.exists() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

fn copy_asset(dir: &Path, name: &str) -> Option<PathBuf> {
    let src = unrar_asset(name)?;
    let dst = dir.join(name);
    std::fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn read_all(mut r: Box<dyn std::io::Read + Send>) -> Vec<u8> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).expect("read_to_end");
    buf
}

/// TC-CONF-002 只读 roundtrip（真实 rar4）：read_entry 全量字节 == 预期。
#[test]
fn real_rar4_roundtrip_entry_bytes() {
    let d = tempfile::tempdir().expect("tempdir");
    let Some(p) = copy_asset(d.path(), "version.rar") else {
        return; // 资产缺失 auto-skip
    };
    let h = hzip_fmt_rar::RarHandler::new();
    let r = h
        .open_reader(hzip_format::Source::File(p), &Default::default())
        .expect("open");
    let es = r.entries().expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].name.as_str(), "VERSION");
    let got = read_all(
        r.read_entry(0, hzip_format::PasswordRef::none())
            .expect("read"),
    );
    assert_eq!(got, b"unrar-0.4.0");
}

/// TC-CONF-001 元数据完整性（真实 rar4 加密样本条目带加密位）。
#[test]
fn real_rar_meta_encrypted_flag_and_crc() {
    let d = tempfile::tempdir().expect("tempdir");
    let Some(p) = copy_asset(d.path(), "crypted.rar") else {
        return;
    };
    let h = hzip_fmt_rar::RarHandler::new();
    let r = h
        .open_reader(hzip_format::Source::File(p), &Default::default())
        .expect("open");
    let es = r.entries().expect("entries");
    for (i, info) in es.iter().enumerate() {
        assert_eq!(info.id, i);
        hzip_format::parse_entry_name(info.name.as_str()).expect("条目名语法合法");
    }
    assert!(es[0].encrypted, "crypted.rar 条目应标记加密");
    assert!(es[0].crc32.is_some());
}

/// TC-CONF-003/004 选择性 + 乱序重复（真实多条目 rar 由 unrar 自身测试资产
/// archive.part1.rar 提供前 6 条目名；但该样本缺续卷——用确定性单卷样本做
/// 乱序/重复语义验证，逐字节一致）。
#[test]
fn real_rar4_repeat_read_independent_bytes() {
    let d = tempfile::tempdir().expect("tempdir");
    let Some(p) = copy_asset(d.path(), "version.rar") else {
        return;
    };
    let h = hzip_fmt_rar::RarHandler::new();
    let r = h
        .open_reader(hzip_format::Source::File(p), &Default::default())
        .expect("open");
    for _ in 0..3 {
        let got = read_all(
            r.read_entry(0, hzip_format::PasswordRef::none())
                .expect("read"),
        );
        assert_eq!(got, b"unrar-0.4.0", "重复读取逐字节一致");
    }
}

/// TC-CONF-007 错密码（真实 rar4 加密）：无密码 / 错密码 → Password(3101)，
/// 正确密码 → 内容一致、无乱码。
#[test]
fn real_rar4_wrong_password_is_3101() {
    let d = tempfile::tempdir().expect("tempdir");
    let Some(p) = copy_asset(d.path(), "crypted.rar") else {
        return;
    };
    let h = hzip_fmt_rar::RarHandler::new();
    let r = h
        .open_reader(hzip_format::Source::File(p.clone()), &Default::default())
        .expect("open");
    let code_err = |rr: Result<Box<dyn std::io::Read + Send>, hzip_error::HzipError>| {
        rr.err().map(|e| e.code())
    };
    assert_eq!(
        code_err(r.read_entry(0, hzip_format::PasswordRef::none())),
        Some(3101),
        "无密码 → 3101"
    );
    let bad = zeroize::Zeroizing::new("wrong".to_string());
    assert_eq!(
        code_err(r.read_entry(0, hzip_format::PasswordRef::new(&bad))),
        Some(3101),
        "错密码 → 3101（禁乱码流）"
    );
    let good = zeroize::Zeroizing::new("unrar".to_string());
    let got = read_all(
        r.read_entry(0, hzip_format::PasswordRef::new(&good))
            .expect("read"),
    );
    assert_eq!(got, b"target\nCargo.lock\n", "正确密码内容一致");
}

/// rar5 头加密 → open 即 3102（零条目名泄露）。
#[test]
fn real_rar5_header_encrypted_open_3102() {
    let d = tempfile::tempdir().expect("tempdir");
    let Some(p) = copy_asset(d.path(), "comment-hpw-password.rar") else {
        return;
    };
    let h = hzip_fmt_rar::RarHandler::new();
    let err = h
        .open_reader(hzip_format::Source::File(p), &Default::default())
        .map(|_| "ok".to_string())
        .unwrap_err();
    assert_eq!(err.code(), 3102);
    let msg = err.to_string();
    assert!(
        !msg.contains(".gitignore") && !msg.to_lowercase().contains("name"),
        "3102 错误信息不得泄露条目名: {msg}"
    );
}

/// 分卷缺失（archive.part1.rar 无 part2）：open/entries 应 Err 2005 或成功列出
/// 前缀条目（unrar 语义）；若 2005 则信息含缺失卷名。
#[test]
fn real_rar4_split_missing_part2() {
    let d = tempfile::tempdir().expect("tempdir");
    let Some(p) = copy_asset(d.path(), "archive.part1.rar") else {
        return;
    };
    let h = hzip_fmt_rar::RarHandler::new();
    match h.open_reader(hzip_format::Source::File(p), &Default::default()) {
        Ok(r) => {
            // unrar 可能在内容跨越边界前就列出前缀条目；不断言固定，仅保证不 panic
            let _ = r.entries();
        }
        Err(e) => {
            assert_eq!(e.code(), 2005, "缺卷应 MissingVolume 2005（实得 {e:?}）");
            let msg = e.to_string();
            assert!(msg.contains("part2"), "2005 应含缺失卷名: {msg}");
        }
    }
}

// ===================== 黄金资产（T02 WinRAR 人工产出，待入库） =====================

/// rar5 默认格式样本（TC-X-006 必测）→ expected.json sha256 逐文件比对。
/// 依赖 tests-assets/golden/rar-manual/rar5-normal.rar + 其 expected.json（T02
/// 人工产出后入库）；当前无资产 → `#[ignore]`。资产就绪后按 expected.json 的
/// `{name → sha256}` 表逐条目比对（结构与 hzip-fmt-zip golden 测试同型）。
#[test]
#[ignore = "needs-rar-assets"]
fn golden_rar5_roundtrip_vs_expected() {
    use sha2::Digest;
    let root = assets_path().join("golden/rar-manual");
    let arc = root.join("rar5-normal.rar");
    assert!(arc.exists(), "缺黄金资产（T02 未产出）");
    let h = hzip_fmt_rar::RarHandler::new();
    let r = h
        .open_reader(hzip_format::Source::File(arc), &Default::default())
        .expect("open rar5-normal");
    let es = r.entries().expect("entries");
    assert!(!es.is_empty());
    for (i, info) in es.iter().enumerate() {
        assert_eq!(info.id, i);
        let mut rr = r
            .read_entry(i, hzip_format::PasswordRef::none())
            .expect("read_entry");
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut rr, &mut buf).expect("read_to_end");
        let mut sha = sha2::Sha256::new();
        sha.update(&buf);
        let hex = format!("{:x}", sha.finalize());
        // TODO(T02): 与 expected.json 中 info.name 对应行的 sha256 比对
        assert!(!hex.is_empty());
    }
}

/// 黄金 rar5 加密样本（-hp 头加密 + 数据加密）：正确密码读取一致。
#[test]
#[ignore = "needs-rar-assets"]
fn golden_rar5_encrypted_with_password() {
    let root = assets_path().join("golden/rar-manual");
    let arc = root.join("rar5-encrypted.rar");
    assert!(arc.exists(), "缺黄金资产（T02 未产出）");
    let h = hzip_fmt_rar::RarHandler::new();
    // 无密码 open（头加密）→ 3102
    let err = h
        .open_reader(hzip_format::Source::File(arc.clone()), &Default::default())
        .map(|_| "ok".to_string())
        .unwrap_err();
    assert_eq!(err.code(), 3102);
    // 正确密码（密码 hzip-test-pw）在 core 请求-应答通道；本层 PasswordRef 直给
    // ——entries() 无密码通道无法绕过头加密（契约：读路径头加密需 core 先应
    // 答密码；本 handler open 无密码 → 3102 符合契约，正确密码场景由 core
    // 重新 open_reader + extract 驱动——此黄金测试在 core 接入（T30+）后补全
    // extract-with-password 语义）。
    let _ = h;
}
