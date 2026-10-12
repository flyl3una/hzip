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
