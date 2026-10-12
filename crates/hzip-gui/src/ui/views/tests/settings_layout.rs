use super::*;

// 分类切换状态机：下标 ↔ 枚举双向、default 起点、越界守界
#[test]
fn tab_state_machine() {
    assert_eq!(SettingsTab::default(), SettingsTab::General);
    for ix in 0..SettingsTab::ALL.len() {
        let tab = SettingsTab::from_index(ix);
        assert_eq!(tab.index(), ix);
    }
    assert_eq!(SettingsTab::from_index(6), SettingsTab::Language);
    assert_eq!(
        SettingsTab::from_index(9),
        SettingsTab::Language,
        "越界守界"
    );
}

// 页签展示文案与分类枚举一一对应（TabBar 渲染契约）。
//
// 断言的是**查表结果**（不是 key 本身）：key 序列对齐由注册表侧
// `SettingsTab::LABEL_KEYS` 的长度/顺序检查覆盖，此处守「渲染出来的
// 文案确实是人话且顺序正确」——i18n 化后仍然有效（locale 由守卫钉死 zh-CN）。
#[test]
fn tab_labels_match_enum() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    assert_eq!(SettingsTab::LABEL_KEYS.len(), SettingsTab::ALL.len());
    assert_eq!(SettingsTab::General.label(), "常规");
    assert_eq!(SettingsTab::Assoc.label(), "文件关联");
    assert_eq!(SettingsTab::ContextMenu.label(), "上下文菜单");
    assert_eq!(SettingsTab::Extract.label(), "解压设置");
    assert_eq!(SettingsTab::Compress.label(), "压缩设置");
    assert_eq!(SettingsTab::Viewer.label(), "查看器");
    assert_eq!(SettingsTab::Language.label(), "语言");
}

// 行内首行对齐契约（用户实测：设置页文字「一会儿高一会儿低」）。
//
// # 守的缺陷
//
// 行内各控件固有高度不同（Switch 20 / 复选框 21 / 文本 22.5 / Select 32），
// 而旧实现每行都按**顶边**对齐（`Field::items_start`）⇒ 标签文字与控件的
// 相对位置随控件高度漂移：Select 行标签中心比控件中心高 4.75px、Switch 行
// 低 1.25px——**跨行相差 6px**，即用户看到的「文字一会儿高一会儿低」；
// 行尾 ？ 图标（20px 按钮）与标签行框（22.5px）顶边对齐，另差 1.25px。
//
// # 契约（`settings_render::ROW_BAND`）
//
// 行内**首行**的一切（标签文本、？ 图标、控件）都在同一高度带内垂直居中：
// - 高 ≤ 行带的控件（Switch/Select/ColorPicker/文本行）：**控件中心 ==
//   标签文本中心**（带内居中 ⇒ 与控件固有高度无关）；
// - 高于行带的块（勾选组/关联组/列块）：**块顶边 == 带顶边**，且**块首行
//   中心 == 标签中心**（`ctlrow0-*` 锚点）；
// - ？ 图标中心 == 标签文本中心；
// - 页级动作行与上下文菜单状态行同样适用。
//
// 断言全部取自**渲染后的真实边界**（`debug_bounds`），不是源码文本。
// 实测差值上限 0.25px（flex 居中取整），故容差 0.5px。
#[gpui_kit::test]
fn row_first_line_alignment_is_uniform(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::settings_registry::{ACTIONS, ActionKind, REGISTRY};

    const TOL: f32 = 0.5;
    // 密度是进程级量：本用例断言**绝对**带高，故先钉住标准档（密度行为
    // 另有 `density_scales_rendered_row_band` 专测，两用例互不牵扯）
    crate::ui::spacing::set_density(crate::viewmodel::density::Density::Standard);
    let band = f32::from(crate::ui::views::settings_render::row_band());

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let candidates = crate::ui::contracts::associable_extensions(&engine);
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, candidates, Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");

    // 本 fork 的 `debug_bounds` 只收 `&'static str`（gpui-pre 0.3.8
    // test_context.rs:990；0.3.4 时代同签名——本仓当时同样靠 leak 调用）；
    // 锚点名按注册表 id 动态生成，故 leak 少量短串换取「全量遍历」。测试进程
    // 退出即回收，不影响任何进程外状态。
    let bounds = |vtcx: &mut gpui_kit::VisualTestContext, name: String| {
        vtcx.debug_bounds(Box::leak(name.into_boxed_str()))
            .map(|b| (f32::from(b.origin.y), f32::from(b.size.height)))
    };

    // 中心线（顶边 + 半高）
    let center = |o: (f32, f32)| o.0 + o.1 / 2.;

    let mut checked = 0usize;
    for tab in SettingsTab::ALL {
        vtcx.update(|window, cx| {
            panel.update(cx, |p, _| p.tab = tab);
            window.draw(cx).clear(cx);
        });
        for def in REGISTRY.iter().filter(|d| d.page == tab) {
            let key = def.id.key();
            let lbl = bounds(vtcx, format!("lbl-{key}"))
                .unwrap_or_else(|| panic!("[{tab:?}] 缺标签锚点 lbl-{key}"));
            let ctl = bounds(vtcx, format!("ctl-{key}"))
                .unwrap_or_else(|| panic!("[{tab:?}] 缺控件锚点 ctl-{key}"));
            let (lbl_top, lbl_h) = lbl;
            let (ctl_top, ctl_h) = ctl;
            let lbl_c = center(lbl);

            // 密集列表（勾选组 / 列块）：标签与块首行**顶对齐**，两边都不进
            // 行带（见 `settings_render::dense_label_with_help`）。断言面 =
            // **文字行框顶边**：标签锚点与首行锚点都落在文字行框上，顶边应当
            // 重合。不比中心，是因为两侧行框高不同（标签 ≈1.4rem 自然行框、
            // 复选行 = 1.5rem ⇒ 中心天然差 0.75px，属设计内）。
            let dense = matches!(
                def.widget,
                WidgetKind::CheckGroup { .. } | WidgetKind::ColumnsBlock
            );
            if dense {
                let r0 = bounds(vtcx, format!("ctlrow0-{key}"))
                    .unwrap_or_else(|| panic!("[{tab:?}] 缺块首行锚点 ctlrow0-{key}"));
                let d = r0.0 - lbl_top;
                assert!(
                    d.abs() <= TOL,
                    "[{tab:?}] {key}: 密集列表首行顶边比标签顶边偏 {d:+.2}px\
                     —— 标签须与块首行同顶（两侧都带 `row_gap` 内边距）"
                );
            } else if ctl_h <= band {
                // ① 单行控件：带内居中 ⇒ 中心与标签文本中心一致
                let d = center(ctl) - lbl_c;
                assert!(
                    d.abs() <= TOL,
                    "[{tab:?}] {key}: 控件中心比标签中心偏 {d:+.2}px（控件高 {ctl_h:.2}）\
                     —— 控件未在行带 {band:.2} 内与标签同轴居中"
                );
            } else {
                // ② 块状控件：块顶边 = 带顶边（首行另有 ③）
                let want_top = lbl_top - (band - lbl_h) / 2.;
                assert!(
                    (ctl_top - want_top).abs() <= TOL,
                    "[{tab:?}] {key}: 块顶边 {ctl_top:.2} ≠ 带顶边 {want_top:.2}（块高 {ctl_h:.2}）"
                );
            }

            // ③ 块首行与标签同轴（关联组标题锚点；密集列表走上面的顶边分支）
            if !dense && let Some(r0) = bounds(vtcx, format!("ctlrow0-{key}")) {
                let d = center(r0) - lbl_c;
                assert!(
                    d.abs() <= TOL,
                    "[{tab:?}] {key}: 块首行中心比标签中心偏 {d:+.2}px（首行高 {:.2}）\
                     —— 块的每个直接行进 `band()` 后首行才会落在行带中心",
                    r0.1
                );
            }

            // ④ ？ 图标与标签同轴（多行标签的图标停在带中心，故只断言单行标签）
            if let Some(h) = bounds(vtcx, format!("help-btn-{key}")).filter(|_| lbl_h <= band) {
                let d = center(h) - lbl_c;
                assert!(
                    d.abs() <= TOL,
                    "[{tab:?}] {key}: ？图标中心比标签中心偏 {d:+.2}px（图标高 {:.2}）",
                    h.1
                );
            }
            checked += 1;
        }

        // ⑤ 页级动作行：按钮中心 == 标签中心
        let actions: Vec<&'static str> = ACTIONS
            .iter()
            .filter(|a| a.page == tab && matches!(a.kind, ActionKind::Emit(_)))
            .map(|a| a.key)
            .collect();
        if !actions.is_empty() {
            let lbl = bounds(vtcx, "lbl-actions".to_string()).expect("缺 lbl-actions");
            let lbl_c = center(lbl);
            for key in actions {
                let b = bounds(vtcx, format!("act-{key}"))
                    .unwrap_or_else(|| panic!("[{tab:?}] 缺动作按钮锚点 act-{key}"));
                let d = center(b) - lbl_c;
                assert!(
                    d.abs() <= TOL,
                    "[{tab:?}] act-{key}: 按钮中心比标签中心偏 {d:+.2}px（按钮高 {:.2}）",
                    b.1
                );
            }
        }

        // ⑥ 上下文菜单页的状态行（不在注册表内）
        if tab == SettingsTab::ContextMenu {
            let lbl = bounds(vtcx, "lbl-ctx-status".to_string()).expect("缺 lbl-ctx-status");
            let ctl = bounds(vtcx, "ctl-ctx-status".to_string()).expect("缺 ctl-ctx-status");
            let d = center(ctl) - center(lbl);
            assert!(
                d.abs() <= TOL,
                "[{tab:?}] ctx-status: 状态行中心比标签中心偏 {d:+.2}px（行高 {:.2}）",
                ctl.1
            );
        }
    }
    assert!(checked >= 24, "注册表行数异常：只检查了 {checked} 行");
}

