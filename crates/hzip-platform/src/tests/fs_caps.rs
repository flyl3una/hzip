use super::*;

#[test]
fn probe_smoke() {
    let dir = crate::testutil::temp_dir("fs-caps");
    let caps = probe(dir.path());
    assert!(caps.max_path_len > 0);
    assert!(caps.max_component_len > 0);
    // 保留字符集必含分隔符（win '\' 之外 '/' 亦可作分隔）
    #[cfg(windows)]
    {
        assert!(caps.reserved_chars.contains('\\') || caps.reserved_chars.contains('<'));
    }
    #[cfg(not(windows))]
    {
        assert!(caps.reserved_chars.contains('/'));
    }
}
