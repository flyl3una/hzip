use super::*;
use gpui_kit::component::theme::ThemeConfigColors;

/// 内容区底色 = 菜单/工具栏底色（用户口径 2026-10-08）。
///
/// 用户报障：「内容界面的背景颜色应该和菜单/工具栏一致（浅色白 / 深色黑），
/// 而不是现在的黑」。契约点 = `theme.background`（内容区/窗口底——`shell`
/// 根、`browse_layout` 三处面板、`empty_state`、jobs 页根都读它）必须与
/// `theme.popover`（菜单条/工具栏/栏面）**逐值相等**，两套模式各成立。
///
/// 同时钉住绝对值（浅色纯白 / 深色 `#1f2226`）：只断言「两者相等」会漏掉
/// 「两边一起变错」（例如两套 token 同时退回 kit 默认值）的情况。
#[gpui_kit::test]
fn content_background_matches_menu_toolbar(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;
    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        let probe = |choice: ThemeModeChoice, cx: &mut gpui_kit::App| {
            let mut cfg = hzip_config::Config::defaults();
            // 真机同款：用户配了自定义主题色（accent 系槽会被整体覆盖）
            cfg.ui.theme_color = "#16A34A".to_string();
            apply_theme_cfg(choice, &cfg, cx);
            let t = cx.theme();
            (t.background, t.popover)
        };
        let (bg_light, popover_light) = probe(ThemeModeChoice::Light, cx);
        assert_eq!(
            bg_light, popover_light,
            "浅色：内容区底色须与菜单/工具栏同色（不得再是灰底）"
        );
        assert_eq!(
            bg_light,
            Hsla::from(gpui_kit::rgb(0xffffff)),
            "浅色内容区 = 纯白（菜单/工具栏同值）"
        );
        let (bg_dark, popover_dark) = probe(ThemeModeChoice::Dark, cx);
        assert_eq!(
            bg_dark, popover_dark,
            "深色：内容区底色须与菜单/工具栏同色（不得再是近黑的 #17191c）"
        );
        assert_eq!(
            bg_dark,
            Hsla::from(gpui_kit::rgb(0x1f2226)),
            "深色内容区 = 栏色 #1f2226"
        );
        assert_ne!(bg_light, bg_dark, "两套模式必须给出不同的内容底色");
    });
}

/// 圆角弧度**由配置驱动**（用户口径 2026-09-23：圆角大小由用户自定）。
///
/// 机制：kit 的 `PopupMenu`/按钮/输入框/弹层容器圆角都取全局 `theme.radius`
/// （组件级无钩子，见 [`apply_theme_cfg`] 注释）⇒ 本用例断言「配置值 → 主题
/// 槽位」的映射：含 `radius_lg` 档比、越界归一、0 = 直角（kit 方形模式）。
/// 滚动条颜色**不跟随主题色**（用户口径 2026-09-24）：换主题色（含自定义
/// `[ui].theme_color`）后滑块灰必须逐值不变；作为对照，accent 系语义槽必须
/// 确实跟着变（证明本用例没有空跑）。
#[gpui_kit::test]
fn scrollbar_color_is_theme_independent(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;
    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        let probe = |theme_color: &str, cx: &mut gpui_kit::App| {
            let mut cfg = hzip_config::Config::defaults();
            cfg.ui.theme_color = theme_color.to_string();
            apply_theme_cfg(ThemeModeChoice::Light, &cfg, cx);
            let t = cx.theme();
            (
                t.scrollbar_thumb,
                t.scrollbar_thumb_hover,
                t.scrollbar,
                t.accent,
            )
        };
        // 默认主题色（空 = 用内置）
        let (thumb_a, hover_a, track_a, accent_a) = probe("", cx);
        // 自定义主题色（红）——accent 必须变，滚动条三色必须**不变**
        let (thumb_b, hover_b, track_b, accent_b) = probe("#E02020", cx);
        assert_ne!(
            accent_a, accent_b,
            "前置：自定义主题色应改变 accent（否则用例空跑）"
        );
        assert_eq!(thumb_a, thumb_b, "滚动条滑块不得随主题色变");
        assert_eq!(hover_a, hover_b, "悬停滑块不得随主题色变");
        assert_eq!(track_a, track_b, "滚动条轨道不得随主题色变");
        // 深色模式同值（两套主题共用同一组固定灰）
        let mut cfg = hzip_config::Config::defaults();
        cfg.ui.theme_color = "#E02020".to_string();
        apply_theme_cfg(ThemeModeChoice::Dark, &cfg, cx);
        let t = cx.theme();
        assert_eq!(t.scrollbar_thumb, thumb_a, "深色主题下同样不染色");
        assert_eq!(t.scrollbar_thumb_hover, hover_a);
        // 灰色本身是中性灰（r=g=b），不是被染色后的偏色值
        let rgba = gpui_kit::Rgba::from(thumb_a);
        assert!(
            (rgba.r - rgba.g).abs() < 0.01 && (rgba.g - rgba.b).abs() < 0.01,
            "滑块色必须是中性灰（r=g=b）：{rgba:?}"
        );
    });
}

