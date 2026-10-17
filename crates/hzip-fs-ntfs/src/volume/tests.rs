//! `volume` 模块的单元用例（T-STD 规则②：业务文件里不写内联 `mod tests` 体）。

use super::*;

#[test]
fn path_helpers_agree_on_root_and_nested() {
    assert_eq!(join_path("", "a"), "a");
    assert_eq!(join_path("d", "a"), "d/a");
    assert_eq!(split_parent("a/b"), ("a", "b"));
    assert_eq!(split_parent("b"), ("", "b"));
}

#[test]
fn content_classification_follows_the_attribute_bits() {
    use attr_io::attr_flags::{COMPRESSED, ENCRYPTED, SPARSE};
    assert_eq!(classify(0), Content::Plain);
    assert_eq!(classify(COMPRESSED), Content::Lznt1);
    assert_eq!(classify(SPARSE), Content::Sparse);
    assert_eq!(classify(ENCRYPTED), Content::Encrypted);
    // 加密优先（两个都置位时按读不出的那一档算）。
    assert_eq!(classify(ENCRYPTED | COMPRESSED), Content::Encrypted);
    assert!(Content::Plain.writable() && Content::Plain.readable());
    assert!(!Content::Lznt1.writable() && Content::Lznt1.readable());
    assert!(!Content::Wof.writable() && !Content::Wof.readable());
    assert!(!Content::Encrypted.writable() && !Content::Encrypted.readable());
}

#[test]
fn upstream_boundary_constant_is_the_documented_one() {
    // 上游建 `$INDEX_ALLOCATION` 时写的常驻 `$Bitmap:$I30` 是 8 字节 ⇒ 64 位。
    assert_eq!(INDEX_BLOCKS_MAX, 8 * 8);
}
