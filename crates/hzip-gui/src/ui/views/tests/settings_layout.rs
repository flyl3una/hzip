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

    // `debug_bounds` 在本 fork 只收 `&'static str`（`gpui-pre` 0.3.8
    // test_context.rs:990——它内部其实是 `FxHashMap<String, _>`，只是签名收窄
    // 了），而锚点名按注册表 id 现拼；故统一走 `settings_render::test_anchor`
    // 的**进程级静态表**（一次构建、终身复用，引用即 `'static`），不做
    // `Box::leak`（那是把每个短串永久泄漏出进程）。表里没登记的名字会 panic：
    // 「锚点查不到」这种断言失误应当立刻可见。
    let bounds = |vtcx: &mut gpui_kit::VisualTestContext, name: String| {
        let selector = crate::ui::views::settings_render::test_anchor(name.clone());
        vtcx.debug_bounds(selector)
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
            // 快捷键块（T-KEYS §9.1）是**全宽自绘块**：用户口径不要左侧 label 列，
            // 故没有「标签列 ↔ 控件列」这对锚点，本用例的同轴前提不成立。块内
            // 自己的版式（单行：名称/芯片/✕）由 `settings_render::tests` 的
            // `keys_block_is_single_line_per_action` 与 keys_* 用例守；这里只
            // 确认它**画出来了**（每行锚点在 ⇒ 注册表每行都被消费，计数与实际
            // 行数一致）。
            if matches!(def.widget, WidgetKind::KeysBlock) {
                assert!(
                    bounds(vtcx, format!("krow-{}", keymap::ACTIONS[0].config_key)).is_some(),
                    "[{tab:?}] {key}: 快捷键块首行未渲染"
                );
                checked += 1;
                continue;
            }
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

// —— 快捷键页（T-KEYS）：录制生命周期 + 三态上行 ——

/// 从上行事件里取出整份快捷键（非本类事件 → None）。
fn keys_of_event(ev: &DialogEvent) -> Option<hzip_config::Keys> {
    match ev {
        DialogEvent::SettingChanged { id, value } if *id == SettingId::Keymap => {
            value.as_keys().cloned()
        }
        _ => None,
    }
}

/// 快捷键页端到端场景：真实面板（事件捕获 + **真实应用**）+ 发布 `Main` 上下文的宿主。
///
/// `on` 走真实上行路径（`AppData::consume_dialog_event` 五步流程），于是
/// 「上行 → 落库/镜像 → reload 回读」整条链都可断言；`quits` 记 `sh::Quit` 的
/// 派发次数——「录制吞掉按键」与「Esc 取消后按键恢复派发」是同一条订阅的两面，
/// 没有它就无法区分「订阅已 drop」与「订阅还在、只是不再写配置」。
struct KeysScene {
    panel: Entity<SettingsPanel>,
    events: Rc<std::cell::RefCell<Vec<DialogEvent>>>,
    quits: Rc<std::cell::Cell<usize>>,
}

/// 快捷键页宿主：给面板套一层 `key_context("Main")` 并自持焦点。
///
/// 生产里 14 条加速键挂在 AppShell 的 `Main` 上下文上（`ui::shell::render`）；
/// 测试里没有壳体，少了这层上下文，绑定永远匹配不到——「按键被吞 / 恢复派发」
/// 就都观测不到（会以「基线没派发」的形式假绿）。
struct KeysHost {
    panel: Entity<SettingsPanel>,
    focus: gpui_kit::FocusHandle,
}

impl gpui_kit::Render for KeysHost {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .key_context("Main")
            .track_focus(&self.focus)
            .child(self.panel.clone())
    }
}

// 探针动作：绑在 `ctrl-q` 上，用来观测「按键有没有被录制吞掉」。
//
// 不复用 `ui::shell::actions::Quit`（那是私有模块，跨模块不可达），本用例要证的
// 也只是「按键是否派发出去」，与具体动作语义无关。
gpui_kit::actions!(hzip_gui_keys_probe, [KeysProbeKey]);

