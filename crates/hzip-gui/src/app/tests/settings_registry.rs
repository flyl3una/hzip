use super::*;
use crate::app::state::SettingsPrefs;

fn app() -> AppData {
    AppData::new(std::sync::Arc::new(crate::viewmodel::testutil::engine()))
}

// 新增三项（界面字体 / 字体大小 / 布局密度）的落库契约：类型守界、
// 越界归一、值↔契约字符串双向一致。
//
// 断言面 = **AppData 镜像**（面板活读与泵取值共用的那一份），不是 config
// 文件——`cfg!(test)` 下 `persist` 零落盘，读文件读到的是开发机上的真实配置。
#[test]
fn appearance_settings_round_trip() {
    use crate::viewmodel::SettingId;
    use crate::viewmodel::density::Density;

    let mut data = app();

    // 字体族：去两侧空白后原样入镜像；空串 = 系统默认
    data.apply_setting(
        SettingId::FontFamily,
        &SettingValue::Text("  Consolas  ".into()),
    );
    assert_eq!(data.settings.font_family, "Consolas");
    data.apply_setting(SettingId::FontFamily, &SettingValue::Text(String::new()));
    assert_eq!(data.settings.font_family, "", "空串 = 系统默认");

    // 字号：区内原样；越界回落默认（控件已按 min/max 归一，store 再兜一次）
    data.apply_setting(SettingId::FontSize, &SettingValue::Number(20));
    assert_eq!(data.settings.font_size, 20);
    data.apply_setting(SettingId::FontSize, &SettingValue::Number(99));
    assert_eq!(
        data.settings.font_size,
        hzip_config::FONT_SIZE_DEFAULT,
        "越界回落默认字号"
    );
    data.apply_setting(SettingId::FontSize, &SettingValue::Number(10));
    assert_eq!(
        data.settings.font_size,
        hzip_config::FONT_SIZE_MIN,
        "下界有效"
    );

    // 圆角：界内原样；越界回落默认（**0 = 直角是合法值**，不是「漏键」）
    data.apply_setting(SettingId::CornerRadius, &SettingValue::Number(8));
    assert_eq!(data.settings.corner_radius, 8);
    data.apply_setting(SettingId::CornerRadius, &SettingValue::Number(0));
    assert_eq!(data.settings.corner_radius, 0, "0 = 直角，必须保留");
    data.apply_setting(
        SettingId::CornerRadius,
        &SettingValue::Number(hzip_config::CORNER_RADIUS_MAX + 1),
    );
    assert_eq!(
        data.settings.corner_radius,
        hzip_config::CORNER_RADIUS_DEFAULT,
        "越界回落默认圆角"
    );

    // 密度：下标 → 档位 → 镜像下标；越界回落标准档
    data.apply_setting(SettingId::Density, &SettingValue::Index(0));
    assert_eq!(data.settings.density_ix, Density::Compact.index());
    data.apply_setting(SettingId::Density, &SettingValue::Index(2));
    assert_eq!(data.settings.density_ix, Density::Loose.index());
    data.apply_setting(SettingId::Density, &SettingValue::Index(9));
    assert_eq!(
        data.settings.density_ix,
        Density::Standard.index(),
        "越界回落标准档"
    );

    // 类型守界：字号行不吃 Index、密度行不吃 Number（跨类型串用整项忽略）
    data.apply_setting(SettingId::FontSize, &SettingValue::Index(3));
    assert_eq!(
        data.settings.font_size,
        hzip_config::FONT_SIZE_MIN,
        "跨类型上行不得改值"
    );
    data.apply_setting(SettingId::Density, &SettingValue::Number(3));
    assert_eq!(
        data.settings.density_ix,
        Density::Standard.index(),
        "跨类型上行不得改值"
    );
}

// 注册表引用的**每个 i18n key** 都必须在两种语言下命中。
//
// 漏网情形：key 拼错（如 `setting.theme.lable`）时 rust-i18n 原样返回 key，
// 界面直接把 `setting.theme.lable` 画出来——不报错、不 panic，只有肉眼能发现。
// 本用例把「代码引用的 key 集合」与「文案表」钉在一起。
#[test]
fn every_referenced_key_resolves_in_both_locales() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    for loc in ["zh-CN", "en"] {
        crate::init_locale(loc);
        let mut keys: Vec<&str> = Vec::new();
        for d in REGISTRY {
            keys.push(d.label);
            if let Some(help) = d.help {
                keys.push(help);
            }
            match d.widget {
                WidgetKind::Select { label_keys } => keys.extend(label_keys.iter().copied()),
                WidgetKind::CheckGroup { keys: whitelist } => {
                    keys.extend(whitelist.iter().map(|(_, label)| *label))
                }
                _ => {}
            }
        }
        keys.extend(ACTIONS.iter().map(|a| a.label));
        for key in keys {
            assert_ne!(
                crate::tr(key),
                key,
                "{loc}：key `{key}` 未命中（文案表缺项或拼写错误）"
            );
        }
    }
    crate::init_locale("zh-CN");
}

