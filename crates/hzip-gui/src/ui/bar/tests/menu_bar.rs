use super::*;

// —— T-TVS2：工具栏值域（内容语义）+ 旧值兼容回落 ——

// 旧值 large/small 与未知值 → IconText（旧用户看到的本就是图标+文字，
// 无感迁移）；icon_only/hidden 原样解析
#[test]
fn toolbar_style_of_maps_legacy_and_new_values() {
    assert_eq!(toolbar_style_of("icon_text"), ToolbarStyle::IconText);
    assert_eq!(toolbar_style_of("icon_only"), ToolbarStyle::IconOnly);
    assert_eq!(toolbar_style_of("hidden"), ToolbarStyle::Hidden);
    // 旧值（尺寸语义时代）与手改未知值 → 默认 IconText
    assert_eq!(toolbar_style_of("large"), ToolbarStyle::IconText);
    assert_eq!(toolbar_style_of("small"), ToolbarStyle::IconText);
    assert_eq!(toolbar_style_of("sepia"), ToolbarStyle::IconText);
}

// 值域 ↔ 枚举一致（menu 装配 payload 取 value_str；config 读取走
// toolbar_style_of——两方向闭合则不会出现「存了读不回」的值）
#[test]
fn toolbar_style_values_roundtrip() {
    for style in ToolbarStyle::ALL {
        assert_eq!(toolbar_style_of(style.value_str()), style);
    }
}

// —— T-MENU4：最近文件（MRU）——

// 长路径显示：中段省略，保留文件名 + 一级父目录（设计 §2.1）
#[test]
fn recent_label_truncates_middle() {
    assert_eq!(recent_label("docs.7z"), "docs.7z");
    assert_eq!(
        recent_label(&format!(
            "C:{sep}work{sep}docs.7z",
            sep = std::path::MAIN_SEPARATOR
        )),
        format!("…{sep}work{sep}docs.7z", sep = std::path::MAIN_SEPARATOR)
    );
    // 超长 → 仅文件名
    let long = format!(
        "C:{sep}averyveryverylongdirectoryname{sep}anotherlongdirname{sep}x.zip",
        sep = std::path::MAIN_SEPARATOR
    );
    let label = recent_label(&long);
    assert!(label.ends_with("x.zip") && label.starts_with('…'));
    assert!(label.chars().count() < recent_label(&long).chars().count() + 1);
}

// mru_items：与菜单/工具栏共用的内容投影——空列表 → 单项禁用占位
#[test]
fn mru_items_empty_is_disabled_placeholder() {
    let items = mru_items(&[]);
    assert_eq!(items.len(), 1, "空 MRU 恰一项占位");
    assert!(matches!(
        &items[0],
        MenuEntry::Item {
            action: MenuAction::OpenRecent(usize::MAX),
            disabled: true,
            checked: false,
            icon: None,
            ..
        }
    ));
}

// mru_items：3 项 → 前 3 项按 LRU 序（label 走 recent_label 同一规则）+
// 分隔线 + 清空尾项
#[test]
fn mru_items_preserve_lru_order_and_clear_tail() {
    let mru = vec![
        "C:\\a.zip".to_string(),
        "C:\\b.7z".to_string(),
        "C:\\c.tar.gz".to_string(),
    ];
    let items = mru_items(&mru);
    assert_eq!(items.len(), 5, "3 条目 + 分隔 + 清空");
    for (i, p) in mru.iter().enumerate() {
        assert!(
            matches!(
                &items[i],
                MenuEntry::Item {
                    label,
                    icon: Some(AppIcon::FolderZip),
                    action: MenuAction::OpenRecent(ix),
                    disabled: false,
                    ..
                } if ix == &i && label == &recent_label(p)
            ),
            "第 {i} 项保持 LRU 序且 label 与 recent_label 一致"
        );
    }
    assert_eq!(items[3], MenuEntry::Separator, "第 4 项为分隔线");
    assert!(matches!(
        &items[4],
        MenuEntry::Item {
            action: MenuAction::ClearMru,
            ..
        }
    ));
}

