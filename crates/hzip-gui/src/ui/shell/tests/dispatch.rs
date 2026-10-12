use super::*;
use crate::viewmodel::{SettingId, VmSignal};

/// 关联结果 → 弹窗（纯逻辑）：写入失败=错误级+原始原因；部分接管=警告级+逐项；
/// 全成功=不弹。级别选错会让用户把致命失败当成提示忽略掉。
#[test]
fn assoc_result_dialog_picks_level_and_title() {
    use crate::viewmodel::message::MessageLevel;
    // ① 写入失败 → 错误级，详情 = 系统原因原文
    let d =
        assoc_result_dialog(Some("拒绝访问"), true, &["zip".to_string()]).expect("失败必须弹窗");
    assert_eq!(d.level, MessageLevel::Error);
    assert_eq!(d.title, crate::tr("assoc.failed.title"));
    assert!(d.detail.contains("拒绝访问"), "{}", d.detail);
    // ② 无错误但格式没接管（本机此时无任何关联记录 → 判「没有生效」）→ 警告级
    let d = assoc_result_dialog(None, true, &["hztest-nope".to_string()]).expect("没接管必须弹窗");
    assert_eq!(d.level, MessageLevel::Warning);
    assert!(d.detail.contains("hztest-nope"), "{}", d.detail);
    // ③ 解除后已不归我们（同上：本来就不归我们）→ 不弹
    assert!(assoc_result_dialog(None, false, &["hztest-nope".to_string()]).is_none());
}

/// 未接管项的弹窗文案（纯逻辑）：三种成因各写清楚；全部成功 → 不弹。
///
/// 守的是用户口径「失败了应该弹窗报错」：写入返回 `Ok` 不等于系统采用了我们
/// （`UserChoice` 层可能没清掉），故必须逐项回读再如实告知，且要能区分
/// 「被受保护层压着」「系统里仍是别人」「压根没有默认程序」三种情况。
#[test]
fn assoc_failure_message_classifies_reasons() {
    use hzip_platform::assoc::ExtAssocState;
    assert!(assoc_failure_message(&[]).is_none(), "全成功不该弹窗");

    let blocked = ExtAssocState {
        effective: Some("Bandizip.7z".into()),
        effective_name: Some("Bandizip".into()),
        ours: false,
        pinned: true,
    };
    let other = ExtAssocState {
        effective: Some("Some.Other".into()),
        effective_name: None,
        ours: false,
        pinned: false,
    };
    let msg = assoc_failure_message(&[
        ("7z".to_string(), blocked),
        ("tgz".to_string(), other),
        ("rar".to_string(), ExtAssocState::default()),
    ])
    .expect("应有弹窗文案");
    assert!(msg.contains(".7z") && msg.contains("Bandizip"), "{msg}");
    assert!(msg.contains("tgz") && msg.contains("Some.Other"), "{msg}");
    assert!(msg.contains("rar") && msg.contains("没有生效"), "{msg}");
}

/// 回归：后台系统侧效果完成必须回投 `SettingApplied`。
///
/// 缺它的症状：点「从系统清除」后注册表/包已清干净，面板状态行仍显示
/// 「已注册 N 项」——后台线程写 AppData 不会触发渲染，只有信号能让主线程泵
/// 重载面板（实测）。
#[test]
fn background_effect_notifies_ui_to_reload_settings() {
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
    let data = std::sync::Arc::new(std::sync::Mutex::new(data));

    notify_settings_applied(&data, SettingId::CtxExtract);

    let got = rx.try_recv().expect("应回投信号（否则界面不刷新）");
    assert!(
        matches!(got, VmSignal::SettingApplied(SettingId::CtxExtract)),
        "信号内容应为 SettingApplied(CtxExtract)，实得 {got:?}"
    );
}

