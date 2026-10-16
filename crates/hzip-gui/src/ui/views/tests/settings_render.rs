use super::*;
use gpui_kit::px;

fn columns(order: Vec<BrowseColumnId>, hidden: Vec<BrowseColumnId>) -> ColumnsConfig {
    ColumnsConfig { order, hidden }
}

// 列移动：原位/越界 → None；合法 → 目标序
#[test]
fn move_to_reorders_columns() {
    let base = columns(BrowseColumnId::ALL.to_vec(), Vec::new());
    let moved = move_to(&base, 2, 0).expect("合法移动");
    // 预期序 = ALL 序经同款 move（不手写全量——列集扩展时测试免改）
    let mut expect = BrowseColumnId::ALL.to_vec();
    let col = expect.remove(2);
    expect.insert(0, col);
    assert_eq!(moved.order, expect);
    assert_eq!(move_to(&base, 0, 0), None, "原位 = 无变更");
    assert_eq!(
        move_to(&base, 0, BrowseColumnId::ALL.len()),
        None,
        "越界 = 无变更"
    );
    assert_eq!(move_to(&base, 99, 0), None, "越界 = 无变更");
}

// 显隐切换只动 hidden（order 保持用户重排——旧实现静默重置列序的回归锚点）
#[test]
fn toggle_hidden_updates_hidden_only() {
    let hidden = vec![BrowseColumnId::Mtime];
    let now = toggle_hidden(&hidden, BrowseColumnId::Size, false);
    assert!(now.contains(&BrowseColumnId::Size));
    assert!(now.contains(&BrowseColumnId::Mtime));
    let now = toggle_hidden(&now, BrowseColumnId::Mtime, true);
    assert_eq!(now, vec![BrowseColumnId::Size]);
    assert_eq!(
        toggle_hidden(&now, BrowseColumnId::Size, true),
        Vec::<BrowseColumnId>::new()
    );
    // 幂等：重复隐藏不重复入集
    let twice = toggle_hidden(&now, BrowseColumnId::Size, false);
    assert_eq!(twice, vec![BrowseColumnId::Size]);
}

// 显隐切换：整份配置只改 hidden，用户重排的 order 原样保留
// （回归：旧实现用 FULL_ORDER 重建 → 勾一次显隐就重置列序）
#[test]
fn toggle_visible_keeps_user_order() {
    let reordered = vec![
        BrowseColumnId::Mtime,
        BrowseColumnId::Name,
        BrowseColumnId::Size,
        BrowseColumnId::Type,
        BrowseColumnId::Packed,
        BrowseColumnId::Encoding,
    ];
    let base = columns(reordered.clone(), vec![BrowseColumnId::Encoding]);
    let next = toggle_visible(&base, BrowseColumnId::Size, false);
    assert_eq!(next.order, reordered, "显隐切换后列序必须原样保留");
    assert_eq!(
        next.hidden,
        vec![BrowseColumnId::Encoding, BrowseColumnId::Size]
    );
    let back = toggle_visible(&next, BrowseColumnId::Size, true);
    assert_eq!(back.hidden, vec![BrowseColumnId::Encoding]);
    assert_eq!(back.order, reordered);
}

// 动作目标项映射（行内挂载依赖）
#[test]
fn action_target_maps_inline_kinds() {
    assert_eq!(
        action_target(&ActionKind::Emit(DialogEvent::SettingsClose)),
        None
    );
    assert_eq!(
        action_target(&ActionKind::Clear(SettingId::Editor)),
        Some(SettingId::Editor)
    );
    assert_eq!(
        action_target(&ActionKind::FilePicker {
            target: SettingId::ImageViewer
        }),
        Some(SettingId::ImageViewer)
    );
}