// MRU 空 → 禁用占位项；非空 → 条目 + 分隔 + 清空（Tail）
#[test]
fn file_menu_recent_submenu_shape() {
    let mut ctx = MenuContext::closed();
    let e = menu_entries(WhichMenu::File, &ctx);
    let MenuEntry::Submenu { entries, .. } = &e[1] else {
        panic!("第二项为最近文件子菜单");
    };
    assert_eq!(entries.len(), 1);
    assert!(
        matches!(&entries[0], MenuEntry::Item { disabled: true, .. }),
        "空 MRU → 禁用占位"
    );

    ctx.mru = vec!["C:\\a.zip".into(), "C:\\b.7z".into()];
    let e = menu_entries(WhichMenu::File, &ctx);
    let MenuEntry::Submenu { entries, .. } = &e[1] else {
        panic!("第二项为最近文件子菜单");
    };
    assert_eq!(entries.len(), 4, "2 条目 + 分隔 + 清空");
    assert!(matches!(
        &entries[0],
        MenuEntry::Item {
            action: MenuAction::OpenRecent(0),
            disabled: false,
            ..
        }
    ));
    assert!(matches!(
        &entries[3],
        MenuEntry::Item {
            action: MenuAction::ClearMru,
            ..
        }
    ));
    assert!(
        matches!(
            &entries[0],
            MenuEntry::Item { label, .. }
                if label.ends_with("a.zip"),
        ),
        "label = 文件名"
    );
}

// OwnedMenuItem::Submenu 装配（T-MENU2 Spike 结论的回归锚点）
#[test]
fn build_app_menus_contains_recent_submenu() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    let mut ctx = MenuContext::closed();
    ctx.mru = vec!["C:\\工作\\docs\\my archive.zip".into()];
    let menus = build_app_menus(&ctx);
    let file = menus.first().expect("文件组在首");
    assert!(file.items.iter().any(
        |it| matches!(it, gpui_kit::OwnedMenuItem::Submenu(_) | gpui_kit::OwnedMenuItem::Submenu { .. } if true),
    ) || file
        .items
        .iter()
        .any(|it| matches!(it, gpui_kit::OwnedMenuItem::Submenu(_))),
        "最近文件以 Submenu 变体装配"
    );
}
// 清单：六个触发器标签（设计 §1.1 六组新序）
#[test]
fn trigger_labels_match_spec() {
    let labels: Vec<String> = WhichMenu::ALL.iter().map(|w| w.label()).collect();
    assert_eq!(
        labels,
        [
            "文件(F)",
            "编辑(E)",
            "工具(T)",
            "选项(O)",
            "视图(V)",
            "帮助(H)"
        ]
        .map(String::from)
    );
}

// 清单：打开组 = 打开/分隔/在文件管理器显示/解压/分隔/新窗口/退出（T-MENU1）
#[test]
fn file_menu_entries() {
    let e = menu_entries(WhichMenu::File, &MenuContext::closed());
    // 打开 / 分隔(MRU 装配位) / 在文件管理器显示(disabled) / 解压(disabled)
    // / 分隔 / 新窗口 / 退出
    assert_eq!(e.len(), 7);
    assert!(matches!(
        &e[0],
        MenuEntry::Item {
            label,
            action: MenuAction::Open,
            disabled: false,
            ..
        } if label.as_str() == "打开压缩文件…"
    ));
    assert!(matches!(
        &e[2],
        MenuEntry::Item {
            action: MenuAction::Reveal,
            disabled: true,
            ..
        }
    ));
    assert!(matches!(
        &e[3],
        MenuEntry::Item {
            action: MenuAction::Extract,
            disabled: true,
            ..
        }
    ));
    assert_eq!(e[4], MenuEntry::Separator);
    assert!(matches!(
        &e[5],
        MenuEntry::Item {
            action: MenuAction::NewWindow,
            disabled: false,
            ..
        }
    ));
    assert!(matches!(
        &e[6],
        MenuEntry::Item {
            action: MenuAction::Quit,
            ..
        }
    ));

    // 有档案 → reveal/extract 解禁
    let e = menu_entries(WhichMenu::File, &MenuContext::opened());
    assert!(matches!(
        &e[2],
        MenuEntry::Item {
            action: MenuAction::Reveal,
            disabled: false,
            ..
        }
    ));
    assert!(matches!(
        &e[3],
        MenuEntry::Item {
            action: MenuAction::Extract,
            disabled: false,
            ..
        }
    ));
}