// 中文文案**零漂移**金标：设置页全部 label/help + 7 页签的 zh-CN 值
// 逐字节一致。
//
// 基线 = 走查 #8 重写后的文案（用户裁定：帮助要「给人看」，描述性/
// 程序性措辞全部重写；无需帮助的项删 desc 键）。此后再改文案必须
// 连本用例一起改——防止「顺手改文案」静默漂移。
#[test]
fn zh_cn_text_matches_pre_i18n_wording() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    let cases: &[(&str, &str)] = &[
        ("tab.general", "常规"),
        ("tab.assoc", "文件关联"),
        ("tab.context_menu", "上下文菜单"),
        ("tab.extract", "解压设置"),
        ("tab.compress", "压缩设置"),
        ("tab.viewer", "查看器"),
        ("tab.language", "语言"),
        ("setting.theme.label", "主题"),
        ("setting.theme_color.label", "主题色"),
        ("setting.show_dotdot.label", "显示 .. 跳上级"),
        ("setting.auto_update.label", "自动更新"),
        ("setting.log_dir.label", "日志目录"),
        ("setting.assoc.label", "关联格式"),
        ("setting.ctx_extract.label", "解压菜单"),
        ("setting.ctx_compress.label", "压缩菜单"),
        ("setting.extract_smart.label", "智能解压"),
        ("setting.extract_conflict.label", "冲突策略"),
        ("setting.extract_best_effort.label", "尽力而为"),
        ("setting.extract_codepage.label", "文件名编码"),
        ("setting.notify_on_finish.label", "完成通知"),
        ("setting.progress_window.label", "进度窗口"),
        ("setting.exit_on_finish.label", "完成后退出"),
        ("setting.format.label", "默认格式"),
        ("setting.level.label", "默认级别"),
        ("setting.keep_metadata.label", "保留元数据"),
        ("setting.verify_after.label", "完成后校验"),
        ("setting.folder_size.label", "目录行显示大小"),
        ("setting.image_viewer.label", "图像查看器"),
        ("setting.editor.label", "编辑器"),
        ("setting.columns.label", "列顺序与显隐"),
        ("setting.show_tree.label", "显示树型目录"),
        ("setting.show_jobs.label", "显示任务中心"),
        ("setting.toolbar_style.label", "工具栏样式"),
        ("setting.view_style.label", "视图样式"),
        ("setting.lang.label", "界面语言"),
        (
            "setting.lang.help",
            "默认跟随系统语言。若系统语言需要中文、而本机没有能显示中文的字体，界面会自动回退英文（避免整屏豆腐块，原因写进日志）。安装任意中文字体（Linux：fonts-noto-cjk）后重启即可恢复中文。",
        ),
        ("setting.file_icons.label", "文件类型图标"),
        (
            "setting.file_icons.help",
            "文件列表按各文件类型使用系统文件管理器里的图标（彩色，随已安装程序变化）；选「内置」则只用 hzip 的单色图标 —— 此时完全不向系统查询图标。",
        ),
        (
            "setting.theme_color.help",
            "按钮、选中高亮等处的强调色；留空使用主题默认色。",
        ),
        (
            "setting.show_dotdot.help",
            "列表顶部显示一行「..」，双击可返回上一级目录。",
        ),
        (
            "setting.auto_update.help",
            "启动时自动检查新版本（该功能尚未实现，目前仅保存偏好）。",
        ),
        (
            "setting.log_dir.help",
            "日志文件保存的位置；留空使用系统默认，修改后重启生效。",
        ),
        (
            "setting.assoc.help",
            // 2026-09-29：应用关联改为「直接接管默认」（清 UserChoice 层 +
            // 摘掉每用户「打开方式」列表里的竞争者 + 可能的 UAC）——用户裁定，
            // 文案随行为一起改（本表同步更新）
            "勾选后双击这类压缩包即用 hzip 打开。点「应用关联」会注册 hzip 并让它**真正生效**：清掉系统里的「用户显式选择」层（FileExts\\UserChoice），并把这些格式在**每用户「打开方式」列表**里的其它程序摘掉（点「解除关联」会原样还回去）。若某一步需要管理员权限，会弹出 UAC 提权确认。代价：这些格式此前由你在系统 UI 里做过的默认选择会被丢弃且无法还原。",
        ),
        (
            // 2026-10-26：解压组改成对**任意文件**都出现（扩展名不是证据，
            // 按内容识别；不是压缩包就弹错误窗）——文案随行为一起改
            "setting.ctx_extract.help",
            "资源管理器右键里出现哪些解压动作。这些动作对任意文件都出现——hzip 按文件内容识别格式，改了后缀名（如 .img/.bin）的压缩包同样能解；确实不是压缩包时会弹窗报错。",
        ),
        (
            "setting.ctx_compress.help",
            "资源管理器右键「压缩」菜单里出现哪些格式。",
        ),
        (
            "setting.extract_smart.help",
            "压缩包只包一层目录时，解压后直接取出里面的内容，不多套一层文件夹。",
        ),
        (
            "setting.extract_conflict.help",
            "解压遇到同名文件时的处理方式。",
        ),
        (
            "setting.extract_best_effort.help",
            "个别文件解压失败时跳过并继续其余，而不是整单中止。",
        ),
        (
            "setting.extract_codepage.help",
            "文件名乱码的老压缩包可手动指定编码；「自动」为自动检测。",
        ),
        (
            "setting.keep_metadata.help",
            "压缩时保留文件的修改时间等属性。",
        ),
        (
            "setting.folder_size.help",
            "文件夹一行显示其内容的总大小；大档案会稍慢。",
        ),
        (
            "setting.image_viewer.help",
            "双击图片时打开的程序；留空用系统默认。",
        ),
        (
            "setting.editor.help",
            "右键「编辑」时打开的程序；留空用系统默认。",
        ),
        (
            "setting.columns.help",
            "调整文件列表的列顺序与显示；「名称」列固定显示。",
        ),
        ("action.open_log_dir", "打开日志目录"),
        ("action.open_config", "打开配置文件"),
        // —— 快捷键页（T-KEYS）：第 8 页签 + 整页文案 ——
        ("tab.keys", "快捷键"),
        ("setting.keymap.label", "快捷键"),
        (
            "setting.keymap.help",
            "点键位后按下想用的组合键，移开焦点即生效；Esc 取消本次编辑；点 ✕ 删除该动作的快捷键绑定；页头「全部恢复默认」可一键复原全部快捷键。",
        ),
        ("keymap.action.open", "打开档案"),
        ("keymap.action.extract", "解压"),
        ("keymap.action.new_window", "新建窗口"),
        ("keymap.action.select_all", "全选"),
        ("keymap.action.select_invert", "反选"),
        ("keymap.action.copy", "复制"),
        ("keymap.action.paste", "粘贴"),
        ("keymap.action.find", "查找"),
        ("keymap.action.test_integrity", "测试完整性"),
        ("keymap.action.settings", "设置"),
        ("keymap.action.show_jobs", "任务中心"),
        ("keymap.action.refresh", "刷新"),
        ("keymap.action.delete", "删除"),
        ("keymap.action.quit", "退出"),
        ("keymap.state.default", "默认"),
        ("keymap.state.unbound", "未绑定"),
        ("keymap.recording", "请按键…"),
        ("keymap.remove_binding", "删除该快捷键绑定"),
        ("keymap.conflict", "已被「{action}」占用"),
        ("keymap.reset_one", "恢复默认"),
        ("keymap.reset_all", "全部恢复默认"),
    ];
    for (key, want) in cases {
        assert_eq!(
            crate::tr(key),
            *want,
            "{key} 中文文案漂移（i18n 改造不得改文案）"
        );
    }
}