// 密集列表的行距（用户口径）：上下各 `row_gap` —— 紧凑 0 / 标准 1 / 宽松 2px，
// 与文件列表 `row_h = 行框 + 2×row_gap` 同一口径。
//
// 断言面 = **块高随密度线性增长**（每行多出 `2 × Δrow_gap`，乘行数即增量）：
// 每行都要靠渲染证据，不能只看源码里写的是不是 `row_gap()`。
// 拦得住的旧实现：① 固定 `py_0p5`（增量为 0）；② 逐行进 `band()`
// （增量为 `row_band` 的缩放量 32→44.8：查看器 6 行 ≈ +77px）。
#[gpui_kit::test]
fn dense_lists_follow_row_gap_per_density(cx: &mut gpui_kit::TestAppContext) {
    use crate::viewmodel::density::Density;
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let candidates = crate::ui::contracts::associable_extensions(&engine);
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, candidates, Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");

    let block_h = |vtcx: &mut gpui_kit::VisualTestContext,
                   tab: SettingsTab,
                   key: &'static str,
                   d: Density|
     -> f32 {
        crate::ui::spacing::set_density(d);
        // 连画两帧：切页签那一帧只登记元素树，锚点边界要到下一帧才稳定
        for _ in 0..2 {
            vtcx.update(|window, cx| {
                panel.update(cx, |p, _| p.tab = tab);
                window.draw(cx).clear(cx);
            });
        }
        let b = vtcx
            .debug_bounds(key)
            .unwrap_or_else(|| panic!("缺块锚点 {key}"));
        f32::from(b.size.height)
    };

    // 查看器页 ▸ 列块：行内文字为固定 `text_sm`（不随密度变档）⇒ 三档可比。
    // 期望：每档 +`2 × Δrow_gap × 行数` = 2×1×行数。
    let cols_key: &'static str =
        Box::leak(format!("ctl-{}", crate::viewmodel::SettingId::Columns.key()).into_boxed_str());
    let rows = crate::ui::contracts::ColumnsConfig::default().order.len() as f32;
    let (c0, c1, c2) = (
        block_h(vtcx, SettingsTab::Viewer, cols_key, Density::Compact),
        block_h(vtcx, SettingsTab::Viewer, cols_key, Density::Standard),
        block_h(vtcx, SettingsTab::Viewer, cols_key, Density::Loose),
    );
    assert!(
        (c1 - c0 - 2. * rows).abs() <= 0.51,
        "查看器列块：紧凑→标准 块高增 {:.2}px，期望 {:.2}px（{rows} 行 × 上下各 1px）",
        c1 - c0,
        2. * rows
    );
    assert!(
        (c2 - c1 - 2. * rows).abs() <= 0.51,
        "查看器列块：标准→宽松 块高增 {:.2}px，期望 {:.2}px",
        c2 - c1,
        2. * rows
    );

    // 上下文菜单页 ▸ 解压菜单勾选组：紧凑档复选框走 kit `Small`（标签字号
    // 掉一档 ⇒ 行框本身矮 3px），故只比**同为 Medium 的**标准↔宽松两档。
    let ctx_key: &'static str = Box::leak(
        format!("ctl-{}", crate::viewmodel::SettingId::CtxExtract.key()).into_boxed_str(),
    );
    let n = crate::ui::contracts::CTX_EXTRACT_KEYS.len() as f32;
    let (s1, s2) = (
        block_h(vtcx, SettingsTab::ContextMenu, ctx_key, Density::Standard),
        block_h(vtcx, SettingsTab::ContextMenu, ctx_key, Density::Loose),
    );
    assert!(
        (s2 - s1 - 2. * n).abs() <= 0.51,
        "解压菜单勾选组：标准→宽松 块高增 {:.2}px，期望 {:.2}px（{n} 行 × 上下各 1px）",
        s2 - s1,
        2. * n
    );
    // 紧凑档必须比标准档更紧（否则「紧凑无间隔」没生效）
    let s0 = block_h(vtcx, SettingsTab::ContextMenu, ctx_key, Density::Compact);
    assert!(
        s0 < s1,
        "解压菜单勾选组：紧凑档块高 {s0:.2}px 未小于标准档 {s1:.2}px"
    );
    crate::ui::spacing::set_density(Density::Standard);
}

