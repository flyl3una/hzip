use super::*;
use hzip_error::{EntryName, FormatErrorKind, FormatId};
use hzip_progress::{EntryFailure, JobStats};

fn stats() -> JobStats {
    JobStats::new(0, 0)
}

/// 归并表逐格（任务卡 §4.3 表）。
#[test]
fn exit_code_table() {
    // Done → 0
    assert_eq!(exit_code_of_result(&Ok(Outcome::Done(stats()))), 0);
    // Partial → 1
    assert_eq!(
        exit_code_of_result(&Ok(Outcome::Partial(
            stats(),
            vec![EntryFailure {
                entry: "a.txt".into(),
                error: Box::new(HzipError::Checksum {
                    entry: EntryName::new("a.txt"),
                    expected: 1,
                    actual: 2,
                }),
            }]
        ))),
        1
    );
    // Failed 常规 → 2
    assert_eq!(
        exit_code_of_result(&Ok(Outcome::Failed(HzipError::Password {
            target: hzip_error::PasswordTarget::Entry,
        }))),
        2
    );
    assert_eq!(
        exit_code_of_result(&Ok(Outcome::Failed(HzipError::Format {
            format: FormatId::from("zip"),
            kind: FormatErrorKind::BadSignature,
        }))),
        2
    );
    // Failed(Cancelled) / Cancelled / RolledBack → 130
    assert_eq!(
        exit_code_of_result(&Ok(Outcome::Failed(HzipError::Cancelled))),
        130
    );
    assert_eq!(exit_code_of_result(&Ok(Outcome::Cancelled)), 130);
    assert_eq!(exit_code_of_result(&Ok(Outcome::RolledBack(vec![]))), 130);
    // Err 通道（submit 前置失败等）→ 2
    assert_eq!(
        exit_code_of_result(&Err(HzipError::Unsupported {
            what: "hz-5003 bad".into(),
        })),
        2
    );
}

/// 未知变体兜底：无法穷举构造（#[non_exhaustive]），以错误侧等价断言。
#[test]
fn unknown_variant_defaults_two() {
    assert_eq!(exit_code_of_error(&HzipError::Cancelled), 130);
    // 全部已知非取消变体都应 2（抽样覆盖文档表）
    assert_eq!(
        exit_code_of_error(&HzipError::Security {
            violation: hzip_error::SecurityViolation::PathTraversal("..".into()),
        }),
        2
    );
}