// REGISTRY 下标互锁：`index(id)` ↔ 表顺序一一对应、无重复、页覆盖完整
#[test]
fn registry_index_is_unique_and_complete() {
    assert_eq!(
        REGISTRY.len(),
        35,
        "设置项总数（35 = 34 + 快捷键整节 + 写回节奏）+ 动作按钮不进表"
    );
    for (ix, d) in REGISTRY.iter().enumerate() {
        assert_eq!(index(d.id), ix, "index({:?}) 与表顺序不一致", d.id);
    }
    let mut ids: Vec<SettingId> = REGISTRY.iter().map(|d| d.id).collect();
    let before = ids.len();
    ids.sort_by_key(|id| format!("{id:?}"));
    ids.dedup();
    assert_eq!(ids.len(), before, "SettingId 变体不得重复登记");
    for tab in SettingsTab::ALL {
        assert!(
            REGISTRY.iter().any(|d| d.page == tab),
            "分类页 {} 无任何设置项",
            tab.label()
        );
    }
}

// 控件类型 ↔ 取值类型一致（`apply_setting` 的类型守卫依赖）
#[test]
fn widget_accepts_matches_declared_value() {
    for d in REGISTRY {
        let v = (d.load)(&app(), &Config::defaults());
        assert!(d.widget.accepts(&v), "{:?} 活读值与控件类型不符", d.id);
        assert!(
            d.widget.accepts(&(d.default)()),
            "{:?} 默认值类型不符",
            d.id
        );
    }
    assert!(!WidgetKind::Switch.accepts(&SettingValue::Index(1)));
    assert!(!WidgetKind::Select { label_keys: &[] }.accepts(&SettingValue::Bool(true)));
    // 菜单入口上行 Text（config 落盘值）——Select 必须放行（T-TVS 走查
    // 缺陷回归：accepts 比 store 的双归一更严，Text 被静默丢弃 → 菜单
    // 「视图 ▸ 工具栏/视图样式」点击无效果）
    assert!(
        WidgetKind::Select { label_keys: &[] }.accepts(&SettingValue::Text("icon_only".into()))
    );
}

/// Select 项 → 其白名单 label 序列（测试辅助；与被测项一一对应）。
fn expected_labels(id: SettingId) -> Vec<&'static str> {
    match id {
        SettingId::Theme => labels::label_keys(labels::THEME).to_vec(),
        SettingId::Lang => labels::label_keys(labels::LANG).to_vec(),
        SettingId::Format => labels::label_keys(labels::UI_FORMAT).to_vec(),
        SettingId::Level => labels::label_keys(labels::LEVEL).to_vec(),
        SettingId::ExtractConflict => labels::label_keys(labels::CONFLICT).to_vec(),
        SettingId::ExtractCodepage => labels::label_keys(labels::CODEPAGE).to_vec(),
        SettingId::FileTypeIcons => labels::label_keys(labels::FILE_ICONS).to_vec(),
        other => panic!("{other:?} 不是 Select 项"),
    }
}

// Select 项：label 序列逐位对齐白名单（下标即 value 下标——面板显示与落盘同源）
#[test]
fn select_labels_align_with_whitelist() {
    for id in [
        SettingId::Theme,
        SettingId::Lang,
        SettingId::Format,
        SettingId::Level,
        SettingId::ExtractConflict,
        SettingId::ExtractCodepage,
        SettingId::FileTypeIcons,
    ] {
        let WidgetKind::Select { label_keys: ls } = def(id).widget else {
            panic!("{id:?} 应为 Select 控件");
        };
        assert_eq!(
            ls.to_vec(),
            expected_labels(id),
            "{id:?} label key 序列漂移"
        );
    }
    // 冲突策略 value 与 CLI/docs 白名单同值域（注册表存的就是这批值）
    assert_eq!(
        labels::values(labels::CONFLICT),
        ["ask", "overwrite", "skip", "rename", "newer"]
    );
    // 代码页 0 项 = 自动（store 特判 None）
    assert_eq!(labels::value_of(&labels::CODEPAGE, 0), "auto");
}