// 密集勾选行的高度跟 `[ui].line_height` 走，且**夹到字形盒下限**。
//
// 用户口径：① 行距由密度（`row_gap`）管，行框由行高配置管——两件事分开；
// ② 行高小了**不能**裁字（「避免调整后文字显示不全」）。
// 断言面 = 渲染后的块高（每行 = 行框 + 上下各 row_gap）。
#[gpui_kit::test]
fn line_height_config_drives_dense_rows_and_clamps_at_glyph_box(cx: &mut gpui_kit::TestAppContext) {
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let candidates = crate::ui::contracts::associable_extensions(&engine);
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, candidates, Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    let id: &'static str = Box::leak(
        format!("ctl-{}", crate::viewmodel::SettingId::CtxExtract.key()).into_boxed_str(),
    );

    let block_h = |vtcx: &mut gpui_kit::VisualTestContext, em: f32| -> f32 {
        crate::ui::spacing::set_line_height(em);
        vtcx.update(|window, cx| {
            panel.update(cx, |p, _| p.tab = SettingsTab::ContextMenu);
            window.draw(cx).clear(cx);
        });
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        f32::from(vtcx.debug_bounds(id).expect("块锚点").size.height)
    };

    let floor = block_h(vtcx, hzip_config::LINE_HEIGHT_DEFAULT); // 1.2 ⇒ 夹到下限
    let tight = block_h(vtcx, 1.0); // 低于下限 ⇒ 必须与上一行逐像素相同
    let at_floor = block_h(vtcx, crate::ui::constant::INK_BOX_EM); // 恰好下限
    let mid = block_h(vtcx, 2.0);
    let airy = block_h(vtcx, 2.6);
    crate::ui::spacing::set_line_height(hzip_config::LINE_HEIGHT_DEFAULT);

    assert!(
        (floor - tight).abs() < 0.01,
        "低于墨迹盒下限的配置必须夹到同一下限：1.0⇒{tight:.2} vs 1.2⇒{floor:.2}"
    );
    assert!(
        (at_floor - floor).abs() < 0.01,
        "恰好等于下限（{}）时不应被改动：{at_floor:.2} vs {floor:.2}",
        crate::ui::constant::INK_BOX_EM
    );
    assert!(
        mid > floor + 0.5 && airy > mid + 0.5,
        "勾选块高应随行高配置变：1.2⇒{floor:.2} / 2.0⇒{mid:.2} / 2.6⇒{airy:.2}"
    );
}

// T-SET9 门禁：七页 × 候选集（可用/降级）全量渲染冒烟。
//
// 断言面 = 「渲染不 panic 且每行都被消费」，覆盖：
// - 24 个注册表行各按 WidgetKind 分派一次（Select 行 `select_of` 命中）；
// - `ACTIONS` 页级 Emit 与行级 Clear/FilePicker 目标均落到真实行；
// - Assoc 页 ext 候选可用（勾选组）与非 Windows/无候选（降级说明）两分支；
// - 列块 `columns_section` 叶子（查看器页）。
#[gpui_kit::test]
fn every_page_renders_all_registry_rows(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::Render as _;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(
            window,
            cx,
            app,
            vec!["zip".to_string(), "7z".to_string()],
            Rc::new(|_| {}),
        );
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    vtcx.update(|window, cx| {
        for tab in SettingsTab::ALL {
            panel.update(cx, |p, cx| {
                p.tab = tab;
                // 元素树构建即执行注册表逐行渲染（Select 缺实体 → debug_assert）
                let _ = p.render(window, cx);
            });
        }
    });
}

// 复选框标签必须放得下字形下降部（用户实测：`gz` 的 g 看着像 a——底部被裁）。
//
// 断言面 = **真实字体度量**：标签行框高度 ≥ `ascent + |descent|`。
// gpui-component 的 Checkbox 把标签行高写死 `relative(1.)`（行框 = 字号），
// 而字体实际需要 1.15~1.45 em ⇒ 下降部溢出，被标签容器的 `overflow_hidden`
// 裁掉。修复 = 经 `ui::widgets::checkbox` 用 children 槽自绘标签并显式行高。
//
// 用度量而非固定像素：换字体/换字号后若仍被裁，本用例会失败（fail-safe 方向）。
#[gpui_kit::test]
fn assoc_labels_fit_font_metrics(cx: &mut gpui_kit::TestAppContext) {
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let candidates = crate::ui::contracts::associable_extensions(&engine);
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, candidates, Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    vtcx.update(|window, cx| {
        panel.update(cx, |p, _| p.tab = SettingsTab::Assoc);
        window.draw(cx).clear(cx);
    });
    // 双击实况状态行必须在页上（用户要能看到「双击到底归谁」；
    // 被 UserChoice 占住的格式另外给一行带「打开方式」按钮的警告）。
    assert!(
        vtcx.debug_bounds("lbl-assoc-status").is_some(),
        "文件关联页应有「双击打开」状态行"
    );

    let (need, rem) = vtcx.update(|window, cx| {
        let ts = cx.text_system();
        let id = ts.resolve_font(&window.text_style().font());
        // 复选框默认 `Size::Medium` ⇒ 标签字号 = 1rem
        let size = window.rem_size();
        let ascent = ts.ascent(id, size);
        let descent = ts.descent(id, size).abs();
        (ascent + descent, size)
    });
    for key in ["assoc-cell-gz", "assoc-cell-tgz", "assoc-cell-tbz2"] {
        let cell = vtcx.debug_bounds(key).expect(key);
        assert!(
            cell.size.height >= need,
            "{key} 标签行框 {:.2}px < 字体 ascent+|descent| {:.2}px（rem={:.1}）\
             —— 下降部会被裁（g 看起来像 a）",
            f32::from(cell.size.height),
            f32::from(need),
            f32::from(rem)
        );
    }
}

// 文件关联页的勾选格必须随密度一起收（用户实测点名的第二处）：
// 格子内边距走 `metrics().pad_cell()`、里面的复选框经 `Sized` 吃
// `component_size()`（紧凑 → `Size::Small`：方框与标签行框一起缩）。
//
// 断言面 = 渲染后的格子高度（内容 + 内边距），紧凑必须明显矮于标准。
#[gpui_kit::test]
fn density_shrinks_assoc_cells(cx: &mut gpui_kit::TestAppContext) {
    use crate::viewmodel::density::Density;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let candidates = crate::ui::contracts::associable_extensions(&engine);
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, candidates, Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");

    let cell_h = |vtcx: &mut gpui_kit::VisualTestContext, density: Density| -> f32 {
        crate::ui::spacing::set_density(density);
        vtcx.update(|window, cx| {
            panel.update(cx, |p, _| p.tab = SettingsTab::Assoc);
            window.draw(cx).clear(cx);
        });
        let cell = vtcx.debug_bounds("assoc-cell-zip").expect("zip 格锚点");
        f32::from(cell.size.height)
    };

    let std_h = cell_h(vtcx, Density::Standard);
    let cmp_h = cell_h(vtcx, Density::Compact);
    let los_h = cell_h(vtcx, Density::Loose);
    crate::ui::spacing::set_density(Density::Standard); // 复位

    assert!(
        cmp_h < std_h,
        "紧凑档勾选格应更矮：{cmp_h} vs {std_h}（内边距与方框都应缩）"
    );
    assert!(los_h > std_h, "宽松档勾选格应更高：{los_h} vs {std_h}");
}

