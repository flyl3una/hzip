use super::*;

#[test]
fn password_ref_zeroize_semantics() {
    let pw = Zeroizing::new("hzip-密码测试".to_string());
    let r = PasswordRef::new(&pw);
    assert_eq!(r.as_str(), Some("hzip-密码测试"));
    assert_eq!(PasswordRef::<'_>::none().as_str(), None);
}
