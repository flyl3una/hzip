use super::*;

#[test]
fn remember_get_forget_clear() {
    let c = PasswordCache::new();
    assert!(c.is_empty());
    let a = Path::new("/tmp/a.zip");
    let b = Path::new("/tmp/b.zip");
    c.remember(a, Zeroizing::new("pw-a".into()));
    c.remember(b, Zeroizing::new("pw-b".into()));
    assert_eq!(c.get(a).as_deref().map(String::as_str), Some("pw-a"));
    assert_eq!(c.get(b).as_deref().map(String::as_str), Some("pw-b"));
    // 覆盖：改密码后重输即刷新
    c.remember(a, Zeroizing::new("pw-a2".into()));
    assert_eq!(c.get(a).as_deref().map(String::as_str), Some("pw-a2"));
    // 单条丢弃互不影响
    c.forget(a);
    assert!(c.get(a).is_none());
    assert_eq!(c.len(), 1);
    // 清空（右键菜单触发的加解密 = 重新打开语义）
    c.clear();
    assert!(c.is_empty());
}

#[test]
fn debug_never_prints_password() {
    let c = PasswordCache::new();
    c.remember(Path::new("/tmp/a.zip"), Zeroizing::new("s3cret".into()));
    let s = format!("{c:?}");
    assert!(!s.contains("s3cret"), "明文绝不进 Debug：{s}");
}
