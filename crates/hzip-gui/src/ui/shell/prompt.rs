//! 请求-应答弹窗（T-GUI T11 自 app.rs 拆分：密码/冲突 open+answer）。
//!
//! # 两条注入路径，一个入口
//!
//! 密码/冲突提示窗（独立 OS 窗口）与设置面板的对话框事件都汇到
//! [`handle_dialog_event`]——顺序是 ① **密码门**（无任务态）→ ② 任务态应答
//! → ③ **数据面落账**（任务中心批量/单任务操作、`SettingChanged` 等命令）。
//! 旧实现把门的密码提交截在设置面板的闭包里，而提示窗走的是另一条回调
//! （`answer_prompt` 的门分支只留取消）⇒ 提示窗提交门密码**无人消费**。
//! 现在门分支就在这里，两条路径一致。

use crate::ui::shell::runtime::AppDataHandle;
use crate::viewmodel::GateAction;

/// **views 上行事件总入口**（shell 层）：密码门 → 请求-应答 → 数据面落账。
///
/// 顺序即语义：门事件（密码提交/取消）先由门消费；其余走任务应答链
/// （`answer_prompt` 只对「当前活跃任务的请求」作答）；最后交
/// [`crate::app::state::AppData::consume_dialog_event`] 落**数据面命令**
/// （任务中心批量/单任务操作、浏览页列序 `SettingChanged` 等）。
///
/// **最后一步为什么必须有**（用户实报 2026-10-09）：任务中心「全部取消 /
/// 清除已完成」按钮经 `on_dialog_event` 上行到本函数——旧实现只走应答链，
/// 而 `answer_prompt` 对这两个事件既非密码也非冲突 ⇒ 落 `_ => {}` 被丢弃，
/// 按钮点了毫无反应（同一链路上的浏览页列序拖动 `SettingChanged` 也一并
/// 丢弃 ⇒ 列序不落盘）。数据面消费后由调用方（runtime 上行闭包）回投
/// `JobsChanged`/`DialogChanged` 信号刷新界面。
pub(crate) fn handle_dialog_event(data: &AppDataHandle, event: &crate::ui::contracts::DialogEvent) {
    let handled_by_gate = handle_gate_event(data, event);
    if let Ok(mut d) = data.lock() {
        if !handled_by_gate {
            answer_prompt(&mut d, event);
        }
        // 未命中（门事件/密码冲突应答/设置面板自消费）的事件返回 false，无副作用。
        d.consume_dialog_event(event);
    }
}

/// 密码门事件（返回 `true` = 由门消费，调用方不必再走任务分支）。
///
/// 无门武装 → `false`（事件按原路走任务分支）。
pub(crate) fn handle_gate_event(
    data: &AppDataHandle,
    event: &crate::ui::contracts::DialogEvent,
) -> bool {
    use crate::ui::contracts::DialogEvent;
    let Some((action, key)) = data.lock().ok().and_then(|d| {
        let r = d.router.lock().ok()?;
        Some((r.gate_action()?.clone(), r.gate_key()?))
    }) else {
        return false;
    };
    match event {
        DialogEvent::PasswordSubmit(pw) => match action {
            // 头加密档案：带密码重开整档（含装会话/MRU/密码缓存，见该函数文档）
            GateAction::OpenArchive(path) => {
                // 缓存密码 + 放行门：动作进待执行槽，主线程泵走 `open_archive_task`
                // 重开（同一套打开流程 ⇒ 有进度窗；旧实现起裸线程、全程无窗口，
                // 用户实测 2026-10-19「输完密码要等一会儿才见反应」）。
                if let Ok(mut d) = data.lock() {
                    d.remember_password(&path, zeroize::Zeroizing::new(pw.clone()));
                    if let Ok(mut r) = d.router.lock() {
                        r.gate_resolve();
                    }
                }
            }
            // 条目/内层容器：后台预检 → 通过则缓存密码 + 放行（主线程泵重跑原操作）
            GateAction::OpenSelection { .. } | GateAction::OpenContainer(_) => {
                spawn_gate_verify(data.clone(), key, pw.clone(), action.clone());
            }
        },
        DialogEvent::PasswordCancel => {
            if let Ok(d) = data.lock()
                && let Ok(mut r) = d.router.lock()
            {
                r.gate_disarm();
            }
        }
        _ => return false,
    }
    true
}