/// 造快捷键页场景：面板切到快捷键页 → 画帧 → 聚焦宿主 → 绑 `ctrl-q` = 探针动作。
fn keys_scene(cx: &mut gpui_kit::TestAppContext) -> (&mut gpui_kit::VisualTestContext, KeysScene) {
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let events: Rc<std::cell::RefCell<Vec<DialogEvent>>> = Rc::default();
    let hits = events.clone();
    let sink = data.clone();
    let on: Rc<dyn Fn(DialogEvent)> = Rc::new(move |ev| {
        hits.borrow_mut().push(ev.clone());
        if let Ok(mut d) = sink.lock() {
            let _ = d.consume_dialog_event(&ev);
        }
    });
    let app = data.clone();
    let slot: Rc<std::cell::RefCell<Option<Entity<SettingsPanel>>>> = Rc::default();
    let focus_slot: Rc<std::cell::RefCell<Option<gpui_kit::FocusHandle>>> = Rc::default();
    let slot2 = slot.clone();
    let focus_slot2 = focus_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let panel = SettingsPanel::new_entity(window, cx, app, Vec::new(), on);
        panel.update(cx, |p, cx| {
            p.tab = SettingsTab::Keys;
            cx.notify();
        });
        *slot2.borrow_mut() = Some(panel.clone());
        let host = cx.new(|cx| KeysHost {
            panel,
            focus: cx.focus_handle(),
        });
        *focus_slot2.borrow_mut() = Some(host.read(cx).focus.clone());
        let view: gpui_kit::AnyView = host.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let panel = slot.borrow().clone().expect("面板实体");
    let focus = focus_slot.borrow().clone().expect("宿主焦点");

    // 画一帧先登记元素树（`track_focus` 在 prepaint 注册），再聚焦 + 绑键
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.focus(&focus, cx);
        window.activate_window();
    });
    let quits: Rc<std::cell::Cell<usize>> = Rc::default();
    let counter = quits.clone();
    vtcx.update(|_window, cx| {
        cx.bind_keys([gpui_kit::KeyBinding::new(
            "ctrl-q",
            KeysProbeKey,
            Some("Main"),
        )]);
        cx.on_action(move |_: &KeysProbeKey, _: &mut App| counter.set(counter.get() + 1));
    });

    (
        vtcx,
        KeysScene {
            panel,
            events,
            quits,
        },
    )
}

/// 画一帧（锚点/命中区只在**已渲染帧**里可查）。
fn keys_draw(vtcx: &mut gpui_kit::VisualTestContext) {
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
}

/// 锚点名 → `&'static str`（`debug_bounds` 只收 `&'static str`）。
///
/// 走 `settings_render::test_anchor`（进程级静态表，不做 `Box::leak`）——锚点
/// 按 `config_key`/页签下标现拼，而表里登记的是本页锚点的全量名单。
fn keys_sel(name: impl AsRef<str>) -> &'static str {
    crate::ui::views::settings_render::test_anchor(name)
}

/// 真实点击某个测试锚点（先画一帧保证命中区已登记）。
fn keys_click(vtcx: &mut gpui_kit::VisualTestContext, anchor: &'static str) {
    keys_draw(vtcx);
    let bounds = vtcx
        .debug_bounds(anchor)
        .unwrap_or_else(|| panic!("缺锚点 {anchor}"));
    vtcx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
    vtcx.update(|_window, cx| cx.background_executor().run_until_parked());
}

/// 面板重读活读投影（生产里由泵在 `SettingApplied` 后调 `reload_open_panel`）。
fn keys_reload(vtcx: &mut gpui_kit::VisualTestContext, scene: &KeysScene) {
    vtcx.update(|_window, cx| {
        scene.panel.update(cx, |p, cx| p.reload(cx));
    });
    keys_draw(vtcx);
}

/// 面板当前录制态（`Some` = 拦截图在挂；订阅与它同生共死）。
fn keys_recording(
    vtcx: &mut gpui_kit::VisualTestContext,
    scene: &KeysScene,
) -> Option<crate::ui::shell::keymap::KeyActionId> {
    vtcx.update(|_window, cx| {
        scene
            .panel
            .update(cx, |p, _cx| p.recording.as_ref().map(|(id, _)| *id))
    })
}

/// 面板当前 pending（录制中已按下、**尚未提交**的组合）。
fn keys_pending(vtcx: &mut gpui_kit::VisualTestContext, scene: &KeysScene) -> Option<String> {
    vtcx.update(|_window, cx| scene.panel.update(cx, |p, _cx| p.pending.clone()))
}

/// 直接把某一行「失焦提交」（不依赖焦点事件——它要等帧边界才派发）。
fn keys_commit(vtcx: &mut gpui_kit::VisualTestContext, scene: &KeysScene, id: KeyActionId) {
    vtcx.update(|_window, cx| {
        scene.panel.update(cx, |p, cx| p.commit_recording(id, cx));
    });
}

/// 面板收口录制（切页签/关面板/离开设置页走的就是这条路）。
fn keys_stop(vtcx: &mut gpui_kit::VisualTestContext, scene: &KeysScene) {
    vtcx.update(|_window, cx| {
        scene.panel.update(cx, |p, _cx| p.stop_recording());
    });
}

