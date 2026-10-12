use super::*;

/// 表本身的自洽性：扩展名小写、无点、不重复；MIME 形如 `type/subtype`。
#[test]
fn table_is_well_formed() {
    let mut seen = std::collections::BTreeSet::new();
    for m in EXT_MIMES {
        assert!(
            !m.ext.is_empty() && !m.ext.contains('.') && m.ext == m.ext.to_ascii_lowercase(),
            "扩展名须为小写无点：{m:?}"
        );
        assert!(seen.insert(m.ext), "扩展名重复：{}", m.ext);
        assert!(
            m.mime
                .split_once('/')
                .is_some_and(|(t, s)| !t.is_empty() && !s.is_empty() && !m.mime.contains(' ')),
            "MIME 格式异常：{m:?}"
        );
    }
}

/// 同一 MIME 可服务多个别名（`tbz`/`tbz2` → 同一个压缩包 MIME）：这不算重复，
/// 但**反过来**（一个扩展名两个 MIME）会导致 `MimeType=` 与 `mimeapps.list`
/// 的键漂移，必须禁止。
#[test]
fn lookup_is_case_and_dot_insensitive() {
    assert_eq!(mime_of("ZIP"), Some("application/zip"));
    assert_eq!(mime_of(".tgz"), Some("application/x-compressed-tar"));
    assert_eq!(mime_of("zip"), mime_of("ZIP"));
    assert_eq!(mime_of("nope"), None);
}
