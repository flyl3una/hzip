//! hzip-error 单元测试：错误码映射、is_retryable、user_action、Display、ErrorContext
//! （任务卡 §6 用例 1–11、14）。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io;

use hzip_error::{
    EntryName, FormatErrorKind, HzipError, PasswordTarget, QuotaKind, SecurityViolation,
    UserAction, code, context::ErrorContext,
};

fn io_err(raw: i32) -> io::Error {
    io::Error::from_raw_os_error(raw)
}

fn io_variant(err: io::Error) -> HzipError {
    HzipError::Io {
        source: err,
        path: None,
    }
}

fn fmt_err(kind: FormatErrorKind) -> HzipError {
    HzipError::Format {
        format: "zip".into(),
        kind,
    }
}

/// 用例 1：io_code_mapping
#[test]
fn io_code_mapping() {
    let perm: HzipError = io_variant(io::Error::from(io::ErrorKind::PermissionDenied));
    assert_eq!(perm.code(), 1003);
    let notfound: HzipError = io_variant(io::Error::from(io::ErrorKind::NotFound));
    assert_eq!(notfound.code(), 1004);
    // ENOSPC(28, unix) / ERROR_DISK_FULL(112, win) → 1002（按 raw os code 双平台判定）
    assert_eq!(io_variant(io_err(28)).code(), 1002);
    assert_eq!(io_variant(io_err(112)).code(), 1002);
    // 其余 → 1001
    assert_eq!(io_variant(io::Error::other("boom")).code(), 1001);
    assert_eq!(io_variant(io_err(1)).code(), 1001);
}

/// 用例 2：format_kind_codes
#[test]
fn format_kind_codes() {
    let cases = [
        (FormatErrorKind::BadSignature, 2001u32),
        (FormatErrorKind::Truncated, 2002),
        (FormatErrorKind::UnknownMethod(99), 2003),
        (FormatErrorKind::BadHeader, 2004),
        (FormatErrorKind::MissingVolume("z.002".into()), 2005),
        (FormatErrorKind::UnknownCrypto, 3103),
    ];
    for (kind, expected) in cases {
        let desc = kind.to_string();
        assert_eq!(fmt_err(kind).code(), expected, "kind: {desc}");
    }
}

/// 用例 3：password_target_codes
#[test]
fn password_target_codes() {
    for target in [PasswordTarget::Archive, PasswordTarget::Entry] {
        let e = HzipError::Password { target };
        assert_eq!(e.code(), 3101, "target: {target}");
    }
    assert_eq!(
        HzipError::Password {
            target: PasswordTarget::Header
        }
        .code(),
        3102
    );
}

/// 用例 4：security_and_quota_codes
#[test]
fn security_and_quota_codes() {
    let violations = [
        (
            SecurityViolation::PathTraversal("../etc/passwd".into()),
            4001u32,
        ),
        (SecurityViolation::SymlinkEscape, 4002),
        (SecurityViolation::ReservedDeviceName, 4003),
        (SecurityViolation::ControlChars, 4004),
    ];
    for (v, expected) in violations {
        let e = HzipError::Security { violation: v };
        assert_eq!(e.code(), expected);
    }
    let quotas = [
        (QuotaKind::MaxFileBytes, 4501u32),
        (QuotaKind::MaxTotalBytes, 4502),
        (QuotaKind::MaxRatio, 4503),
        (QuotaKind::MaxEntries, 4504),
    ];
    for (q, expected) in quotas {
        assert_eq!(HzipError::Quota { quota: q }.code(), expected);
    }
}

/// 用例 5：simple_variant_codes
#[test]
fn simple_variant_codes() {
    let unsupported = HzipError::Unsupported {
        what: "rar 压缩".into(),
    };
    assert_eq!(unsupported.code(), 5001);
    assert_eq!(HzipError::Cancelled.code(), 6001);
    let partial = HzipError::Partial {
        succeeded: 1,
        failed: vec![],
    };
    assert_eq!(partial.code(), 6002);
    let checksum = HzipError::Checksum {
        entry: EntryName::new("a/b.txt"),
        expected: 1,
        actual: 2,
    };
    assert_eq!(checksum.code(), 3001);
}