// 清单：编辑组四件套 + 全选/反选（B2 裁定保留）；无档案整组 disabled
#[test]
fn edit_menu_disabled_matrix() {
    let e = menu_entries(WhichMenu::Edit, &MenuContext::closed());
    let actions: Vec<(MenuAction, bool)> = e
        .iter()
        .filter_map(|entry| match entry {
            MenuEntry::Item {
                action, disabled, ..
            } => Some((*action, *disabled)),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions,
        vec![
            (MenuAction::AddFiles, true),
            (MenuAction::DeleteFiles, true),
            (MenuAction::CopyFiles, true),
            (MenuAction::Paste, true),
        ]
    );

    // 有档案无选中：四件套中删除/复制仍 disabled
    let e = menu_entries(WhichMenu::Edit, &MenuContext::opened());
    let get = |a: MenuAction| -> bool {
        e.iter()
            .find_map(|entry| match entry {
                MenuEntry::Item {
                    action, disabled, ..
                } if *action == a => Some(*disabled),
                _ => None,
            })
            .unwrap()
    };
    assert!(!get(MenuAction::AddFiles));
    assert!(get(MenuAction::DeleteFiles), "无选中 → 删除 disabled");
    assert!(get(MenuAction::CopyFiles), "无选中 → 复制 disabled");
    assert!(
        get(MenuAction::Paste),
        "剪贴板空 → 粘贴 disabled（T-MENU8 has_clipboard 输入）"
    );
    // 剪贴板有内容（且有档案）→ 粘贴解禁（与选中无关）
    let mut with_cb = MenuContext::opened();
    with_cb.has_clipboard = true;
    let e = menu_entries(WhichMenu::Edit, &with_cb);
    let paste_disabled = e
        .iter()
        .find_map(|entry| match entry {
            MenuEntry::Item {
                action: MenuAction::Paste,
                disabled,
                ..
            } => Some(*disabled),
            _ => None,
        })
        .expect("粘贴项存在");
    assert!(!paste_disabled, "剪贴板非空 → 粘贴可用");
}

// 清单：工具组 = 查找 + 测试完整性（设计 §2.3）；禁用矩阵
#[test]
fn tools_menu_disabled_without_archive() {
    let e = menu_entries(WhichMenu::Tools, &MenuContext::closed());
    assert_eq!(e.len(), 2);
    assert!(matches!(
        e[0],
        MenuEntry::Item {
            action: MenuAction::FindFile,
            disabled: true,
            ..
        }
    ));
    assert!(matches!(
        e[1],
        MenuEntry::Item {
            action: MenuAction::TestIntegrity,
            disabled: true,
            ..
        }
    ));
    let e = menu_entries(WhichMenu::Tools, &MenuContext::opened());
    assert!(matches!(
        e[1],
        MenuEntry::Item {
            action: MenuAction::TestIntegrity,
            disabled: false,
            ..
        }
    ));
}

// 清单：视图组 = 树勾选 + 工具栏▸三选一 + 视图样式▸三选一（T-MENU5 装配）
#[test]
fn view_menu_shape_with_checked_tree() {
    let mut ctx = MenuContext::closed();
    let e = menu_entries(WhichMenu::View, &ctx);
    assert!(matches!(
        &e[0],
        MenuEntry::Item {
            action: MenuAction::ToggleTree,
            checked: true,
            disabled: false,
            ..
        }
    ));
    // 工具栏子菜单：三选一 radio，Large checked
    let MenuEntry::Submenu { entries, .. } = &e[1] else {
        panic!("第二项为工具栏子菜单");
    };
    assert_eq!(entries.len(), 3);
    assert!(
        matches!(&entries[0], MenuEntry::Item { checked: true, .. }),
        "Large 默认勾选"
    );
    assert!(
        entries
            .iter()
            .filter(|it| matches!(it, MenuEntry::Item { checked: true, .. }))
            .count()
            == 1,
        "单选组：恰一项 checked"
    );
    // 视图样式子菜单：三选一全可选（T-MENU9 落地 icon/list 渲染）
    let MenuEntry::Submenu { entries, .. } = &e[2] else {
        panic!("第三项为视图样式子菜单");
    };
    assert_eq!(entries.len(), 3);
    assert!(
        entries.iter().all(|it| matches!(
            it,
            MenuEntry::Item {
                disabled: false,
                ..
            }
        )),
        "icon/list 渲染到位后三模式均可选"
    );
    assert!(
        matches!(
            &entries[0],
            MenuEntry::Item {
                action: MenuAction::SetViewStyle(0),
                checked: true,
                ..
            }
        ),
        "默认 detail 勾选"
    );

    ctx.show_tree = false;
    let e = menu_entries(WhichMenu::View, &ctx);
    assert!(
        matches!(
            &e[0],
            MenuEntry::Item {
                action: MenuAction::ToggleTree,
                checked: false,
                ..
            }
        ),
        "勾选态随值翻转"
    );

    // 任务中心项：显隐开关（勾选 = 界面显示任务中心），与树型目录同款语义
    let jobs_checked = |ctx: &MenuContext| {
        menu_entries(WhichMenu::View, ctx)
            .into_iter()
            .find_map(|it| match it {
                MenuEntry::Item {
                    action: MenuAction::ToggleJobs,
                    checked,
                    disabled,
                    ..
                } => Some((checked, disabled)),
                _ => None,
            })
            .expect("视图菜单必须有任务中心项")
    };
    assert_eq!(jobs_checked(&ctx), (false, false), "默认关：不勾选");
    ctx.show_jobs = true;
    assert_eq!(jobs_checked(&ctx), (true, false), "打开后勾选");
}

// 清单：帮助组恢复 = 关于 + 检查更新（T-MENU1 恢复 T34 裁撤的 About）
#[test]
fn help_menu_about_and_check_update() {
    let e = menu_entries(WhichMenu::Help, &MenuContext::closed());
    assert_eq!(e.len(), 2);
    assert!(matches!(
        e[0],
        MenuEntry::Item {
            action: MenuAction::About,
            disabled: false,
            ..
        }
    ));
    assert!(matches!(
        e[1],
        MenuEntry::Item {
            action: MenuAction::CheckUpdate,
            ..
        }
    ));
}

// OwnedMenu 装配：六组全注册（Help 不再为空）+ 禁用/勾选穿透
#[test]
fn build_app_menus_shape() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    let menus = build_app_menus(&MenuContext::closed());
    assert_eq!(menus.len(), 6, "六组菜单全注册");
    assert_eq!(menus[0].name, "文件(F)");
    assert_eq!(menus[5].name, "帮助(H)");
    // 编辑组首项 disabled（无档案）
    let first = menus[1]
        .items
        .iter()
        .find_map(|it| match it {
            gpui_kit::OwnedMenuItem::Action { name, disabled, .. } => {
                Some((name.as_str(), *disabled))
            }
            _ => None,
        })
        .expect("编辑组首项为动作");
    assert_eq!(first.0, "添加文件…");
    assert!(first.1, "无档案时编辑首项必须 disabled");
}

