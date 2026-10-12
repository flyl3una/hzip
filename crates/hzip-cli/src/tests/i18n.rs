use super::*;

#[test]
fn i18n_loaded() {
    // locales 存在且宏可解析（编译期已保证）；此处仅行为抽查
    let s = err_text(3001);
    assert!(!s.is_empty());
}