// 走查 #11 回归：超宽标签在列内**换行**——宽 ≤ 本档列宽（不横向
// 溢出）、高 > 28px（≥ 2 行；若被外层 overflow_x_hidden 裁剪或 nowrap
// 则高度只有一行）。英文/其它语言的长名称由此保证完整可见。
#[gpui_kit::test]
fn long_label_wraps_within_column(cx: &mut gpui_kit::TestAppContext) {
    struct Probe;
    impl gpui_kit::Render for Probe {
        fn render(
            &mut self,
            _: &mut gpui_kit::Window,
            _: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            label_cell(
                "A sufficiently long setting label that must exceed the column width".into(),
                None,
                "probe",
            )
        }
    }
    let (_root, vtcx) = cx.add_window_view(|window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view: gpui_kit::AnyView = cx.new(|_| Probe).into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let bounds = vtcx.debug_bounds("lbl-probe").expect("标签锚点已绘制");
    assert!(
        bounds.size.width <= label_col_w() + px(1.),
        "标签不得横向溢出列宽：{bounds:?}"
    );
    assert!(
        bounds.size.height > px(28.),
        "超宽标签必须换行为多行（高度仅一行 = 被裁剪或 nowrap）：{bounds:?}"
    );
}

/// 双击实况判定（纯逻辑）：三态判据 + 占用者名的回落。
///
/// 守的是「界面对用户说真话」：把被 `UserChoice` 压着的格式报成「已生效」，
/// 用户就会以为双击归 hzip（实际打开的是 Bandizip）；反过来把待应用的报成
/// 「被占用」，用户会白点一次系统对话框。
#[test]
fn assoc_status_buckets_follow_system_precedence() {
    use hzip_platform::assoc::ExtAssocState;
    // 归我们（Windows 上还可能同时被 UserChoice 指向我们 = 用户自己选过）
    let ours = |name: Option<&str>| ExtAssocState {
        effective: Some(crate::ui::contracts::PROG_ID.to_string()),
        effective_name: name.map(str::to_string),
        ours: true,
        pinned: false,
    };
    let states = vec![
        ("zst".to_string(), ours(Some("hzip 压缩档案"))),
        // 被占用：别人的 UserChoice（哈希保护 ⇒ 程序改不动）
        (
            "zip".to_string(),
            ExtAssocState {
                effective: Some("Bandizip.zip".into()),
                effective_name: Some("Bandizip".into()),
                ours: false,
                pinned: true,
            },
        ),
        // 被占用但查不到可读名 → 回落标识原文（不能显示空串）
        (
            "rar".to_string(),
            ExtAssocState {
                effective: Some("WinRAR".into()),
                effective_name: None,
                ours: false,
                pinned: true,
            },
        ),
        // 待应用：系统里是别人，但**程序改得动**（Linux/macOS 常态；Windows
        // 无 UserChoice 时同此）——点「应用关联」即生效
        (
            "tar".to_string(),
            ExtAssocState {
                effective: Some("Bandizip.tar".into()),
                effective_name: None,
                ours: false,
                pinned: false,
            },
        ),
    ];
    let got = assoc_status_of(&states);
    assert_eq!(
        got,
        vec![
            AssocStatus::Effective("zst".into()),
            AssocStatus::Blocked {
                ext: "zip".into(),
                owner: "Bandizip".into()
            },
            AssocStatus::Blocked {
                ext: "rar".into(),
                owner: "WinRAR".into()
            },
            AssocStatus::Pending("tar".into()),
        ]
    );
}

// —— 快捷键块（T-KEYS） ——

/// 从上行事件里取出整份快捷键（非本类事件 → None）。
fn keys_of_event(ev: &DialogEvent) -> Option<hzip_config::Keys> {
    match ev {
        DialogEvent::SettingChanged { id, value } if *id == SettingId::Keymap => {
            value.as_keys().cloned()
        }
        _ => None,
    }
}

/// [`keys_of_event`] 的断言版（失败信息带上真事件，免得只看「不是期望类型」）。
fn expect_keys_event(ev: &DialogEvent) -> hzip_config::Keys {
    keys_of_event(ev).unwrap_or_else(|| panic!("期望 SettingChanged{{Keymap, Keys}}，实得 {ev:?}"))
}

/// 「禁用」哨兵必须与 keymap 的三态判读同义。
///
/// `settings_render::KEYS_DISABLED` 是重写的字面量（keymap 侧那个常量私有），
/// 两侧一旦分叉，写进配置的「禁用」会变成一个名叫 `none` 的普通键位：配置看着
/// 改了、动作却依旧失灵，且没有任何报错可循。故这条一致性**必须有**断言。
#[test]
fn keys_disabled_sentinel_agrees_with_keymap() {
    assert_eq!(KEYS_DISABLED, "none");
    assert_eq!(
        keymap::binding_of(KEYS_DISABLED),
        Binding::Disabled,
        "哨兵串在 keymap 侧必须判为「已禁用」"
    );
    assert!(
        keymap::spec_is_valid(KEYS_DISABLED),
        "前提核对：`none` 其实是个**可解析**的键名——正因如此才必须靠哨兵语义拦截，\
         不能指望解析失败来兜（顺序见 keymap::binding_of）"
    );
}

/// 冲突提示必须点名占用者（用户得知道去哪一行腾键位）。
#[test]
fn conflict_hint_names_the_occupant() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    let hint = conflict_hint(keymap::def_of(KeyActionId::SelectAll));
    assert!(
        hint.contains(&crate::tr("keymap.action.select_all")),
        "提示应含占用者动作名：{hint}"
    );
    assert!(!hint.contains("{action}"), "占位符必须被替换：{hint}");
}