/// 门内密码验证（独立后台线程；用户口径 2026-09-28）。
///
/// 走 core [`hzip_core::services::browse::preflight_password`]（头加密 = 解头成败；
/// 数据加密 = 有界真读首条目，流交付前完成预验）。通过 → 缓存密码 + `gate_resolve`
/// （动作进待执行槽，主线程泵重跑）；密码错 → `gate_fail`（attempts+1，达上限解门）；
/// 其它错误 → 解门并留痕。最后回投信号刷新弹窗/浏览投影。
fn spawn_gate_verify(data: AppDataHandle, key: std::path::PathBuf, pw: String, action: GateAction) {
    let spawned = std::thread::Builder::new()
        .name("hzip-gate-verify".into())
        .spawn(move || {
            // 密码持有：一律 `Zeroizing`（02 §1.2），线程结束时清零
            let pw = zeroize::Zeroizing::new(pw);
            let engine = match data.lock() {
                Ok(d) => d.engine.clone(),
                Err(_) => return,
            };
            let res =
                hzip_core::services::browse::preflight_password(engine.registry(), &key, &pw);
            match res {
                Ok(()) => {
                    let mut resolved = false;
                    if let Ok(mut d) = data.lock() {
                        d.remember_password(&key, pw);
                        if let Ok(mut r) = d.router.lock() {
                            resolved = r.gate_resolve();
                        }
                    }
                    // 放行链路的可见性（用户实测 2026-09-28「输密码后不续走」必需：
                    // 本路径此前一条日志都没有，故障现场无从判断停在哪一步）
                    tracing::info!(
                        archive = %key.display(),
                        resolved,
                        "密码门：验证通过 → 缓存密码 + 动作入待执行槽（等泵重跑）"
                    );
                }
                Err(hzip_error::HzipError::Password { .. }) => {
                    // 错密码：门内重问（attempts+1；达上限解门关窗）
                    let retryable = data
                        .lock()
                        .ok()
                        .and_then(|d| d.router.lock().ok().map(|mut r| r.gate_fail()))
                        .unwrap_or(false);
                    let attempts = data
                        .lock()
                        .ok()
                        .and_then(|d| d.router.lock().ok().and_then(|r| r.gate_attempts()));
                    if retryable {
                        tracing::warn!(?attempts, "密码门：密码错误，门内重问（窗内显示第 n/max 次）");
                    } else {
                        // 达上限：解门关窗 + **弹错误窗** + **任务中心留痕**（用户口径
                        // 2026-09-28：旧实现静默关窗，用户不知道发生了什么，也不知道
                        // 任务中心里能查）。
                        let max = crate::viewmodel::router::PASSWORD_MAX_RETRIES;
                        let title = crate::tr("dlg.password.failed_title");
                        let body = crate::tr_with(
                            "dlg.password.failed_body",
                            &[("max", &max.to_string())],
                        );
                        let target = gate_record_target(&action, &key);
                        let msg = format!("{title}：{body}\n{target}");
                        tracing::warn!(?attempts, target = %target, "密码门：重试达上限，解门关窗 + 弹错误窗 + 任务中心留痕");
                        if let Ok(d) = data.lock() {
                            if let Ok(mut jobs) = d.jobs.lock() {
                                jobs.record_op("open", &target, &key.display().to_string(), &Err(msg.clone()));
                            }
                            if let Some(tx) = d.signal_tx.as_ref() {
                                let _ = tx.try_send(crate::viewmodel::VmSignal::ShowMessage(
                                    crate::viewmodel::message::MessageDialog::error(
                                        crate::tr("dlg.password.failed.title"),
                                        msg,
                                    ),
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(archive = %key.display(), error = %e, "密码预检失败（非密码错）");
                    if let Ok(d) = data.lock()
                        && let Ok(mut r) = d.router.lock()
                    {
                        r.gate_disarm();
                    }
                    // **必须让用户看见**（用户实测 2026-10-22：密码输对了、门却静默
                    // 关闭——磁盘不足/IO 失败只进日志，界面看起来「什么都没发生」）。
                    let body = format!("{e}\n{}", key.display());
                    if let Ok(d) = data.lock() {
                        if let Ok(mut jobs) = d.jobs.lock() {
                            jobs.record_op(
                                "open",
                                &key.display().to_string(),
                                &key.display().to_string(),
                                &Err(body.clone()),
                            );
                        }
                        if let Some(tx) = d.signal_tx.as_ref() {
                            let _ = tx.try_send(crate::viewmodel::VmSignal::ShowMessage(
                                crate::viewmodel::message::MessageDialog::error(
                                    crate::tr("dlg.password.verify_failed.title"),
                                    body,
                                ),
                            ));
                        }
                    }
                }
            }
            if let Ok(d) = data.lock()
                && let Some(tx) = d.signal_tx.as_ref()
            {
                let _ = tx.try_send(crate::viewmodel::VmSignal::DialogChanged);
                let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
                // 门放行 → 专用信号：主线程泵**无条件**取出待执行动作重跑原操作
                // （用户口径 2026-09-28：密码窗是门，验证通过不得拦断后续流程）
                let _ = tx.try_send(crate::viewmodel::VmSignal::GateActionReady);
            }
        });
    if let Err(e) = spawned {
        tracing::error!("密码门验证线程创建失败: {e}");
    }
}

/// 门动作 → 任务中心记录用的「显示目标」（档案名 / 条目名 / 内层容器路径）。
fn gate_record_target(action: &GateAction, key: &std::path::Path) -> String {
    match action {
        GateAction::OpenSelection { name, .. } => format!("{} :: {name}", key.display()),
        GateAction::OpenContainer(path) => format!("{} :: {path}", key.display()),
        GateAction::OpenArchive(_) => key
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| key.display().to_string()),
    }
}

/// 任务态应答（密码/冲突；锁序 jobs → router，与泵 drain_requests 一致）。
fn answer_prompt(d: &mut crate::app::state::AppData, event: &crate::ui::contracts::DialogEvent) {
    use crate::ui::contracts::DialogEvent;
    use zeroize::Zeroizing;
    let Some(job_id) = d.router.lock().ok().and_then(|r| r.active_job()) else {
        return;
    };
    // Arc 先克隆：守卫不能借自 `d`（本函数随后要 `&mut d` 登记密码）
    let jobs_arc = d.jobs.clone();
    let Ok(jobs) = jobs_arc.lock() else {
        return;
    };
    let Some(handle) = jobs.handle(job_id) else {
        return;
    };
    match event {
        DialogEvent::PasswordSubmit(pw) => {
            // 用户口径 2026-09-29：任务链里输的密码**只要输一次**——此处按该任务的
            // 缓存键登记，任务成功结束后进会话缓存（后续同档案操作不再问）。
            // （`jobs` 守卫在本分支已被持有：不可再锁一次，直接复用它）
            let key = jobs.password_key_of(job_id).map(|p| p.to_path_buf());
            if let Some(key) = key {
                d.note_job_password(job_id, key, Zeroizing::new(pw.clone()));
            }
            if let Ok(mut r) = d.router.lock()
                && let Err(e) = r.submit_password(handle, Zeroizing::new(pw.clone()))
            {
                tracing::warn!("密码提交失败: {e}");
            }
        }
        DialogEvent::PasswordCancel => {
            if let Ok(mut r) = d.router.lock() {
                r.cancel_request(handle);
            }
        }
        DialogEvent::ConflictAnswer(decision, apply_all) => {
            // T-GUI T32 接线：apply_all 来自冲突框复选态（UI 侧 Cell 透传）；
            // Esc/✕/遮罩路径回传 false（单条决策语义）
            if let Ok(mut r) = d.router.lock()
                && let Err(e) = r.submit_conflict(handle, *decision, *apply_all)
            {
                tracing::warn!("冲突决策提交失败: {e}");
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "tests/prompt.rs"]
mod tests;