/// ① 渲染：第 8 个页签是「快捷键」，**没有左侧 label 列**，逐行列出 14 条动作
/// （名称 / 芯片 / ✕ 同一行），页头是「全部恢复默认」。
#[gpui_kit::test]
fn keys_page_lists_every_action(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::keymap;

    let (vtcx, _scene) = keys_scene(cx);
    keys_draw(vtcx);

    // 8 个页签：快捷键在下标 7（`SettingsTab::ALL` 末位）
    assert_eq!(SettingsTab::ALL.len(), 8);
    assert!(
        vtcx.debug_bounds(keys_sel("settings-tab-7")).is_some(),
        "第 8 个页签（快捷键）未渲染"
    );
    // **无左侧 label 列**：快捷键块不走通用行 ⇒ 通用行的标签/控件锚点都不该在
    // （`ctl-set-keymap` 是 `row()` 给每个注册表控件打的锚点，`lbl-*` 是标签列）。
    assert!(
        vtcx.debug_bounds("ctl-set-keymap").is_none(),
        "快捷键块不该走通用行（用户口径：不要左侧 label 列）"
    );
    assert!(
        vtcx.debug_bounds("lbl-set-keymap").is_none(),
        "快捷键块不该有标签列单元格"
    );
    // 页头：标题左 + 「全部恢复默认」右，且在**所有动作行之上**
    let reset = vtcx
        .debug_bounds("kreset-all")
        .expect("页头缺「全部恢复默认」");
    let first_row = vtcx
        .debug_bounds(keys_sel(format!("krow-{}", keymap::ACTIONS[0].config_key)))
        .expect("缺首行动作锚点");
    assert!(
        reset.origin.y < first_row.origin.y,
        "「全部恢复默认」应在页头（动作行之上）"
    );
    for def in keymap::ACTIONS {
        let row = vtcx
            .debug_bounds(keys_sel(format!("krow-{}", def.config_key)))
            .unwrap_or_else(|| panic!("缺动作行锚点：{}", def.config_key));
        let chip = vtcx
            .debug_bounds(keys_sel(format!("kc-{}", def.config_key)))
            .unwrap_or_else(|| panic!("缺键位芯片锚点：{}", def.config_key));
        let remove = vtcx
            .debug_bounds(keys_sel(format!("kremove-{}", def.config_key)))
            .unwrap_or_else(|| panic!("缺 ✕ 锚点：{}", def.config_key));
        let restore = vtcx
            .debug_bounds(keys_sel(format!("krestore-{}", def.config_key)))
            .unwrap_or_else(|| panic!("缺 ↻ 还原锚点：{}", def.config_key));
        // 三者同在一行内（单行版式）：中心线重合 + x 递增（名称 → 芯片 → ↻ → ✕）
        let cy = |b: gpui_kit::Bounds<gpui_kit::Pixels>| {
            f32::from(b.origin.y) + f32::from(b.size.height) / 2.
        };
        assert!(
            (cy(chip) - cy(remove)).abs() <= 0.5 && (cy(chip) - cy(restore)).abs() <= 0.5,
            "{}: 芯片 / ↻ / ✕ 不在同一行（中心差 {:+.2}/{:+.2}px）",
            def.config_key,
            cy(chip) - cy(remove),
            cy(chip) - cy(restore)
        );
        assert!(
            chip.origin.x >= row.origin.x
                && restore.origin.x >= chip.origin.x
                && remove.origin.x >= restore.origin.x,
            "{}: 行内顺序应为 名称 → 芯片 → ↻ 还原 → ✕",
            def.config_key
        );
        assert!(
            f32::from(row.size.height)
                <= f32::from(crate::ui::views::settings_render::row_band())
                    + 2. * f32::from(crate::ui::spacing::row_gap())
                    + 1.,
            "{}: 行高超过一带——行应为单行版式（旧的两行式版式会到两带）",
            def.config_key
        );
    }
    // 左右留白（用户口径 2026-10-16：「两侧都要 padding，右侧键位与按钮贴着边框
    // 不好看」）：块内容与内容列左右缘之间必须各有一段空隙。用**相对量**断言——
    // 绝对 x 会随页签轨道宽度/弹窗位置一起漂（同 `settings-content` 锚点的原意）。
    let content = vtcx
        .debug_bounds("settings-content")
        .expect("缺内容列锚点（settings-content）");
    let left_gap = f32::from(first_row.origin.x) - f32::from(content.origin.x);
    let row0_remove = vtcx
        .debug_bounds(keys_sel(format!(
            "kremove-{}",
            keymap::ACTIONS[0].config_key
        )))
        .expect("缺首行 ✕ 锚点");
    let right_gap = (f32::from(content.origin.x) + f32::from(content.size.width))
        - (f32::from(row0_remove.origin.x) + f32::from(row0_remove.size.width));
    assert!(
        left_gap >= 10.0,
        "左侧缺留白（{left_gap:.1}px）：动作行紧贴内容列左缘"
    );
    assert!(
        right_gap >= 10.0,
        "右侧缺留白（{right_gap:.1}px）：键位芯片/✕ 贴到面板右缘"
    );
    // 整块**收窄 + 居中**（用户口径 2026-10-16：「内容需要居中对齐……320px 太窄，600px 左右」）
    // + **不再被 Form 标签列推右**（那会把整块右移 ≈170px）；尺寸相对**本窗口的 rem
    // 基准**量，绝对 px 会随字号漂。
    let block = vtcx.debug_bounds("kblock").expect("缺块根锚点（kblock）");
    let rem = vtcx.update(|window, _cx| window.rem_size());
    let cap = f32::from(gpui_kit::rems(settings_render::KEYS_BLOCK_W_REM).to_pixels(rem));
    let block_w = f32::from(block.size.width);
    assert!(
        block_w <= cap + 1.0,
        "快捷键块宽 {block_w:.1}px 超过封顶 {cap:.1}px——整块被拉宽，名称与键位之间会出现大片空白"
    );
    // 居中：左右余量均分（差 ≤2px）。注意**不能**量「离右缘还有多少」——测试窗口宽
    // 1920、内容列 1769px，那个余量恒真（第一版断言就是这么写废的）。
    let content_left = f32::from(content.origin.x);
    let content_right = content_left + f32::from(content.size.width);
    let left_margin = f32::from(block.origin.x) - content_left;
    let right_margin = content_right - (f32::from(block.origin.x) + block_w);
    assert!(
        (left_margin - right_margin).abs() <= 2.0,
        "块未居中：左余量 {left_margin:.1}px、右余量 {right_margin:.1}px"
    );
    let row_in_block = f32::from(first_row.origin.x) - f32::from(block.origin.x);
    assert!(
        row_in_block >= 8.0,
        "块内左侧留白不足（{row_in_block:.1}px）"
    );
    // 页头两枚控件也必须落在块内（收窄到 ≈600px 后仍要放得下 标题 + ？ + 全部恢复默认）
    let block_left = f32::from(block.origin.x);
    let block_right = block_left + block_w;
    for (anchor, what) in [("kreset-all", "「全部恢复默认」"), ("khelp", "？ 帮助钮")] {
        let b = vtcx
            .debug_bounds(anchor)
            .unwrap_or_else(|| panic!("缺页头锚点 {anchor}"));
        let x = f32::from(b.origin.x);
        let right = x + f32::from(b.size.width);
        assert!(
            x >= block_left && right <= block_right + 1.0,
            "{what} 跑出块外（控件 {x:.1}..{right:.1}，块 {block_left:.1}..{block_right:.1}）"
        );
    }
    // ① ？ 必须紧贴**标题右侧**、而不是挨着「全部恢复默认」（用户口径 2026-10-16）：
    //    离块左缘一小段（标题宽 + 间距），且与动作钮之间留出**大段余量**。
    let help = vtcx.debug_bounds("khelp").expect("缺页头 ？ 锚点");
    let reset = vtcx.debug_bounds("kreset-all").expect("缺页头动作钮锚点");
    let help_left = f32::from(help.origin.x) - block_left;
    let help_to_reset =
        f32::from(reset.origin.x) - (f32::from(help.origin.x) + f32::from(help.size.width));
    assert!(
        help_left <= 80.0,
        "？ 不在标题右侧（离块左缘 {help_left:.1}px；标题≈35px + 间距≈8px）"
    );
    assert!(
        help_to_reset >= 100.0,
        "？ 仍挨着「全部恢复默认」（两者只差 {help_to_reset:.1}px）"
    );
    // ② 行尾三件套：芯片 ↔ ↻ ↔ ✕ 各 1px（用户口径 2026-10-16「margin 调到 1px」），
    //    且两枚图标钮与设置页其它按钮**同款**（默认变体 + `component_size()`：
    //    高度与页头动作钮一致、icon-only ⇒ 近似正方形）。
    let chip0 = vtcx
        .debug_bounds(keys_sel(format!("kc-{}", keymap::ACTIONS[0].config_key)))
        .expect("缺芯片锚点");
    let restore0 = vtcx
        .debug_bounds(keys_sel(format!(
            "krestore-{}",
            keymap::ACTIONS[0].config_key
        )))
        .expect("缺 ↻ 锚点");
    let remove0 = vtcx
        .debug_bounds(keys_sel(format!(
            "kremove-{}",
            keymap::ACTIONS[0].config_key
        )))
        .expect("缺 ✕ 锚点");
    let right_edge =
        |b: gpui_kit::Bounds<gpui_kit::Pixels>| f32::from(b.origin.x) + f32::from(b.size.width);
    assert!(
        f32::from(restore0.origin.x) - right_edge(chip0) <= 2.0,
        "芯片与 ↻ 之间不是 1px（实得 {:.1}px）",
        f32::from(restore0.origin.x) - right_edge(chip0)
    );
    assert!(
        f32::from(remove0.origin.x) - right_edge(restore0) <= 2.0,
        "↻ 与 ✕ 之间不是 1px（实得 {:.1}px）",
        f32::from(remove0.origin.x) - right_edge(restore0)
    );
    for (b, what) in [(restore0, "↻ 还原"), (remove0, "✕ 删除")] {
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        assert!(
            (w - h).abs() <= 1.0,
            "{what} 不是 icon-only（{w:.1}×{h:.1}）"
        );
        assert!(
            (h - f32::from(reset.size.height)).abs() <= 1.0,
            "{what} 与设置页其它按钮档位不一致（高 {h:.1} vs {:.1}）",
            f32::from(reset.size.height)
        );
    }
    // 交互说明从页底常驻行改为标题旁 ？ 悬浮（用户口径 2026-10-16）
    assert!(
        vtcx.debug_bounds("khelp").is_some(),
        "页头缺 ？ 帮助钮（交互说明应改为悬浮提示）"
    );
    // 行内旧按钮（「禁用」/「恢复本项默认」）与行内恢复默认锚点都不该再有
    assert!(vtcx.debug_bounds(keys_sel("kdisable-open")).is_none());
    assert!(vtcx.debug_bounds(keys_sel("kreset-open")).is_none());
    assert!(vtcx.debug_bounds(keys_sel("kreset-all")).is_some());
}

