//! `init_locale`（crate 根的语言落地）用例：显式选择与「跟随系统」探测。
//!
//! locale 是进程级全局，且这里的用例**必须**断言非默认语言（en）——故一律进
//! [`locale_lock`] 临界区（见 `viewmodel::testutil` 模块文档），并沿用
//! 「读原值 → 改 → 复原」的写法，避免串扰同进程其它用例。

use crate::viewmodel::testutil::locale_lock;

/// 「跟随系统」的期望结果：语言主标签为 `en` → `en`，其余/探测不到 → `zh-CN`。
///
/// 与 `init_locale` 的判定同源（都读同一份探测值），因此这里断言的是**映射**，
/// 而不是本机恰好是什么语言。
fn expected_system_locale() -> &'static str {
    match hzip_platform::lang::system_language() {
        Some(tag)
            if tag
                .split('-')
                .next()
                .is_some_and(|p| p.eq_ignore_ascii_case("en")) =>
        {
            "en"
        }
        _ => "zh-CN",
    }
}

/// 显式选择：`en`/`zh-CN` 原样落地（设置面板三选一的两个具体项）。
#[test]
fn explicit_choice_is_applied_verbatim() {
    let _locale = locale_lock();
    let before = crate::current_locale();

    crate::init_locale("en");
    assert_eq!(crate::current_locale(), "en");
    crate::init_locale("zh-CN");
    assert_eq!(crate::current_locale(), "zh-CN");

    crate::init_locale(&before);
}

/// `"system"`：真探测系统语言，结果只能是受支持的两种之一，且与探测值吻合。
#[test]
fn system_follows_detected_language() {
    let _locale = locale_lock();
    let before = crate::current_locale();

    crate::init_locale("system");
    let got = crate::current_locale();
    assert!(
        got == "en" || got == "zh-CN",
        "system 不得落到未维护的语言：{got}"
    );
    assert_eq!(
        got,
        expected_system_locale(),
        "system 应映射自系统探测值 {:?}",
        hzip_platform::lang::system_language()
    );

    // 脏值（历史配置/手改配置文件）走同一条回退路径：不 panic、不落未维护语言。
    crate::init_locale("klingon-42");
    assert_eq!(crate::current_locale(), expected_system_locale());

    crate::init_locale(&before);
}
