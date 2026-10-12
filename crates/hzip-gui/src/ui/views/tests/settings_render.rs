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