/// ② 改键：点芯片 → 按键**只预览** → 焦点移开（提交）→ 整份上行
/// （`SettingChanged{Keymap, Keys}`）。
///
/// 走**真实拦截器**（`vtcx.simulate_keystrokes`，与用户按键盘同一条链路），
/// 不是直调 `on_captured_key`——那样测不出「录制中按键被吞」这一层。
#[gpui_kit::test]
fn keys_recording_previews_then_commits_on_blur(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    keys_click(vtcx, "kc-select_all");
    assert_eq!(
        keys_recording(vtcx, &scene),
        Some(crate::ui::shell::keymap::KeyActionId::SelectAll),
        "点键位芯片应进入该行的录制态"
    );
    keys_draw(vtcx);
    assert!(
        vtcx.debug_bounds("kc-rec-select_all").is_some(),
        "录制态文案块未渲染（芯片视觉没跟着录制态走）"
    );
    assert!(
        vtcx.debug_bounds("kc-idle-select_all").is_none(),
        "录制中的芯片不该还是空闲态"
    );

    // 按下组合：**只做预览，不写配置**（新交互：写值看失焦提交）
    vtcx.simulate_keystrokes("ctrl-shift-a");
    keys_draw(vtcx);
    assert!(
        vtcx.debug_bounds("kc-pending-select_all").is_some(),
        "按下的组合应作为预览显示在芯片上"
    );
    assert_eq!(
        keys_pending(vtcx, &scene).as_deref(),
        Some("ctrl-shift-a"),
        "pending 应记下物理键的规范串（ASCII，不是显示字形）"
    );
    assert!(
        scene.events.borrow().is_empty(),
        "录制中不得写配置（实得上行 {:?}）",
        scene.events.borrow()
    );

    // 焦点移开 = 提交。真实路径是点**另一行**芯片：`window.focus` 换行 ⇒
    // 这一行失焦 ⇒ 提交（面板在换行/失焦两处都会尝试，只有一方能过守卫）。
    keys_click(vtcx, "kc-open");
    let events = scene.events.borrow().clone();
    assert_eq!(events.len(), 1, "提交只该上行一条：{events:?}");
    let keys = keys_of_event(&events[0]).expect("上行必须是 SettingChanged{Keymap, Keys}");
    assert_eq!(keys.field("select_all"), Some("ctrl-shift-a"));
    assert_eq!(keys.field("open"), Some(""), "其余动作不动");
    assert_eq!(keys_pending(vtcx, &scene), None, "提交后 pending 清空");
    assert_eq!(
        keys_recording(vtcx, &scene),
        Some(crate::ui::shell::keymap::KeyActionId::Open),
        "换行后录制态转到新行（上一行已提交）"
    );
    keys_stop(vtcx, &scene);
    assert_eq!(keys_recording(vtcx, &scene), None, "收口后无录制态");
}

