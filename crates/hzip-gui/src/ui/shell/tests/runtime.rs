use super::*;
use crate::viewmodel::{SettingId, VmSignal};

/// 模式划分：动作动词且无 `--open` → 直接模式（不弹主界面）。
///
/// 守的是「右键点了却弹出主界面」这类回归：模式判错的直接后果就是用户
/// 被塞一个他不要的窗口。
#[test]
fn launch_mode_splits_main_and_direct() {
    let run = |argv: &[&str]| {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        match LaunchArgs::parse(&argv) {
            ArgvParse::Run(a) => launch_mode(&a),
            _ => panic!("应解析为启动参数"),
        }
    };
    // 无参 / 只有 --open → 主界面
    assert_eq!(run(&[]), UiMode::Main);
    assert_eq!(run(&["--open", "C:/a.zip"]), UiMode::Main);
    // 动作动词 → 直接模式（全部六种入口逐一覆盖）
    assert_eq!(run(&["--extract-here", "a.zip"]), UiMode::Direct);
    assert_eq!(run(&["--auto-extract", "a.zip"]), UiMode::Direct);
    assert_eq!(run(&["--extract-dir", "a.zip"]), UiMode::Direct);
    assert_eq!(run(&["--extract-to", "a.zip"]), UiMode::Direct);
    assert_eq!(run(&["--extract-options", "a.zip"]), UiMode::Direct);
    assert_eq!(run(&["--compress", "d"]), UiMode::Direct);
    assert_eq!(run(&["--compress-to", "d"]), UiMode::Direct);
    // `--open` 优先：显式要看档案时主界面是唯一合理解释
    assert_eq!(
        run(&["--open", "a.zip", "--auto-extract", "b.zip"]),
        UiMode::Main
    );
}