// 布局密度（设置-常规 ▸ 布局）：三档必须真的改变**渲染结果**——
// 行带高度与组件之间的间距同步缩放。
//
// 断言面 = 渲染后的真实边界（不是常量、也不是源码文本）：
// ① ？ 图标钮外层（`h(row_band())`）的高度；
// ② 文件关联页**同一行相邻两格**的水平间距（`gap(sp(2.))`）。
// 把 `row_band()` / `sp()` 改成与密度无关，本用例即失败。
//
// 为什么用这两个锚点：行带与间距是密度的**唯一**两条作用面（字号另由窗口
// rem 基准控制、互不牵扯）；`assoc-cell-zip`/`assoc-cell-7z` 是同一组里
// 第 1、2 格——任何密度下都同行（换行只影响第 5 格起），前提稳定。
#[gpui_kit::test]
fn density_scales_rendered_geometry(cx: &mut gpui_kit::TestAppContext) {
    use crate::viewmodel::density::Density;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let candidates = crate::ui::contracts::associable_extensions(&engine);
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, candidates, Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");

    let draw = |vtcx: &mut gpui_kit::VisualTestContext, tab: SettingsTab| {
        vtcx.update(|window, cx| {
            panel.update(cx, |p, _| p.tab = tab);
            window.draw(cx).clear(cx);
        });
    };

    // 三个探针：① 行带高、② 关联页格间距（自家 `sp()`）、③ 组件固有尺寸
    // （下拉高度——gpui-component 按 `Size` 算，`sp()` 管不到）。
    //
    // 行带探针取 `ctlrow0-set-assoc`（块首行经 `band()` 包一层 `min_h(row_band())`
    // ——其高度就是带高；标签/？ 图标的锚点都只包**内容**，量不到带）。
    let probe = |vtcx: &mut gpui_kit::VisualTestContext| -> (f32, f32, f32, f32, f32) {
        draw(vtcx, SettingsTab::General);
        let ctl = vtcx.debug_bounds("ctl-set-theme").expect("主题下拉锚点");
        let control_h = f32::from(ctl.size.height);
        // ④ 分组框内边距（设置项左起缩进）：16px → 紧凑 12 / 宽松 19.2。
        // 量**差值**（相对内容列左边）：竖排页签后绝对 x 还含轨道宽+列间距，
        // 且轨道宽本身随密度缩——绝对值比不出内边距。
        let pad_x = f32::from(
            vtcx.debug_bounds("lbl-set-theme")
                .expect("主题行标签锚点")
                .origin
                .x,
        ) - f32::from(
            vtcx.debug_bounds("settings-content")
                .expect("内容列锚点")
                .origin
                .x,
        );

        draw(vtcx, SettingsTab::Assoc);
        let head = vtcx
            .debug_bounds("ctlrow0-set-assoc")
            .expect("关联组块首行锚点");
        let band_h = f32::from(head.size.height);

        let a = vtcx.debug_bounds("assoc-cell-zip").expect("zip 格锚点");
        let b = vtcx.debug_bounds("assoc-cell-7z").expect("7z 格锚点");
        assert!(
            (f32::from(a.origin.y) - f32::from(b.origin.y)).abs() < 0.01,
            "两格应同行（间距测量的前提）"
        );
        let gap = f32::from(b.origin.x) - (f32::from(a.origin.x) + f32::from(a.size.width));

        // ⑤ 页签栏整条高度（`TabBar` 是 `Sizable`，紧凑档 → `Size::Small`：
        //    Tab 高 36→30px）——用户要求「设置的 tabs 的 tab 栏也要一起调」
        let tabs_h = f32::from(
            vtcx.debug_bounds("settings-tabs-wrap")
                .expect("设置页签栏锚点")
                .size
                .height,
        );
        (band_h, gap, control_h, pad_x, tabs_h)
    };

    let mut measured = Vec::new();
    for density in [Density::Standard, Density::Compact, Density::Loose] {
        crate::ui::spacing::set_density(density);
        measured.push(probe(vtcx));
    }
    // 复位标准档（进程级量；同进程其它用例按标准档断言绝对几何）
    crate::ui::spacing::set_density(Density::Standard);

    let (std_band, std_gap, std_ctl, std_pad, std_tabs) = measured[0];
    let (cmp_band, cmp_gap, cmp_ctl, cmp_pad, cmp_tabs) = measured[1];
    let (los_band, los_gap, los_ctl, los_pad, los_tabs) = measured[2];

    // 单调：紧凑 < 标准 < 宽松（用户要求的「紧凑更小、宽松再大一点」）
    assert!(
        cmp_gap < std_gap && std_gap < los_gap,
        "格间距未随密度单调：{cmp_gap} / {std_gap} / {los_gap}"
    );
    assert!(
        cmp_band < std_band && std_band < los_band,
        "行带未随密度单调：{cmp_band} / {std_band} / {los_band}"
    );

    // ② 间距系数真的落到渲染上：以**差值**断言（格子边框盒比报告宽少 1px，
    //    差值与该常量偏移无关）。期望值由密度系数推导——改系数（0.75/1.0/1.4）
    //    时本用例自动跟随，不会因「数写死在断言里」而假红。
    const STD_GAP: f32 = 8.; // sp(2.) @16px 字号（标准档）
    let (d_cmp, d_los) = (std_gap - cmp_gap, los_gap - std_gap);
    let want_cmp = STD_GAP * (1. - Density::Compact.scale());
    let want_los = STD_GAP * (Density::Loose.scale() - 1.);
    assert!(
        (d_cmp - want_cmp).abs() < 0.51,
        "紧凑档应把格距收 {want_cmp:.1}px（8 × (1−{})），实得 −{d_cmp}",
        Density::Compact.scale()
    );
    assert!(
        (d_los - want_los).abs() < 0.51,
        "宽松档应把格距放 {want_los:.1}px（8 × ({:.1}−1)），实得 +{d_los}",
        Density::Loose.scale()
    );

    // ③ 组件固有尺寸（gpui-component 按 `Size` 算的那层——用户报的
    //    「按钮/控件 padding 还宽」）：标准 = Medium（实测下拉高 28px），
    //    紧凑 = Small ⇒ 恰好 0.75×（21px）；宽松与标准同档（更松由 `sp()` 承担，
    //    因为 `Size::Large` 会把控件字号提到 text_base——密度不该动字号）。
    // 标准档的**绝对值**由组件库定（Medium 下拉 = 内高 28 + 2px 描边 = 30px，
    // 与本体字号/密度无关）——这里只锚「紧凑恰好 0.75×、宽松与标准同档」，
    // 不写死库内像素（否则换组件版本就假红）。
    assert!(
        cmp_ctl < std_ctl,
        "紧凑档控件必须比标准档矮：{cmp_ctl} vs {std_ctl}"
    );
    assert!(
        (cmp_ctl / std_ctl - Density::Compact.scale()).abs() < 0.02,
        "紧凑档控件高应为标准档 {}×（Size::Small）：{cmp_ctl} vs {std_ctl}",
        Density::Compact.scale()
    );
    assert!(
        (los_ctl - std_ctl).abs() < 0.51,
        "宽松档组件尺寸应与标准档一致：{los_ctl} vs {std_ctl}"
    );

    // ④ 设置项的内容缩进（分组框内边距随密度缩放）：用户报的
    //    「设置页面各个设置项 padding 还宽」之一。
    //
    // 基准取**实测的标准档**而非写死 16px：`pad_group = space(4.)` 是 **rem** 值
    // ⇒ 渲染 px = 界面字号（`[ui].font_size` 用户可改，出厂默认 16）。写死 16 会在
    // 字号 ≠ 16 的机器上假红（2026-10-08 实测：font_size=14 时标准档渲染 14px，
    // 而比例关系仍成立）。「pad_group == space(4.)」这条**绝对值**关系由
    // `ui/spacing.rs` 的 `semantic_tokens_track_the_base_scale` 断言守住。
    assert!(std_pad > 1., "标准档内边距须可测（防退化基准）：{std_pad}");
    let want_pad_cmp = std_pad * (1. - Density::Compact.scale());
    let want_pad_los = std_pad * (Density::Loose.scale() - 1.);
    assert!(
        (std_pad - cmp_pad - want_pad_cmp).abs() < 0.51,
        "紧凑档分组内边距应左移 {want_pad_cmp:.1}px：{std_pad} → {cmp_pad}"
    );
    assert!(
        (los_pad - std_pad - want_pad_los).abs() < 0.51,
        "宽松档分组内边距应右移 {want_pad_los:.1}px：{std_pad} → {los_pad}"
    );

    // ⑤ 页签栏整条随密度（用户要求「菜单和设置的 tabs 的 tab 栏也要一起调整间隔」）。
    //    竖排页签（用户要求改为垂直 tab）后，轨道是自绘的一列：每项的内边距
    //    取间距刻度 ⇒ 整条高度按密度单调变化（紧凑 0.75× / 宽松 1.4×）。
    //    字号不动（密度不能改字号），故只锚**单调 + 行数**。
    assert!(
        cmp_tabs < std_tabs && std_tabs < los_tabs,
        "竖排页签轨道高度未随密度单调：{cmp_tabs} / {std_tabs} / {los_tabs}"
    );
    // 7 个分类各一项：轨道高度 ≈ Σ 项高 + 项间距 ⇒ 用项锚点复核「一列七项」
    let first = vtcx.debug_bounds("settings-tab-0").expect("首项锚点");
    let last_key: &'static str =
        Box::leak(format!("settings-tab-{}", SettingsTab::ALL.len() - 1).into_boxed_str());
    let last = vtcx.debug_bounds(last_key).expect("末项锚点");
    assert!(
        f32::from(last.origin.y) > f32::from(first.origin.y),
        "竖排页签应自上而下排列（首项 y {} 未高于末项 y {}）",
        f32::from(first.origin.y),
        f32::from(last.origin.y)
    );
}

