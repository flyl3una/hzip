//! 套件自验证（05 T21 §6.1）：StubHandler 展开 10 组——
//! 007（无加密）/009（无编辑）自动 Skip 且原因正确，其余全 Pass。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hzip_format::FormatHandler;
use hzip_format_testkit::stub::StubHandler;
use hzip_format_testkit::{CapsFilter, ConfCaseId, KitCtx, run_conformance};

/// 测试资产根（套件未实际使用 assets——桩自产档案；路径存在即可）。
fn kit_ctx() -> KitCtx {
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-assets");
    KitCtx::new(&assets).expect("KitCtx")
}

#[test]
fn stub_conformance_all_pass() {
    let stub = StubHandler::new();
    let ctx = kit_ctx();
    let list = run_conformance(&stub, &ctx);
    for t in list.tests() {
        eprintln!("[{}] {}", t.name, t.outcome);
    }
    list.assert_all_pass();
    let s = list.summary();
    // 10 组全执行：007/009 Skip（无加密/无编辑），其余 8 组 Pass
    assert_eq!(s.total, 10, "总组数 10");
    assert_eq!(s.failed, 0);
    assert_eq!(s.passed + s.skipped, 10);
    assert!(s.skipped >= 2, "007/009 应 Skip（no encryption / no edit）");
    // Skip 原因可追溯
    for t in list.tests() {
        if t.id == ConfCaseId::Conf007 {
            assert!(
                matches!(t.outcome, hzip_format_testkit::DynOutcome::Skip(_)),
                "007 应因无加密 Skip"
            );
        }
        if t.id == ConfCaseId::Conf009 {
            assert!(
                matches!(t.outcome, hzip_format_testkit::DynOutcome::Skip(_)),
                "009 应因无编辑 Skip"
            );
        }
    }
}

#[test]
fn caps_filter_skips() {
    // 无 write caps 的 handler：002 的写路径降级为读（Run）；无 append → 005 负断言 Run；
    // 无 encryption → 007 Skip
    let mut f = CapsFilter::auto();
    f.skip.insert(ConfCaseId::Conf010); // 显式跳过
    let caps = hzip_format::FormatCaps {
        read: true,
        ..Default::default()
    };
    use hzip_format_testkit::RunDecision;
    assert!(matches!(
        f.decide(&caps, ConfCaseId::Conf001),
        RunDecision::Run
    ));
    assert!(matches!(
        f.decide(&caps, ConfCaseId::Conf005),
        RunDecision::Run
    ));
    assert!(matches!(
        f.decide(&caps, ConfCaseId::Conf007),
        RunDecision::Skip(_)
    ));
    assert!(matches!(
        f.decide(&caps, ConfCaseId::Conf009),
        RunDecision::Skip(_)
    ));
    assert!(matches!(
        f.decide(&caps, ConfCaseId::Conf010),
        RunDecision::Skip(_)
    ));
}

#[test]
fn dyn_test_list_naming() {
    use hzip_format_testkit::mock::MockHandler;
    let mock = MockHandler::new(
        "stub",
        hzip_format::FormatCaps {
            read: true,
            write: true,
            ..Default::default()
        },
    );
    let ctx = kit_ctx();
    let list = hzip_format_testkit::run_cases(&mock, &ctx, &[ConfCaseId::Conf001]);
    let t = &list.tests()[0];
    assert_eq!(
        t.name, "TC-CONF-001-stub",
        "展开名格式={{id}}-{{handler_id}}"
    );
}

#[test]
fn mock_panic_detected() {
    use hzip_format_testkit::mock::MockHandler;
    use hzip_format_testkit::{Hook, Scripted};
    // 注入 Panic@Extract → 006 结果 Fail 且 panic_count 计入
    let mock = MockHandler::new(
        "stub",
        hzip_format::FormatCaps {
            read: true,
            write: true,
            ..Default::default()
        },
    );
    mock.inject(Hook::Extract, Scripted::Panic);
    let ctx = kit_ctx();
    // 直接对 mock 跑 006（mock 的 extract 注入 panic）
    let list = hzip_format_testkit::run_cases(&mock, &ctx, &[ConfCaseId::Conf006]);
    let s = list.summary();
    // mock caps 全 true → 006 Run；extract 注入 panic 被套件捕获为 Fail
    assert_eq!(s.failed, 1, "注入 panic 的用例应报 Fail");
    let msg = list.tests()[0].outcome.to_string();
    assert!(
        msg.contains("panic") || s.panic_count > 0,
        "Fail 消息应含 panic 线索: {msg}"
    );
}

#[test]
fn malformed_deterministic() {
    let mut a = vec![7u8; 100];
    let mut b = vec![7u8; 100];
    hzip_format_testkit::malformed::bit_flip(&mut a, 20, 0xABCD);
    hzip_format_testkit::malformed::bit_flip(&mut b, 20, 0xABCD);
    assert_eq!(a, b);
    let flips = a.iter().filter(|&&x| x != 7).count();
    assert!(flips > 0);
}

#[test]
fn mock_calls_recorded() {
    use hzip_format_testkit::mock::MockHandler;
    use hzip_format_testkit::{Hook, MockEntry};
    let mock = MockHandler::new(
        "stub",
        hzip_format::FormatCaps {
            read: true,
            ..Default::default()
        },
    )
    .with_entries(vec![MockEntry::file("a.txt", b"hi".to_vec())]);
    let reader = mock
        .open_reader(
            hzip_format::Source::File(std::path::PathBuf::from("x")),
            &Default::default(),
        )
        .expect("open_reader");
    let _ = reader.entries().expect("entries");
    let _ = reader.read_entry(0, hzip_format::PasswordRef::none());
    let calls = mock.calls();
    assert_eq!(
        calls,
        vec![Hook::OpenReader, Hook::Entries, Hook::ReadEntry(0)]
    );
    assert_eq!(mock.read_count(), 1);
}