/// ②b 提交的第二个入口：直接调 `commit_recording`（不依赖失焦事件的帧边界派发）。
///
/// 「点别的元素」在生产里也会走真实失焦，但那条链要等一帧才派发——本用例把
/// 状态机本身钉死，`keys_recording_previews_then_commits_on_blur` 负责通路。
#[gpui_kit::test]
fn keys_commit_writes_whole_config(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    keys_click(vtcx, "kc-select_all");
    vtcx.simulate_keystrokes("ctrl-shift-a");
    keys_commit(
        vtcx,
        &scene,
        crate::ui::shell::keymap::KeyActionId::SelectAll,
    );

    let events = scene.events.borrow().clone();
    assert_eq!(events.len(), 1, "提交只该上行一条：{events:?}");
    let keys = keys_of_event(&events[0]).expect("上行必须是 SettingChanged{Keymap, Keys}");
    assert_eq!(keys.field("select_all"), Some("ctrl-shift-a"));
    assert_eq!(keys.field("open"), Some(""), "其余动作不动");
    assert_eq!(keys_recording(vtcx, &scene), None, "提交即结束录制");

    // 落库 → 镜像 → 回读：行态跟着变（芯片回空闲态，显示新键位）
    keys_reload(vtcx, &scene);
    assert!(
        vtcx.debug_bounds(keys_sel("kc-idle-select_all")).is_some(),
        "提交结束后芯片回到空闲态文案"
    );
    assert!(
        vtcx.debug_bounds("kc-pending-select_all").is_none(),
        "pending 已清，芯片不该还挂着预览"
    );
}

