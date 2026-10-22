//! 外部编辑写回的 UI 半场（`AppShell`）。
//!
//! 数据面（登记/判重/在飞串行/决定）在 [`crate::app::external_edit`]；本文件只管
//! **只有主线程能做的事**：建/持 `FileWatch` 句柄、把去抖线程的事件收回来、
//! 提交任务、把「写不回」的原因变成用户看得见的一条通知。
//!
//! # 线程模型
//!
//! ```text
//! 打开流程（hzip-open 线程）──EditEvent::Arm──┐
//!                                            ├─▶ smol channel ──▶ 主线程 drain（本文件）
//! FileWatch 去抖线程 ──EditEvent::Saved──────┘        │
//!                                                     ├─ Submit ⇒ dispatch::submit_spec_locked
//!                                                     └─ Refused ⇒ VmSignal::Notify
//! ```
//!
//! watch 回调只做「读一次 stat + try_send」，立即返回（与 `runtime.rs` 里转发队列
//! 回调同一纪律：去抖线程不做任何重活，也不碰 `AppData`）。

use std::path::PathBuf;

use crate::app::external_edit::{EditEvent, SaveAction, WriteBack};
use crate::app::state::AppData;
use crate::ui::contracts::SignalTx;
use crate::viewmodel::pump::{NotifyEvent, VmSignal};

use super::app_shell::AppShell;

/// 通知标题（写回相关三类通知共用；正文说明具体是什么情况）。
const TITLE_KEY: &str = "edit.writeback.title";

/// 同时保活的写回监听数上限。
///
/// 每条监听 = 一条去抖线程 + 一处 OS 文件监听；上限之外的丢弃策略见
/// [`AppShell::arm_edit_watch`]。取值按「同时改几个文件」的用户实况留足余量。
const MAX_EDIT_WATCHES: usize = 16;

