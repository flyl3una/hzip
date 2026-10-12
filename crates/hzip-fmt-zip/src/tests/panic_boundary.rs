use super::*;

#[test]
fn catch_panic_converts_badheader() {
    let r: Result<u64, HzipError> = guard(|| panic!("boom"));
    assert!(matches!(
        r,
        Err(HzipError::Format {
            kind: FormatErrorKind::BadHeader,
            ..
        })
    ));
}

#[test]
fn ok_passthrough() {
    assert_eq!(guard(|| Ok::<u64, HzipError>(42)).unwrap(), 42);
}

#[test]
fn error_propagates() {
    let r: Result<u64, HzipError> = guard(|| Err(HzipError::Cancelled));
    assert!(matches!(r, Err(HzipError::Cancelled)));
}

#[test]
fn guard_ok_works() {
    assert_eq!(guard_ok(|| 7).unwrap(), 7);
}