/// ②c 没按键就移开焦点 = 取消（收口但不写配置）。
#[gpui_kit::test]
fn keys_commit_without_pending_is_a_noop(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    keys_click(vtcx, "kc-select_all");
    keys_commit(
        vtcx,
        &scene,
        crate::ui::shell::keymap::KeyActionId::SelectAll,
    );

    assert!(
        scene.events.borrow().is_empty(),
        "一个字没按就移开焦点不得写配置（实得 {:?}）",
        scene.events.borrow()
    );
    assert_eq!(keys_recording(vtcx, &scene), None, "录制应收口");
    // 键盘必须同时恢复：没有订阅残留
    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(scene.quits.get(), 1, "收口后 ctrl-q 应恢复派发");
}

/// ③ 冲突拒绝：录到已被别的动作占用的键 → 失焦提交**无**上行事件 +
/// 指名占用者 + 录制收口（键盘不再被吞）。
///
/// 冲突发生在**提交**（失焦）而不是键入时：录制中按键只进 pending，用户可以先
/// 看完预览再决定；被拒时红字挂在那一行下面。
#[gpui_kit::test]
fn keys_conflict_on_commit_is_refused_with_occupant(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    // 基线（反假绿）：没在录制时 `ctrl-q` 真的会派发「退出」
    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(scene.quits.get(), 1, "未录制时 ctrl-q 应派发探针动作");

    keys_click(vtcx, "kc-select_all");
    vtcx.simulate_keystrokes("ctrl-q");
    keys_draw(vtcx);
    assert_eq!(
        scene.quits.get(),
        1,
        "录制中 ctrl-q 必须被吞（不得退出程序）"
    );
    assert!(
        scene.events.borrow().is_empty(),
        "录制中不写配置（实得上行 {:?}）",
        scene.events.borrow()
    );
    assert!(
        vtcx.debug_bounds("kconflict-select_all").is_none(),
        "冲突由**失焦提交**触发——键入时不该报"
    );

    // 失焦提交 → 撞上「退出」占用
    keys_commit(
        vtcx,
        &scene,
        crate::ui::shell::keymap::KeyActionId::SelectAll,
    );
    keys_draw(vtcx);
    assert!(
        scene.events.borrow().is_empty(),
        "冲突必须**不写配置**（实得上行 {:?}）",
        scene.events.borrow()
    );
    assert_eq!(keys_recording(vtcx, &scene), None, "被拒后录制收口");
    assert_eq!(keys_pending(vtcx, &scene), None, "被拒后 pending 清空");
    assert!(
        vtcx.debug_bounds("kconflict-select_all").is_some(),
        "冲突提示应渲染在**被拒的那一行**"
    );
    assert!(
        vtcx.debug_bounds("kconflict-open").is_none(),
        "冲突提示不该跑到别的行"
    );
    // 提示文案真的点名占用者（`{action}` 已替换成动作名）
    let hint = settings_render::conflict_hint(crate::ui::shell::keymap::def_of(
        crate::ui::shell::keymap::KeyActionId::Quit,
    ));
    assert!(
        hint.contains(&crate::tr("keymap.action.quit")),
        "冲突提示应含占用者动作名：{hint}"
    );
    // 录制已收口 ⇒ 键盘恢复派发（订阅必须随冲突一起释放，否则键盘被永久吞掉）
    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(
        scene.quits.get(),
        2,
        "被拒之后 ctrl-q 必须恢复派发（拦截图不得滞留）"
    );
}