#[gpui_kit::test]
fn theme_radius_follows_config(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;
    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        let probe = |choice: ThemeModeChoice, cfg_radius: u32, cx: &mut gpui_kit::App| {
            let mut cfg = hzip_config::Config::defaults();
            cfg.ui.corner_radius = cfg_radius;
            apply_theme_cfg(choice, &cfg, cx);
            (
                cx.theme().radius,
                cx.theme().radius_lg,
                cx.theme().radius_full(),
            )
        };
        // 默认 = 直角：radius 与 radius_full 都归零（徽标/头像不残留圆形）
        let (r, lg, full) = probe(ThemeModeChoice::Light, 0, cx);
        assert_eq!(gpui_kit::px(0.), r, "0 必须是直角");
        assert_eq!(gpui_kit::px(0.), lg, "对话框档（radius_lg）也要跟着归零");
        assert_eq!(gpui_kit::px(0.), full, "radius_full 应随之为 0");
        // kit 原生默认档 6 → radius_lg 8（档比 6:8 保持，观感与组件库一致）
        let (r, lg, _) = probe(ThemeModeChoice::Light, 6, cx);
        assert_eq!(gpui_kit::px(6.), r);
        assert_eq!(gpui_kit::px(8.), lg, "半径档比 6:8 应保持");
        // 越大越圆 + 越界回落默认
        let (r, _, _) = probe(ThemeModeChoice::Light, 20, cx);
        assert_eq!(gpui_kit::px(20.), r);
        let (r, lg, _) = probe(ThemeModeChoice::Light, u32::MAX, cx);
        assert_eq!(gpui_kit::px(0.), r, "越界应回落默认（直角）");
        assert_eq!(gpui_kit::px(0.), lg);
        // 换主题（深色）后仍按配置取圆角
        let (r, _, _) = probe(ThemeModeChoice::Dark, 6, cx);
        assert_eq!(gpui_kit::px(6.), r, "换主题不影响圆角配置");
    });
}

/// **「大框」圆角 = 设置圆角 × 2**（用户口径 2026-10-16）。
///
/// 进度窗输出框这类大容器的圆角取值点就是 [`big_radius`]——本用例钉住这条规则
/// 本身（含 0 = 直角同倍率成立、上限 20 → 40），免得日后被「顺手改成和
/// `theme.radius` 同值」而悄悄回退。
#[gpui_kit::test]
fn big_radius_is_double_the_configured_radius(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;
    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        let probe = |cfg_radius: u32, cx: &mut gpui_kit::App| {
            let mut cfg = hzip_config::Config::defaults();
            cfg.ui.corner_radius = cfg_radius;
            apply_theme_cfg(ThemeModeChoice::Light, &cfg, cx);
            (cx.theme().radius, big_radius(cx))
        };
        // 直角模式：基础档与大框都是 0（大框不得凭空长出圆角）
        assert_eq!(
            (gpui_kit::px(0.), gpui_kit::px(0.)),
            probe(0, cx),
            "0 圆角下大框也必须直角"
        );
        // 圆角模式：6 → 12；20（上限）→ 40
        assert_eq!(
            (gpui_kit::px(6.), gpui_kit::px(12.)),
            probe(6, cx),
            "大框 = 设置 × 2"
        );
        assert_eq!(
            (gpui_kit::px(20.), gpui_kit::px(40.)),
            probe(20, cx),
            "上限档同样按 2 倍"
        );
    });
}