// 设置现在是**页签**（2026-09-23：弹层路径已删——用户口径「少一个弹窗、
// 空间利用更好」）：点「设置」装配面板实体并切到设置页签，页签栏出现可关闭的
// 「设置」页签。
//
// 断言面 = 渲染后真实边界：页签栏里的设置关闭钮、面板根、具体某行标签都在且
// 有高度——任何「容器塌成 0 高」的改法都会被拦下。
#[gpui_kit::test]
fn settings_panel_renders_content_in_tab(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::{ShellHandlers, ToolbarAction};
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(8);
    let tx = std::sync::Arc::new(tx);
    let shell_slot: Rc<std::cell::RefCell<Option<Entity<AppShell>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(cx, ShellHandlers::default(), Some(app), Some(tx));
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");
    // 工具栏「设置」= 打开设置页签（与菜单「设置…」同源）
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, ToolbarAction::Settings);
        });
    });
    for _ in 0..2 {
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    // ① 页签栏里真的有「设置」页签（关闭钮锚点只在存在时渲染）
    assert!(
        vtcx.debug_bounds("tab-close-settings").is_some(),
        "设置页签未出现在页签栏（设置应作为页签打开，不再是弹层）"
    );
    // ② 面板真的画出内容，而不只是空容器
    let panel = vtcx
        .debug_bounds("settings-panel-root")
        .expect("面板根锚点");
    // 首行标签（常规页第一项）
    let row = vtcx
        .debug_bounds("lbl-set-theme")
        .expect("常规页首行标签锚点");
    assert!(
        f32::from(panel.size.height) > 100.,
        "面板根高度 {:.1}px 过小——内容没画出来",
        f32::from(panel.size.height)
    );
    assert!(
        f32::from(row.size.height) > 5.,
        "首行标签高度 {:.1}px 过小",
        f32::from(row.size.height)
    );
    // 首行标签要在面板可见区内（垂直方向未被裁掉）
    assert!(
        f32::from(row.origin.y + row.size.height)
            <= f32::from(panel.origin.y + panel.size.height) + 1.01,
        "首行标签超出面板块（被裁）：row_bottom={:.1} panel_bottom={:.1}",
        f32::from(row.origin.y + row.size.height),
        f32::from(panel.origin.y + panel.size.height)
    );

    // 竖排页签与页内容之间**用一道竖线分隔**（用户要求）：
    // ① 线在两者之间（页签轨右缘 ≤ 线 ≤ 内容左缘）；② 线高随内容列（`self_stretch`）
    // ——否则只有 1px 高，肉眼看不见。
    let rail = vtcx.debug_bounds("settings-tabs-wrap").expect("页签轨锚点");
    let content = vtcx.debug_bounds("settings-content").expect("内容列锚点");
    let divider = vtcx
        .debug_bounds("settings-tab-divider")
        .expect("分隔线锚点");
    let (rail_r, div_l, div_r, cont_l) = (
        f32::from(rail.origin.x + rail.size.width),
        f32::from(divider.origin.x),
        f32::from(divider.origin.x + divider.size.width),
        f32::from(content.origin.x),
    );
    assert!(
        rail_r <= div_l + 0.51 && div_r <= cont_l + 0.51,
        "分隔线不在页签轨与内容列之间：轨右缘 {rail_r} / 线 [{div_l},{div_r}] / 内容左缘 {cont_l}"
    );
    assert!(
        f32::from(divider.size.width) >= 0.5 && f32::from(divider.size.width) <= 2.01,
        "分隔线宽 {:.1}px（应为 1px 细线）",
        f32::from(divider.size.width)
    );
    assert!(
        f32::from(divider.size.height) >= f32::from(content.size.height) - 1.01,
        "分隔线高 {:.1} < 内容列高 {:.1}——没撑满内容区",
        f32::from(divider.size.height),
        f32::from(content.size.height)
    );
}

