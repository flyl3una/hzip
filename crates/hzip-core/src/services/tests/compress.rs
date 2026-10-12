use super::validate_encryption_pair;
use hzip_error::HzipError;
use hzip_format::{AesKey, EncryptionRequest};
use zeroize::Zeroizing;

/// TC-ENC-CORE-001：成对携带放行、单边携带 HZ-5003 拒绝（守 3.6 契约）。
#[test]
fn pair_validation_rejects_single_sided_carry() {
    let enc = Some(EncryptionRequest::Aes(AesKey::Bits256));
    let pw = Some(Zeroizing::new("s3cret".into()));
    assert!(validate_encryption_pair(&enc, &pw).is_ok());
    assert!(validate_encryption_pair(&None, &None).is_ok());
    let e = validate_encryption_pair(&enc, &None).unwrap_err();
    assert!(
        matches!(e, HzipError::Unsupported { .. }) && e.to_string().contains("5003"),
        "单边加密方式 → HZ-5003: {e}"
    );
    let e = validate_encryption_pair(&None, &pw).unwrap_err();
    assert!(e.to_string().contains("5003"), "单边密码 → HZ-5003: {e}");
}
