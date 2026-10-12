use super::*;
use crate::ui::bar::menu_bar::ToolbarStyle;

fn default_props<'a>(mru: &'a [String], columns: &'a ColumnsConfig) -> ToolbarProps<'a> {
    ToolbarProps {
        enable: ToolbarEnable {
            has_archive: true,
            can_extract: true,
            can_add: true,
            can_delete: true,
            can_preview: true,
            can_columns: true,
            desktop_available: true,
            busy: false,
        },
        mru,
        columns,
    }
}

/// 灰态口径（用户口径 2026-09-29）：删除须有聚焦选中、预览须单选一个文件、
/// 添加随档案可编辑、解压随档案；忙碌一律置灰。
#[test]
fn enabled_matrix_follows_toolbar_enable() {
    let e = ToolbarEnable {
        has_archive: false,
        ..Default::default()
    };
    assert!(enabled_for(&e, &ToolbarAction::Open), "无档案也能打开");
    assert!(!enabled_for(&e, &ToolbarAction::Extract));
    assert!(!enabled_for(&e, &ToolbarAction::ExtractAll));
    assert!(!enabled_for(&e, &ToolbarAction::AddFiles));
    assert!(!enabled_for(&e, &ToolbarAction::Delete));
    assert!(!enabled_for(&e, &ToolbarAction::Preview));
    assert!(!enabled_for(
        &e,
        &ToolbarAction::ToggleColumn("size".into())
    ));

    // 有档案 + 有选中 + 可编辑 → 全开；`can_preview` 由快照按「单选文件」给
    let columns = ColumnsConfig::default();
    let open = default_props(&[], &columns);
    for a in [
        ToolbarAction::Extract,
        ToolbarAction::AddFiles,
        ToolbarAction::Delete,
        ToolbarAction::Preview,
        ToolbarAction::ToggleColumn("size".into()),
        ToolbarAction::ExtractMode(ExtractMode::Desktop),
    ] {
        assert!(enabled_for(&open.enable, &a), "{a:?} 应可点");
    }
    // 忙碌 → 一律置灰
    let busy = ToolbarEnable {
        busy: true,
        ..open.enable
    };
    assert!(!enabled_for(&busy, &ToolbarAction::Open));
    assert!(!enabled_for(&busy, &ToolbarAction::Delete));
}

/// 解压▾ 五种方式：数量、菜单顺序（需求给序）与 i18n key 唯一。
#[test]
fn extract_modes_are_the_five_required_in_order() {
    assert_eq!(ExtractMode::ALL.len(), 5);
    let keys: Vec<&str> = ExtractMode::ALL.iter().map(|m| m.key()).collect();
    assert_eq!(
        keys,
        vec![
            "toolbar.extract_mode.auto",
            "toolbar.extract_mode.here",
            "toolbar.extract_mode.dir",
            "toolbar.extract_mode.desktop",
            "toolbar.extract_mode.auto_desktop",
        ],
        "顺序 = 需求顺序：自动解压 / 解压到当前文件夹 / 解压到「压缩文件名」文件夹 / 解压到桌面 / 自动解压到桌面"
    );
    assert!(ExtractMode::Desktop.needs_desktop());
    assert!(ExtractMode::AutoDesktop.needs_desktop());
    assert!(!ExtractMode::Auto.needs_desktop());
}

/// 查看▾ 菜单项：全列在列 + 勾选态取自配置 + Name 恒勾（不可隐藏）。
#[test]
fn column_menu_entries_list_all_columns_with_checked_state() {
    let default = ColumnsConfig::default();
    let entries = column_menu_entries(&default);
    assert_eq!(entries.len(), BrowseColumnId::ALL.len());
    assert_eq!(
        entries.iter().map(|(_, k, _)| *k).collect::<Vec<_>>(),
        BrowseColumnId::ALL
            .iter()
            .map(|c| c.key())
            .collect::<Vec<_>>(),
        "菜单顺序 = 默认全序"
    );
    assert!(entries[0].2, "名称列恒勾选");
    for (_, key, checked) in &entries {
        let col = BrowseColumnId::ALL
            .iter()
            .find(|c| c.key() == *key)
            .expect("key 必来自 ALL");
        assert_eq!(
            *checked,
            !default.hidden.contains(col),
            "{key} 勾选态 = 未隐藏"
        );
    }
    assert!(
        entries.iter().filter(|(_, _, c)| !*c).count() > 0,
        "默认存在隐藏列（Level/Encryption/Crc/Attrs），否则本测试测不到勾选态差异"
    );
}

