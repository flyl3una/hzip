//! `password.rs` 单测（T-STD：自内联 `mod tests` 平移）。
//!
//! ⚠️ 本文件内的测试**不得触发 stdin 交互通道**：`PasswordResolver::resolve`
//! 在 `env_used` 之后必然落 `resolve_stdin`，而 stdin 是 TTY 时会进
//! `rpassword` 交互等待 ⇒ 在交互终端里跑 `cargo test` 会**永久挂起**。
//! 要覆盖「非 TTY → EOF → 空密码应答、不挂起」的看门狗语义，请走
//! `tests/e2e.rs`（assert_cmd 管道 stdin 天然非 TTY）。
use super::*;

fn pw_req(reason: hzip_core::jobs::PasswordReason) -> JobRequest {
    JobRequest::Password {
        entry: None,
        reason,
    }
}

/// ① --password-env 缺失 → preflight Err（5003 语义）。
#[test]
fn password_env_missing_preflight() {
    let r = PasswordResolver::new(Some("HZIP_TEST_UNSET_VAR_9F2E".into()));
    let e = r.preflight().unwrap_err();
    // HzipError::Unsupported 的 code() 恒 5001；5003 语义经 what 串带码
    assert!(
        e.to_string().contains("hz-5003"),
        "预检缺失应报 5003 语义: {e}"
    );
}

/// ② HZIP_PASSWORD 通道取值 + 「命中后置位、不再重试」不变式。
#[test]
fn hzip_password_channel() {
    // 需要无 HZIP_PASSWORD 干扰：设置后读
    // SAFETY: 测试隔离（hzip_password_channel 单测独占本二进制进程；
    // Rust 2024 起 set_var 标记 unsafe——单线程测试上下文无并发读者）
    unsafe { std::env::set_var("HZIP_PASSWORD", "pw-from-env") };
    let mut r = PasswordResolver::new(None);
    let resp = r
        .resolve(&pw_req(hzip_core::jobs::PasswordReason::Initial))
        .unwrap();
    match resp {
        Some(JobResponse::Password(pw)) => assert_eq!(pw.as_str(), "pw-from-env"),
        _ => panic!("应返回密码"),
    }
    // 02 §10.2 E「不再重试」：命中一次即置位，后续请求不再读 ② 通道。
    // 这里**不再调用 resolve 验证**：`env_used` 之后必然落 stdin 通道，
    // 而 stdin 是 TTY 时 `resolve_stdin` 会进 `rpassword` 交互等待——在交互
    // 终端里跑 `cargo test` 会**永久挂起**（本测试原先正是这么卡住的）。
    // 非 TTY 的「EOF → 空密码应答、不挂起」等价语义由 e2e 覆盖：assert_cmd
    // 管道 stdin 天然非 TTY（TC-CLI-008 `t -`、HZIP_PASSWORD 通道用例）。
    assert!(r.env_used, "env 通道命中后必须置位（该通道不重试）");
    unsafe { std::env::remove_var("HZIP_PASSWORD") };
}

/// 重试计数语义常量（TTY 通道上限 3）。
#[test]
fn max_retries_constant() {
    assert_eq!(PASSWORD_MAX_RETRIES, 3);
}