// 出厂默认 = 下层默认（hzip-config B.1 + SettingsPrefs::default；防漂移）
#[test]
fn defaults_match_lower_layer_defaults() {
    let mut cfg = hzip_config::Config::defaults();
    // 先把全部设置项改成非默认值（证明 apply_defaults 真在重置）
    cfg.ui.theme = "dark".into();
    cfg.ui.lang = "en".into();
    cfg.ui.default_format = "7z".into();
    cfg.ui.default_level = "ultra".into();
    cfg.ui.show_dotdot = false;
    cfg.ui.auto_update = true;
    cfg.ui.assoc_formats = vec!["zip".into()];
    cfg.ui.ctx_extract = vec!["extract_here".into()];
    cfg.ui.ctx_compress = vec!["add_zip".into()];
    cfg.ui.show_folder_size = true;
    cfg.ui.image_viewer = "C:\\imv.exe".into();
    cfg.ui.editor = "C:\\ed.exe".into();
    cfg.defaults.format = "7z".into();
    cfg.defaults.level = "ultra".into();
    cfg.extract.smart = true;
    cfg.extract.conflict = "newer".into();
    cfg.extract.best_effort = true;
    cfg.extract.codepage = Some("gbk".into());
    cfg.compress.keep_metadata = false;
    cfg.compress.verify_after = false;
    cfg.jobs.notify_on_finish = true;
    cfg.jobs.progress_window = false;
    cfg.jobs.exit_on_finish = true;
    cfg.ui.font_family = "Consolas".into();
    cfg.ui.font_size = 22;
    cfg.ui.density = "loose".into();
    cfg.ui.file_type_icons = "builtin".into();
    // 快捷键（T-KEYS）：一个自定义键位 + 一个「禁用」——两者都必须被重置清掉
    cfg.keys.select_all = "f2".into();
    cfg.keys.quit = "none".into();
    // 非设置项：恢复默认必须原值保留
    cfg.extract.delete_after = true;
    cfg.compress.solid_block_mib = 64;

    let mut data = app();
    apply_defaults(&mut cfg, &mut data);

    let want = hzip_config::Config::defaults();
    assert_eq!(cfg.ui.theme, want.ui.theme);
    assert_eq!(cfg.ui.lang, want.ui.lang);
    assert_eq!(cfg.ui.default_format, want.ui.default_format);
    assert_eq!(cfg.ui.default_level, want.ui.default_level);
    assert_eq!(cfg.ui.show_dotdot, want.ui.show_dotdot);
    assert_eq!(cfg.ui.auto_update, want.ui.auto_update);
    assert_eq!(cfg.ui.assoc_formats, want.ui.assoc_formats);
    assert_eq!(cfg.ui.ctx_extract, want.ui.ctx_extract);
    assert_eq!(cfg.ui.ctx_compress, want.ui.ctx_compress);
    assert_eq!(cfg.ui.show_folder_size, want.ui.show_folder_size);
    assert_eq!(cfg.ui.image_viewer, want.ui.image_viewer);
    assert_eq!(cfg.ui.editor, want.ui.editor);
    assert_eq!(cfg.ui.columns, want.ui.columns);
    assert_eq!(cfg.defaults.format, want.defaults.format);
    assert_eq!(cfg.defaults.level, want.defaults.level);
    assert_eq!(cfg.extract.smart, want.extract.smart);
    assert_eq!(cfg.extract.conflict, want.extract.conflict);
    assert_eq!(cfg.extract.best_effort, want.extract.best_effort);
    assert_eq!(cfg.extract.codepage, want.extract.codepage);
    assert_eq!(cfg.compress.keep_metadata, want.compress.keep_metadata);
    assert_eq!(cfg.compress.verify_after, want.compress.verify_after);
    assert_eq!(cfg.jobs.notify_on_finish, want.jobs.notify_on_finish);
    assert_eq!(cfg.jobs.progress_window, want.jobs.progress_window);
    assert_eq!(cfg.jobs.exit_on_finish, want.jobs.exit_on_finish);
    assert_eq!(cfg.ui.font_family, want.ui.font_family);
    assert_eq!(cfg.ui.font_size, want.ui.font_size);
    assert_eq!(cfg.ui.density, want.ui.density);
    assert_eq!(cfg.ui.file_type_icons, want.ui.file_type_icons);
    assert_eq!(
        cfg.keys, want.keys,
        "恢复默认必须把 [keys] 清成全空（= 走内置默认键位）"
    );
    assert_eq!(
        cfg.keys.select_all, "",
        "自定义键位串必须被清掉（空串 = 默认态）"
    );
    assert_eq!(cfg.keys.quit, "", "「禁用」哨兵同样必须被清掉");
    assert_eq!(data.settings, SettingsPrefs::default());
    // 非设置项原值保留
    assert!(cfg.extract.delete_after, "delete_after 非设置项须保留");
    assert_eq!(cfg.compress.solid_block_mib, 64, "solid_block_mib 须保留");
}

// store：越界下标/未知集合值回落（normalize 语义）
#[test]
fn store_normalizes_out_of_range_values() {
    let mut cfg = Config::defaults();
    let mut data = app();
    (def(SettingId::Theme).store)(&mut cfg, &mut data, &SettingValue::Index(99));
    assert_eq!(cfg.ui.theme, "system", "越界下标回落 0 项");
    assert_eq!(data.settings.theme_ix, 0);
    (def(SettingId::ExtractConflict).store)(&mut cfg, &mut data, &SettingValue::Index(9));
    assert_eq!(cfg.extract.conflict, "ask");
    (def(SettingId::ExtractCodepage).store)(&mut cfg, &mut data, &SettingValue::Index(2));
    assert_eq!(cfg.extract.codepage.as_deref(), Some("big5"));
    (def(SettingId::ExtractCodepage).store)(&mut cfg, &mut data, &SettingValue::Index(0));
    assert_eq!(cfg.extract.codepage, None, "0 = 自动 = None");
    (def(SettingId::CtxExtract).store)(
        &mut cfg,
        &mut data,
        &SettingValue::StrSet(vec!["bogus".into(), "extract_to".into()]),
    );
    assert_eq!(
        cfg.ui.ctx_extract,
        vec!["extract_to".to_string()],
        "非白名单 key 丢弃"
    );
    (def(SettingId::Assoc).store)(
        &mut cfg,
        &mut data,
        &SettingValue::StrSet(vec!["zip".into(), "zip".into(), String::new()]),
    );
    assert_eq!(cfg.ui.assoc_formats, vec!["zip".to_string()], "去空去重");
}