/// 「恢复默认配置」按钮：原设置 Dialog 底部按钮（2026-09-23 设置改页签后
/// 弹层 footer 下线）挪到**常规页**底部——注册表驱动、事件 = `SettingsReset`。
///
/// 两条断言：① 注册表里它在常规页且确为重置事件（渲染器的过滤/接线都靠它）；
/// ② 常规页**真的画出了**这枚按钮（锚点在），否则「注册了没渲染」照样是缺陷。
/// 本用例**不点击**——点它会真跑 `apply_reset` 落盘，测试不该碰用户配置。
#[gpui_kit::test]
fn general_page_has_reset_defaults_button(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::settings_registry::{ACTIONS, ActionKind};
    let def = ACTIONS
        .iter()
        .find(|a| a.key == "set-reset-defaults")
        .expect("注册表必须有「恢复默认配置」动作");
    assert_eq!(def.page, SettingsTab::General, "恢复默认按钮在常规页");
    assert!(
        matches!(def.kind, ActionKind::Emit(DialogEvent::SettingsReset)),
        "恢复默认必须发 SettingsReset（AppData 五步流程 → apply_reset）"
    );

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let candidates = crate::ui::contracts::associable_extensions(&engine);
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, candidates, Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    vtcx.update(|window, cx| {
        panel.update(cx, |p, _| p.tab = SettingsTab::General);
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("act-set-reset-defaults").is_some(),
        "常规页必须画出「恢复默认配置」按钮"
    );
}

/// 「常规 → 日志目录」整行在**每档密度**下都排得下：控件（路径输入框）与行内
/// 动作按钮都完整落在内容列内、不被压瘦（用户五/六轮实测：按钮显示不全、
/// 紧凑档整行显示不全）。
///
/// 断言面 = 真实渲染边界：按钮盒子的左右缘都在内容列（`settings-content`）
/// 之内，且宽度没有塌陷。窄列下由按钮行的 `flex_wrap` + 每个按钮 `flex_none`
/// 兜底——把 `flex_none` 去掉（按钮被压瘦到文字不全）或把 `flex_wrap` 去掉
/// （按钮被挤出列外），这条立刻红。
#[gpui_kit::test]
fn inline_action_buttons_fit_at_every_density(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::contracts::ShellHandlers;
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(8);
    let tx = std::sync::Arc::new(tx);
    let shell_slot: Rc<std::cell::RefCell<Option<Entity<AppShell>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(cx, ShellHandlers::default(), Some(app), Some(tx));
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch_toolbar(window, cx, crate::ui::contracts::ToolbarAction::Settings);
        });
    });

    for density in [
        crate::viewmodel::density::Density::Compact,
        crate::viewmodel::density::Density::Standard,
        crate::viewmodel::density::Density::Loose,
    ] {
        crate::ui::spacing::set_density(density);
        for _ in 0..2 {
            vtcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        {
            let content = vtcx.debug_bounds("settings-content").expect("内容列锚点");
            let (cl, cr) = (
                f32::from(content.origin.x),
                f32::from(content.origin.x + content.size.width),
            );
            // ① 整行的控件（日志目录路径输入框）：不越界、且宽度留得下可读内容
            let ctl = vtcx
                .debug_bounds("ctl-set-log-dir")
                .expect("日志目录控件锚点");
            let (kl, kr, kw) = (
                f32::from(ctl.origin.x),
                f32::from(ctl.origin.x + ctl.size.width),
                f32::from(ctl.size.width),
            );
            assert!(
                kl >= cl - 0.51 && kr <= cr + 0.51,
                "{density:?}：日志目录控件 [{kl:.1},{kr:.1}] 越出内容列 [{cl:.1},{cr:.1}]"
            );
            assert!(
                kw >= 300.,
                "{density:?}：日志目录控件仅 {kw:.1}px 宽 —— 整行被挤到显示不全（列 [{cl:.1},{cr:.1}]）"
            );
            // ② 行内动作按钮：同样不越界、不被压瘦
            for key in [
                "set-log-dir-open",
                "set-log-dir-clear",
                "set-config-file-open",
            ] {
                let sel: &'static str = Box::leak(format!("act-{key}").into_boxed_str());
                let b = vtcx
                    .debug_bounds(sel)
                    .unwrap_or_else(|| panic!("缺按钮锚点 {key}"));
                let (bl, br, bw) = (
                    f32::from(b.origin.x),
                    f32::from(b.origin.x + b.size.width),
                    f32::from(b.size.width),
                );
                assert!(
                    bl >= cl - 0.51 && br <= cr + 0.51,
                    "{density:?}：按钮 {key} 盒子 [{bl:.1},{br:.1}] 越出内容列 [{cl:.1},{cr:.1}]（显示不全）"
                );
                assert!(
                    bw >= 32.,
                    "{density:?}：按钮 {key} 宽仅 {bw:.1}px —— 被压扁了（文字显示不全）"
                );
            }
        }
    }
    crate::ui::spacing::set_density(crate::viewmodel::density::Density::Standard);
}

// 字体下拉数据源的三条性质：首项 = 系统默认（空串）、其余**大小写不敏感
// 有序**、无重复项。（性质断言，不是把实现重抄一遍：改成乱序/漏首项即失败。
// 不断言「必须枚举到 N 个字体」——CI 字体环境不同，那是环境差异不是缺陷。）
#[gpui_kit::test]
fn font_select_source_is_default_first_and_sorted(cx: &mut gpui_kit::TestAppContext) {
    let fonts = cx.update(|cx| system_fonts(cx));
    assert_eq!(
        fonts.first().map(String::as_str),
        Some(""),
        "首项必须是「系统默认」（空串 = 主题默认字体族）"
    );
    let tail = &fonts[1..];
    for pair in tail.windows(2) {
        assert!(
            pair[0].to_lowercase() <= pair[1].to_lowercase(),
            "字体名未按（大小写不敏感）有序排列：{:?} 在 {:?} 之前",
            pair[0],
            pair[1]
        );
    }
    let unique: std::collections::HashSet<&String> = tail.iter().collect();
    assert_eq!(unique.len(), tail.len(), "候选集含重复字体名");
}

// 文件关联页版式（用户要求「一行多个选项」）：候选按类别分三组，
// 组内流式换行——断言**同一行内多格并存**（相邻格 y 相同、x 递增），
// 而不是此前的一扩展名一行。
//
// 用真实候选集（`associable_extensions` = 表 ∩ 注册表）而非构造值：
// 用户报的「格式太少」正是这个集合的内容问题，构造值测不出来。
#[gpui_kit::test]
fn assoc_page_lays_candidates_out_in_rows(cx: &mut gpui_kit::TestAppContext) {
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let candidates = crate::ui::contracts::associable_extensions(&engine);
    // 别名必须在（用户报的「格式太少」= 这批别名缺席）
    for must in [
        "zip", "7z", "tar", "tgz", "tbz", "tbz2", "txz", "tzst", "gz", "bz2", "xz", "zst",
    ] {
        assert!(
            candidates.iter().any(|c| c == must),
            "候选缺 {must}：{candidates:?}"
        );
    }
    assert!(
        candidates.iter().all(|e| !e.contains('.')),
        "关联候选只含单点扩展名：{candidates:?}"
    );

    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, candidates, Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    vtcx.update(|window, cx| {
        panel.update(cx, |p, _| p.tab = SettingsTab::Assoc);
        window.draw(cx).clear(cx);
    });

    // 三组标题都在（分组渲染路径确实跑到）
    for key in [
        "assoc-group-archive",
        "assoc-group-composite",
        "assoc-group-stream",
    ] {
        assert!(vtcx.debug_bounds(key).is_some(), "缺分组标题 {key}");
    }
    // 组内同行：归档组第 1/2 格（zip、7z）y 相同、x 递增
    let first = vtcx
        .debug_bounds("assoc-cell-zip")
        .expect("zip 格应有渲染命中区");
    let second = vtcx
        .debug_bounds("assoc-cell-7z")
        .expect("7z 格应有渲染命中区");
    assert_eq!(
        first.origin.y, second.origin.y,
        "同组候选须在同一行（flex_wrap 流式布局）"
    );
    assert!(
        second.origin.x > first.origin.x,
        "同行内按表序左→右排列：{first:?} → {second:?}"
    );
    // 换个组：复合组的 tgz 与归档组的 zip 不同行（分组确实分段）
    let tgz = vtcx
        .debug_bounds("assoc-cell-tgz")
        .expect("tgz 格应有渲染命中区");
    assert!(
        tgz.origin.y > first.origin.y,
        "不同组应另起一段：{tgz:?} vs {first:?}"
    );
}

