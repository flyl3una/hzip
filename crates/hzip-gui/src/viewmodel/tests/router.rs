use super::DialogRouter;

/// 浏览密码门：武装幂等 / 失败计数 / 上限解门（设计稿 11 §4.3）。
#[test]
fn browse_gate_arm_retry_cap() {
    let mut r = DialogRouter::new();
    assert!(r.gate_attempts().is_none());
    assert!(r.arm_gate(GateAction::OpenArchive, "/tmp/x.7z".into()));
    assert!(
        !r.arm_gate(
            GateAction::OpenSelection {
                id: 1,
                name: "a.txt".into(),
                form: OpenForm::Configured
            },
            "/tmp/y.7z".into()
        ),
        "重复武装拒绝"
    );
    assert_eq!(r.gate_attempts(), Some(0));
    assert_eq!(r.gate_action(), Some(&GateAction::OpenArchive));
    assert_eq!(r.gate_key().unwrap().display().to_string(), "/tmp/x.7z");
    assert!(r.gate_fail(), "1/3 仍开");
    assert!(r.gate_fail(), "2/3 仍开");
    assert!(!r.gate_fail(), "3/3 达上限解门");
    assert!(r.gate_attempts().is_none());
    r.gate_disarm();
    assert!(r.gate_attempts().is_none());
}

/// 门验证通过 → 动作进待执行槽（泵取出重跑；用户口径 2026-09-28）。
#[test]
fn gate_resolve_moves_action_to_pending() {
    let mut r = DialogRouter::new();
    assert!(r.take_pending_gate_action().is_none());
    assert!(!r.gate_resolve(), "未武装时无动作可放行");
    assert!(r.arm_gate(
        GateAction::OpenContainer("inner.7z".into()),
        "/tmp/x.zip".into()
    ));
    assert!(r.gate_resolve());
    assert!(r.gate_attempts().is_none(), "放行即解门");
    assert_eq!(
        r.take_pending_gate_action(),
        Some(GateAction::OpenContainer("inner.7z".into()))
    );
    assert!(r.take_pending_gate_action().is_none(), "取出即清空");
}
use super::*;

fn req_password(reason: PasswordReason) -> JobRequest {
    JobRequest::Password {
        entry: Some(EntryName::new("secret.txt")),
        reason,
    }
}

#[test]
fn password_three_states() {
    let mut r = DialogRouter::new();
    assert_eq!(r.password_state(), &PasswordDialogState::Hidden);
    // ① Initial → Awaiting
    assert!(r.intake(1, req_password(PasswordReason::Initial)));
    match r.password_state() {
        PasswordDialogState::Awaiting {
            job_id,
            target,
            reason,
            ..
        } => {
            assert_eq!(*job_id, 1);
            assert_eq!(target.as_ref().map(|e| e.as_str()), Some("secret.txt"));
            assert_eq!(*reason, PasswordReason::Initial);
        }
        other => panic!("应 Awaiting: {other:?}"),
    }
    // 提交（瞬态动作）→ Hidden
    // 无真实 handle：以 mock 断言 submit 在无 handle 下不可达——直接验证
    // 状态复位语义经 cancel 路径（handle 由 pump 持有）
    r.cancel_request_without_handle(); // 测试辅助：直接复位（同 cancel_request 语义）
    assert_eq!(r.password_state(), &PasswordDialogState::Hidden);
}

#[test]
fn password_wrong_password_transitions_to_retry() {
    let mut r = DialogRouter::new();
    r.intake(1, req_password(PasswordReason::Initial));
    // 错密码重问 → Retry(attempts=1)
    assert!(r.intake(1, req_password(PasswordReason::WrongPassword)));
    match r.password_state() {
        PasswordDialogState::Retry {
            attempts, job_id, ..
        } => {
            assert_eq!(*attempts, 1);
            assert_eq!(*job_id, 1);
        }
        other => panic!("应 Retry: {other:?}"),
    }
    // 再错 → Retry(attempts=2)
    r.intake(1, req_password(PasswordReason::WrongPassword));
    match r.password_state() {
        PasswordDialogState::Retry { attempts, .. } => assert_eq!(*attempts, 2),
        other => panic!("应 Retry(2): {other:?}"),
    }
    // 第三次错 → attempts=3 → 达上限（提交动作后再收到重问 → pump cancel）
    r.intake(1, req_password(PasswordReason::WrongPassword));
    match r.password_state() {
        PasswordDialogState::Retry { attempts, .. } => assert_eq!(*attempts, 3),
        other => panic!("应 Retry(3): {other:?}"),
    }
    // Retry 态不持有密码明文副本（无 pw 字段——编译面即断言）
    // 达上限后用户再提交 → 由调用方（pump）判 attempts>=MAX → cancel
    assert_eq!(
        r.password_retry_count().expect("计数"),
        PASSWORD_MAX_RETRIES
    );
}