/// ④ `Delete` 照常录成绑定（旧契约的「Delete = 禁用」作废：删除绑定改由 ✕）。
///
/// 前提：`delete` 默认被「删除」动作占着，得先腾出来（`delete` 如今只是个普通
/// 键位串，不再有「按下即写入哨兵」的特权）。
#[gpui_kit::test]
fn keys_delete_is_recorded_as_binding(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    // 腾键位：✕ 掉「删除」动作的绑定
    keys_click(vtcx, keys_sel("kremove-delete"));
    keys_reload(vtcx, &scene);
    let before = scene.events.borrow().len();

    keys_click(vtcx, "kc-open");
    vtcx.simulate_keystrokes("delete");
    assert_eq!(
        keys_pending(vtcx, &scene).as_deref(),
        Some("delete"),
        "Delete 应作为普通键位进 pending"
    );
    keys_commit(vtcx, &scene, crate::ui::shell::keymap::KeyActionId::Open);

    let events = scene.events.borrow().clone();
    assert_eq!(events.len(), before + 1, "实得 {events:?}");
    let keys = keys_of_event(&events[before]).expect("上行必须是 SettingChanged{Keymap, Keys}");
    assert_eq!(
        keys.field("open"),
        Some("delete"),
        "Delete 该被录成绑定，而不是禁用哨兵"
    );
    assert_eq!(keys_recording(vtcx, &scene), None, "提交即结束录制");
    assert_eq!(
        keys.field("delete"),
        Some(settings_render::KEYS_DISABLED),
        "腾键位那一步写的是「未绑定」哨兵"
    );
}

/// ⑤ ✕ = 删除该动作的绑定：上行 `Keys{该字段 == "none"}`，行态转「未绑定」。
#[gpui_kit::test]
fn keys_remove_button_writes_none(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    keys_click(vtcx, keys_sel("kremove-open"));
    let events = scene.events.borrow().clone();
    assert_eq!(events.len(), 1, "点 ✕ 只该上行一条：{events:?}");
    let keys = keys_of_event(&events[0]).expect("上行必须是 SettingChanged{Keymap, Keys}");
    assert_eq!(
        keys.field("open"),
        Some(settings_render::KEYS_DISABLED),
        "✕ 写的是「未绑定」哨兵"
    );
    assert_eq!(keys.field("select_all"), Some(""), "其余动作不受影响");

    // 落库 → 回读：该行芯片转「未绑定」态（行内不再有旧版式的「禁用」按钮）
    keys_reload(vtcx, &scene);
    assert!(
        vtcx.debug_bounds(keys_sel("kc-unbound-open")).is_some(),
        "删除绑定后该行应显示「未绑定」"
    );
    assert!(vtcx.debug_bounds(keys_sel("kdisable-open")).is_none());
    assert!(vtcx.debug_bounds(keys_sel("kreset-open")).is_none());
}

/// ⑥ 页头「全部恢复默认」→ 整份出厂默认（未绑定的行一并复位）。
#[gpui_kit::test]
fn keys_reset_all_restores_defaults(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    // 先删掉一个绑定（「整份复位」才有可观测差异）
    keys_click(vtcx, keys_sel("kremove-select_all"));
    keys_reload(vtcx, &scene);
    let before = scene.events.borrow().len();
    assert!(
        vtcx.debug_bounds(keys_sel("kc-unbound-select_all"))
            .is_some(),
        "删掉绑定后该行应为「未绑定」"
    );

    keys_click(vtcx, "kreset-all");
    let events = scene.events.borrow().clone();
    assert_eq!(events.len(), before + 1, "实得 {events:?}");
    let keys = keys_of_event(&events[before]).expect("上行必须是 SettingChanged{Keymap, Keys}");
    assert_eq!(keys, hzip_config::Keys::default());

    keys_reload(vtcx, &scene);
    assert!(
        vtcx.debug_bounds(keys_sel("kc-unbound-select_all"))
            .is_none(),
        "全部恢复默认后不该再有「未绑定」行"
    );
}

