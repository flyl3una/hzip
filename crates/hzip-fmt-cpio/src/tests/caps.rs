use super::*;

#[test]
fn caps_are_read_only() {
    let c = caps();
    assert!(c.read);
    assert!(!c.write && !c.append && !c.edit);
    assert_eq!(c.encryption, EncryptionCaps::None);
    assert!(c.methods.is_empty() && c.write_encryption.is_empty());
}

#[test]
fn signature_is_newc_at_zero() {
    assert_eq!(SIGNATURE.offset, 0);
    assert_eq!(SIGNATURE.magic, b"070701");
    assert_eq!(SIGNATURE.extensions, &["cpio"]);
    assert_eq!(SIGNATURE.display, "cpio");
}