/// T-GUI T01 防再犯门禁：主题 JSON 的 colors key 必须是 gpui-kit
/// `ThemeConfigColors` 已知字段（serde 不拒绝未知 key，拼错即静默失效
/// ——历史事故：`"primary"`/`"surface"` 写错后主按钮/卡片长期落默认色）。
/// 合法 key 全集 = 空配置序列化（`Serialize` 全字段导出）。
#[test]
fn theme_json_keys_are_known() {
    let json = light_theme_json();
    let colors = json
        .get("colors")
        .and_then(|c| c.as_object())
        .expect("light_theme_json 必含 colors 对象");

    let known: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&serde_json::to_string(&ThemeConfigColors::default()).unwrap())
            .unwrap();
    assert!(!known.is_empty(), "ThemeConfigColors 序列化不得为空");

    let unknown: Vec<&String> = colors.keys().filter(|k| !known.contains_key(*k)).collect();
    assert!(
        unknown.is_empty(),
        "主题 colors 含未知 key（gpui-kit 会静默丢弃）: {unknown:?}"
    );
}

/// 界面字体族/字号必须经 `font.family` / `font.size` 落地（serde 不拒绝
/// 未知 key ⇒ 键名写错只会静默失效，这里锚住「真的进了 ThemeConfig」）。
#[test]
fn font_config_lands_in_theme_config() {
    let mut json = light_theme_json();
    merge_font_config(&mut json, "Microsoft YaHei", 18);
    let cfg: ThemeConfig = serde_json::from_value(json).expect("注入后仍可解析为 ThemeConfig");
    assert_eq!(
        cfg.font_family.as_deref(),
        Some("Microsoft YaHei"),
        "字体族未落到 font.family"
    );
    assert_eq!(cfg.font_size, Some(18.), "字号未落到 font.size");

    // 空字体族 = 不注入（保持主题默认 `.SystemUIFont`），字号照旧注入
    let mut json2 = light_theme_json();
    merge_font_config(&mut json2, "", 10);
    let cfg2: ThemeConfig = serde_json::from_value(json2).expect("可解析");
    assert_eq!(cfg2.font_family, None, "空族不应写入 font.family");
    assert_eq!(cfg2.font_size, Some(10.));
}

/// T-SEL 走查 #3：hex 解析/回写往返 + 家族派生（hover 提亮、active
/// 压暗、soft 半透明尾缀）。
#[test]
fn theme_color_family_derivation() {
    // 非法 hex → None（不覆盖）
    assert!(theme_color_family("").is_none());
    assert!(theme_color_family("#xyz123").is_none());
    let f = theme_color_family("#2f6fed").expect("合法 hex");
    assert_eq!(f.base, "#2F6FED", "规范化大写");
    let base = parse_theme_hex(&f.base).unwrap();
    let hover = parse_theme_hex(&f.hover).unwrap();
    let active = parse_theme_hex(&f.active).unwrap();
    assert!(hover.l > base.l, "hover 提亮一档");
    assert!(active.l < base.l, "active 压暗一档");
    assert_eq!(f.soft.len(), 9, "soft = base + AA 尾缀");
    // 往返：theme_hex_of(parse(hex)) == 规范化 hex
    assert_eq!(theme_hex_of(base), "#2F6FED");
}

