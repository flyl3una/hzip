use super::*;
#[test]
fn entry_name_rules() {
    use hzip_error::FormatErrorKind;
    use hzip_error::HzipError;
    let assert_bad = |s: &str| {
        assert!(
            matches!(
                parse_entry_name(s),
                Err(HzipError::Format {
                    kind: FormatErrorKind::BadHeader,
                    ..
                })
            ),
            "{s:?} 应拒绝"
        );
    };
    assert_bad(""); // N1
    assert_bad("a\\b"); // N2
    assert_bad("a\x00b"); // N3 NUL
    assert_bad("a\x01b"); // N3 C0
    assert_bad("a\x7fb"); // N3 DEL
    assert_bad("a\u{9f}b"); // N3 C1
    // N4 剥 ./ 与 ././，N5 剥空段
    assert_eq!(parse_entry_name("./a").unwrap().as_str(), "a");
    assert_eq!(parse_entry_name("././a").unwrap().as_str(), "a");
    assert_eq!(parse_entry_name("a//b/").unwrap().as_str(), "a/b");
    assert_eq!(parse_entry_name("a/./b").unwrap().as_str(), "a/b");
    // 目录性由 is_dir 表达：尾 / 剥除后仍合法
    assert_eq!(parse_entry_name("dir/").unwrap().as_str(), "dir");
    // N6 放行（净化归 core）
    assert_eq!(parse_entry_name("/abs").unwrap().as_str(), "abs");
    assert!(parse_entry_name("C:/x").is_ok());
    assert!(parse_entry_name("../up").is_ok());
    assert!(parse_entry_name("CON").is_ok());
    assert!(parse_entry_name("trail ").is_ok());
    assert!(parse_entry_name("trail.").is_ok());
}

#[test]
fn entry_info_clone_and_default() {
    let meta = EntryMeta::default();
    let _ = meta.clone();
    let attrs = EntryAttrs::default();
    assert!(!attrs.read_only && !attrs.hidden && !attrs.system && attrs.executable.is_none());
}
