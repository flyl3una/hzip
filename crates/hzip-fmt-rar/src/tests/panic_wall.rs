use super::*;
use hzip_error::{FormatErrorKind, PasswordTarget};

fn mk(code: Code, when: When) -> UnrarError {
    UnrarError::from(code, when)
}

#[test]
fn map_unknown_format_signature() {
    let e = map_unrar_err(&mk(Code::UnknownFormat, When::Open));
    assert!(matches!(
        e,
        HzipError::Format {
            kind: FormatErrorKind::BadSignature,
            ..
        }
    ));
    assert_eq!(e.code(), 2001);
}

#[test]
fn map_end_archive_truncated() {
    let e = map_unrar_err(&mk(Code::EndArchive, When::Read));
    assert!(matches!(
        e,
        HzipError::Format {
            kind: FormatErrorKind::Truncated,
            ..
        }
    ));
    assert_eq!(e.code(), 2002);
}

#[test]
fn map_bad_archive_header() {
    let e = map_unrar_err(&mk(Code::BadArchive, When::Open));
    assert!(matches!(
        e,
        HzipError::Format {
            kind: FormatErrorKind::BadHeader,
            ..
        }
    ));
    assert_eq!(e.code(), 2004);
    // BadData @Open/Read 也归坏头
    let e2 = map_unrar_err(&mk(Code::BadData, When::Open));
    assert_eq!(e2.code(), 2004);
}

#[test]
fn map_bad_password_missing_password() {
    for c in [Code::MissingPassword, Code::BadPassword] {
        let e = map_unrar_err(&mk(c, When::Process));
        assert!(matches!(
            e,
            HzipError::Password {
                target: PasswordTarget::Archive
            }
        ));
        assert_eq!(e.code(), 3101);
    }
}

#[test]
fn map_bad_data_process_checksum() {
    let e = map_unrar_err(&mk(Code::BadData, When::Process));
    assert!(matches!(e, HzipError::Checksum { .. }));
    assert_eq!(e.code(), 3001);
}

#[test]
fn map_io_codes() {
    for c in [Code::ERead, Code::EWrite, Code::ECreate, Code::EClose] {
        let e = map_unrar_err(&mk(c, When::Process));
        assert!(matches!(e, HzipError::Io { .. }), "{c:?} → Io");
    }
}

#[test]
fn map_unknown_code_fallback_2004() {
    // Unknown / NoMemory / EReference → 2004（未知码表外兜底）
    for c in [
        Code::Unknown,
        Code::NoMemory,
        Code::EReference,
        Code::Success,
    ] {
        let e = map_unrar_err(&mk(c, When::Open));
        assert_eq!(e.code(), 2004, "{c:?} → 2004");
    }
}

#[test]
fn mutex_serializes_and_panic_intercepted() {
    // 并发调用经互斥串行化；panic 闭包被拦截为 Panicked（进程存活）
    let out = call_unrar("test-panic", || -> Result<(), UnrarError> {
        panic!("boom from c boundary");
    })
    .expect("call_unrar 自身不 panic");
    assert!(matches!(out, UnrarOutcome::Panicked));

    // 普通 unrar 错误结果保留
    let out2 = call_unrar("test-err", || -> Result<(), UnrarError> {
        Err(UnrarError::from(Code::BadArchive, When::Open))
    })
    .expect("ok");
    assert!(matches!(out2, UnrarOutcome::Err(_)));

    // 成功结果保留
    let out3 = call_unrar("test-ok", || -> Result<i32, UnrarError> { Ok(42) }).expect("ok");
    assert!(matches!(out3, UnrarOutcome::Ok(42)));
}

#[test]
fn concurrent_calls_serialized() {
    // 2 线程并发各 50 次调用：互斥锁串行化，无崩溃、无锁毒
    let handles: Vec<_> = (0..2)
        .map(|_| {
            std::thread::spawn(|| {
                for i in 0..50 {
                    let out = call_unrar("conc", move || {
                        if i % 7 == 0 {
                            panic!("periodic panic {i}");
                        }
                        Ok::<u32, UnrarError>(i)
                    })
                    .expect("no poison");
                    match out {
                        UnrarOutcome::Ok(_) | UnrarOutcome::Panicked => {}
                        UnrarOutcome::Err(_) => unreachable!(),
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread join");
    }
    // 锁未被毒化：仍可正常获取（guard 就地释放，不用 Arc 包裹——MutexGuard
    // 非 Send，仅验证 lock 成功）
    let _g = UNRAR_MUTEX.lock().expect("互斥锁未被 panic 毒化");
}