/// 用例 6：io_is_retryable（瞬断集内 raw os code → true；PermissionDenied 与非 Io 变体 → false）
#[test]
fn io_is_retryable() {
    // unix EPIPE=32 / win ERROR_NETNAME_DELETED=64（RETRYABLE_OS 双平台并集，按 raw code 判定）
    assert!(io_variant(io_err(32)).is_retryable());
    assert!(io_variant(io_err(64)).is_retryable());
    assert!(io_variant(io_err(104)).is_retryable()); // ECONNRESET
    assert!(io_variant(io_err(110)).is_retryable()); // ETIMEDOUT
    assert!(io_variant(io_err(121)).is_retryable()); // ERROR_SEM_TIMEOUT
    assert!(io_variant(io_err(53)).is_retryable()); // ERROR_BAD_NETPATH
    assert!(!io_variant(io::Error::from(io::ErrorKind::PermissionDenied)).is_retryable());
    assert!(!io_variant(io::Error::other("boom")).is_retryable());
    let non_io = [
        fmt_err(FormatErrorKind::BadSignature),
        HzipError::Cancelled,
        HzipError::Unsupported { what: "x".into() },
    ];
    for e in non_io {
        assert!(!e.is_retryable(), "non-Io must not retry: {e}");
    }
}

/// 用例 7：user_action_spot_checks
#[test]
fn user_action_spot_checks() {
    assert_eq!(
        HzipError::Password {
            target: PasswordTarget::Entry
        }
        .user_action(),
        UserAction::RetryPassword
    );
    assert_eq!(
        io_variant(io_err(112)).user_action(),
        UserAction::FreeSpaceOrChangeTarget
    );
    let partial = HzipError::Partial {
        succeeded: 0,
        failed: vec![],
    };
    assert_eq!(partial.user_action(), UserAction::ShowFailureDetails);
    let symlink = HzipError::Security {
        violation: SecurityViolation::SymlinkEscape,
    };
    assert_eq!(symlink.user_action(), UserAction::AdjustSymlinkPolicy);
}

/// 用例 8：display_formatting
#[test]
fn display_formatting() {
    let checksum = HzipError::Checksum {
        entry: EntryName::new("dir/文件.bin"),
        expected: 0xDEAD_BEEF,
        actual: 0x0F0F_0F0F,
    };
    let s = checksum.to_string();
    assert!(s.contains("dir/文件.bin"), "{s}");
    assert!(s.contains("deadbeef"), "{s}");
    assert!(s.contains("0f0f0f0f"), "{s}");

    let partial = HzipError::Partial {
        succeeded: 2,
        failed: vec![(EntryName::new("x.bin"), Box::new(HzipError::Cancelled))],
    };
    assert!(partial.to_string().contains("2 成功, 1 失败"));

    let io_with_path = HzipError::Io {
        source: io::Error::from(io::ErrorKind::PermissionDenied),
        path: Some("D:\\目标\\a.zip".into()),
    };
    let s = io_with_path.to_string();
    assert!(s.contains("D:\\目标\\a.zip"), "{s}");
    assert!(s.contains("(D:\\目标\\a.zip)"), "{s}");
    // 无 path 时不带括注
    let io_no_path = io_variant(io::Error::from(io::ErrorKind::PermissionDenied));
    assert!(!io_no_path.to_string().contains('('), "{}", io_no_path);
}

/// 用例 9：i18n_key_derivation
#[test]
fn i18n_key_derivation() {
    assert_eq!(
        HzipError::Password {
            target: PasswordTarget::Entry
        }
        .i18n_key(),
        "err.hz_3101"
    );
    assert_eq!(HzipError::Cancelled.i18n_key(), "err.hz_6001");
}

/// 用例 10：partial_box_recursion（两层嵌套 Box，code 归并 6002，Debug 不爆栈；02 §13.2）
#[test]
fn partial_box_recursion() {
    let inner = HzipError::Partial {
        succeeded: 0,
        failed: vec![(EntryName::new("a/b"), Box::new(HzipError::Cancelled))],
    };
    let outer = HzipError::Partial {
        succeeded: 1,
        failed: vec![(EntryName::new("c.zip"), Box::new(inner))],
    };
    assert_eq!(outer.code(), 6002);
    let dbg = format!("{outer:?}");
    assert!(dbg.contains("Partial"), "{dbg}");
    assert!(dbg.contains("Cancelled"), "{dbg}");
}