/// `with_field` 写三态、且不认识的键名不改任何字段。
#[test]
fn with_field_writes_the_three_states() {
    let base = hzip_config::Keys::default();
    assert_eq!(
        with_field(&base, "select_all", "").field("select_all"),
        Some("")
    );
    assert_eq!(
        with_field(&base, "select_all", KEYS_DISABLED).field("select_all"),
        Some(KEYS_DISABLED)
    );
    assert_eq!(
        with_field(&base, "select_all", "ctrl-shift-a").field("select_all"),
        Some("ctrl-shift-a")
    );
    // 未知键名：整份原样（不 panic、不新建字段）
    let unknown = with_field(&base, "no-such-action", "ctrl-b");
    assert_eq!(unknown, base, "未知键名不得改动配置");
    // 原配置不被就地改动（整份替换语义：上行的是**新**的一份）
    assert_eq!(base.field("select_all"), Some(""));
}

/// 芯片文案与三态映射（纯函数，渲染与测试共用）。
///
/// 守的是「芯片说的就是配置里的事实」：把未绑定显示成某个键位（或反过来）会让
/// 用户以为改键成功/失败，而配置其实没动。
#[test]
fn chip_display_covers_the_three_states() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    let def = keymap::def_of(KeyActionId::SelectAll);

    // 默认态：显示内置键位的**显示串**（macOS 出字形、Windows 单字符大写）
    let (text, unbound) = chip_display(def, "", false, false);
    assert_eq!(text, keymap::display_spec(def.default_spec));
    assert!(!unbound, "空串 = 内置默认，不是「未绑定」");

    // 自定义态
    let (text, unbound) = chip_display(def, "ctrl-shift-a", false, false);
    assert_eq!(text, keymap::display_spec("ctrl-shift-a"));
    assert!(!unbound);

    // 已删除绑定（✕ 写下的哨兵）
    let (text, unbound) = chip_display(def, KEYS_DISABLED, false, false);
    assert_eq!(text, crate::tr("keymap.state.unbound"));
    assert!(unbound);

    // 录制中还没按键：提示语（芯片另外带录制态视觉）
    let (text, unbound) = chip_display(def, "", true, false);
    assert_eq!(text, crate::tr("keymap.recording"));
    assert!(!unbound, "录制态不该同时报未绑定（视觉由 recording 主导）");

    // 录制中已有 pending：基础文案仍是**当前**有效键位——新按键的预览在芯片
    // 自己的预览块里（两者叠着显示，用户能看出「原来是什么 → 要变成什么」）
    let (text, _) = chip_display(def, "", true, true);
    assert_eq!(text, keymap::display_spec(def.default_spec));
}

