use super::*;
use gpui_kit::component::h_flex;

/// 页签本体 / 关闭钮的 id 两两不同（keyed state 撞键会让点击落到别的页签上；
/// 页签 id 还兼作版式测试锚点）。
#[test]
fn ids_are_unique() {
    let tabs = [MainTab::Browse, MainTab::Jobs, MainTab::Settings];
    let mut ids: Vec<&str> = tabs
        .iter()
        .flat_map(|&t| [tab_id(t), close_id(t)])
        .collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "页签/关闭钮 id 必须两两不同");
}

/// 切换当前页签**不改变任何页签的边界**（页签栏不跳动）。
///
/// 为什么值得锁：聚焦提示是**绝对定位**的 2px 下划线（不占内容盒），且页签本体
/// 的行高/内边距与选中与否无关 ⇒ 每个页签的宽高都不随 `active` 变。若有人改成
/// 「给聚焦项加内边距/边框」而没给其他页签预留同样空间，切换时整条页签栏就会抖动
/// ——本用例立刻红。
///
/// 探针 = 页签本体（`main-tab-*`）+ 每枚关闭钮（`tab-close-*`，在页签内部）。
#[gpui_kit::test]
fn switching_active_tab_keeps_strip_geometry(cx: &mut gpui_kit::TestAppContext) {
    struct Host {
        active: MainTab,
    }
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            _window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let on_click: TabClick = std::rc::Rc::new(|_: MainTab, _: &mut App| {});
            let on_close: TabClose = std::rc::Rc::new(|_: MainTab, _: &mut App| {});
            h_flex().child(tab_row(
                &[MainTab::Browse, MainTab::Jobs, MainTab::Settings],
                self.active,
                3,
                on_click,
                on_close,
                cx,
            ))
        }
    }

    let (host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        Host {
            active: MainTab::Browse,
        }
    });
    const PROBES: [&str; 6] = [
        "main-tab-browse",
        "main-tab-jobs",
        "main-tab-settings",
        "tab-close-browse",
        "tab-close-jobs",
        "tab-close-settings",
    ];
    let probe = |vtcx: &mut gpui_kit::VisualTestContext| -> Vec<[f32; 4]> {
        PROBES
            .iter()
            .map(|sel| {
                let b = vtcx.debug_bounds(sel).expect("页签锚点缺失");
                [
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                ]
            })
            .collect()
    };

    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let before = probe(vtcx);
    // 相邻页签**紧贴**（用户口径 2026-09-23：TabBar 中间不要 margin 间隔）：
    // 后一个页签的左缘 == 前一个的右缘（容差 0.51px）。给这条栏加 `gap`/外边距
    // 立刻红。
    for (prev, next) in [
        ("main-tab-browse", "main-tab-jobs"),
        ("main-tab-jobs", "main-tab-settings"),
    ] {
        let a = vtcx.debug_bounds(prev).expect("页签锚点缺失");
        let b = vtcx.debug_bounds(next).expect("页签锚点缺失");
        let gap = f32::from(b.origin.x) - f32::from(a.origin.x + a.size.width);
        assert!(
            gap.abs() <= 0.51,
            "页签 {prev} → {next} 之间不该有间隔，实得 {gap:.2}px"
        );
    }
    for active in [MainTab::Jobs, MainTab::Settings, MainTab::Browse] {
        vtcx.update(|_window, cx| {
            host.update(cx, |h, cx| {
                h.active = active;
                cx.notify();
            });
        });
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            probe(vtcx),
            before,
            "切到 {active:?} 后页签边界必须逐像素不变（下划线不得改动版式）"
        );
    }
    drop(host);
}

/// 悬停时**文字颜色必须与常态不同**，且取的是与悬停底色成对的 token
/// （`accent_foreground`——与 kit `ButtonVariant::Ghost` 同源，工具栏/菜单项
/// 也正是这么配的）。
///
/// 为什么值得锁（2026-10-13 用户报障）：页签悬停原先写
/// `.hover(|s| s.bg(hover_bg).text_color(tab_active_foreground))`，而
/// **`tab_foreground` 与 `tab_active_foreground` 在深浅两套主题下都是同一个颜色**
/// （实测：浅色 l≈0.12 / 深色 l≈0.91）⇒ 那句 `text_color` 实际是**空操作**：
/// 底色变了、文字一字不变。用户把 `[ui].theme_color` 设成饱和色（#16A34A）后，
/// 悬停底色成了饱和绿、文字仍是深色 ⇒「鼠标移上去还是深字，看不清」；
/// 而菜单栏/工具栏悬停文字看着正常，差别正在这里——它们走 Ghost 的成对 token。
///
/// 四条组合（浅/深 × 默认/自定义主题色）都断言，是因为两类 token 的关系在两套
/// 主题下正好相反：浅色下常态文字深、`accent_foreground` 是白；深色下常态文字
/// 浅、`accent_foreground` 是深。只测一套会漏掉另一半（该 bug 在浅色下最先暴露，
/// 但同一句空操作在深色下同样存在）。
#[gpui_kit::test]
fn hover_foreground_is_paired_and_differs_from_idle(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::theme::{ThemeModeChoice, apply_theme_cfg};
    use gpui_kit::component::ActiveTheme as _;
    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        for (mode, color) in [
            (ThemeModeChoice::Light, ""),
            (ThemeModeChoice::Light, "#16A34A"), // 用户实机主题色
            (ThemeModeChoice::Dark, ""),
            (ThemeModeChoice::Dark, "#16A34A"),
        ] {
            let mut cfg = hzip_config::Config::defaults();
            cfg.ui.theme_color = color.to_string();
            apply_theme_cfg(mode, &cfg, cx);
            let theme = cx.theme();
            let (idle_fg, active_fg, accent_fg) = (
                theme.tab_foreground,
                theme.tab_active_foreground,
                theme.accent_foreground,
            );
            let (_, hover_fg) = hover_style(cx);
            assert_eq!(
                hover_fg, accent_fg,
                "{mode:?} color={color:?}：悬停前景必须取 `accent_foreground`（与悬停底色 \
                 `tokens.accent` 成对 = kit Ghost 按钮同款，工具栏/菜单项也这么配）"
            );
            assert_ne!(
                hover_fg, idle_fg,
                "{mode:?} color={color:?}：悬停前景不得等于常态前景（写成一个空操作正是本 bug）"
            );
            assert_ne!(
                hover_fg, active_fg,
                "{mode:?} color={color:?}：悬停前景不得等于「活动页签」前景（同上，空操作）"
            );
        }
    });
}