// store：格式/级别同写 [defaults] 与 [ui] 镜像（旧代码只写一处 → 面板回弹根因）
#[test]
fn store_format_and_level_keep_both_keys_in_sync() {
    let mut cfg = Config::defaults();
    let mut data = app();
    (def(SettingId::Format).store)(&mut cfg, &mut data, &SettingValue::Index(1));
    assert_eq!(cfg.defaults.format, "7z");
    assert_eq!(cfg.ui.default_format, "7z");
    (def(SettingId::Level).store)(&mut cfg, &mut data, &SettingValue::Index(5));
    assert_eq!(cfg.defaults.level, "ultra");
    assert_eq!(cfg.ui.default_level, "ultra");
    assert_eq!(data.settings.format_ix, 1);
    assert_eq!(data.settings.level_ix, 5);
}

// load：Config 两半 → 活读值（含旧配置大小写宽容 + 未知值回落）
#[test]
fn load_reads_config_and_falls_back() {
    let mut cfg = Config::defaults();
    cfg.extract.conflict = "RENAME".into();
    cfg.extract.codepage = Some("GBK".into());
    cfg.jobs.exit_on_finish = true;
    let data = app();
    let view = SettingsView::load(&data, &cfg, vec!["zip".into()], None, Vec::new());
    assert_eq!(
        view.index_of(SettingId::ExtractConflict),
        3,
        "大小写不敏感匹配"
    );
    assert_eq!(view.index_of(SettingId::ExtractCodepage), 1);
    assert!(view.bool_of(SettingId::ExitOnFinish));
    assert_eq!(view.assoc_candidates(), ["zip".to_string()]);
    // 未知值回落 0
    cfg.extract.conflict = "bogus".into();
    cfg.extract.codepage = Some("klingon".into());
    let view = SettingsView::load(&data, &cfg, Vec::new(), None, Vec::new());
    assert_eq!(view.index_of(SettingId::ExtractConflict), 0);
    assert_eq!(view.index_of(SettingId::ExtractCodepage), 0);
}

// 活读视图：每项一值、缺项守界
#[test]
fn view_defaults_cover_every_registry_row() {
    let view = SettingsView::defaults(Vec::new());
    for d in REGISTRY {
        assert!(view.value(d.id).is_some(), "{:?} 缺活读值", d.id);
    }
    assert!(view.bool_of(SettingId::ProgressWindow), "开窗默认开");
    assert!(!view.bool_of(SettingId::ExitOnFinish), "完成退出默认关");
    assert_eq!(view.index_of(SettingId::Level), 3, "默认级别 = 标准");
    assert_eq!(view.text_of(SettingId::Editor), "", "空 = 系统默认");
    assert!(view.set_of(SettingId::CtxExtract).is_empty());
    assert!(view.columns_of(SettingId::Columns).is_some());
}

// 勾选切换纯函数：整组上行 = 白名单顺序全量集（幂等）
#[test]
fn toggle_set_outputs_full_whitelist_order() {
    let keys = CTX_EXTRACT_KEYS.as_slice();
    assert_eq!(
        toggle_set(keys, &[], "extract_to", true),
        vec!["extract_to".to_string()]
    );
    assert_eq!(
        toggle_set(keys, &["extract_to".into()], "extract_here", true),
        vec!["extract_here".to_string(), "extract_to".to_string()],
        "输出按白名单顺序（非追加顺序）"
    );
    assert_eq!(
        toggle_set(keys, &["extract_to".into()], "extract_to", true),
        vec!["extract_to".to_string()],
        "重复勾选幂等"
    );
    assert!(toggle_set(keys, &["extract_to".into()], "extract_to", false).is_empty());
    assert_eq!(
        toggle_set(keys, &["bogus".into()], "auto_extract", true),
        vec!["auto_extract".to_string()],
        "非白名单 key 丢弃"
    );
}

// 关联勾选纯函数
#[test]
fn assoc_toggle_adds_and_removes() {
    let set = assoc_toggle(&[], "zip", true);
    assert_eq!(set, vec!["zip".to_string()]);
    let set = assoc_toggle(&set, "zip", true);
    assert_eq!(set, vec!["zip".to_string()], "重复勾选幂等");
    let set = assoc_toggle(&set, "7z", true);
    assert_eq!(set, vec!["zip".to_string(), "7z".to_string()]);
    let set = assoc_toggle(&set, "zip", false);
    assert_eq!(set, vec!["7z".to_string()]);
}

// 元素 id 唯一（gpui 跨帧状态复用键；重复 = 勾选态串台）
#[test]
fn element_keys_are_unique() {
    let mut ids: Vec<&'static str> = REGISTRY.iter().map(|d| d.id.key()).collect();
    ids.extend(ACTIONS.iter().map(|a| a.key));
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "元素 id 重复");
}