/// T-SEL 走查 #3：主题色 merge 后的 colors key 仍是 ThemeConfigColors
/// 已知字段（防 serde 静默丢弃）。
#[test]
fn theme_color_merged_keys_are_known() {
    use gpui_kit::component::theme::ThemeConfigColors;
    let mut json = theme_json(ThemeModeChoice::Light);
    let family = theme_color_family("#2f6fed").unwrap();
    merge_theme_color(&mut json, &family);
    let colors = json
        .get("colors")
        .and_then(|c| c.as_object())
        .expect("必含 colors");
    let known: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&serde_json::to_string(&ThemeConfigColors::default()).unwrap())
            .unwrap();
    let unknown: Vec<&String> = colors.keys().filter(|k| !known.contains_key(*k)).collect();
    assert!(unknown.is_empty(), "merge 后含未知 key: {unknown:?}");
    // 覆盖槽生效：primary.background = 新色
    assert_eq!(colors.get("primary.background").unwrap(), "#2F6FED");
    // 反序列化仍合法
    let config: ThemeConfig = serde_json::from_value(json).expect("merge 后主题 JSON 必须合法");
    assert_eq!(config.name, "hzip-light");
}

/// T-SEL 走查 #4：统一选中色 = 背景 × accent 30% 不透明混色
///（浅于 accent、深于 15% 淡染；不透明——覆盖 kit 单选 accent 背景
/// 后颜色稳定）。
#[gpui_kit::test]
fn selection_row_bg_is_opaque_blend(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;
    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx); // Theme global 装配
        apply_light_theme(cx);
        let bg = selection_row_bg(cx);
        assert!(
            bg.a >= 0.999,
            "选中色必须不透明（覆盖 kit accent 背景后颜色稳定）"
        );
        let accent = cx.theme().accent;
        assert!(bg.l > accent.l, "选中色浅于主题色（浅色主题）");
        assert!(bg.l < cx.theme().background.l, "选中色深于纯背景（可见）");
        // 深色主题：accent 提亮（l 更高）→ 混色仍浅于 accent 且深于背景
        apply_theme(ThemeModeChoice::Dark, cx);
        let bg_dark = selection_row_bg(cx);
        let accent_dark = cx.theme().accent;
        assert!(
            bg_dark.l < accent_dark.l,
            "深色主题选中色浅于（提亮的）主题色"
        );
        assert!(
            bg_dark.l > cx.theme().background.l,
            "深色主题选中色亮于纯背景（可见）"
        );
    });
}

/// T-GUI T01：light_theme_json 必须能被 `ThemeConfig` 反序列化
///（key 合法 + 值可解析；解析失败 apply_light_theme 会静默回退默认主题）。
#[test]
fn light_theme_json_parses_into_theme_config() {
    let config: ThemeConfig =
        serde_json::from_value(light_theme_json()).expect("hzip 浅色主题 JSON 必须合法");
    assert_eq!(config.name, "hzip-light");
    assert_eq!(config.mode, ThemeMode::Light);
}

/// T-GUI T25：dark 覆写同一批槽位 + 可被 `ThemeConfig` 反序列化。
#[test]
fn dark_theme_json_parses_and_matches_slots() {
    let json = theme_json(ThemeModeChoice::Dark);
    let config: ThemeConfig =
        serde_json::from_value(json.clone()).expect("hzip 深色主题 JSON 必须合法");
    assert_eq!(config.name, "hzip-dark");
    assert_eq!(config.mode, ThemeMode::Dark);

    // 浅深两套槽位集合一致（同一批 key，值不同——视觉语言一致的前提）
    let light_keys: Vec<String> = {
        let light = light_theme_json();
        let o = light.get("colors").unwrap().as_object().unwrap();
        o.keys().cloned().collect()
    };
    let dark_keys: Vec<String> = {
        let dark = json.get("colors").unwrap().as_object().unwrap();
        dark.keys().cloned().collect()
    };
    assert_eq!(light_keys, dark_keys, "浅深两套必须覆写同一批槽位");

    // 交互高亮语义：ring/list.active.border 走各自 ACCENT
    let colors = json.get("colors").unwrap();
    assert_eq!(colors.get("ring").unwrap(), dark::ACCENT);
    assert_eq!(colors.get("list.active.border").unwrap(), dark::ACCENT);
    assert_eq!(colors.get("background").unwrap(), dark::BG);
    // 压缩级别滑块（09 §6）：轨道走主色、手指走卡片面——浅深两套各自取值
    assert_eq!(colors.get("slider.background").unwrap(), dark::ACCENT);
    assert_eq!(
        colors.get("slider.thumb.background").unwrap(),
        dark::SURFACE
    );
    let light_colors = light_theme_json();
    let lc = light_colors.get("colors").unwrap();
    assert_eq!(lc.get("slider.background").unwrap(), light::ACCENT);
    assert_eq!(lc.get("slider.thumb.background").unwrap(), light::SURFACE);
}

