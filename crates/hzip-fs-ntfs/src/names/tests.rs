//! `names` 模块的单元用例（T-STD 规则②：业务文件里不写内联 `mod tests` 体）。

use super::*;

#[test]
fn chinese_and_emoji_round_trip() {
    for name in [
        "中文名.txt",
        "表情🙂包.tar",
        "日本語のフォルダ",
        "混合-mix-🙂-中",
    ] {
        let utf16 = entry_name_to_utf16(name);
        assert_eq!(
            utf16_to_entry_name(&utf16),
            name,
            "往返必须逐字符一致: {name}"
        );
        assert!(!is_lossy_name(name));
    }
}

#[test]
fn lone_surrogate_becomes_replacement_and_the_entry_survives() {
    // 未配对的高代理 + 正常尾巴：典型「非法代理对」形态。
    let raw = [0xD83D, 0x0041];
    let name = utf16_to_entry_name(&raw);
    assert_eq!(name, "\u{FFFD}A");
    assert!(is_lossy_name(&name), "替换字符是丢信息的标志");
    assert!(!name.is_empty(), "条目名不为空 ⇒ 条目不会被丢掉");
}

#[test]
fn name_length_limit_counts_utf16_units_not_bytes() {
    // 255 个 BMP 汉字 = 255 个 UTF-16 码元（UTF-8 是 765 字节）⇒ 合法。
    let ok: String = "汉".repeat(NAME_MAX);
    assert_eq!(entry_name_to_utf16(&ok).len(), NAME_MAX);
    validate_new_name(&ok).expect("255 码元合法");
    // 256 个码元 ⇒ 拒。
    let too_long = "汉".repeat(NAME_MAX + 1);
    assert!(matches!(
        validate_new_name(&too_long),
        Err(VolError::Unsupported("vol.err.name-too-long"))
    ));
    // 代理对（emoji）一个字算两个码元。
    let pairs: String = "🙂".repeat(NAME_MAX / 2 + 1);
    assert!(matches!(
        validate_new_name(&pairs),
        Err(VolError::Unsupported("vol.err.name-too-long"))
    ));
}

#[test]
fn illegal_names_are_refused_before_upstream_sees_them() {
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a*b",
        "a?b",
        "a\"b",
        "a<b",
        "a>b",
        "a|b",
        "a:b",
        "nul\0",
        "tab\tname",
        "trail ",
        "trail.",
    ] {
        assert!(validate_new_name(bad).is_err(), "{bad:?} 应当被写前拒掉");
    }
    for good in ["a", "a b", "点到为止.txt", " leading-space-is-fine"] {
        validate_new_name(good).expect("合法名不该被拒");
    }
}
