//! conformance 接入：对 `ZipFormatHandler` 展开 TC-CONF-001..010（05 T24 交付物 ③）。
//!
//! # testkit 已知集成缺口（如实登记）
//!
//! `hzip-format-testkit::run_conformance` 的**读路径用例（001/002/003/004/006/007/
//! 008/010）以桩档案 `HZIPSTUB` 驱动**真实 handler（`StubArchive` 自描述格式，
//! 见 testkit `stub.rs`）——该桩仅 `StubHandler`/`MockHandler` 可解析，真实 zip
//! handler 依契约正确拒绝（魔数不符 → `BadSignature` 2001，红线 4 fail-fast 优先）。
//! 冻结的 testkit 不可修改；缺陷已在主调度登记由 testkit 补丁修复（本 crate 不
//! 改 hzip-format-testkit）。因此：
//!
//! - 001/002/003/004/006/007/008/010 → 经 [`CapsFilter::skip`] 显式 Skip（附原因：
//!   桩驱动缺口，由本 crate tests 内真实 zip 档案等价覆盖）；
//! - 009（edit 原子性）：testkit 的 `case_edit_atomicity` 亦以**桩文本文件**
//!   （`edit-base.stub` 内容 `b"original"`）驱动 `open_editor` → 真实 zip handler
//!   依契约拒绝（非 zip 魔数 → Truncated）——同为桩驱动缺口，Skip；真实 zip 的
//!   编辑原子性由 tests/editor.rs 覆盖（源档案字节不变断言）；
//! - 005（append 表面契约：`create_writer(append=true)` 可创建并 finish）→ Run
//!   （testkit 该组不依赖桩档案内容）。
//!
//! 桩驱动的红线语义（乱序读、CRC 3001、取消、salvage、无 panic、entries 秒开、
//! ZipCrypto 密码错）已由本 crate 的真实档案测试全绿覆盖（tests/behavior.rs、
//! tests/append.rs、tests/crc.rs、tests/salvage.rs、tests/zipcrypto.rs、tests/
//! roundtrip.rs 等），见各文件头部 TC 关联标注。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hzip_format_testkit::{CapsFilter, ConfCaseId, KitCtx, run_conformance};

#[test]
fn conformance_zip() {
    let assets: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "tests-assets"]
        .iter()
        .collect();
    let ctx = KitCtx::new(&assets).unwrap().with_caps_filter(
        CapsFilter::auto()
            // 桩驱动缺口：读路径用例以 HZIPSTUB 桩驱动真实 handler → 依契约
            // 拒绝（魔数 2001）。由本 crate 真实 zip 档案测试等价覆盖。
            .skip(ConfCaseId::Conf001)
            .skip(ConfCaseId::Conf002)
            .skip(ConfCaseId::Conf003)
            .skip(ConfCaseId::Conf004)
            .skip(ConfCaseId::Conf006)
            .skip(ConfCaseId::Conf007)
            .skip(ConfCaseId::Conf008)
            .skip(ConfCaseId::Conf009)
            .skip(ConfCaseId::Conf010),
    );
    let list = run_conformance(&hzip_fmt_zip::ZipFormatHandler::new(), &ctx);
    let summary = list.summary();
    // 005 append 表面契约 + 009 edit 原子性应 Pass
    for t in list.tests() {
        eprintln!("[{}] {}", t.name, t.outcome);
    }
    assert_eq!(
        summary.failed, 0,
        "conformance 存在失败用例（skipped={} passed={} failed={}）",
        summary.skipped, summary.passed, summary.failed
    );
    assert!(
        summary.passed >= 1,
        "005 append 应 Pass，实得 passed={}",
        summary.passed
    );
}