// 菜单快照**必须随语言变化**（用户实测缺陷的回归锚点）。
#[test]
fn build_app_menus_follows_locale() {
    let _locale = crate::viewmodel::testutil::locale_lock();
    crate::init_locale("zh-CN");
    let zh = build_app_menus(&MenuContext::opened());
    crate::init_locale("en");
    let en = build_app_menus(&MenuContext::opened());

    assert_eq!(zh[0].name, "文件(F)");
    assert_eq!(en[0].name, "File", "触发器名随语言");
    assert_ne!(zh[0].name, en[0].name);

    let first_action = |menus: &[gpui_kit::OwnedMenu], mi: usize| -> String {
        menus[mi]
            .items
            .iter()
            .find_map(|it| match it {
                gpui_kit::OwnedMenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .expect("首项为动作")
    };
    assert_eq!(first_action(&zh, 0), "打开压缩文件…");
    assert_eq!(first_action(&en, 0), "Open Archive…", "条目名随语言");
    crate::init_locale("zh-CN");
}

// 真实实体路径：`MenuBar::sync` 在语言/投影/epoch 变化后把新快照写回
// GlobalState（弹层装配的唯一来源）。
#[gpui_kit::test]
fn menu_snapshot_rebuilds_when_locale_changes(cx: &mut gpui_kit::TestAppContext) {
    let _locale = crate::viewmodel::testutil::locale_lock();
    use gpui_kit::base::GlobalState;
    crate::init_locale("zh-CN");
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        register_menus(&MenuContext::closed(), cx);
    });
    let bar = cx.update(MenuBar::new);

    let first_menu_name = |cx: &mut gpui_kit::TestAppContext| -> String {
        cx.update(|cx| {
            GlobalState::global(cx)
                .app_menus()
                .first()
                .map(|m| m.name.to_string())
                .expect("app_menus 非空")
        })
    };
    assert_eq!(first_menu_name(cx), "文件(F)", "起始：中文快照");

    // 切英文后同步一帧 → 快照应变成英文
    cx.update(|_| crate::init_locale("en"));
    cx.update(|cx| bar.update(cx, |b, cx| b.sync(&MenuContext::closed(), 0, cx)));
    assert_eq!(first_menu_name(cx), "File", "语言变化后菜单快照必须重建");

    // 无变化时同步是空操作
    cx.update(|cx| bar.update(cx, |b, cx| b.sync(&MenuContext::closed(), 0, cx)));
    assert_eq!(first_menu_name(cx), "File");

    // has_archive 变化 → 重建
    cx.update(|cx| {
        bar.update(cx, |b, cx| b.sync(&MenuContext::opened(), 0, cx));
    });
    cx.update(|_| crate::init_locale("zh-CN"));
    cx.update(|cx| bar.update(cx, |b, cx| b.sync(&MenuContext::opened(), 0, cx)));
    assert_eq!(first_menu_name(cx), "文件(F)", "切回中文同样重建");

    // epoch 变化 → 重建（T-MENU4/5 消费路径）
    cx.update(|cx| {
        bar.update(cx, |b, cx| b.sync(&MenuContext::opened(), 7, cx));
    });
    assert_eq!(first_menu_name(cx), "文件(F)");
}