// 动作按钮 ↔ 目标项接线（FilePicker/Clear 只能落在 Text 行——渲染器行内挂载依赖）
#[test]
fn actions_target_text_rows() {
    for a in ACTIONS {
        match &a.kind {
            ActionKind::Emit(_) => {}
            // 只读展示动作（打开日志目录/配置文件）：不改值、不落盘
            ActionKind::Open(_) => {}
            ActionKind::Clear(target) | ActionKind::FilePicker { target } => {
                assert!(
                    matches!(def(*target).widget, WidgetKind::Text),
                    "动作 {} 目标 {:?} 非 Text 行",
                    a.key,
                    target
                );
            }
        }
    }
    // Text 行必须有「清空」动作（否则空路径无法复位）
    for d in REGISTRY
        .iter()
        .filter(|d| matches!(d.widget, WidgetKind::Text))
    {
        assert!(
            ACTIONS
                .iter()
                .any(|a| matches!(a.kind, ActionKind::Clear(t) if t == d.id)),
            "{:?} 缺清空动作",
            d.id
        );
    }
}

// ColumnsConfig ↔ config POD round-trip（T-G54 验收 #2；R-GUI4 #2 含类型列）
#[test]
fn columns_roundtrip_through_pod() {
    let config = ColumnsConfig {
        hidden: vec![
            crate::ui::contracts::BrowseColumnId::Encoding,
            crate::ui::contracts::BrowseColumnId::Mtime,
            // 新四列也在往返集合里（2026-09-25；备注列 2026-09-26 下线）
            crate::ui::contracts::BrowseColumnId::Level,
            crate::ui::contracts::BrowseColumnId::Encryption,
            crate::ui::contracts::BrowseColumnId::Crc,
            crate::ui::contracts::BrowseColumnId::Attrs,
        ],
        order: crate::ui::contracts::BrowseColumnId::ALL.to_vec(),
    };
    let pod = columns_to_pod(&config);
    assert_eq!(columns_from_pod(&pod), config);
}

// 未知列名忽略 + 缺失列补全 + name 锁定（向后兼容 / 防漂移）
//
// 补全列（老配置里不存在的）默认隐藏——2026-09-25 五列「默认不显示」的
// 用户口径不能被旧配置文件绕过。
#[test]
fn columns_from_pod_tolerates_unknown_and_missing() {
    use crate::ui::contracts::BrowseColumnId as C;
    let pod = hzip_config::Columns {
        order: vec!["mtime".into(), "future-col".into()],
        hidden: vec!["name".into(), "size".into(), "bogus".into()],
    };
    let cfg = columns_from_pod(&pod);
    assert_eq!(cfg.order.first(), Some(&C::Mtime));
    assert_eq!(cfg.order.len(), C::ALL.len());
    assert!(!cfg.hidden.contains(&C::Name), "name 强制显示");
    assert!(cfg.hidden.contains(&C::Size));
    for col in C::DEFAULT_HIDDEN {
        assert!(cfg.hidden.contains(&col), "补全列默认隐藏：{:?}", col.key());
    }
    // 老六列显式出现（旧配置升级后可见列不缩水）
    for col in [C::Name, C::Type, C::Size, C::Packed, C::Encoding, C::Mtime] {
        assert!(
            !cfg.hidden.contains(&col) || col == C::Size,
            "老列保持原显隐（Size 由 pod.hidden 显式隐藏）：{:?}",
            col.key()
        );
    }
}

// 出厂默认：四新列全部隐藏（用户口径「默认不显示」；备注列已下线）
#[test]
fn default_columns_hide_new_four() {
    use crate::ui::contracts::BrowseColumnId as C;
    let cfg = ColumnsConfig::default();
    assert_eq!(cfg.hidden, C::DEFAULT_HIDDEN.to_vec());
    let vis = cfg.visible();
    assert!(!vis.contains(&C::Level) && !vis.contains(&C::Attrs));
    assert!(vis.contains(&C::Name) && vis.contains(&C::Size));
}

// 工具栏「查看」下拉勾选：一次切换隐藏、再一次切换复原（幂等）
#[test]
fn toggle_column_hides_then_shows() {
    use crate::ui::contracts::BrowseColumnId as C;
    let base = ColumnsConfig::default();

    let hidden_cfg = toggle_column(&base, "size");
    assert!(hidden_cfg.hidden.contains(&C::Size), "一次切换应隐藏 size");
    let vis = hidden_cfg.visible();
    assert!(!vis.contains(&C::Size), "隐藏后 visible() 不含 size");
    assert!(vis.contains(&C::Name), "名称列始终在渲染序列里");

    let back = toggle_column(&hidden_cfg, "size");
    assert_eq!(
        back.visible(),
        base.visible(),
        "对同一 key 再切一次应回到原 visible() 序列"
    );
}

// 名称列不可隐藏（否则配置里会留下「隐藏但不生效」的假状态）；
// 未知 key（手改配置 / 版本漂移）不得改写配置。
#[test]
fn toggle_column_refuses_name_and_unknown() {
    let base = ColumnsConfig::default();
    assert_eq!(toggle_column(&base, "name"), base, "名称列原样返回");
    assert_eq!(toggle_column(&base, "nope"), base, "未知 key 原样返回");
}

// 显隐与列序正交：切换只动 hidden，order 逐项不变
#[test]
fn toggle_column_keeps_order() {
    let base = ColumnsConfig::default();
    let toggled = toggle_column(&base, "mtime");
    assert_eq!(toggled.order, base.order, "列序归拖拽/设置页顺序段");
}