/// 直驱探针：只渲染快捷键块（不装配设置面板），用来单测块自身的锚点与上行。
///
/// 走**真宿主**（`KeysCtx` 齐备）：新交互下芯片需要焦点句柄才画得出来，而
/// 「点芯片 → 起录制」也要有个能记账的闭包（面板的事这里只记调用）。
struct KeysProbe {
    keys: hzip_config::Keys,
    hits: Rc<std::cell::RefCell<Vec<DialogEvent>>>,
    /// 录制态：`Some` = 那一行正在录
    recording: Option<KeyActionId>,
    /// 已按下的组合（实时预览）
    pending: Option<String>,
    /// 被拒的占用者 + 它属于哪一行（失焦提交的产物）
    conflict: Option<&'static KeyActionDef>,
    conflict_on: Option<KeyActionId>,
    /// 「起录制」被调的记录（面板侧行为，本层只断言它被调到了）
    starts: Rc<std::cell::RefCell<Vec<KeyActionId>>>,
}

impl gpui_kit::Render for KeysProbe {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl IntoElement {
        let hits = self.hits.clone();
        let starts = self.starts.clone();
        // 探针没有装配期，每帧现造 14 个句柄（测试专用代价；生产路径的句柄在
        // `SettingsPanel::new_entity` 建一次）
        let handles: Vec<(KeyActionId, gpui_kit::FocusHandle)> = keymap::ACTIONS
            .iter()
            .map(|def| (def.id, cx.focus_handle()))
            .collect();
        let ctx = PageCtx {
            on: Rc::new(move |ev| hits.borrow_mut().push(ev)),
            refresh: Rc::new(|_| {}),
            keys: Some(KeysCtx {
                recording: self.recording,
                pending: self.pending.clone().map(SharedString::from),
                conflict: self.conflict,
                conflict_on: self.conflict_on,
                focus_of: Rc::new(move |id: KeyActionId| {
                    handles
                        .iter()
                        .find(|(action, _)| *action == id)
                        .map(|(_, handle)| handle.clone())
                        .expect("每行动作都有焦点句柄")
                }),
                start: Rc::new(move |id: KeyActionId, _cx| starts.borrow_mut().push(id)),
            }),
        };
        keys_section(&self.keys, &ctx, cx)
    }
}

/// 直驱场景的返回类型（三个 `Rc<RefCell<Vec<..>>>` 直接写在签名里会触发
/// `clippy::type_complexity`）。
type ProbeScene<'a> = (
    &'a mut gpui_kit::VisualTestContext,
    Rc<std::cell::RefCell<Vec<DialogEvent>>>,
    Rc<std::cell::RefCell<Vec<KeyActionId>>>,
);

/// 直驱场景：探针实体 + 窗口（锚点只在**已渲染帧**里可查）。
///
/// 返借窗口上下文活到 `cx` 那一借：显式命名生命周期，因为签名里还有第二个输入
/// 引用（`pending: &str`）——两处输入引用让省略规则失效（`E0106`）。
fn probe_scene<'a>(
    cx: &'a mut gpui_kit::TestAppContext,
    keys: hzip_config::Keys,
    recording: Option<KeyActionId>,
    pending: Option<&str>,
) -> ProbeScene<'a> {
    let hits: Rc<std::cell::RefCell<Vec<DialogEvent>>> = Rc::default();
    let starts: Rc<std::cell::RefCell<Vec<KeyActionId>>> = Rc::default();
    let probe = cx.new({
        let hits = hits.clone();
        let starts = starts.clone();
        let pending = pending.map(str::to_string);
        move |_| KeysProbe {
            keys,
            hits,
            recording,
            pending,
            conflict: None,
            conflict_on: None,
            starts,
        }
    });
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view: gpui_kit::AnyView = probe.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    (vtcx, hits, starts)
}