// 文件关联页降级分支（候选集为空 → `assoc_fallback`，不渲染勾选组/动作行）
#[gpui_kit::test]
fn assoc_page_renders_fallback_without_candidates(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::Render as _;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, Vec::new(), Rc::new(|_| {}));
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    vtcx.update(|window, cx| {
        panel.update(cx, |p, cx| {
            p.tab = SettingsTab::Assoc;
            let _ = p.render(window, cx);
        });
    });
}

// 端到端（面板层）：值控件上行 → AppData 五步 → 面板重读。
//
// 断言面 = 「改完不弹回」：面板投影随改动更新（活读，非冻结快照），
// 且 UI 半场生效信号（`SettingApplied`）已发往泵。
#[gpui_kit::test]
fn setting_change_round_trips_and_refreshes_panel(cx: &mut gpui_kit::TestAppContext) {
    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    if let Ok(mut d) = data.lock() {
        d.signal_tx = Some(std::sync::Arc::new(tx));
    }
    // 上行闭包 = 真实 shell 路径（consume_dialog_event 内部走五步流程）
    let sink = data.clone();
    let on: Rc<dyn Fn(DialogEvent)> = Rc::new(move |ev| {
        if let Ok(mut d) = sink.lock() {
            let _ = d.consume_dialog_event(&ev);
        }
    });
    let on_for_panel = on.clone();
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, Vec::new(), on_for_panel);
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");

    // 构建期快照：主题 = 跟随系统（白名单下标 0）
    vtcx.update(|_window, cx| {
        panel.update(cx, |p, _cx| {
            assert_eq!(p.view.index_of(SettingId::Theme), 0, "构建期投影");
        });
    });

    // 模拟 Select 确认「深色」→ 上行（真实路径中 wire_select 随后 reload）
    on(DialogEvent::SettingChanged {
        id: SettingId::Theme,
        value: SettingValue::Index(2),
    });
    vtcx.update(|_window, cx| {
        panel.update(cx, |p, cx| p.reload(cx));
        panel.update(cx, |p, _cx| {
            assert_eq!(p.view.index_of(SettingId::Theme), 2, "事件后投影已刷新");
        });
    });

    // 数据面同源（AppData 镜像）——面板与 AppData 不再各持一份
    match data.lock() {
        Ok(d) => {
            assert_eq!(d.settings.theme_ix, 2, "AppData 镜像已更新");
            assert_eq!(
                d.settings_view(Vec::new()).index_of(SettingId::Theme),
                2,
                "活读视图与镜像一致"
            );
        }
        Err(_) => panic!("AppData 锁中毒"),
    }
    // 生效分派：UI 半场信号已发（泵据 `SettingApplied(Theme)` 调 Theme::change）
    match rx.try_recv() {
        Ok(crate::viewmodel::VmSignal::SettingApplied(id)) => {
            assert_eq!(id, SettingId::Theme);
        }
        other => panic!("期望 SettingApplied(Theme)，实得 {other:?}"),
    }
}

// 主题色恢复默认（T-SEL 后走查 #7 用户报告面）：选色落库 → 实体显示
// 新色 → SettingsReset（apply_reset 恢复空值）→ reload → render 帧
// `sync_pickers` 必须把实体清空。修复前：幂等追踪位从不随选色更新，
// 恢复后「旧色实体 vs 空配置」被判无变化 → picker 显示旧色 =
// 「恢复默认没生效」。
#[gpui_kit::test]
fn theme_color_resets_to_default_in_picker(cx: &mut gpui_kit::TestAppContext) {
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let sink = data.clone();
    let on: Rc<dyn Fn(DialogEvent)> = Rc::new(move |ev| {
        if let Ok(mut d) = sink.lock() {
            let _ = d.consume_dialog_event(&ev);
        }
    });
    let on_for_panel = on.clone();
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, Vec::new(), on_for_panel);
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    let picker = vtcx.update(|_window, cx| {
        let (_, p) = panel
            .read(cx)
            .pickers
            .iter()
            .find(|(id, _)| *id == SettingId::ThemeColor)
            .cloned()
            .expect("ThemeColor 行缺 picker 装配");
        p
    });

    // ① 选色上行（落库 + 镜像）——真实路径 = wire_picker 订阅
    on(DialogEvent::SettingChanged {
        id: SettingId::ThemeColor,
        value: SettingValue::Text("#2f6fed".into()),
    });
    match data.lock() {
        Ok(d) => assert_eq!(d.settings.theme_color, "#2f6fed", "选色落库"),
        Err(_) => panic!("锁中毒"),
    }
    // ② 实体显示新色（用户选色后的 UI 态）
    vtcx.update(|window, cx| {
        picker.update(cx, |st, cx| {
            st.set_value(
                crate::ui::theme::parse_theme_hex("#2f6fed").expect("合法 hex"),
                window,
                cx,
            )
        });
    });

    // ③ 恢复默认（footer 路径 = 事件 → 面板 reload → render）
    on(DialogEvent::SettingsReset);
    match data.lock() {
        Ok(d) => assert_eq!(d.settings.theme_color, "", "数据面恢复默认"),
        Err(_) => panic!("锁中毒"),
    }
    vtcx.update(|window, cx| {
        panel.update(cx, |p, cx| {
            p.reload(cx);
            let _ = p.render(window, cx); // 驱动 sync_pickers
        });
    });
    // ④ 断言面：picker 实体清空（修复前停留旧色）
    vtcx.update(|_window, cx| {
        assert!(
            picker.read(cx).value().is_none(),
            "恢复默认后 picker 必须清空（旧实现幂等误判不回写）"
        );
    });
    // 配置镜像与实体一致（再选一次仍能生效——非一次性死态）
    vtcx.update(|_window, cx| {
        panel.update(cx, |p, _cx| {
            assert_eq!(p.view.text_of(SettingId::ThemeColor), "");
        });
    });
}