/// 界面字号的**接线**：`Theme::font_size` 必须驱动窗口 rem 基准——这是
/// 「设置-常规 ▸ 界面字体大小」生效的唯一链路（gpui-component 的 `Root`
/// 每帧 `window.set_rem_size(cx.theme().font_size)`，全界面文字与 rem 间距
/// 随之等比缩放）。
///
/// 断言面 = **真实窗口的 `rem_size()`**：把 `font.size` 键名拼错、或把字号
/// 改从别处下发，本用例失败。末段反向改回 16 再断言一次——否则「恰好 20」
/// 可能来自别的地方，用例会假绿。
#[gpui_kit::test]
fn theme_font_size_drives_window_rem(cx: &mut gpui_kit::TestAppContext) {
    // 本模块平时不碰 gpui 元素 API（只有 token），用例内按需引一次
    use gpui_kit::prelude::*;
    use gpui_kit::px;

    struct Host;
    impl gpui_kit::Render for Host {
        fn render(
            &mut self,
            _: &mut gpui_kit::Window,
            _: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            gpui_kit::div().child("x")
        }
    }
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view = cx.new(|_| Host);
        // 与生产同一条路径：配置 → `ThemeConfig`（`font.size`）→ 全局 Theme
        let mut json = theme_json(ThemeModeChoice::Light);
        merge_font_config(&mut json, "", 20);
        let config: ThemeConfig = serde_json::from_value(json).expect("主题配置可解析");
        Theme::global_mut(cx).apply_config(&Rc::new(config));
        gpui_kit::component::Root::new(view, window, cx)
    });

    // 保存当前值，用例末尾还原——`Theme` 是**进程全局**，别的用例可能正在
    // 按「本机配置推导出的字号」断言几何（还原成 16 会把它们带偏）。
    let prev_size = vtcx.update(|_, cx| Theme::global(cx).font_size);

    let rem_20 = vtcx.update(|window, _| window.rem_size());
    assert_eq!(
        rem_20,
        px(20.),
        "Root 渲染帧应把 theme.font_size(20) 设为窗口 rem 基准"
    );

    // 反向：改回默认 16 再画一帧 → rem 跟随（排除「恰好 20」的假绿）
    vtcx.update(|window, cx| {
        Theme::global_mut(cx).font_size = px(16.);
        window.draw(cx).clear(cx);
    });
    let rem_16 = vtcx.update(|window, _| window.rem_size());
    assert_eq!(rem_16, px(16.), "字号改回 16 后 rem 基准应跟随");

    vtcx.update(|_, cx| Theme::global_mut(cx).font_size = prev_size);
}

mod interlock_tests {
    use super::*;

    /// UI 侧互锁锚（对应 app/labels.rs 的零依赖自检）：
    /// labels THEME 表 == ThemeModeChoice::LABELS；dialogs 白名单 ==
    /// labels 表派生（T-GUI T12 收口后 dialogs 常量即 labels 派生面）。
    // 运行时切语言 → **界面框架文案**（页签/菜单/工具栏/状态栏）确实换语言。
    //
    // 这是用户报的「改了语言界面没变」的直接断言面：改造前只有错误原因文案走
    // i18n，界面文案全是中文硬编码，所以切到 English 主界面纹丝不动。现在这些
    // 文案全部来自 `crate::tr`，本用例抽样断言其随 locale 变化。
    #[test]
    fn chrome_text_follows_locale_switch() {
        let _locale = crate::viewmodel::testutil::locale_lock();
        use crate::app::settings_registry::SettingsTab;
        // 中文基线
        crate::init_locale("zh-CN");
        assert_eq!(SettingsTab::General.label(), "常规");
        assert_eq!(crate::tr("toolbar.extract"), "解压");
        assert_eq!(crate::tr("menu.file.label"), "文件(F)");
        assert_eq!(crate::tr("tab_bar.jobs"), "任务中心");
        assert_eq!(crate::tr("shell.app_title"), "hzip 档案管理器");
        // 切英文 → 同一批文案全变（不再是硬编码）
        crate::init_locale("en");
        assert_eq!(SettingsTab::General.label(), "General");
        assert_eq!(crate::tr("toolbar.extract"), "Extract");
        assert_eq!(crate::tr("menu.file.label"), "File");
        assert_eq!(crate::tr("tab_bar.jobs"), "Jobs");
        assert_eq!(crate::tr("shell.app_title"), "hzip Archive Manager");
        // 主题白名单与错误原因文案同样跟随（两处既有消费者）
        assert_eq!(
            crate::app::labels::label_of(&crate::app::labels::THEME, 2),
            "Dark"
        );
        assert_eq!(rust_i18n::t!("err.hz_1004"), "Source not found");
        crate::init_locale("zh-CN");
    }