/// 画一帧（锚点/命中区只在已渲染帧里可查）。
fn probe_draw(vtcx: &mut gpui_kit::VisualTestContext) {
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
}

/// 真实点击某个锚点。
fn probe_click(vtcx: &mut gpui_kit::VisualTestContext, anchor: &'static str) {
    probe_draw(vtcx);
    let bounds = vtcx
        .debug_bounds(anchor)
        .unwrap_or_else(|| panic!("缺锚点 {anchor}"));
    vtcx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
    vtcx.update(|_window, cx| cx.background_executor().run_until_parked());
}

/// 快捷键块：14 行**单行**版式（名称 / 芯片 / ✕ 同一行）+ 页头「全部恢复默认」。
///
/// 断言面全是**渲染后的真实几何**（`debug_bounds`），故「两行式版式回潮」这类
/// 改动拦得住：旧版式的行高 ≥ 2×行带、且芯片与 ✕ 不在同一行。
#[gpui_kit::test]
fn keys_block_is_single_line_per_action(cx: &mut gpui_kit::TestAppContext) {
    const TOL: f32 = 0.5;
    crate::ui::spacing::set_density(crate::viewmodel::density::Density::Standard);
    let (vtcx, _hits, _starts) = probe_scene(cx, hzip_config::Keys::default(), None, None);
    probe_draw(vtcx);

    let band = f32::from(crate::ui::views::settings_render::row_band());
    let row_gap = f32::from(crate::ui::spacing::row_gap());
    let bounds = |vtcx: &mut gpui_kit::VisualTestContext, name: String| {
        let selector = test_anchor(name.clone());
        vtcx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("缺锚点 {name}"))
    };
    let center = |b: gpui_kit::Bounds<gpui_kit::Pixels>| {
        (
            f32::from(b.origin.y) + f32::from(b.size.height) / 2.,
            (f32::from(b.origin.x), f32::from(b.size.width)),
        )
    };

    // 页头在**所有行之上**（标题左、动作右；块内没有左侧 label 列）
    let reset_all = bounds(vtcx, "kreset-all".to_string());
    for def in keymap::ACTIONS {
        let row = bounds(vtcx, format!("krow-{}", def.config_key));
        let chip = bounds(vtcx, format!("kc-{}", def.config_key));
        let remove = bounds(vtcx, format!("kremove-{}", def.config_key));
        assert!(
            f32::from(reset_all.origin.y) < f32::from(row.origin.y),
            "页头「全部恢复默认」应在动作行之上（{}）",
            def.config_key
        );
        // 单行：行高 = max(行带, 行尾图标钮边长) + 行距（两行式版式 ≥ 2×带）。
        // 图标钮比行带高 4px（2026-10-16 用户口径「内边距 +2px/侧」）⇒ 上限按两者取大。
        let row_h = f32::from(row.size.height);
        let icon_side = vtcx.update(|_window, cx| f32::from(super::icon_button_side(cx)));
        let limit = band.max(icon_side) + 2. * row_gap + 1.;
        assert!(
            row_h <= limit,
            "{}: 行高 {row_h:.2} 超过单行上限 {limit:.2}（行带 {band:.2} / 图标钮 {icon_side:.2}）——行应为单行版式",
            def.config_key
        );
        // 名称 / 芯片 / ✕ 同一行：芯片与 ✕ 中心线重合，✕ 在芯片右侧
        let (chip_c, (chip_x, _)) = center(chip);
        let (remove_c, (remove_x, _)) = center(remove);
        assert!(
            (chip_c - remove_c).abs() <= TOL,
            "{}: 芯片与 ✕ 不在同一行（中心差 {:+.2}px）",
            def.config_key,
            chip_c - remove_c
        );
        assert!(
            remove_x >= chip_x,
            "{}: ✕ 应在芯片右侧（芯片 x {chip_x:.2}，✕ x {remove_x:.2}）",
            def.config_key
        );
        // 未录制 → 无录制态/预览锚点
        assert!(
            vtcx.debug_bounds(test_anchor(format!("kc-rec-{}", def.config_key)))
                .is_none(),
            "{}: 未录制却渲染了录制态文案",
            def.config_key
        );
        assert!(
            vtcx.debug_bounds(test_anchor(format!("kc-pending-{}", def.config_key)))
                .is_none(),
            "{}: 未录制却渲染了按键预览",
            def.config_key
        );
        // 行内不再有「恢复默认」/「禁用」两个文本按钮（§9.1 取代项）
        assert!(
            vtcx.debug_bounds(test_anchor(format!("kreset-{}", def.config_key)))
                .is_none(),
            "{}: 行内不该再有「恢复默认」文本按钮",
            def.config_key
        );
        assert!(
            vtcx.debug_bounds(test_anchor(format!("kdisable-{}", def.config_key)))
                .is_none(),
            "{}: 行内不该再有「禁用」文本按钮（✕ 取代）",
            def.config_key
        );
        assert!(
            vtcx.debug_bounds(test_anchor(format!("kc-idle-{}", def.config_key)))
                .is_some(),
            "{}: 未录制应渲染空闲态文案块",
            def.config_key
        );
    }
}