// 文件类型图标来源（T-ICON 决策 #1）：`labels::FILE_ICONS` 与注册项互锁
// （Select 的 label 序列 = 白名单 label 列，下标 = value 下标），默认下标 = 0
// = `system`（与系统文件管理器逐类型一致）。
#[test]
fn file_type_icons_matches_label_whitelist_and_defaults_to_system() {
    let WidgetKind::Select { label_keys } = def(SettingId::FileTypeIcons).widget else {
        panic!("FileTypeIcons 应为 Select 控件");
    };
    assert_eq!(
        label_keys.to_vec(),
        labels::label_keys(labels::FILE_ICONS).to_vec(),
        "label key 序列漂移"
    );
    // 白名单下标 0 = system（默认 / 回落项），1 = builtin
    assert_eq!(labels::value_of(&labels::FILE_ICONS, 0), "system");
    assert_eq!(labels::value_of(&labels::FILE_ICONS, 1), "builtin");
    assert_eq!(labels::index_of_value(&labels::FILE_ICONS, "builtin"), 1);
    // 出厂默认 = 下标 0；且与 hzip-config 的下层默认同值（防两处漂移）
    assert_eq!(
        (def(SettingId::FileTypeIcons).default)(),
        SettingValue::Index(0)
    );
    assert_eq!(
        hzip_config::Config::defaults().ui.file_type_icons,
        labels::value_of(&labels::FILE_ICONS, 0),
        "config 默认值必须等于白名单 0 项"
    );
    // 生效半场声明：进程级图标缓存/worker 须由泵消费该 id
    assert_eq!(def(SettingId::FileTypeIcons).effect, Effect::FileIconSource);
}

// `store` 归一（T-ICON 决策 #1）：大小写不敏感、未知/空/越界 ⇒ `system`；
// 落盘值写 `[ui].file_type_icons`，镜像写 `AppData.settings`（泵取值来源）。
#[test]
fn store_file_type_icons_normalizes_case_and_unknown() {
    let mut cfg = Config::defaults();
    let mut data = app();
    let store = def(SettingId::FileTypeIcons).store;

    // 面板下拉：Index(1) = builtin
    store(&mut cfg, &mut data, &SettingValue::Index(1));
    assert_eq!(cfg.ui.file_type_icons, "builtin");
    assert_eq!(data.settings.file_type_icons_ix, 1);

    // 菜单/文本入口：大小写不敏感
    store(&mut cfg, &mut data, &SettingValue::Text("SYSTEM".into()));
    assert_eq!(cfg.ui.file_type_icons, "system");
    assert_eq!(data.settings.file_type_icons_ix, 0);

    // 未知值 / 空值 / 越界下标 ⇒ system（下标 0）
    for bad in [
        SettingValue::Text("nonsense".into()),
        SettingValue::Text(String::new()),
        SettingValue::Index(9),
    ] {
        store(&mut cfg, &mut data, &SettingValue::Index(1)); // 先脏，证明下面确实改了
        store(&mut cfg, &mut data, &bad);
        assert_eq!(cfg.ui.file_type_icons, "system", "{bad:?} 应回落 system");
        assert_eq!(data.settings.file_type_icons_ix, 0);
    }

    // 跨类型上行整项忽略（Bool 不是本控件取值）：值保持上一次
    store(&mut cfg, &mut data, &SettingValue::Index(1));
    store(&mut cfg, &mut data, &SettingValue::Bool(false));
    assert_eq!(cfg.ui.file_type_icons, "builtin", "跨类型上行不得改值");
    assert_eq!(data.settings.file_type_icons_ix, 1);
}

// —— T-KEYS：快捷键整节（页签「快捷键」+ 一项顶一节）——

/// 造一份「非默认」的 `Keys`：自定义键位 + `"none"` 禁用 + 空串默认三态齐备。
fn keys_sample() -> hzip_config::Keys {
    let mut k = hzip_config::Keys::default();
    *k.field_mut("select_all").expect("select_all 是契约键名") = "f2".into();
    *k.field_mut("quit").expect("quit 是契约键名") = "none".into();
    k
}

// 页签集合 ↔ 下标互锁（T-KEYS：7 → 8；越界守界语义**不得**变）
#[test]
fn settings_tab_collection_interlocks_with_index() {
    assert_eq!(SettingsTab::ALL.len(), 8, "新增「快捷键」页后共 8 页");
    assert_eq!(
        SettingsTab::LABEL_KEYS.len(),
        SettingsTab::ALL.len(),
        "文案 key 序列与页集合必须等长（渲染按下标取）"
    );
    for (ix, tab) in SettingsTab::ALL.iter().enumerate() {
        assert_eq!(tab.index(), ix, "index() 与 ALL 顺序漂移：{tab:?}");
        assert_eq!(SettingsTab::from_index(ix), *tab);
    }
    // 既有 7 页下标一个都不能动（E 侧渲染/测试按绝对下标断言）
    assert_eq!(SettingsTab::from_index(6), SettingsTab::Language);
    assert_eq!(SettingsTab::Keys.index(), 7);
    // 越界回落仍钉在语言页（既有守界语义；不是「最后一个变体」）
    assert_eq!(
        SettingsTab::from_index(8),
        SettingsTab::Language,
        "越界守界"
    );
    assert_eq!(SettingsTab::from_index(9), SettingsTab::Language);
    assert_eq!(SettingsTab::from_index(usize::MAX), SettingsTab::Language);
    // 快捷键页必须有登记项（否则页签是空页——`registry_index_is_unique_and_complete`
    // 的页覆盖断言会先炸，这里把根因也钉住）
    assert_eq!(def(SettingId::Keymap).page, SettingsTab::Keys);
    // 页签文案 key 一一对应（顺序 = ALL）
    assert_eq!(
        SettingsTab::LABEL_KEYS[SettingsTab::Keys.index()],
        "tab.keys"
    );
}