/// 紧排口径（用户要求 2026-09-23）——断言面全是真实渲染边界：
///
/// ① 按钮上缘 **==** 整条上缘、整条下缘只多出自己那 1px 下边框 ⇒ 上下内边距
///   真的没了（旧实现 `.py(sp(1.))` 会让上下各多出 `sp(1.)`）；
/// ② 相邻工具按钮的左右缘**重合** ⇒ 按钮组 `gap` 真的没了（按钮不带左右
///   外边距，间隔只由各自 `px_2p5` 的内部内边距提供）；
/// ③ 整条仍保留 `px(sp(2.))` 的左右留白（用户只要求删上下 padding）。
#[gpui_kit::test]
fn buttons_are_flush_and_the_row_has_no_vertical_padding(cx: &mut gpui_kit::TestAppContext) {
    struct Host;
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            _window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let emit: ToolbarEmit =
                std::rc::Rc::new(|_: &ToolbarAction, _: &mut gpui_kit::Window, _: &mut App| {});
            let mru: Vec<String> = Vec::new();
            let columns = ColumnsConfig::default();
            let props = default_props(&mru, &columns);
            // 无页签右槽：整条高度只由按钮决定（页签更高会顶起整条，量不准）
            let bar = toolbar(&props, ToolbarStyle::IconText, &emit, None, cx);
            // 先落进容器再返回（同 shell 的用法）：`toolbar` 的 `impl IntoElement`
            // 会捕获 `&emit` 的借用生命周期，直接当返回值即要求它 `'static`。
            h_flex().child(bar)
        }
    }

    let (_host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        Host
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let row = vtcx.debug_bounds("toolbar").expect("工具栏锚点缺失");
    // T-BAR：六钮 + 三下拉，**去掉「解压全部」**（用户口径 2026-09-29）：
    // 打开/解压各是**一个拆分控件**（主动作 + 箭头半，中间 1px 竖线），
    // 故 id 序列里相邻的对之间多出 `HAIRLINE` 的缝。
    let ids = [
        "tb-open",
        "tb-open-menu",
        "tb-extract",
        "tb-extract-menu",
        "tb-add",
        "tb-delete",
        "tb-preview",
        "tb-columns",
    ];
    // 期望缝宽：拆分对之间 = 分隔竖线；其余 = 0（按钮不带外边距）
    let gaps = [1.0f32, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    assert!(
        vtcx.debug_bounds("tb-extract-all").is_none(),
        "「解压全部」按钮已移除（用户口径 2026-09-29）"
    );
    assert!(
        vtcx.debug_bounds("tb-compress").is_none(),
        "旧「压缩」按钮已改语义为「添加」（id 改 `tb-add`）"
    );
    let btns: Vec<_> = ids
        .iter()
        .map(|id| {
            vtcx.debug_bounds(id)
                .unwrap_or_else(|| panic!("工具栏应保留 {id}"))
        })
        .collect();

    // ① 上下 padding 已移除：按钮上缘 = 整条上缘，整条下缘只多出自己的
    //    1px 下边框（`.border_b_1()`）——旧实现 `.py(sp(1.))` 会各多 2×`sp(1.)`。
    assert_eq!(
        f32::from(btns[0].origin.y),
        f32::from(row.origin.y),
        "工具栏顶部不得有内边距"
    );
    assert_eq!(
        f32::from(row.origin.y + row.size.height)
            - f32::from(btns[0].origin.y + btns[0].size.height),
        1.0,
        "工具栏底部只应有 1px 下边框，不得有内边距"
    );
    // ② 无外边距：相邻控件左右缘相贴（拆分对内多出 1px 分隔竖线——那正是
    //    「合到一起」的可见证据，不是两个独立按钮的间隔）。
    for (pair, expect) in btns.windows(2).zip(gaps) {
        let gap = f32::from(pair[1].origin.x) - f32::from(pair[0].origin.x + pair[0].size.width);
        assert_eq!(
            gap,
            expect * f32::from(HAIRLINE),
            "相邻控件的缝宽应为 {expect}px（拆分对 = 分隔线宽 1px；其余 = 0）"
        );
    }
    // ②b 拆分按钮「看成一个控件」：箭头半**只画图标**（无文字）——宽度明显
    //     小于同排带文字的主按钮（用户口径 2026-09-29）
    for (main_id, arrow_id) in [
        ("tb-open", "tb-open-menu"),
        ("tb-extract", "tb-extract-menu"),
    ] {
        let main = vtcx.debug_bounds(main_id).expect("主半锚点");
        let arrow = vtcx.debug_bounds(arrow_id).expect("箭头半锚点");
        assert!(
            f32::from(arrow.size.width) + 24.0 <= f32::from(main.size.width),
            "{arrow_id} 必须只画图标：宽度 {:.1}px 应比带文字的 {main_id}（{:.1}px）窄至少 24px",
            f32::from(arrow.size.width),
            f32::from(main.size.width)
        );
    }
    // ③ 左右留白保留：第一枚按钮左缘 = 整条左缘 + sp(2.)
    let pad_x =
        vtcx.update(|window, _| f32::from(crate::ui::spacing::sp(2.).to_pixels(window.rem_size())));
    assert_eq!(
        f32::from(btns[0].origin.x) - f32::from(row.origin.x),
        pad_x,
        "工具栏左右留白应保持 sp(2.)（用户只要求删上下 padding）"
    );
}

/// 页签栏与工具按钮**同高、同一条基线、不多一条线**（用户口径 2026-09-23：
/// 「这条栏不要自己单独的边框…不然两边高度不一致」）。
///
/// 把**真实**页签行（`ui/bar/tab_bar.rs`）塞进工具栏右槽后断言：
/// ① 每个页签与工具按钮**等高**（页签走同一 `component_size()` + 同一套
///   `h_5/h_6/h_8` 映射）；② 两者**上缘重合**（同一条基线，不一边高一边低）；
/// ③ 整条高度 = 按钮高 + 1px —— 那 1px 只能是工具栏自己的 `border_b_1`，
///   页签栏不得再带自己的边框线（旧 `TabVariant::Underline` 是 36px + 一条
///   全宽 `#border-b`，正是这条口径要防的回归）。
///
/// 两个密度档都量一遍：紧凑档两边一起收，仍须同高。
#[gpui_kit::test]
fn tab_strip_matches_button_height(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::MainTab;
    use crate::viewmodel::density::Density;

    struct Host;
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            _window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let emit: ToolbarEmit =
                std::rc::Rc::new(|_: &ToolbarAction, _: &mut gpui_kit::Window, _: &mut App| {});
            let on_click: crate::ui::bar::tab_bar::TabClick =
                std::rc::Rc::new(|_: MainTab, _: &mut App| {});
            let on_close: crate::ui::bar::tab_bar::TabClose =
                std::rc::Rc::new(|_: MainTab, _: &mut App| {});
            let tabs = crate::ui::bar::tab_bar::tab_row(
                &[MainTab::Browse, MainTab::Jobs, MainTab::Settings],
                MainTab::Browse,
                2,
                on_click,
                on_close,
                cx,
            )
            .into_any_element();
            let mru: Vec<String> = Vec::new();
            let columns = ColumnsConfig::default();
            let props = default_props(&mru, &columns);
            let bar = toolbar(&props, ToolbarStyle::IconText, &emit, Some(tabs), cx);
            h_flex().child(bar)
        }
    }

    let (_host, vtcx) = cx.add_window_view(|_window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        Host
    });

    let measure = |vtcx: &mut gpui_kit::VisualTestContext| {
        let row = vtcx.debug_bounds("toolbar").expect("工具栏锚点缺失");
        let btn = vtcx.debug_bounds("tb-extract").expect("工具按钮锚点缺失");
        let tabs: Vec<(&str, f32, f32)> = ["main-tab-browse", "main-tab-jobs", "main-tab-settings"]
            .iter()
            .map(|sel| {
                let b = vtcx.debug_bounds(sel).expect("页签锚点缺失");
                (*sel, f32::from(b.size.height), f32::from(b.origin.y))
            })
            .collect();
        (
            f32::from(row.size.height),
            f32::from(btn.size.height),
            f32::from(btn.origin.y),
            tabs,
        )
    };

    for density in [Density::Standard, Density::Compact] {
        crate::ui::spacing::set_density(density);
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (row_h, btn_h, btn_y, tabs) = measure(vtcx);
        for (sel, tab_h, tab_y) in tabs {
            assert_eq!(
                tab_h, btn_h,
                "{density:?}：{sel} 必须与工具按钮等高（用户口径：两边高度一致）"
            );
            assert_eq!(
                tab_y, btn_y,
                "{density:?}：{sel} 必须与工具按钮上缘重合（同一条基线）"
            );
        }
        assert_eq!(
            row_h - btn_h,
            1.0,
            "{density:?}：整条只应比按钮高 1px（工具栏自己的下边框）；\
             页签栏不得再带独立边框线，也不得把整条顶高"
        );
    }
    crate::ui::spacing::set_density(Density::Standard);
}