/// 三态显隐在上行之外也看得见：`"none"` 的行芯片报「未绑定」
/// （`kc-unbound-*`），其余行不报——默认/自定义/未绑定三态的第一档。
#[gpui_kit::test]
fn keys_block_marks_unbound_rows(cx: &mut gpui_kit::TestAppContext) {
    let keys = with_field(&hzip_config::Keys::default(), "open", KEYS_DISABLED);
    let (vtcx, _hits, _starts) = probe_scene(cx, keys, None, None);
    probe_draw(vtcx);

    assert!(
        vtcx.debug_bounds("kc-unbound-open").is_some(),
        "写 `none` 的行应显示「未绑定」"
    );
    assert!(
        vtcx.debug_bounds("kc-unbound-select_all").is_none(),
        "默认态的行不该显示「未绑定」"
    );
}

/// 录制预览只挂在**正在录制**的那一行（面板的 pending 是单份的）。
#[gpui_kit::test]
fn keys_block_previews_pending_on_the_recording_row(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, _hits, _starts) = probe_scene(
        cx,
        hzip_config::Keys::default(),
        Some(KeyActionId::SelectAll),
        Some("ctrl-shift-a"),
    );
    probe_draw(vtcx);

    assert!(vtcx.debug_bounds("kc-rec-select_all").is_some());
    assert!(
        vtcx.debug_bounds("kc-pending-select_all").is_some(),
        "录制中已按下的组合应显示为预览"
    );
    assert!(
        vtcx.debug_bounds("kc-idle-select_all").is_none(),
        "录制中的芯片不该还是空闲态文案块"
    );
    assert!(
        vtcx.debug_bounds("kc-rec-open").is_none()
            && vtcx.debug_bounds("kc-pending-open").is_none(),
        "预览只该出现在录制行（别的行是默认/自定义态）"
    );
}

