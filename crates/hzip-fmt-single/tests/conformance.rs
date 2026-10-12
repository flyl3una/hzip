//! T23 conformance 接入（05 T23 交付物 ②；TC-CONF-001..010）。
//!
//! # 已知 testkit 缺陷（记录并跳过，不修 testkit）
//!
//! hzip-format-testkit（T21）当前所有读路径用例（001~004/006/008/010）以
//! **`HZIPSTUB` 桩文件**驱动被测 handler（`cases.rs::write_stub_archive` 写桩、
//! `open_file_reader` 用真实 handler 打开桩文件）。真实 fmt（本 crate 首个接入）
//! 按魔数拒绝桩文件 → 这些组对真实 handler 必 Fail，属 testkit 尚未为真实
//! handler 就绪的**对方缺陷**（T21 自验证仅以 StubHandler 自身通过；T22 tar
//! 尚未合入即同此矛盾）。按 T23 §7「确属对方缺陷则记录并跳过」，本文件以
//! `CapsFilter::skip` 显式跳过这些组（可追溯原因见上），保留：
//!
//! - **TC-CONF-005**：Run —— 负断言（append=true → `Err Unsupported` 5001），
//!   不依赖桩，真实校验；
//! - **TC-CONF-007 / 009**：CapsFilter 自动 Skip（encryption=None / edit=false）。
//!
//! 其余契约红线（001/002/003/004/006/008/010）的等价验证由本 crate 的
//! `tests/behavior.rs`（roundtrip 矩阵 / extract / test / 截断位翻转 / cancel /
//! 列表秒开）真实 handler 直测覆盖。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use hzip_format_testkit::{CapsFilter, ConfCaseId, KitCtx, run_conformance};

fn assets_path() -> &'static std::path::Path {
    // 仓库 tests-assets/（KitCtx 约定；当前用例经 stub 驱动未消费黄金资产）
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-assets");
    Box::leak(p.into_boxed_path())
}

/// 显式跳过桩驱动的读路径组（记录原因见模块文档）。
fn ctx() -> KitCtx {
    let filter = CapsFilter::auto()
        .skip(ConfCaseId::Conf001)
        .skip(ConfCaseId::Conf002)
        .skip(ConfCaseId::Conf003)
        .skip(ConfCaseId::Conf004)
        .skip(ConfCaseId::Conf006)
        .skip(ConfCaseId::Conf008)
        .skip(ConfCaseId::Conf010);
    KitCtx::new(assets_path())
        .expect("tests-assets 存在")
        .with_caps_filter(filter)
}

/// 打印汇总（供 `--nocapture` 观察各组 PASS/SKIP）。
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
fn conformance_single_gz() {
    #[cfg(feature = "gz")]
    {
        let h = hzip_fmt_single::gz();
        let list = run_conformance(&h, &ctx());
        report(&h, &list);
        list.assert_all_pass();
    }
}

#[cfg(feature = "bz2")]
#[test]
fn conformance_single_bz2() {
    let h = hzip_fmt_single::bz2();
    let list = run_conformance(&h, &ctx());
    report(&h, &list);
    list.assert_all_pass();
}

#[cfg(feature = "xz")]
#[test]
fn conformance_single_xz() {
    let h = hzip_fmt_single::xz();
    let list = run_conformance(&h, &ctx());
    report(&h, &list);
    list.assert_all_pass();
}

#[cfg(feature = "zst")]
#[test]
fn conformance_single_zst() {
    let h = hzip_fmt_single::zst();
    let list = run_conformance(&h, &ctx());
    report(&h, &list);
    list.assert_all_pass();
}

/// 全关默认 feature 时空集 crate 编译占位（--no-default-features 无 feature 时）。
#[cfg(not(any(feature = "gz", feature = "bz2", feature = "xz", feature = "zst")))]
#[test]
fn conformance_none_enabled() {
    // 无格式 feature：无可注册 handler（02 §7-7）；空占位
    let _ = assets_path();
}