    #[test]
    fn theme_mode_choice_labels_match() {
        let _locale = crate::viewmodel::testutil::locale_lock();
        use crate::app::labels;
        assert_eq!(
            labels::label_keys(labels::THEME),
            ThemeModeChoice::LABEL_KEYS,
            "THEME 表 key 序列 == ThemeModeChoice::LABEL_KEYS"
        );
        assert_eq!(
            crate::ui::dialogs::CONFLICT_LABEL_KEYS.as_slice(),
            labels::label_keys(labels::CONFLICT)
        );
        assert_eq!(
            crate::ui::dialogs::CODEPAGE_LABEL_KEYS.as_slice(),
            labels::label_keys(labels::CODEPAGE)
        );
        assert_eq!(
            crate::ui::dialogs::LANG_LABEL_KEYS.as_slice(),
            labels::label_keys(labels::LANG)
        );
        // 09 压缩对话框新增白名单（同款互锁：dialogs 常量 == labels 表派生）
        assert_eq!(
            crate::ui::dialogs::METHOD_LABEL_KEYS.as_slice(),
            labels::label_keys(labels::METHOD)
        );
        assert_eq!(
            crate::ui::dialogs::SPLIT_LABEL_KEYS.as_slice(),
            labels::label_keys(labels::SPLIT)
        );
        assert_eq!(
            crate::ui::dialogs::THREADS_LABEL_KEYS.as_slice(),
            labels::label_keys(labels::THREADS)
        );
        assert_eq!(
            crate::ui::dialogs::ENC_METHOD_LABEL_KEYS.as_slice(),
            labels::label_keys(labels::ENC_METHOD)
        );
    }
}

/// 主题重放取**传入的内存值**，不读盘（用户实报 2026-10-09「界面字号/圆角点一次
/// 不生效、再点一次才生效」的回归锚点）。
///
/// 判据构造：连续两次重放**不同**值——若实现退回读盘（`Config::load()`），两次
/// 都会得到盘上同一份，两个断言不可能同时成立。
#[gpui_kit::test]
fn theme_replay_takes_passed_values_not_disk(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;
    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        let replay = |size_px: u32, radius_px: u32| ThemeReplay {
            mode_ix: 0,
            theme_color: String::new(),
            family: String::new(),
            size_px,
            radius_px,
        };
        apply_theme_replay(&replay(11, 0), cx);
        assert_eq!(cx.theme().font_size, gpui_kit::px(11.), "字号须来自传入值");
        assert_eq!(
            cx.theme().radius,
            gpui_kit::px(0.),
            "圆角须来自传入值（0 = 直角）"
        );
        apply_theme_replay(&replay(23, 18), cx);
        assert_eq!(
            cx.theme().font_size,
            gpui_kit::px(23.),
            "第二次重放同样以传入值为准"
        );
        assert_eq!(cx.theme().radius, gpui_kit::px(18.));
        assert_eq!(
            cx.theme().radius_lg,
            gpui_kit::px(24.),
            "半径档比 6:8 保持（18 → 24）"
        );
    });
}
