//! [`super`] 的用例：中文字形缺失 → 回退英文的**纯策略**，以及探针真的在问
//! 文本系统（不是返回常量的哑实现）。

use super::*;

/// 「自动」语言（`system` / 脏值）+ 缺中文字形 → 回退英文。
#[test]
fn auto_language_falls_back_when_cjk_glyphs_are_missing() {
    for lang in ["system", "", "klingon-42", "zh", "zh-Hans"] {
        assert_eq!(
            decide(lang, ZH, Some(false)),
            Some("en"),
            "自动语言 {lang:?} 在缺中文字形时必须回退英文"
        );
    }
}

/// 显式 `zh-CN` → 尊重用户选择（只告警，不擅自改语言）。
#[test]
fn explicit_chinese_is_respected() {
    assert_eq!(decide("zh-CN", ZH, Some(false)), None);
    assert_eq!(
        decide("  zh-CN  ", ZH, Some(false)),
        None,
        "首尾空白不该改变判定"
    );
}

/// 中文可渲染 / 探测不可信 / 界面本来就不是中文 → 一律不动。
#[test]
fn nothing_changes_otherwise() {
    assert_eq!(
        decide("system", ZH, Some(true)),
        None,
        "中文可渲染就不该回退"
    );
    assert_eq!(decide("system", ZH, None), None, "探测不可信时不做任何回退");
    assert_eq!(
        decide("system", "en", Some(false)),
        None,
        "英文界面与中文字形无关"
    );
}

/// 探针走真实文本系统：拉丁字形必须拿得到（否则 [`cjk_available`] 会自我否决，
/// 本用例同时钉住「探针不是哑实现」）。
#[gpui_kit::test]
fn probe_asks_the_live_text_system(cx: &mut gpui_kit::TestAppContext) {
    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        assert_eq!(
            glyphs_available(cx, LATIN_PROBE),
            Some(true),
            "拉丁字形探测失败：文本系统没给出字形"
        );
        // 中文结论随本机字体而变（有 CJK 字体 → Some(true)，没有 → Some(false)），
        // 但同一环境里两次探测必须给出**同一个**结论（探针不得依赖缓存/时序）。
        let cjk = cjk_available(cx);
        assert_eq!(
            cjk_available(cx),
            cjk,
            "同一环境下两次中文字形探测结论不一致"
        );
    });
}