/// ⑦ Esc 取消：丢弃 pending、不写配置，且**真的退订**——此后同一个键恢复原有派发。
///
/// 这条用例守的是本切片最容易出的重症 bug：只把「录制态」清掉而漏 drop
/// `Subscription`，界面看不出区别（芯片回空闲态），但全窗口键盘从此被吞——
/// 所以断言面必须是「按键恢复派发」，不能只看 `recording` 字段。
#[gpui_kit::test]
fn keys_escape_releases_keyboard(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(scene.quits.get(), 1, "基线：未录制时 ctrl-q 正常派发");

    keys_click(vtcx, "kc-select_all");
    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(scene.quits.get(), 1, "录制中 ctrl-q 被吞");
    assert_eq!(keys_pending(vtcx, &scene).as_deref(), Some("ctrl-q"));
    let seen = scene.events.borrow().len();

    vtcx.simulate_keystrokes("escape");
    keys_draw(vtcx);
    assert_eq!(keys_recording(vtcx, &scene), None, "Esc 应结束录制");
    assert_eq!(
        keys_pending(vtcx, &scene),
        None,
        "Esc 应丢弃未提交的 pending"
    );
    assert!(
        vtcx.debug_bounds(keys_sel("kc-pending-select_all"))
            .is_none(),
        "取消后芯片上不该还挂着预览"
    );
    assert_eq!(
        scene.events.borrow().len(),
        seen,
        "Esc 取消不得写配置（不得上行任何事件）"
    );
    assert!(
        vtcx.debug_bounds(keys_sel("kconflict-select_all"))
            .is_none(),
        "取消后冲突提示应一并清掉"
    );

    // 订阅真的没了：同一个键恢复原有派发
    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(
        scene.quits.get(),
        2,
        "Esc 之后 ctrl-q 必须恢复派发（订阅没 drop = 键盘被永久吞）"
    );
}

/// ⑧ 收口路径（切主页签 / 关面板 / 壳层离开设置页 / reload）：**丢弃**未提交的
/// pending——与「失焦提交」是两条语义相反的路。
///
/// 走的是壳层调的那个入口（`stop_recording`，见
/// `shell::tests::leaving_settings_tab_releases_key_recording`），即**没有焦点变化**
/// 的那条路：焦点若已经移开，`§9.2` 的「焦点离开芯片 = 提交」会先落地（见下一条
/// 用例）——本节只管「录到一半被收口」这半边：丢弃 + 释放键盘。
#[gpui_kit::test]
fn keys_stop_recording_discards_pending(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(scene.quits.get(), 1);

    keys_click(vtcx, "kc-select_all");
    vtcx.simulate_keystrokes("ctrl-shift-a");
    assert_eq!(
        keys_pending(vtcx, &scene).as_deref(),
        Some("ctrl-shift-a"),
        "先确认录制里真的攒下了 pending"
    );
    assert_eq!(
        keys_recording(vtcx, &scene),
        Some(crate::ui::shell::keymap::KeyActionId::SelectAll)
    );

    keys_stop(vtcx, &scene);
    assert_eq!(keys_recording(vtcx, &scene), None, "收口应结束录制");
    assert_eq!(
        keys_pending(vtcx, &scene),
        None,
        "收口应丢弃未提交的 pending"
    );
    assert!(
        scene.events.borrow().is_empty(),
        "收口不得提交（实得上行 {:?}）",
        scene.events.borrow()
    );

    // 画一帧：芯片上不该还挂着预览（pending 已丢）
    keys_draw(vtcx);
    assert!(
        vtcx.debug_bounds(keys_sel("kc-pending-select_all"))
            .is_none()
    );
    // 键盘同时恢复派发（拦截图必须随录制态一起释放）
    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(
        scene.quits.get(),
        2,
        "收口后 ctrl-q 必须恢复派发（订阅没 drop = 键盘被永久吞）"
    );
}

/// ⑧b 真实鼠标点别处（这里点设置内页签）：焦点先离开芯片 ⇒ 按 §9.2 的
/// 「焦点离开芯片 = 提交」**先落地**，页签处理器随后收口（不重复写、键盘释放）。
///
/// 这一条把「移开焦点即生效」钉在**真实鼠标链路**上：gpui 的可聚焦元素在点击
/// 时会接过焦点，而焦点事件在帧边界派发——故「点页签丢弃 pending」在生产里
/// 通常表现为「先提交、再收口」。收口那一半的语义由
/// [`keys_stop_recording_discards_pending`] 单独守住。
#[gpui_kit::test]
fn keys_clicking_elsewhere_commits_before_release(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, scene) = keys_scene(cx);

    keys_click(vtcx, "kc-select_all");
    vtcx.simulate_keystrokes("ctrl-shift-a");
    assert!(scene.events.borrow().is_empty(), "录制中不写配置");

    // 点「常规」页签（下标 0）：焦点离开芯片 → 提交；随后页签处理器收口
    keys_click(vtcx, keys_sel("settings-tab-0"));
    keys_draw(vtcx);
    let events = scene.events.borrow().clone();
    assert_eq!(events.len(), 1, "焦点移开应提交一条（实得 {events:?}）");
    let keys = keys_of_event(&events[0]).expect("上行必须是 SettingChanged{Keymap, Keys}");
    assert_eq!(keys.field("select_all"), Some("ctrl-shift-a"));
    assert_eq!(keys_recording(vtcx, &scene), None, "页签切换应收口录制");
    assert_eq!(keys_pending(vtcx, &scene), None);

    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(
        scene.quits.get(),
        1,
        "收口后 ctrl-q 必须恢复派发（拦截图必须随录制态一起释放）"
    );
}
