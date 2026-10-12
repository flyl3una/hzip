//! i18n 初始化（02 §12.2；rust-i18n，zh-CN/en 两套）。
//!
//! CLI 文案源在 `locales/{zh-CN,en}.toml`；错误文案 key 即 `err.hz_{code}`
//! （与 hzip-error `i18n_key()` 一致）。当前 CLI 文案以内联中文为主、
//! locales 提供错误码总表对应文案（冻结 key 集合），供 GUI 共用源对齐。
//! rust-i18n 的 t! 宏在未匹配 locale 时回退默认文案（不 panic）。

use rust_i18n::t;

/// 按错误码取文案（02 §12.2：key = `err.hz_{code}`；未登记回退码串）。
pub(crate) fn err_text(code: u32) -> String {
    t!("err.hz_0000", code = format!("HZ-{code:04}")).to_string()
}

#[cfg(test)]
#[path = "tests/i18n.rs"]
mod tests;