/// ✕ 与页头「全部恢复默认」的上行载荷（直驱块 + 闭包捕获）。
#[gpui_kit::test]
fn keys_block_writes_none_and_reset_all(cx: &mut gpui_kit::TestAppContext) {
    let (vtcx, hits, starts) = probe_scene(cx, hzip_config::Keys::default(), None, None);

    // ① 行尾 ✕ → 该字段 = 哨兵（删除该动作的绑定），其余字段不动
    probe_click(vtcx, "kremove-select_all");
    let events = hits.borrow().clone();
    assert_eq!(events.len(), 1, "点一次只该上行一条：{events:?}");
    let keys = expect_keys_event(&events[0]);
    assert_eq!(keys.field("select_all"), Some(KEYS_DISABLED));
    assert_eq!(keys.field("open"), Some(""), "其余动作不受影响");

    // ② 页头「全部恢复默认」→ 整份出厂默认
    probe_click(vtcx, "kreset-all");
    let events = hits.borrow().clone();
    assert_eq!(events.len(), 2, "页头按钮只该再上行一条：{events:?}");
    assert_eq!(expect_keys_event(&events[1]), hzip_config::Keys::default());

    // ③ 点芯片 = 起录制（面板的事）：本层**不得**凭空上行任何事件
    probe_click(vtcx, "kc-open");
    assert_eq!(hits.borrow().len(), 2, "点芯片不得写配置（录制中不落库）");
    assert_eq!(
        starts.borrow().as_slice(),
        &[KeyActionId::Open],
        "点芯片应把该行交给「起录制」"
    );
}

/// 行尾 ↻ 还原钮（用户口径 2026-10-16：✕ 旁边要一个「恢复该快捷键到默认值」）：
/// 点击写**空串**（回落内置默认）而不是写回当前默认键位串——空串才是「默认」的
/// 唯一表示，写死键位串会让该行从此不再跟随默认值变化。
#[gpui_kit::test]
fn keys_block_restore_button_restores_default(cx: &mut gpui_kit::TestAppContext) {
    let mut custom = hzip_config::Keys::default();
    *custom.field_mut("select_all").expect("契约键名") = "f2".into();
    *custom.field_mut("quit").expect("契约键名") = KEYS_DISABLED.into();
    let (vtcx, hits, _starts) = probe_scene(cx, custom, None, None);

    probe_click(vtcx, "krestore-select_all");
    let events = hits.borrow().clone();
    assert_eq!(events.len(), 1, "还原钮只该上行一条：{events:?}");
    let keys = expect_keys_event(&events[0]);
    assert_eq!(
        keys.field("select_all"),
        Some(""),
        "还原写空串（回落内置默认），不是写回默认键位串"
    );
    assert_eq!(keys.field("quit"), Some(KEYS_DISABLED), "只动本行字段");

    // 未绑定的行同样能还原（删除绑定后逐项复原的唯一入口）
    probe_click(vtcx, "krestore-quit");
    let events = hits.borrow().clone();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(expect_keys_event(&events[1]).field("quit"), Some(""));
}

/// 冲突红字挂在**被拒的那一行**下面（`conflict_on` 决定行，`conflict` 决定文案）。
#[gpui_kit::test]
fn keys_block_shows_conflict_on_the_rejected_row(cx: &mut gpui_kit::TestAppContext) {
    let occupant = keymap::def_of(KeyActionId::Quit);
    let hits: Rc<std::cell::RefCell<Vec<DialogEvent>>> = Rc::default();
    let starts: Rc<std::cell::RefCell<Vec<KeyActionId>>> = Rc::default();
    let probe = cx.new({
        let hits = hits.clone();
        let starts = starts.clone();
        move |_| KeysProbe {
            keys: hzip_config::Keys::default(),
            hits,
            recording: None,
            pending: None,
            conflict: Some(occupant),
            conflict_on: Some(KeyActionId::SelectAll),
            starts,
        }
    });
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view: gpui_kit::AnyView = probe.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    probe_draw(vtcx);

    assert!(
        vtcx.debug_bounds("kconflict-select_all").is_some(),
        "冲突红字应挂在被拒的那一行（录制态已收口，靠 conflict_on 定位）"
    );
    assert!(
        vtcx.debug_bounds("kconflict-other_rows").is_none(),
        "冲突不该出现在别的行"
    );
    // 文案点名占用者（渲染与提示同源，防止渲染层各写一套）
    let hint = conflict_hint(occupant);
    assert!(hint.contains(&crate::tr("keymap.action.quit")), "{hint}");
}
