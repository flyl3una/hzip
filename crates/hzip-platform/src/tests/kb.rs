use super::*;

// 纯冒烟：无键按下时多数环境返回 false（CI 无人按键；本机有人按键
// 也不 panic——不锁具体值）
#[test]
fn async_modifiers_smoke() {
    let _ = async_key_modifiers();
}