/// 契约（用户明确要求）：**只有「应用」/「清除」允许动操作系统**。
///
/// 勾选/取消走 `SettingChanged` → 只改草稿；若这里有值，就意味着「勾一下就已
/// 写系统、未点应用无法撤销」——正是本轮被否掉的行为（历史缺陷回归锚点）。
#[test]
fn only_apply_or_clear_touch_the_os() {
    use crate::app::settings_registry::SettingValue;
    use crate::ui::contracts::DialogEvent;
    assert_eq!(
        ctx_menu_os_action(&DialogEvent::SettingChanged {
            id: SettingId::CtxExtract,
            value: SettingValue::StrSet(vec!["extract_here".into()]),
        }),
        None,
        "勾选不得触发系统写入"
    );
    assert_eq!(
        ctx_menu_os_action(&DialogEvent::SettingChanged {
            id: SettingId::CtxCompress,
            value: SettingValue::StrSet(vec!["add_zip".into()]),
        }),
        None,
        "勾选不得触发系统写入"
    );
    assert_eq!(
        ctx_menu_os_action(&DialogEvent::SettingsContextMenuApply),
        Some(CtxOsAction::Install)
    );
    assert_eq!(
        ctx_menu_os_action(&DialogEvent::SettingsContextMenuClear),
        Some(CtxOsAction::Uninstall)
    );
    // T-SEL 后走查 #6：恢复默认 → OS 侧先卸再装（与恢复后配置一致）
    assert_eq!(
        ctx_menu_os_action(&DialogEvent::SettingsReset),
        Some(CtxOsAction::Reinstall)
    );
}

mod extract_plan_tests {
    //! 解压方式 → 落点（T-BAR4；与右键动词家族逐条对齐，见 [`extract_plan`] 文档）。
    use super::*;
    use std::path::{Path, PathBuf};

    fn plan(mode: ExtractMode, archive: &str, desktop: Option<&str>) -> ExtractPlan {
        extract_plan(mode, Path::new(archive), desktop.map(Path::new)).expect("应可规划")
    }

    #[test]
    fn five_modes_map_to_expected_destinations() {
        let a = "C:/dl/demo.zip";
        let desk = Some("C:/Users/u/Desktop");
        // 自动解压：档案目录 + smart 强制 true（「智能解压」承诺）
        let p = plan(ExtractMode::Auto, a, desk);
        assert_eq!(p.dest, PathBuf::from("C:/dl"));
        assert_eq!(p.smart, Some(true));
        // 解压到当前文件夹：档案目录 + smart 读配置
        let p = plan(ExtractMode::Here, a, desk);
        assert_eq!(p.dest, PathBuf::from("C:/dl"));
        assert_eq!(p.smart, None);
        // 解压到「压缩文件名」文件夹：档案目录 / 去扩展名的档案名
        let p = plan(ExtractMode::Dir, a, desk);
        assert_eq!(p.dest, PathBuf::from("C:/dl/demo"));
        assert_eq!(p.smart, None);
        // 桌面两项
        let p = plan(ExtractMode::Desktop, a, desk);
        assert_eq!(p.dest, PathBuf::from("C:/Users/u/Desktop"));
        assert_eq!(p.smart, None);
        let p = plan(ExtractMode::AutoDesktop, a, desk);
        assert_eq!(p.dest, PathBuf::from("C:/Users/u/Desktop"));
        assert_eq!(p.smart, Some(true));
    }

    #[test]
    fn desktop_modes_fail_without_desktop_dir() {
        let a = "C:/dl/demo.zip";
        for mode in [ExtractMode::Desktop, ExtractMode::AutoDesktop] {
            assert!(
                extract_plan(mode, Path::new(a), None).is_err(),
                "{mode:?}：取不到桌面目录应报错（由调用方置灰），不得回落到别处"
            );
        }
        // 非桌面方式不受影响
        assert!(extract_plan(ExtractMode::Here, Path::new(a), None).is_ok());
    }

    #[test]
    fn archive_without_parent_is_rejected() {
        assert!(extract_plan(ExtractMode::Here, Path::new("demo.zip"), None).is_err());
        // 桌面方式不需要父目录
        assert!(
            extract_plan(
                ExtractMode::Desktop,
                Path::new("demo.zip"),
                Some(Path::new("C:/Users/u/Desktop"))
            )
            .is_ok()
        );
    }
}