// 语言项真实链路（用户报告面）：Lang 的 Select **实体**发出 Confirm
// → `wire_select` 订阅上行 → 五步流程（store → 落盘 → 分派）。
//
// 断言面 = 「改得动 + 落得下 + 信号出得去」；与 Theme 用例不同，这里
// 从真实 `SelectEvent` 起步（不经手调 `on`），覆盖 label→下标反查。
#[gpui_kit::test]
fn lang_select_event_flows_to_store_and_signal(cx: &mut gpui_kit::TestAppContext) {
    // rust-i18n locale 是进程级全局 → 与其它 locale 用例串行
    let _locale_guard = crate::viewmodel::testutil::locale_lock();

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    // 信号通道（泵在真实进程中消费 SettingApplied(Lang) → init_locale）
    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    if let Ok(mut d) = data.lock() {
        d.signal_tx = Some(std::sync::Arc::new(tx));
    }
    let sink = data.clone();
    let on: Rc<dyn Fn(DialogEvent)> = Rc::new(move |ev| {
        if let Ok(mut d) = sink.lock() {
            let _ = d.consume_dialog_event(&ev);
        }
    });
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, Vec::new(), on);
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");

    // 面板持有的 Lang Select 实体（窗口亲和，装配期订阅已挂）
    let select = vtcx.read(|cx| {
        panel
            .read(cx)
            .selects
            .iter()
            .find(|(id, _)| *id == SettingId::Lang)
            .map(|(_, e)| e.clone())
            .expect("Lang 项应装配 Select")
    });

    // 真实事件：确认「English」。
    //
    // 先落选中态再 emit——这就是 `SelectState` 真实点击的次序（`on_confirm`
    // 先写 `state.selection` 再 `emit(Confirm)`），而接线侧按**选中下标**
    // 取值（下标与语言无关；按文案反查会在切语言后错位）。
    vtcx.update(|window, cx| {
        select.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::new(2)), window, cx);
            cx.emit(SelectEvent::<SearchableVec<String>>::Confirm(Some(
                "English".to_string(),
            )));
        });
    });

    // ① 数据面：镜像已改（面板/主窗口活读同源）
    match data.lock() {
        Ok(d) => assert_eq!(d.settings.lang_ix, 2, "English = LANG 白名单下标 2"),
        Err(_) => panic!("AppData 锁中毒"),
    }
    // ② 面板面：投影随 reload 刷新（不回弹）
    vtcx.update(|_window, cx| {
        panel.update(cx, |p, _cx| {
            assert_eq!(p.view.index_of(SettingId::Lang), 2, "面板投影已刷新");
        });
    });
    // ③ 生效面：UI 半场信号已出（泵据此 init_locale + relocalize）
    match rx.try_recv() {
        Ok(crate::viewmodel::VmSignal::SettingApplied(id)) => {
            assert_eq!(id, SettingId::Lang, "应发出 Lang 生效信号");
        }
        other => panic!("期望 SettingApplied(Lang)，实得 {other:?}"),
    }
    // ④ 效果面：泵的分派动作（init_locale）确实切换 rust-i18n 全局 locale
    crate::init_locale(crate::app::labels::value_of(&crate::app::labels::LANG, 2));
    assert_eq!(
        rust_i18n::locale().to_string(),
        "en",
        "全局 locale 已切到 en"
    );
    assert_eq!(
        rust_i18n::t!("err.hz_1004"),
        "Source not found",
        "错误文案随 locale 切换（Lang 项的真实消费者）"
    );
    crate::init_locale("zh-CN"); // 复位，避免影响同进程其它用例
}

// 真实点击链路（最强自动化面）：`simulate_click` 命中 Switch 真实命中区
// → 回调上行 → AppData 五步 → 面板投影刷新。
//
// 断言面 = 「点得动 + 值停留」：镜像翻转且面板投影随之翻转（旧实现
// 「点击无效 / 回弹」在此失败）。
#[gpui_kit::test]
fn switch_click_flows_to_data_and_sticks(cx: &mut gpui_kit::TestAppContext) {
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let sink = data.clone();
    let on: Rc<dyn Fn(DialogEvent)> = Rc::new(move |ev| {
        if let Ok(mut d) = sink.lock() {
            let _ = d.consume_dialog_event(&ev);
        }
    });
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, Vec::new(), on);
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    let id = SettingId::ShowDotdot;

    // 起点：取活读投影当前值（用户配置不得影响断言，只断言「翻转」）
    let before = match data.lock() {
        Ok(d) => d.settings.show_dotdot,
        Err(_) => panic!("AppData 锁中毒"),
    };
    vtcx.update(|_window, cx| {
        panel.update(cx, |p, _cx| {
            assert_eq!(p.view.bool_of(id), before, "构建期投影 = 数据面现值");
        });
    });

    // 绘制一帧（登记 debug_selector 边界 + 命中区）后真实点击
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let bounds = vtcx
        .debug_bounds("ctl-set-show-dotdot")
        .expect("控件已绘制（T-SET 行渲染锚点）");
    vtcx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
    vtcx.update(|_window, cx| cx.background_executor().run_until_parked());

    // 数据面：值已落库并停留（未回弹）
    match data.lock() {
        Ok(d) => assert_eq!(d.settings.show_dotdot, !before, "点击后镜像翻转且停留"),
        Err(_) => panic!("AppData 锁中毒"),
    }
    // 面板面：投影同源刷新（事件后 reload）
    vtcx.update(|_window, cx| {
        panel.update(cx, |p, _cx| {
            assert_eq!(p.view.bool_of(id), !before, "面板投影跟随刷新（不回弹）");
        });
    });
}

// 走查 #13 纯逻辑面：后台安装失败写入的 Degraded 必须被状态行优先
// 消费（不触注册表查询）——否则失败原因只在日志里，状态行停在
// 「尚未注册」，用户无从知道为什么点了没反应。
#[test]
fn effective_ctx_status_prefers_stored_degraded() {
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    data.lock().unwrap().ctx_menu_status = Some(crate::app::state::CtxMenuStatus::Degraded {
        reason: "Shell 扩展未随程序安装".into(),
    });
    assert!(matches!(
        effective_ctx_status(&data),
        crate::app::state::CtxMenuStatus::Degraded { .. }
    ));
}

// 走查 #13 UI 链路面：上下文菜单页点「应用到系统」→ on 闭包必须收到
// `SettingsContextMenuApply`（用户实测：点击后日志零记录——按钮 →
// 事件上行链路必须可回归）。
#[gpui_kit::test]
fn ctx_apply_button_click_emits_event(cx: &mut gpui_kit::TestAppContext) {
    use std::cell::RefCell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let hits: Rc<RefCell<Vec<DialogEvent>>> = Rc::new(RefCell::new(Vec::new()));
    let hits_for_closure = hits.clone();
    let on: Rc<dyn Fn(DialogEvent)> =
        Rc::new(move |ev| hits_for_closure.borrow_mut().push(ev.clone()));
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, Vec::new(), on);
        *slot2.borrow_mut() = Some(panel.clone());
        let view: gpui_kit::AnyView = panel.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");

    // 切到上下文菜单页 + 画一帧（登记 act- 锚点边界/命中区）
    vtcx.update(|window, cx| {
        panel.update(cx, |p, cx| {
            p.tab = SettingsTab::ContextMenu;
            let _ = p.render(window, cx);
        });
        window.draw(cx).clear(cx);
    });
    let bounds = vtcx
        .debug_bounds("act-set-ctx-apply")
        .expect("「应用到系统」按钮已绘制（act- 锚点）");
    vtcx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
    vtcx.update(|_window, cx| cx.background_executor().run_until_parked());

    let events = hits.borrow().clone();
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, DialogEvent::SettingsContextMenuApply)),
        "点击「应用到系统」必须上行 SettingsContextMenuApply（实得 {events:?}）"
    );
}