// 取值类型守界：`Keys` 只被 `KeysBlock` 吃；其它控件不吃 `Keys`，`KeysBlock` 也不吃别的
#[test]
fn keys_block_accepts_only_keys_value() {
    let sample = keys_sample();
    assert!(WidgetKind::KeysBlock.accepts(&SettingValue::Keys(Box::new(sample.clone()))));
    assert!(
        def(SettingId::Keymap)
            .widget
            .accepts(&SettingValue::Keys(Box::new(sample.clone()))),
        "注册表行的控件必须吃 Keys 值（否则 apply_setting 静默忽略）"
    );
    // 其余控件类型一律不吃（跨类型串用整项忽略，不 panic）
    for w in [
        WidgetKind::Switch,
        WidgetKind::Text,
        WidgetKind::Color,
        WidgetKind::ExtGroup,
        WidgetKind::ColumnsBlock,
        WidgetKind::FontSelect,
        WidgetKind::Number {
            min: 0,
            max: 9,
            step: 1,
        },
    ] {
        assert!(
            !w.accepts(&SettingValue::Keys(Box::new(sample.clone()))),
            "{w:?} 不应接受 Keys 值"
        );
    }
    // `KeysBlock` 只吃 Keys（Columns/Bool/Text/StrSet/Index/Number 全部拒绝）
    for v in [
        SettingValue::Bool(true),
        SettingValue::Index(1),
        SettingValue::Text("f2".into()),
        SettingValue::StrSet(vec!["zip".into()]),
        SettingValue::Columns(ColumnsConfig::default()),
        SettingValue::Number(3),
    ] {
        assert!(
            !WidgetKind::KeysBlock.accepts(&v),
            "KeysBlock 不应接受 {v:?}"
        );
    }
    // 取值访问器：非 Keys 变体回 None
    assert!(SettingValue::Bool(true).as_keys().is_none());
    assert_eq!(
        SettingValue::Keys(Box::new(sample.clone())).as_keys(),
        Some(&sample)
    );
}

// `store` 双写 + `load` 活读镜像（T-KEYS §3.3：`cfg.keys` 与 `app.settings.keys` 同源）
#[test]
fn store_keys_dual_writes_config_and_mirror() {
    let mut cfg = Config::defaults();
    let mut data = app();
    let store = def(SettingId::Keymap).store;

    let sample = keys_sample();
    store(
        &mut cfg,
        &mut data,
        &SettingValue::Keys(Box::new(sample.clone())),
    );
    assert_eq!(cfg.keys, sample, "配置侧 [keys] 整节落库");
    assert_eq!(data.settings.keys, sample, "AppData 镜像同步（泵取值来源）");

    // 跨类型上行整项忽略（不 panic、不改值）：Bool 不是本控件取值
    store(&mut cfg, &mut data, &SettingValue::Bool(false));
    assert_eq!(cfg.keys, sample, "跨类型上行不得改值");
    assert_eq!(data.settings.keys, sample);

    // 活读走镜像（不读盘）：改了镜像即改活读值——落盘是异步的，读盘会拿到上一拍
    if let Some(f) = data.settings.keys.field_mut("select_all") {
        *f = "ctrl-shift-a".into();
    }
    let view = SettingsView::load(&data, &cfg, Vec::new(), None, Vec::new());
    assert_eq!(
        view.keys_of(SettingId::Keymap)
            .and_then(|k| k.field("select_all")),
        Some("ctrl-shift-a"),
        "活读取镜像而不是读盘"
    );
    assert_eq!(
        cfg.keys.field("select_all"),
        Some("f2"),
        "活读不回写配置（两半各自持有；下一次 store 才同步）"
    );
    // 缺项/非 Keys 项守界
    assert!(view.keys_of(SettingId::Theme).is_none());
    assert_eq!(
        SettingsView::defaults(Vec::new())
            .keys_of(SettingId::Keymap)
            .cloned(),
        Some(hzip_config::Keys::default()),
        "全默认视图 = 全空串（= 全部动作走内置默认键位）"
    );

    // 出厂默认 = 全空 + 生效半场声明 = DataOnly（UI 半场按 id 挂钩，不新增 Effect 变体）
    assert_eq!(
        (def(SettingId::Keymap).default)(),
        SettingValue::Keys(Box::default())
    );
    assert_eq!(def(SettingId::Keymap).effect, Effect::DataOnly);
    assert_eq!(def(SettingId::Keymap).commit, Commit::Immediate);
    assert_eq!(def(SettingId::Keymap).label, "setting.keymap.label");
    assert_eq!(def(SettingId::Keymap).help, Some("setting.keymap.help"));
}

// `apply_setting` 全链路（事件 → store → 镜像 → 信号）：快捷键整节一次上行即生效面就绪
#[test]
fn apply_setting_keymap_updates_mirror_and_emits_signal() {
    let (tx, rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::unbounded();
    let mut data = app();
    data.signal_tx = Some(std::sync::Arc::new(tx));

    let sample = keys_sample();
    data.apply_setting(
        SettingId::Keymap,
        &SettingValue::Keys(Box::new(sample.clone())),
    );
    assert_eq!(data.settings.keys, sample, "镜像落地");
    match rx.try_recv() {
        Ok(crate::viewmodel::VmSignal::SettingApplied(id)) => {
            assert_eq!(
                id,
                SettingId::Keymap,
                "应发出 Keymap 生效信号（泵据它重装绑定）"
            )
        }
        other => panic!("期望 SettingApplied(Keymap)，实得 {other:?}"),
    }

    // 类型不符 → 整项忽略（不 panic、不改状态、不发信号）
    data.apply_setting(SettingId::Keymap, &SettingValue::Bool(true));
    assert_eq!(data.settings.keys, sample, "跨类型上行不得改值");
    assert!(
        rx.try_recv().is_err(),
        "被忽略的上行不得发信号（否则泵会白重装一次 keymap）"
    );
}