/// 用例 11：context_attach
#[test]
fn context_attach() {
    let err = ErrorContext::new()
        .with_path("out/data.7z")
        .attach(io_variant(io::Error::other("write failed")));
    match err {
        HzipError::Io { path, .. } => {
            assert_eq!(path.as_deref(), Some(std::path::Path::new("out/data.7z")));
        }
        other => panic!("expected Io variant, got {other:?}"),
    }
    // 非 Io 变体原样返回
    let cancelled = ErrorContext::new()
        .with_path("ignored")
        .attach(HzipError::Cancelled);
    assert!(matches!(cancelled, HzipError::Cancelled));
    // entry() 取回挂载条目
    let ctx = ErrorContext::new().with_entry(EntryName::new("e/x.txt"));
    assert_eq!(ctx.entry().map(EntryName::as_str), Some("e/x.txt"));
    let empty = ErrorContext::new();
    assert!(empty.entry().is_none());
}

/// 用例 14（TC-CORE-021c）：variant_code_covers_registry
#[test]
fn variant_code_covers_registry() {
    fn sample(num: u32) -> Option<HzipError> {
        let e = match num {
            1001 => io_variant(io::Error::other("x")),
            1002 => io_variant(io_err(28)),
            1003 => io_variant(io::Error::from(io::ErrorKind::PermissionDenied)),
            1004 => io_variant(io::Error::from(io::ErrorKind::NotFound)),
            2001 => fmt_err(FormatErrorKind::BadSignature),
            2002 => fmt_err(FormatErrorKind::Truncated),
            2003 => fmt_err(FormatErrorKind::UnknownMethod(14)),
            2004 => fmt_err(FormatErrorKind::BadHeader),
            2005 => fmt_err(FormatErrorKind::MissingVolume("v.003".into())),
            3001 => HzipError::Checksum {
                entry: EntryName::new("e"),
                expected: 0,
                actual: 0,
            },
            3101 => HzipError::Password {
                target: PasswordTarget::Archive,
            },
            3102 => HzipError::Password {
                target: PasswordTarget::Header,
            },
            3103 => fmt_err(FormatErrorKind::UnknownCrypto),
            4001 => HzipError::Security {
                violation: SecurityViolation::PathTraversal("../x".into()),
            },
            4002 => HzipError::Security {
                violation: SecurityViolation::SymlinkEscape,
            },
            4003 => HzipError::Security {
                violation: SecurityViolation::ReservedDeviceName,
            },
            4004 => HzipError::Security {
                violation: SecurityViolation::ControlChars,
            },
            4501 => HzipError::Quota {
                quota: QuotaKind::MaxFileBytes,
            },
            4502 => HzipError::Quota {
                quota: QuotaKind::MaxTotalBytes,
            },
            4503 => HzipError::Quota {
                quota: QuotaKind::MaxRatio,
            },
            4504 => HzipError::Quota {
                quota: QuotaKind::MaxEntries,
            },
            5001 => HzipError::Unsupported { what: "w".into() },
            6001 => HzipError::Cancelled,
            6002 => HzipError::Partial {
                succeeded: 0,
                failed: vec![],
            },
            _ => return None,
        };
        Some(e)
    }
    for &(num, _key, _action, _exit, bound) in code::REGISTRY {
        if bound {
            let err = sample(num).unwrap_or_else(|| panic!("missing sample error for {num}"));
            assert_eq!(err.code(), num, "registry row {num} bound variant mismatch");
        }
    }
    // 无变体绑定行（5002/5003/5004/5005/6003/0000）：常量与登记行存在（构造方在 core/cli，任务卡 §7）
    let unbound = [
        (code::business::DECISION_TIMEOUT, 5002),
        (code::business::INVALID_PARAM, 5003),
        (code::business::SOURCE_NOT_FOUND, 5004),
        (code::business::TARGET_NOT_EMPTY, 5005),
        (code::job::QUEUE_CLOSED, 6003),
        (code::INTERNAL_ZERO, 0),
    ];
    for (constant, expected) in unbound {
        assert_eq!(constant, expected);
        assert!(
            code::REGISTRY
                .iter()
                .any(|(num, _, _, _, bound)| *num == expected && !*bound),
            "registry row {expected} must exist as unbound"
        );
    }
    // user_action 与登记表第二字段一致性（绑定行）
    for &(num, _key, action, _exit, bound) in code::REGISTRY {
        if let Some(err) = bound.then(|| sample(num)).flatten() {
            assert_eq!(err.user_action(), action, "user_action for {num}");
        }
    }
}