#[test]
fn header_encrypted_reason() {
    let mut r = DialogRouter::new();
    r.intake(
        1,
        JobRequest::Password {
            entry: None,
            reason: PasswordReason::HeaderEncrypted,
        },
    );
    match r.password_state() {
        PasswordDialogState::Awaiting { target, reason, .. } => {
            assert!(target.is_none(), "头加密 = 档案级");
            assert_eq!(*reason, PasswordReason::HeaderEncrypted);
        }
        other => panic!("应 Awaiting: {other:?}"),
    }
}

#[test]
fn conflict_apply_all_session() {
    let mut r = DialogRouter::new();
    // 首个冲突 → 弹窗
    assert!(r.intake(
        1,
        JobRequest::Conflict {
            path: std::path::PathBuf::from("a.txt"),
            existing: hzip_core::jobs::FileBrief {
                is_dir: false,
                size: 1,
                mtime: None,
            },
        }
    ));
    assert!(r.conflict_prompt().is_some());
    assert_eq!(r.auto_decision_for(1), None);
    // 提交 Overwrite + apply_all → 写会话表
    // （无真实 handle：直接经会话表断言——submit 的 handle 侧由 pump 持有）
    r.apply_all_for_test(1, ConflictDecision::Overwrite);
    assert_eq!(r.auto_decision_for(1), Some(ConflictDecision::Overwrite));
    // 后续冲突 → 会话命中 → 不弹窗（intake 返回 false）
    let changed = r.intake(
        1,
        JobRequest::Conflict {
            path: std::path::PathBuf::from("b.txt"),
            existing: hzip_core::jobs::FileBrief {
                is_dir: false,
                size: 2,
                mtime: None,
            },
        },
    );
    assert!(!changed, "会话命中自动应答不弹窗");
    assert!(r.conflict_prompt().is_none());
    // 不勾选 apply_all 的另一任务 → 每次仍进 Awaiting
    let changed2 = r.intake(
        2,
        JobRequest::Conflict {
            path: std::path::PathBuf::from("c.txt"),
            existing: hzip_core::jobs::FileBrief {
                is_dir: false,
                size: 3,
                mtime: None,
            },
        },
    );
    assert!(changed2, "无会话 → 弹窗");
    assert!(r.auto_decision_for(2).is_none());
    // 任务终局清会话
    r.on_job_finished(1);
    assert_eq!(r.auto_decision_for(1), None);
}

#[test]
fn abort_never_cached() {
    let mut r = DialogRouter::new();
    r.intake(
        1,
        JobRequest::Conflict {
            path: std::path::PathBuf::from("a.txt"),
            existing: hzip_core::jobs::FileBrief {
                is_dir: false,
                size: 0,
                mtime: None,
            },
        },
    );
    r.apply_all_for_test(1, ConflictDecision::Abort);
    assert_eq!(r.auto_decision_for(1), None, "Abort 一次性不缓存");
}

#[test]
fn cancel_and_finish_reset_dialogs() {
    let mut r = DialogRouter::new();
    r.intake(1, req_password(PasswordReason::Initial));
    assert!(r.dialog_open());
    // 终局（超时 Failed / 取消）→ 复位 Hidden
    r.on_job_finished(1);
    assert!(!r.dialog_open());
    assert_eq!(r.password_state(), &PasswordDialogState::Hidden);
    assert!(r.active_job().is_none());

    // 冲突同理
    r.intake(
        2,
        JobRequest::Conflict {
            path: "x".into(),
            existing: hzip_core::jobs::FileBrief {
                is_dir: false,
                size: 0,
                mtime: None,
            },
        },
    );
    assert!(r.dialog_open());
    r.cancel_request_without_handle();
    assert!(!r.dialog_open());
}

#[test]
fn dialogs_are_mutually_exclusive() {
    let mut r = DialogRouter::new();
    r.intake(1, req_password(PasswordReason::Initial));
    assert!(r.dialog_open());
    // 冲突请求到达（同任务不可能——通道容量 1；跨任务串行上抛）→ 密码复位
    r.intake(
        2,
        JobRequest::Conflict {
            path: "x".into(),
            existing: hzip_core::jobs::FileBrief {
                is_dir: false,
                size: 0,
                mtime: None,
            },
        },
    );
    assert_eq!(r.password_state(), &PasswordDialogState::Hidden);
    assert!(r.conflict_prompt().is_some());
}