impl AppShell {
    /// 外部编辑事件 drain（**首帧起一次**）。
    ///
    /// 为什么放在首帧而不是 `with_data`：spawn 需要一个已存在的实体句柄
    /// （`WeakEntity<Self>`），构造期还没有。
    pub(super) fn ensure_edit_drain(&mut self, cx: &mut gpui_kit::Context<Self>) {
        if self.edit_drain_started {
            return;
        }
        let Some(rx) = self.edit_rx.take() else {
            return;
        };
        self.edit_drain_started = true;
        let tx = self.edit_tx.clone();
        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                // 壳层已销毁（窗口关闭）→ 收工；在飞的 watch 句柄随壳层一起 drop
                // （`FileWatch::drop` 停监听）。
                if this
                    .update(cx, |shell, cx| shell.handle_edit_event(ev, &tx, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// 单条事件：登记（+挂监听）或保存（+写回）。
    fn handle_edit_event(
        &mut self,
        ev: EditEvent,
        tx: &smol::channel::Sender<EditEvent>,
        cx: &mut gpui_kit::Context<Self>,
    ) {
        match ev {
            EditEvent::Arm { name, temp, target } => {
                if let Some(data) = self.data.clone()
                    && let Ok(mut d) = data.lock()
                {
                    d.arm_external_edit(&name, &temp, target.clone());
                }
                self.arm_edit_watch(&name, &temp, tx.clone(), cx);
            }
            EditEvent::Saved { temp, stat } => {
                let Some(data) = self.data.clone() else {
                    return;
                };
                let action = data
                    .lock()
                    .ok()
                    .and_then(|mut d| d.on_external_save(&temp, stat));
                let (notify, job) = apply_save_action(&data, action);
                if let Some(ev) = notify {
                    self.push_notice(ev);
                }
                if let Some(job_id) = job
                    && let Some(tx) = self.signal_tx.as_ref()
                {
                    // 进度窗：写回可能很慢（组合链全量重压 / 大镜像就地写）
                    let _ = tx.try_send(VmSignal::OpenProgress(job_id));
                }
            }
        }
    }

    /// 挂（或换）写回监听：同一物化路径重开时旧句柄先 drop（停旧监听）。
    ///
    /// **有上界的**：`FileWatch` 每个都带一条去抖线程 + 一处 OS 监听，长期使用不能无上限
    /// 堆积（浏览大量文件逐一外部打开的场景）⇒ 超过 [`MAX_EDIT_WATCHES`] 丢最旧的
    /// （丢句柄 = 停监听），并同步清掉对应会话——**不装作还在看着**（那正是「保存没反应」
    /// 的旧症状）。
    fn arm_edit_watch(
        &mut self,
        name: &str,
        temp: &std::path::Path,
        tx: smol::channel::Sender<EditEvent>,
        cx: &mut gpui_kit::Context<Self>,
    ) {
        self.edit_watches.retain(|(p, _)| p != temp);
        let temp_cb = temp.to_path_buf();
        let cb: Box<dyn Fn(PathBuf) + Send> = Box::new(move |_changed| {
            // 去抖线程：只读一次 stat + 投递（不持锁、不做重活）
            if let Some(stat) = crate::app::external_edit::stat_of(&temp_cb) {
                let _ = tx.try_send(EditEvent::Saved {
                    temp: temp_cb.clone(),
                    stat,
                });
            }
        });
        match hzip_platform::watch::FileWatch::new(temp, cb) {
            Ok(w) => {
                tracing::info!(name, temp = %temp.display(), "外部编辑写回：已挂监听");
                self.edit_watches.push((temp.to_path_buf(), w));
                while self.edit_watches.len() > MAX_EDIT_WATCHES {
                    let (stale, _dropped) = self.edit_watches.remove(0);
                    tracing::debug!(
                        temp = %stale.display(),
                        "外部编辑写回：监听数超上限，丢最旧（该文件此后保存不再写回）"
                    );
                    if let Some(data) = self.data.clone()
                        && let Ok(mut d) = data.lock()
                    {
                        d.forget_external_edit(&stale);
                    }
                }
            }
            Err(e) => {
                // 挂不上监听 = 保存不会写回（用户必须知道，不能静默）
                tracing::warn!(name, temp = %temp.display(), "外部编辑写回：挂监听失败 {e}");
                self.push_notice(NotifyEvent {
                    title: crate::tr(TITLE_KEY),
                    body: crate::tr_with("edit.writeback.watch-failed", &[("name", name)]),
                    force_system: false,
                });
            }
        }
        cx.notify();
    }

    /// 投一条界面通知（走既有信号通道，与任务泵同一出口）。
    pub(super) fn push_notice(&self, ev: NotifyEvent) {
        if let Some(tx) = &self.signal_tx {
            let _ = tx.try_send(VmSignal::Notify(ev));
        }
    }
}

/// 执行一条保存决定：提交任务（成功则回填会话）或产出「写不回」的通知。
///
/// 返回 `Some(NotifyEvent)` = 需要提示用户（写回失败/不可写回）；`None` = 无动作或
/// 已提交（提交成功的用户可见出口 = 任务中心的任务行 + 既有完成通知）。
pub(crate) fn apply_save_action(
    data: &std::sync::Arc<std::sync::Mutex<AppData>>,
    action: Option<SaveAction>,
) -> (Option<NotifyEvent>, Option<u64>) {
    let Some(action) = action else {
        return (None, None);
    };
    match action {
        SaveAction::Refused { reason, name } => (
            Some(NotifyEvent {
                title: crate::tr(TITLE_KEY),
                body: crate::tr_with(reason, &[("name", &name)]),
                force_system: false,
            }),
            None,
        ),
        SaveAction::Submit { spec, stat, temp } => {
            let Ok(mut d) = data.lock() else {
                return (None, None);
            };
            let name = d
                .external_edit(&temp)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            // 提交前释放浏览会话句柄：Windows 上目标仍被打开时原子替换必失败
            // （既有编辑入口同一处置，实现收在 `release_session_for_edit`）。
            // 手动写回模式（设置-通用「改动后立即写回」= 关）：**挂起**，不提交。
            // 用户改多个文件后点浏览页「保存」一次性合并写回（组合链只压一次）。
            if d.manual_write_back() {
                let name = name.clone();
                match d.stage_pending_from_spec(&spec) {
                    Ok(n) => {
                        d.note_external_edit_staged(&temp, stat);
                        tracing::info!(pending = n, temp = %temp.display(), "手动写回：改动已挂起");
                        return (
                            Some(NotifyEvent {
                                title: crate::tr(TITLE_KEY),
                                body: crate::tr_with(
                                    "edit.writeback.staged",
                                    &[("name", &name), ("n", &n.to_string())],
                                ),
                                force_system: false,
                            }),
                            None,
                        );
                    }
                    Err(msg) => {
                        tracing::warn!(temp = %temp.display(), "手动写回：挂起失败 {msg}");
                        return (
                            Some(NotifyEvent {
                                title: crate::tr(TITLE_KEY),
                                body: crate::tr_with(
                                    "edit.writeback.failed",
                                    &[("name", &name), ("error", &msg)],
                                ),
                                force_system: false,
                            }),
                            None,
                        );
                    }
                }
            }
            let archive = spec.source.clone();
            if spec.write_back.is_some() {
                // 嵌套会话：外层句柄在 open_stack 里 ⇒ 必须收回顶层（保住内层物化档）
                d.detach_nested_for_write_back(&archive);
            } else {
                d.release_session_for_edit(&archive);
            }
            match super::dispatch::submit_spec_locked(&mut d, *spec) {
                Ok(job_id) => {
                    d.note_external_edit_submitted(&temp, stat, job_id);
                    tracing::info!(job = job_id, temp = %temp.display(), "外部编辑写回：已提交任务");
                    // 开进度窗跟踪：写回可能是**全量重压**（组合链）或大镜像就地写，
                    // 只留任务中心一行的话用户面对的是「保存了但没反应」。
                    (None, Some(job_id))
                }
                Err(msg) => {
                    tracing::warn!(temp = %temp.display(), "外部编辑写回：提交失败 {msg}");
                    (
                        Some(NotifyEvent {
                            title: crate::tr(TITLE_KEY),
                            body: crate::tr_with(
                                "edit.writeback.failed",
                                &[("name", &name), ("error", &msg)],
                            ),
                            force_system: false,
                        }),
                        None,
                    )
                }
            }
        }
    }
}

/// 写回目标判定（打开流程用；**已持 `AppData` 锁**）。
///
/// - 嵌套会话（进了内层容器）⇒ 不写回：临时文件属于**内层**档案的条目空间，
///   拿它的 id 去改外层档案会写错东西（用户口径 2026-09-28「不要改动其他信息」
///   的硬要求）；
/// - 当前会话的格式不支持编辑（`.tar`、只读卷…）⇒ 不写回；
/// - 其余 ⇒ 写回（卷容器走就地写、zip/7z 走重写 + 原子替换，由 core 选择路径），
///   失败（例如加密档没密码、需要新分配空间的写）如实出现在任务中心。
pub(crate) fn write_back_target(
    d: &AppData,
    archive: &std::path::Path,
    entry: hzip_format::EntryId,
) -> WriteBack {
    // 嵌套会话（T-IMG0）：按**写回链**判定——链可写就真写回（外层逐层冒泡），
    // 建不起来（rar/cpio 外层、深度 > 1、延迟层）才拒绝并给出具体理由。
    if let Some(sc) = &d.session_chain {
        if sc.editable {
            return WriteBack::Enabled {
                archive: archive.to_path_buf(),
                entry,
                chain: Some(sc.chain.clone()),
            };
        }
        return WriteBack::Disabled {
            reason: sc.reason.unwrap_or("edit.writeback.nested"),
        };
    }
    if !d.caps_edit() {
        return WriteBack::Disabled {
            reason: "edit.writeback.unsupported",
        };
    }
    WriteBack::Enabled {
        archive: archive.to_path_buf(),
        entry,
        chain: None,
    }
}

/// 供 `runtime` 每 wake 调用：对账 + 执行（返回需投递的通知）。
///
/// 单独抽出来是因为 `runtime` 的信号批处理循环里没有 `&mut AppShell`
/// （只有 `App` 上下文），只能出一个自由函数。
pub(crate) fn reconcile_and_apply(
    data: &std::sync::Arc<std::sync::Mutex<AppData>>,
    tx: &std::sync::Arc<SignalTx>,
) {
    let actions = data
        .lock()
        .map(|mut d| d.reconcile_external_edits())
        .unwrap_or_default();
    for action in actions {
        let (notify, job) = apply_save_action(data, Some(action));
        if let Some(ev) = notify {
            let _ = tx.try_send(VmSignal::Notify(ev));
        }
        if let Some(job_id) = job {
            // 进度窗（与压缩/解压同款形态）：写回可能是组合链全量重压/大镜像就地写
            let _ = tx.try_send(VmSignal::OpenProgress(job_id));
        }
    }
}