/// `--extract-options` 是**批次**动词：一次进程收下全部选中档案，参数窗
/// 只弹一次、参数共用（`%SELS`）。单值解析会让第 2 个路径掉进「裸路径 =
/// --open」，变成弹出主界面。
#[test]
fn extract_options_is_batched_and_keeps_verb() {
    let argv: Vec<String> = ["--extract-options", "C:/a.zip", "C:/b.7z"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let ArgvParse::Run(a) = LaunchArgs::parse(&argv) else {
        panic!("应解析为启动参数");
    };
    assert_eq!(a.extract_options, vec!["C:/a.zip", "C:/b.7z"]);
    assert!(a.open.is_empty(), "第二个路径不得被当成 --open");
    let intents = args_intents(&a);
    match intents.as_slice() {
        [LaunchIntent::ExtractOptions { archives }] => assert_eq!(archives.len(), 2),
        other => panic!("应为单条解压参数窗意图: {other:?}"),
    }
}

// argv 解析是**转发的还原器**：队列里存原始 argv，由同一个解析器还原。
// 守两件事：① 动词不丢（丢了就退化成「打开」）；② `--compress` 是多值
// （右键 `%SELS` 把全部选中项塞进一条命令行，单值解析会把第 2 个路径当成
// 「裸路径 = --open」）。
#[test]
fn parse_preserves_verbs_and_multi_path_compress() {
    let argv: Vec<String> = [
        "--auto-extract",
        "C:/a.zip",
        "--compress",
        "C:/x",
        "C:/y",
        "--format",
        "7z",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let ArgvParse::Run(a) = LaunchArgs::parse(&argv) else {
        panic!("应解析为启动参数");
    };
    assert_eq!(a.auto_extract, vec!["C:/a.zip"]);
    assert_eq!(a.compress, vec!["C:/x", "C:/y"], "多值动词须吸收全部路径");
    assert!(a.open.is_empty(), "第二个路径不得被当成 --open");
    assert_eq!(a.format.as_deref(), Some("7z"));

    // 意图：压缩整批合成一条（拆成多条 = 每个输入各打一个包）
    let intents = args_intents(&a);
    assert_eq!(intents.len(), 2);
    assert!(matches!(intents[0], LaunchIntent::AutoExtract { .. }));
    match &intents[1] {
        LaunchIntent::Compress {
            paths,
            choose_output,
            format,
        } => {
            assert_eq!(paths.len(), 2);
            assert!(!choose_output, "--compress 不弹输出选择");
            assert_eq!(format.as_deref(), Some("7z"));
        }
        other => panic!("第二条应为压缩意图: {other:?}"),
    }
}

// 「添加到压缩文件…」走 --compress-to：同一批输入 + 交给用户选输出
#[test]
fn compress_to_asks_for_output_path() {
    let argv: Vec<String> = ["--compress-to", "C:/p", "C:/q"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let ArgvParse::Run(a) = LaunchArgs::parse(&argv) else {
        panic!("应解析为启动参数");
    };
    let intents = args_intents(&a);
    match intents.as_slice() {
        [
            LaunchIntent::Compress {
                paths,
                choose_output: true,
                ..
            },
        ] => assert_eq!(paths.len(), 2),
        other => panic!("应为单条「选输出」压缩意图: {other:?}"),
    }
}

// 帮助/版本不启动 UI；裸路径 = --open（资源管理器双击 %1 的形态）
#[test]
fn parse_help_version_and_bare_path() {
    assert!(matches!(
        LaunchArgs::parse(&["--help".into()]),
        ArgvParse::Help
    ));
    assert!(matches!(
        LaunchArgs::parse(&["-V".into()]),
        ArgvParse::Version
    ));
    let ArgvParse::Run(a) = LaunchArgs::parse(&["C:/a.zip".into()]) else {
        panic!("裸路径应可启动");
    };
    assert_eq!(a.open, vec!["C:/a.zip"]);
    // 转发的 argv 若含 help/version → 无意图（二实例永不转发它们）
    assert!(intents_of_argv(&["--help".into()]).is_empty());
}

// 批内请求判定：按 id 精确匹配（泵据此决定是否做 UI 半场动作）。
//
// 守住「别的设置项的信号不得触发本项动作」——历史上主题/语言都靠这条
// 判定分流，误配会让「改语言顺带重置主题」或漏配变成「改完没反应」。
#[test]
fn batch_requested_matches_only_the_named_setting() {
    let batch = vec![
        VmSignal::SettingApplied(SettingId::Lang),
        VmSignal::SettingApplied(SettingId::Columns),
    ];
    assert!(batch_requested(&batch, SettingId::Lang));
    assert!(batch_requested(&batch, SettingId::Columns));
    assert!(
        !batch_requested(&batch, SettingId::Theme),
        "未在批内的设置项不得被判定为已请求"
    );
    // 非设置信号（进度/退出）不产生误判
    assert!(!batch_requested(
        &[VmSignal::QuitRequested],
        SettingId::Theme
    ));
    assert!(!batch_requested(&[], SettingId::Theme));
}

// T-PERF1 信号分级：只有 EntriesChanged / SettingApplied 触发行重建；
// 进度/弹窗/菜单类信号不碰行集（任务进度 tick 不再重做 O(行集) 重建）。
// 漏放 EntriesChanged = 过滤/树/选择不刷新；误放 JobsChanged = 回到
// 任务运行时每 100ms 全量重做的旧病。
#[test]
fn needs_browse_refresh_gates_on_entries_and_settings() {
    assert!(needs_browse_refresh(&[VmSignal::EntriesChanged]));
    assert!(needs_browse_refresh(&[VmSignal::SettingApplied(
        SettingId::FolderSize
    )]));
    assert!(
        needs_browse_refresh(&[VmSignal::JobsChanged, VmSignal::EntriesChanged]),
        "批内混合含条目信号即刷新"
    );
    assert!(!needs_browse_refresh(&[]));
    let non_refresh = vec![
        VmSignal::JobsChanged,
        VmSignal::DialogChanged,
        // 门放行信号只是「把待执行动作重跑起来」（可能间接发 EntriesChanged），
        // 自身不得触发行重建（用户口径 2026-09-28）
        VmSignal::GateActionReady,
        VmSignal::QuitRequested,
        VmSignal::MenuEpochBump,
        VmSignal::Notify(crate::viewmodel::pump::NotifyEvent {
            title: "t".into(),
            body: "b".into(),
            force_system: false,
        }),
        VmSignal::OpenProgress(1),
        VmSignal::ProgressWindowOpen(1, true),
        // 图标缓存填充只重绘（位图不在快照里，重导快照也拿不到它）——
        // 走行重建会把「一次探测」放大成 O(行集) 重建（T-ICON）。
        VmSignal::IconCacheFilled,
    ];
    for sig in non_refresh {
        assert!(
            !needs_browse_refresh(std::slice::from_ref(&sig)),
            "{sig:?} 不应触发行重建"
        );
    }
}

/// 主题重放的批量取值：只在相关设置项在批内时给出，值**取自内存镜像**。
///
/// 这条钉住「差一拍」的根因面：旧实现让重放内部读盘，而设置落盘是异步的
/// （`Config::save_async`）⇒ 每次点击重放的都是上一拍的值。现在取值只认
/// `SettingsPrefs`（内存镜像，`store` 步骤已就地更新）。
#[test]
fn theme_replay_of_gates_on_batch_and_reads_memory_prefs() {
    use crate::app::state::SettingsPrefs;

    let prefs = SettingsPrefs {
        theme_ix: 2,
        theme_color: "#123456".into(),
        font_family: "Noto Sans".into(),
        font_size: 21,
        corner_radius: 3,
        ..SettingsPrefs::default()
    };

    // 无关信号 → 不重放（零开销）
    assert!(
        theme_replay_of(&[VmSignal::EntriesChanged], &prefs).is_none(),
        "批内无相关设置项时不得重放"
    );

    // 五个相关设置项任一在批内 → 重放，且值 = 内存镜像值
    for id in [
        SettingId::Theme,
        SettingId::ThemeColor,
        SettingId::FontFamily,
        SettingId::FontSize,
        SettingId::CornerRadius,
    ] {
        let got = theme_replay_of(&[VmSignal::SettingApplied(id)], &prefs)
            .unwrap_or_else(|| panic!("{id:?} 在批内应触发重放"));
        assert_eq!(got.size_px, 21, "{id:?}：字号取镜像值");
        assert_eq!(got.radius_px, 3, "{id:?}：圆角取镜像值");
        assert_eq!(got.mode_ix, 2, "{id:?}：模式取镜像值");
        assert_eq!(got.theme_color, "#123456", "{id:?}：主题色取镜像值");
        assert_eq!(got.family, "Noto Sans", "{id:?}：字体族取镜像值");
    }
}

/// UI 半场接线：`apply_ui_effects(Some(replay))` 必须把取值落到全局 `Theme`
/// （泵 → 生效半场的端到端锚点；取值 19/4 与磁盘配置无关，退回读盘即失败）。
#[gpui_kit::test]
fn apply_ui_effects_applies_theme_replay(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;

    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        let replay = crate::ui::theme::ThemeReplay {
            mode_ix: 0,
            theme_color: String::new(),
            family: String::new(),
            size_px: 19,
            radius_px: 4,
        };
        apply_ui_effects(cx, Some(replay), None, None, &[]);
        assert_eq!(
            cx.theme().font_size,
            gpui_kit::px(19.),
            "生效半场须把重放字号落到全局 Theme"
        );
        assert_eq!(cx.theme().radius, gpui_kit::px(4.), "圆角同理");
    });
}

/// 端到端复刻用户那次点击（设置-常规 ▸ 字号 `+`）：
/// `DialogEvent::SettingChanged` → `apply_setting`（store 就地更新内存镜像）
/// → 批内信号 → `theme_replay_of` → 生效半场 `apply_ui_effects`，全界面字号
/// **立即**跟上。
///
/// 关键判据：`cfg!(test)` 下 `persist()` 零落盘 ⇒ 盘上永远是旧值——若实现退回
/// 「重放内部读盘」，这里必然拿到上一拍的字号（用户实报的「点一次不生效」）。
#[gpui_kit::test]
fn font_size_click_applies_immediately_without_disk(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::settings_registry::SettingValue;
    use gpui_kit::component::ActiveTheme as _;

    let engine = std::sync::Arc::new(
        hzip_core::engine::Engine::new(hzip_core::engine::EngineConfig {
            cancel_on_signal: false,
            orphan_cleanup: false,
            ..Default::default()
        })
        .expect("engine"),
    );
    let mut data = crate::app::state::AppData::new(engine);
    let (tx, rx) = smol::channel::unbounded::<VmSignal>();
    data.signal_tx = Some(std::sync::Arc::new(tx));

    // 用户点 `+`：面板同一条路（SettingChanged → consume_dialog_event → 五步）
    let before = data.settings.font_size;
    let want = before + 1;
    data.consume_dialog_event(&crate::ui::contracts::DialogEvent::SettingChanged {
        id: SettingId::FontSize,
        value: SettingValue::Number(want),
    });
    assert_eq!(data.settings.font_size, want, "store 须就地更新内存镜像");

    // 泵：批内信号 → 取值 → 生效半场
    let mut batch = Vec::new();
    while let Ok(sig) = rx.try_recv() {
        batch.push(sig);
    }
    let replay = theme_replay_of(&batch, &data.settings).expect("批内字号项必须触发主题重放");
    assert_eq!(replay.size_px, want, "重放取值必须是**刚点击的新值**");

    cx.update(|cx: &mut gpui_kit::App| {
        gpui_kit::component::init(cx);
        apply_ui_effects(cx, Some(replay), None, None, &[]);
        assert_eq!(
            cx.theme().font_size,
            gpui_kit::px(want as f32),
            "点击后全界面字号应立即跟上（旧实现差一拍 → 此处得旧值）"
        );
    });
}

// —— macOS Finder 扩展意图（appex/Services → 宿主）的 argv 展开 ——
//
// 扩展侧（沙盒）只发 `{key, paths}`，绝不自己拼 argv；宿主按 verb 表的占位符
// 展开成 argv 再走同一个解析器。这条展开是右键动作能落到正确语义的**唯一**
// 环节——错一个占位符，`%SEL` 就会被当成字面量路径、或整批只剩第一个。
#[test]
fn extension_intent_expands_per_verb_placeholder() {
    use hzip_platform::context_menu::ExtensionIntent;

    // `%SEL`（逐项各一条 argv）：两个档案 → 两条 ExtractHere 意图。
    let per = intents_of_extension(&[ExtensionIntent {
        key: "extract_here".into(),
        paths: vec!["/a.zip".into(), "/b.7z".into()],
    }]);
    assert_eq!(per.len(), 2, "%SEL 应逐路径展开成两条 argv");
    assert!(matches!(&per[0], LaunchIntent::ExtractHere { path } if path.to_string_lossy() == "/a.zip"));
    assert!(matches!(&per[1], LaunchIntent::ExtractHere { path } if path.to_string_lossy() == "/b.7z"));

    // `%SELS`（一条 argv 收全批，用于压缩/参数窗）：两个输入 → 一条批次意图。
    let batch = intents_of_extension(&[ExtensionIntent {
        key: "add_zip".into(),
        paths: vec!["/a".into(), "/b".into()],
    }]);
    assert_eq!(batch.len(), 1, "%SELS 应合并成一条批次意图");
    match &batch[0] {
        LaunchIntent::Compress { paths, format, .. } => {
            assert_eq!(paths.len(), 2, "批次须收下全部输入（漏一个 = 少打一个包）");
            assert_eq!(format.as_deref(), Some("zip"));
        }
        other => panic!("应为 Compress，实得 {other:?}"),
    }
}

// 白名单外的 key 与空路径都被安全忽略（扩展是外部输入面）。
#[test]
fn extension_intent_ignores_unknown_key_and_empty_paths() {
    use hzip_platform::context_menu::ExtensionIntent;
    let out = intents_of_extension(&[
        ExtensionIntent {
            key: "definitely_not_a_verb".into(),
            paths: vec!["/a.zip".into()],
        },
        ExtensionIntent {
            key: "extract_here".into(),
            paths: vec![],
        },
    ]);
    assert!(out.is_empty(), "未知 key 与空路径不应产生任何意图");
}
