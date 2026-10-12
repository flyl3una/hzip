//! 用户通知收口组件（2026-09-15 重构；R-GUI4 #3 的组件化）。
//!
//! # 职责
//!
//! 全部业务操作的用户通知**只经本模块投递**——组件内部决定走哪个目标：
//!
//! | 目标 | 载体 | 适用 |
//! |---|---|---|
//! | 界面通知 | gpui-kit `Notification`（窗口内 toast） | hzip-gui 主窗口在场（任务在主界面提交/观察） |
//! | 系统通知 | gpui `App::show_system_notification`（OS 通知中心） | 无主窗口（资源管理器右键 direct 模式进程） |
//!
//! # 判定规则（`pick_delivery`）
//!
//! `force_system = true` → 系统通知（direct 模式：没有主界面，用户看不到
//! 窗口内 toast，必须走 OS 通知中心）。否则：主窗口注册表非空 → 界面
//! toast；注册表为空（或全部已关）→ 系统通知。
//!
//! # 线程语义
//!
//! 产出侧（任务泵线程）**不直接投递**——经 `VmSignal::Notify` 回投主线程
//! 泵批处理（gpui 线程亲和：toast 需要 Window，系统通知走 App 但统一在
//! 主线程执行简单可靠）。主线程消费入口 = [`dispatch_notify`]（runtime 泵
//! 批处理调用）。

use crate::ui::shell::windows::WindowRegistry;
use crate::viewmodel::pump::NotifyEvent;
use gpui_kit::component::notification::{Notification, NotificationDelivery, NotificationType};

/// 界面通知去重标记（同 marker 重复推送 = 替换前条——任务完成通知按 job
/// 取代旧态，不堆叠）。
pub(crate) struct JobNoticeMarker;

/// 主线程投递一条通知（runtime 泵批处理消费 `VmSignal::Notify` 的执行半场）。
///
/// `registry`：主窗口注册表（判「主界面在场」）；`cx`：主线程 App。
pub(crate) fn dispatch_notify(cx: &mut gpui_kit::App, registry: &WindowRegistry, ev: &NotifyEvent) {
    if pick_delivery(registry, ev.force_system) == NotificationDelivery::System {
        // 系统通知中心：App 级 API（无需窗口；Windows 需 set_app_identity
        // ——runtime 启动段已设）。tag 固定 = 同类通知替换不堆叠。
        cx.show_system_notification(gpui_kit::SystemNotification {
            tag: "hzip-job-notice".into(),
            title: ev.title.clone().into(),
            body: ev.body.clone().into(),
            actions: Vec::new(),
        });
        return;
    }
    // 界面 toast：宿主 = 任一存活主窗口（注册表非空时必存在）
    if let Some(handle) = registry.last_handle() {
        let _ = handle.update(cx, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.push_notification(build_note(ev), cx);
        });
    }
}

/// 通知构建（语义类型 + 界面去重 id；系统侧的 tag 在投递点固定）。
fn build_note(ev: &NotifyEvent) -> Notification {
    let n = match notice_kind(&ev.body) {
        NotificationType::Error => Notification::error(ev.body.clone()),
        NotificationType::Success => Notification::success(ev.body.clone()),
        NotificationType::Warning => Notification::warning(ev.body.clone()),
        _ => Notification::info(ev.body.clone()),
    };
    n.title(ev.title.clone())
        .delivery(NotificationDelivery::InApp)
        .id::<JobNoticeMarker>()
}

/// 投递目标判定（模块文档 §判定规则）。
fn pick_delivery(registry: &WindowRegistry, force_system: bool) -> NotificationDelivery {
    if force_system || registry.last_handle().is_none() {
        NotificationDelivery::System
    } else {
        NotificationDelivery::InApp
    }
}

/// 语义类型投影（正文含失败字样 → Error；终态文案由 i18n key 保证可控）。
fn notice_kind(body: &str) -> NotificationType {
    if body.contains("失败") || body.contains("failed") || body.contains("Failed") {
        NotificationType::Error
    } else if body.contains("取消") || body.contains("cancelled") {
        NotificationType::Warning
    } else {
        NotificationType::Success
    }
}

#[cfg(test)]
#[path = "tests/notify.rs"]
mod tests;
