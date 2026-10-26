//! 工具栏业务分发（T-GUI T11 自 app.rs 拆分：注册表写/任务提交/文件选择）。

use gpui_kit::{App, Window};

use super::AppShell;
use crate::ui::contracts::{ExtractMode, MainTab, ToolbarAction};

/// 解压方式的落点规划（**唯一** `dest`/`smart` 派生点，T-BAR §3.4）。
///
/// 与右键动词家族（`runtime.rs` 的 `LaunchIntent`：extract-here / auto-extract /
/// extract-dir）逐条对齐——两套宿主（直接模式进程 vs 主界面工具栏）**共用同一条
/// 语义**，故规则只写在这里：
///
/// | 方式 | dest | smart |
/// |---|---|---|
/// | 自动解压 | 档案所在目录 | `Some(true)`（= 「智能解压」，强制覆盖配置） |
/// | 解压到当前文件夹 | 档案所在目录 | `None`（读配置） |
/// | 解压到「压缩文件名」文件夹 | 档案目录 / 去掉扩展名的档案名 | `None` |
/// | 解压到桌面 | 桌面目录 | `None` |
/// | 自动解压到桌面 | 桌面目录 | `Some(true)` |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractPlan {
    /// 目标目录
    pub dest: std::path::PathBuf,
    /// `Some(true)` = 强制智能解压；`None` = 读配置
    pub smart: Option<bool>,
}

/// 方式 → 落点（纯函数；`desktop = None` = 取不到桌面目录 → `Err`，
/// 由调用方置灰菜单项/报错，**不**回落到别处）。
pub fn extract_plan(
    mode: ExtractMode,
    archive: &std::path::Path,
    desktop: Option<&std::path::Path>,
) -> Result<ExtractPlan, String> {
    let parent = || {
        archive
            .parent()
            // 相对路径（"demo.zip"）的 `parent()` 是 `Some("")`：空目录不是可用
            // 落点（解到进程 CWD = 用户没预期的地方），故与「无父目录」同判。
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .ok_or_else(|| "档案无父目录".to_string())
    };
    let desktop_dir = || {
        desktop
            .map(|p| p.to_path_buf())
            .ok_or_else(|| "取不到桌面目录".to_string())
    };
    Ok(match mode {
        ExtractMode::Auto => ExtractPlan {
            dest: parent()?,
            smart: Some(true),
        },
        ExtractMode::Here => ExtractPlan {
            dest: parent()?,
            smart: None,
        },
        ExtractMode::Dir => {
            let stem = archive
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "archive".into());
            ExtractPlan {
                dest: parent()?.join(stem),
                smart: None,
            }
        }
        ExtractMode::Desktop => ExtractPlan {
            dest: desktop_dir()?,
            smart: None,
        },
        ExtractMode::AutoDesktop => ExtractPlan {
            dest: desktop_dir()?,
            smart: Some(true),
        },
    })
}

impl AppShell {
    /// 档案类动作**把浏览页拉回前台**：浏览页签被用户关掉后，这些动作（都是
    /// 对着档案做的）若还落在别的页上，界面会「点了没反应」（关掉的页没有
    /// 别的入口回来——只能再开一次档案）。开关与勾选类动作（Settings 自身、
    /// 视图样式、工具栏样式）不在此列。
    ///
    /// 两个入口（`dispatch_toolbar` / `dispatch_toolbar_ui`）共用，幂等。
    fn ensure_browse_visible(&mut self, action: &ToolbarAction, cx: &mut App) {
        if matches!(
            action,
            ToolbarAction::Extract
                | ToolbarAction::ExtractAll
                | ToolbarAction::ExtractMode(_)
                | ToolbarAction::AddFiles
                | ToolbarAction::Delete
                | ToolbarAction::Preview
                | ToolbarAction::ToggleColumn(_)
                | ToolbarAction::Refresh
        ) && self.state.read(cx).browse_hidden
        {
            self.state.update(cx, |s, cx| {
                s.browse_hidden = false;
                if s.tab != MainTab::Browse {
                    s.tab = MainTab::Browse;
                }
                cx.notify();
            });
        }
    }

    /// 工具栏动作入口（UI 版；**渲染侧监听器一律走这里**）。
    ///
    /// 「添加」「删除」要装配对话框并把任务交给实体上下文（`cx.spawn_in` +
    /// `window.open_dialog` + `shell.update`），只能拿 `&mut Context<Self>`，
    /// 故在 `dispatch_toolbar`（`&mut App`）之外单开一层；其余动作原样下传。
    pub fn dispatch_toolbar_ui(
        &mut self,
        window: &mut Window,
        cx: &mut gpui_kit::Context<Self>,
        action: ToolbarAction,
    ) {
        self.ensure_browse_visible(&action, cx);
        match action {
            ToolbarAction::AddFiles => self.begin_add_files(window, cx),
            ToolbarAction::Delete => self.begin_delete_files(window, cx),
            other => self.dispatch_toolbar(window, cx, other),
        }
    }

    pub fn dispatch_toolbar(&mut self, window: &mut Window, cx: &mut App, action: ToolbarAction) {
        self.ensure_browse_visible(&action, cx);
        match action {
            ToolbarAction::Open => {
                // 打开/浏览：rfd 原生选择器（阻塞调用经 smol::unblock 离 UI 线程，
                // T-G52 退役旧 gpui 内置路径弹窗 oneshot 管道）。锁不跨 await：
                // engine 在 spawn 的同步闭包内从 data.lock() 取出。
                let data = self.data.clone();
                let tx = self.signal_tx.clone();
                cx.spawn(async move |cx| {
                    let Some(data) = data else {
                        return;
                    };
                    let picker_data = data.clone();
                    let picked = cx
                        .background_executor()
                        .spawn(async move {
                            smol::unblock(move || {
                                let engine = picker_data.lock().ok().map(|d| d.engine.clone());
                                engine.and_then(|engine| {
                                    crate::app::file_picker::pick_archive(&engine)
                                })
                            })
                            .await
                        })
                        .await;
                    let Some(path) = picked else {
                        return; // 取消：静默
                    };
                    // 统一入口：后台 browse（不卡界面）+ 按需「正在打开」窗
                    // （R-GUI2-06）；默认不展开（R-GUI2 #5，展开态经快照保留）。
                    let data_open = data.clone();
                    let path_open = path.clone();
                    let task = cx.update(|cx| {
                        crate::ui::shell::open_archive_task(
                            cx,
                            data_open.clone(),
                            path_open.clone(),
                            true,
                        )
                    });
                    let res = task.await;
                    if let Some(tx) = tx {
                        let _ = tx.try_send(if res.is_ok() {
                            crate::viewmodel::VmSignal::EntriesChanged
                        } else {
                            crate::viewmodel::VmSignal::DialogChanged
                        });
                        // T-MENU4：MRU 变化 → 菜单快照重建
                        let _ = tx.try_send(crate::viewmodel::VmSignal::MenuEpochBump);
                    }
                })
                .detach();
            }
            ToolbarAction::Extract | ToolbarAction::ExtractAll => {
                // 解压接线（R-GUI2 需求 #6；照 Open 分支三段式：后台选择器 →
                // 后台提交 → 回投信号）。选中空 → 回退全部（Bandizip 语义）。
                let (archive, names) = {
                    let Some(data) = self.data.as_ref() else {
                        tracing::warn!("解压：当前没有打开的档案");
                        return;
                    };
                    let Ok(d) = data.lock() else {
                        return;
                    };
                    match &d.archive {
                        Some(a) => (a.clone(), d.main.selection_names()),
                        None => {
                            tracing::warn!("解压：当前没有打开的档案");
                            return;
                        }
                    }
                };
                let sel = match action {
                    ToolbarAction::ExtractAll => crate::viewmodel::dialog_vm::SelectionArg::All,
                    _ if names.is_empty() => {
                        tracing::info!("解压：无选中条目，回退为全部解压");
                        crate::viewmodel::dialog_vm::SelectionArg::All
                    }
                    _ => crate::viewmodel::dialog_vm::SelectionArg::Names(names),
                };
                // 不切任务页签（R-GUI4 #3：留在浏览页；进度经状态栏任务胶囊
                // 呈现，用户可自行点「任务中心」查看明细）
                let data = self.data.clone();
                cx.spawn(async move |cx| {
                    let Some(data) = data else {
                        return;
                    };
                    // ② 后台目录选择（pick_directory 阻塞调用必须离 UI 线程）
                    let picked = cx
                        .background_executor()
                        .spawn(async {
                            smol::unblock(crate::app::file_picker::pick_directory).await
                        })
                        .await;
                    let Some(dest) = picked else {
                        return; // 取消：静默
                    };
                    // ③ 后台提交：config [extract] 填默认 → build_spec → submit → 泵
                    let archive_disp = archive.display().to_string();
                    let dest_disp = dest.display().to_string();
                    // 提交走 `smol::unblock`：`submit_extract_locked` 要读 config +
                    // 注册任务行，`join()` 会让**本来就在 UI 线程上**的本任务干等
                    // （用户口径 2026-09-24：这类重活不得占 UI 线程）。
                    let data_submit = data.clone();
                    let (archive_submit, dest_submit, sel_submit) =
                        (archive.clone(), dest.clone(), sel.clone());
                    let signals = cx
                        .background_executor()
                        .spawn(async move {
                            smol::unblock(move || {
                                // 工具栏解压：smart 取配置（右键「智能解压」才强制 true）
                                submit_extract_locked(
                                    &data_submit,
                                    &archive_submit,
                                    &dest_submit,
                                    &sel_submit,
                                    None,
                                )
                            })
                            .await
                        })
                        .await;
                    match signals {
                        Ok(job_id) => {
                            tracing::info!("提交解压任务 {job_id}: {archive_disp} -> {dest_disp}");
                            // T-PW5 + 设计稿 11 §4.2：解压进度窗**挂起**——
                            // 首个非 JobStarted 事件（密码已应答）由泵开；
                            // force=false 维持原 `open_progress_window` 配置语义
                            if let Ok(app) = data.lock()
                                && let Ok(mut j) = app.jobs.lock()
                            {
                                j.register_progress_open(job_id, false);
                            }
                        }
                        Err(e) => tracing::error!("解压任务提交失败: {e}"),
                    }
                })
                .detach();
            }
            ToolbarAction::OpenRecent(path) => {
                // 「打开▾」：与工具栏「打开」同一条打开链，只是路径来自 MRU
                //（不弹选择器）。`show_window=true` 与打开链一致。
                let data = self.data.clone();
                let tx = self.signal_tx.clone();
                let path = std::path::PathBuf::from(path);
                cx.spawn(async move |cx| {
                    let Some(data) = data else {
                        return;
                    };
                    let data_open = data.clone();
                    let path_open = path.clone();
                    let task = cx.update(|cx| {
                        crate::ui::shell::open_archive_task(
                            cx,
                            data_open.clone(),
                            path_open.clone(),
                            true,
                        )
                    });
                    let res = task.await;
                    if let Some(tx) = tx {
                        let _ = tx.try_send(if res.is_ok() {
                            crate::viewmodel::VmSignal::EntriesChanged
                        } else {
                            crate::viewmodel::VmSignal::DialogChanged
                        });
                        // MRU 次序可能变化 → 菜单与「打开▾」都需要重建
                        let _ = tx.try_send(crate::viewmodel::VmSignal::MenuEpochBump);
                    }
                })
                .detach();
            }
            ToolbarAction::ExtractMode(mode) => {
                // 「解压▾」：整档（`SelectionArg::All`）解到方式规定的落点。
                // 与平按钮不同——**不弹目录选择器**（落点由方式决定）。
                let Some(data) = self.data.as_ref() else {
                    tracing::warn!("解压：当前没有打开的档案");
                    return;
                };
                let archive = {
                    let Ok(d) = data.lock() else {
                        return;
                    };
                    match d.archive.clone() {
                        Some(a) => a,
                        None => {
                            tracing::warn!("解压：当前没有打开的档案");
                            return;
                        }
                    }
                };
                let desktop = hzip_platform::dirs::desktop_dir();
                let plan = match extract_plan(mode, &archive, desktop.as_deref()) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!("解压方式 {mode:?} 不可用: {e}");
                        return;
                    }
                };
                let (dest, smart) = (plan.dest, plan.smart);
                let data = self.data.clone();
                let archive_disp = archive.display().to_string();
                let dest_disp = dest.display().to_string();
                cx.spawn(async move |cx| {
                    let Some(data) = data else {
                        return;
                    };
                    let data_submit = data.clone();
                    let (archive_submit, dest_submit) = (archive.clone(), dest.clone());
                    let signals = cx
                        .background_executor()
                        .spawn(async move {
                            smol::unblock(move || {
                                submit_extract_locked(
                                    &data_submit,
                                    &archive_submit,
                                    &dest_submit,
                                    &crate::viewmodel::dialog_vm::SelectionArg::All,
                                    smart,
                                )
                            })
                            .await
                        })
                        .await;
                    match signals {
                        Ok(job_id) => {
                            tracing::info!(
                                "提交解压任务 {job_id}（{mode:?}）: {archive_disp} -> {dest_disp}"
                            );
                            // 与平按钮同款：进度窗挂起，待密码应答后由泵开窗
                            if let Ok(app) = data.lock()
                                && let Ok(mut j) = app.jobs.lock()
                            {
                                j.register_progress_open(job_id, false);
                            }
                        }
                        Err(e) => tracing::error!("解压任务提交失败: {e}"),
                    }
                })
                .detach();
            }
            ToolbarAction::ToggleColumn(key) => {
                // 「查看▾」：列显隐——**必须**走 `apply_setting(Columns)` 五步流
                //（store → 落盘 → effect → SettingApplied 信号 → 设置面板同步），
                // 禁止旁路写 Global（T-SET 修过的旧缺陷）。
                let Some(data) = self.data.as_ref() else {
                    return;
                };
                if let Ok(mut d) = data.lock() {
                    let next =
                        crate::app::settings_registry::toggle_column(&d.settings.columns, &key);
                    if next != d.settings.columns {
                        d.apply_setting(
                            crate::viewmodel::SettingId::Columns,
                            &crate::app::settings_registry::SettingValue::Columns(next),
                        );
                    }
                }
                if let Some(tx) = self.signal_tx.as_ref() {
                    let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
                }
            }
            ToolbarAction::ClearMru => {
                // 「打开▾」尾部：清空最近记录（与菜单同名项同一动作）
                let Some(data) = self.data.as_ref() else {
                    return;
                };
                if let Ok(mut d) = data.lock() {
                    d.clear_mru();
                }
                if let Some(tx) = self.signal_tx.as_ref() {
                    let _ = tx.try_send(crate::viewmodel::VmSignal::MenuEpochBump);
                }
            }
            ToolbarAction::AddFiles | ToolbarAction::Delete => {
                // 这两个动作要 `Context<Self>`（装配确认对话框），由
                // [`Self::dispatch_toolbar_ui`] 拦截。走到这里说明调用方绕过了它
                // ——显式告警，不静默（T-BAR2：旧的 `on_toolbar` 兜底黑洞教训）。
                tracing::warn!("{action:?} 需实体上下文：请从 dispatch_toolbar_ui 进入");
            }
            ToolbarAction::TestIntegrity => {
                // T-MENU1 修正（菜单暴露的 T-GUI 占位缺陷）：原实现只切页签，
                // 从不提交测试任务——用户看到的是「点了没反应」。现真实提交
                // JobKind::Test（当前档案）→ 进度窗/任务中心行由既有泵链承接。
                let archive = {
                    let Some(data) = self.data.as_ref() else {
                        tracing::warn!("测试完整性：当前没有打开的档案");
                        return;
                    };
                    let Ok(d) = data.lock() else {
                        return;
                    };
                    match &d.archive {
                        Some(a) => a.clone(),
                        None => {
                            tracing::warn!("测试完整性：当前没有打开的档案");
                            return;
                        }
                    }
                };
                let mut spec =
                    hzip_core::jobs::JobSpec::new(hzip_core::jobs::JobKind::Test, archive.clone());
                spec.dest = None;
                let data_handle = self.data.clone();
                let submitted = {
                    let Some(data) = data_handle.as_ref() else {
                        return;
                    };
                    let Ok(d) = data.lock() else {
                        return;
                    };
                    // 投递走唯一出口（已持锁形态）；失败给用户可读日志
                    match d.jobs.lock() {
                        Ok(mut jobs) => match jobs.submit(&d.engine, spec) {
                            Ok(job_id) => {
                                let jobs2 = d.jobs.clone();
                                let router = d.router.clone();
                                if let Some(tx) = d.signal_tx.clone() {
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::JobsChanged);
                                    // 用户可见出口（2026-09-15 用户报障）：测试任务
                                    // 无参数窗承载——与其他任务同款经 OpenProgress
                                    // 开进度窗（强制显示），否则过程只存在于任务中心。
                                    let _ = tx
                                        .try_send(crate::viewmodel::VmSignal::OpenProgress(job_id));
                                    crate::app::pump_host::spawn_job_pump(
                                        jobs2, router, job_id, tx,
                                    );
                                }
                                tracing::info!("提交完整性测试任务 {job_id}");
                                Some(job_id)
                            }
                            Err(e) => {
                                tracing::error!("测试任务提交失败: {e}");
                                None
                            }
                        },
                        Err(_) => None,
                    }
                };
                // 无论成败都切任务中心（成功看进度；失败有日志——后续可挂通知）
                if submitted.is_some() {
                    self.switch_tab(cx, MainTab::Jobs);
                }
            }
            ToolbarAction::Settings => {
                // 设置**页签**（2026-09-23：不再开弹层——用户口径「少一个弹窗、
                // 空间利用更好」）。面板实体懒建：已开则只切过去（保留页内态）。
                // 事件上行口径与弹层版**完全一致**（R-GUI2 七分类；编辑 → 落盘 →
                // 生效统一走 `AppData::consume_dialog_event` 五步流程）。
                // R-GUI4 #4：window 来自 listener 现场回调（SelectState 装配要用）。
                if self.settings_panel.is_some() {
                    self.activate_settings_tab(cx);
                    return;
                }
                let Some(data) = self.data.clone() else {
                    return;
                };
                let engine = match data.lock() {
                    Ok(d) => d.engine.clone(),
                    Err(_) => return,
                };
                let candidates = crate::ui::contracts::associable_extensions(&engine);
                let app = data.clone();
                let on_event = move |event: crate::ui::contracts::DialogEvent| {
                    use crate::ui::contracts::DialogEvent;

                    // 上下文菜单「应用」：**先**在主线程提交草稿（落盘一次），
                    // 算出要装什么，再交给后台线程写操作系统。
                    // 勾选本身只改草稿（`Commit::Manual`），故此处是唯一的 OS 写入点。
                    //
                    // 走查 #13：①入口**无条件**留痕——后台失败只在日志里的话，
                    // 用户面对的是「点了没反应」（状态行停『尚未注册』）；②锁
                    // 中毒**恢复**而非跳过——某后台线程持锁 panic 后锁永久中毒，
                    // 旧写法 `if let Ok(...)` 会静默跳过提交/消费/安装全链路
                    //（实测：日志里连一条失败记录都没有）。
                    let ctx_apply = matches!(event, DialogEvent::SettingsContextMenuApply);
                    if ctx_apply {
                        tracing::info!("右键菜单应用：提交配置并转后台安装");
                    }
                    if ctx_apply
                        && let Some(mut d) = lock_recovered(&data)
                        && !d.commit_ctx_draft()
                    {
                        // 无草稿 = 用户没改过勾选：仍继续（幂等重装，
                        // 用于修复被外部清理掉/首次安装的注册），但日志区分。
                        tracing::info!("右键菜单应用：无草稿改动，按当前配置幂等安装");
                    }

                    // 上下文菜单「清除」：清空勾选 + 丢弃草稿（config 落盘），
                    // 再后台撤销系统侧全部痕迹。
                    if matches!(event, DialogEvent::SettingsContextMenuClear)
                        && let Some(mut d) = lock_recovered(&data)
                    {
                        d.clear_ctx_menu_selection();
                    }

                    // 密码门（设计稿 11 §4.3 + 用户口径 2026-09-28）的**密码提交/
                    // 取消**在这里截住：头加密档案转后台重开、条目/容器门转后台预检。
                    // 与提示窗共用同一入口（`handle_dialog_event` 的门分支）。
                    if crate::ui::shell::prompt::handle_gate_event(&data, &event) {
                        return;
                    }
                    // 事件上行：值变更/复位 → AppData 五步流程（落盘在 AppData 内）
                    if let Some(mut d) = lock_recovered(&data) {
                        d.consume_dialog_event(&event);
                    }

                    // 上下文菜单勾选：**只改草稿，不碰操作系统**（用户明确要求）。
                    //
                    // 语义 = 「编辑 → 应用」：勾选/取消只进内存草稿（`Commit::Manual`
                    // → `stage_ctx_draft`，连配置都不落盘），面板随即显示「有未应用的
                    // 更改」；点「应用到系统」才 `commit_ctx_draft()`（落盘）＋装系统，
                    // 点「从系统清除」才撤系统侧痕迹。见 `ctx_menu_os_action`。
                    //
                    // 历史两版都走过弯路：① 勾选只改草稿但**毫无提示**（用户以为点了
                    // 没生效）；② 改成勾选即时同步 OS（用户想先改配置、再决定何时应用
                    // ——未点应用就已写系统且无处撤销）。现在 ① 的提示由面板的
                    // `has_pending_ctx_changes()` → 「有未应用的更改」承担，② 不再做。

                    // 关联应用/解除：winreg 阻塞调用必须后台线程（R2-03 §3.5；
                    // HKCU 免管理员——不写 HKLM）。仅「应用/解除」按钮触发；扩展名
                    // 勾选只落盘（[ui].assoc_formats），不即时写注册表。
                    // T-CM：解压/压缩 verb 已从 assoc 剥离——本分支只做文件关联。
                    let assoc_event = match event {
                        DialogEvent::SettingsApplyAssoc => Some(true),
                        DialogEvent::SettingsRemoveAssoc => Some(false),
                        _ => None,
                    };
                    // 「打开系统默认应用设置页」（批量入口）：同样后台执行（拉起设置应用）
                    let open_app_settings =
                        matches!(event, DialogEvent::SettingsAssocOpenAppSettings);

                    // 上下文菜单事件同样在后台线程执行（注册表/文件系统/PowerShell）
                    let ctx_event = ctx_menu_os_action(&event);

                    if assoc_event.is_none() && ctx_event.is_none() && !open_app_settings {
                        return;
                    }

                    let data = data.clone();
                    std::thread::spawn(move || {
                        if let Some(apply) = assoc_event {
                            run_assoc(apply, &data);
                        }
                        if let Some(action) = ctx_event {
                            run_context_menu(action, &data);
                        }
                        if open_app_settings {
                            run_open_app_settings(&data);
                        }
                    });
                };
                let panel = crate::ui::views::settings_layout::SettingsPanel::new_entity(
                    window,
                    cx,
                    app,
                    candidates,
                    std::rc::Rc::new(on_event),
                );
                // 面板事件 → 立即把「影响页签集的设置」同步进 ShellState。
                //
                // 为什么需要（用户实测缺陷 2026-09-24）：在设置里打开「显示任务
                // 中心」后页签栏里**不出现**该页签——`show_jobs` 平时只靠异步信号
                // 链（AppData → 泵批次 → 快照 → ShellState.apply）回流，面板自身的
                // 重画不带页签。这里订阅面板：它一 notify（设置项变更即 notify），
                // 就地把 `show_jobs` 从 AppData 同步过来，与 `switch_tab`/`close_tab`
                // 的「本地先行」同口径（事实源仍是 AppData，不另存状态）。
                let data_sync = self.data.clone();
                let state_sync = self.state.clone();
                // `App` 层的 `observe` 回调是「(被观察实体, &mut App)」两参（壳层自身
                // 拿不到 `&mut Self`）——故捕获 `ShellState` 实体直更；它一变，挂在其
                // 上的壳层订阅（`observe_state`）自然重渲。
                let sub = cx.observe(&panel, move |_, cx: &mut App| {
                    let Some(show_jobs) = data_sync
                        .as_ref()
                        .and_then(|d| d.lock().ok().map(|d| d.settings.show_jobs))
                    else {
                        return;
                    };
                    state_sync.update(cx, |s, cx| {
                        if s.show_jobs != show_jobs {
                            s.show_jobs = show_jobs;
                            cx.notify();
                        }
                    });
                });
                self._observe_settings_panel = Some(sub);
                self.settings_panel = Some(panel);
                self.activate_settings_tab(cx);
                tracing::info!("设置页签已装配");
            }
            ToolbarAction::Refresh => {
                // 刷新（重读档案）：审计 B20 修缺陷——旧实现落到下面的
                // `handlers.on_toolbar` 兜底（默认 no-op）⇒ 菜单/工具栏「刷新」
                // 点了没反应。现在真重读：走 `run_open_flow`（后台线程 + 进度窗 +
                // 任务中心留痕），与「打开档案」同一条路。
                let Some(data) = self.data.clone() else {
                    return;
                };
                let path = data
                    .lock()
                    .ok()
                    .and_then(|d| d.main.source_path().map(|p| p.to_path_buf()));
                let Some(path) = path else {
                    tracing::info!("刷新：当前没有打开的档案");
                    return;
                };
                let label = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let path_disp = path.display().to_string();
                // 换会话发起（刷新也是换会话：同目标连点忽略；不同目标顶替并关旧窗）
                let Some(epoch) = super::start_session_switch(
                    cx,
                    &data,
                    crate::app::state::SwitchTarget {
                        key: path_disp.clone(),
                        label: label.clone(),
                    },
                ) else {
                    return;
                };
                let work = {
                    let data = data.clone();
                    let path = path.clone();
                    move |_detail: &crate::ui::views::open_progress::OpenDetail| {
                        // 锁内摘出会话浏览器（重活不持锁）——`refresh` 要 `&mut main`
                        let taken = data
                            .lock()
                            .map_err(|_| "state 锁中毒".to_string())?
                            .main
                            .take_browser();
                        let engine = data
                            .lock()
                            .map_err(|_| "state 锁中毒".to_string())?
                            .engine
                            .clone();
                        let Some(browser_old) = taken else {
                            return Err(crate::tr("msg.copy_no_archive"));
                        };
                        // 锁外：重开（与 open 同口径，组合档延迟展开）
                        let opened = engine.browse(
                            hzip_format::Source::File(path.clone()),
                            &hzip_format::OpenOptions {
                                defer_composite: true,
                                ..Default::default()
                            },
                        );
                        match opened {
                            Ok(browser) => {
                                let mut d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
                                // 落账前比对代数：被更新的换会话顶替 → 丢弃结果
                                if !d.session_switch_is_current(epoch) {
                                    return Err(super::SESSION_SUPERSEDED.to_string());
                                }
                                // 注意：**不清嵌套栈**——刷新的是当前那一层，
                                // 父层帧要留着（`..` 还要能逐层退回）
                                d.main
                                    .install_browser(browser, &path)
                                    .map_err(|e| super::hz_reason(&e))?;
                                d.archive = Some(path.clone());
                                drop(d);
                                let _ = browser_old;
                                Ok(())
                            }
                            Err(e) => {
                                // 失败：旧浏览器放回（会话不因刷新失败而空掉）
                                if let Ok(mut d) = data.lock() {
                                    d.main.restore_browser(browser_old);
                                }
                                Err(super::hz_reason(&e))
                            }
                        }
                    }
                };
                let labels = crate::ui::views::open_progress::OpLabels {
                    title_key: "openprogress.title_refresh",
                    running_key: "openprogress.refreshing",
                    done_key: "openprogress.refreshed",
                    record_kind: "refresh",
                };
                let tx = self.signal_tx.clone();
                let task = super::run_open_flow(
                    cx,
                    data.clone(),
                    super::OpenFlow {
                        label,
                        path: path_disp,
                        show_window: true,
                        labels,
                        epoch,
                        toast_on_error: true,
                        immediate_show: false,
                    },
                    work,
                );
                let data_end = data.clone();
                cx.spawn(async move |_cx| {
                    let res = task.await;
                    if let Ok(mut d) = data_end.lock() {
                        d.end_session_switch(epoch);
                    }
                    if let Some(tx) = tx
                        && res.as_ref().err().map(String::as_str) != Some(super::SESSION_SUPERSEDED)
                    {
                        let _ = tx.try_send(if res.is_ok() {
                            crate::viewmodel::VmSignal::EntriesChanged
                        } else {
                            crate::viewmodel::VmSignal::DialogChanged
                        });
                    }
                })
                .detach();
            }
            ToolbarAction::Preview => {
                // 预览 = 右键「打开方式…」同义（用户口径 2026-09-29）：物化后交
                // 系统询问对话框；加密条目先过密码门（设计稿 11 §9.7）。
                // 与右键菜单项 `BrowseCommand::OpenWith` **同一条**实现。
                self.open_selection_entry_with_picker(cx);
            }
        }
        // 注：**没有**兜底分支——`ToolbarAction` 新增变体即编译失败（T-BAR2：
        // 旧的 `handlers.on_toolbar` 兜底是 no-op，`Compress`/`Preview` 两个按钮
        // 因此静默死了很久，实测缺陷）。
    }

    /// 打开并激活设置页签（实体已建/刚建共用；幂等）。
    ///
    /// 次序要紧：先置 `settings_open`（页签存在）再 `switch_tab`——切换门槛
    /// 就是「页签是否存在」（`ShellState::tab_present`）。
    fn activate_settings_tab(&mut self, cx: &mut App) {
        self.state.update(cx, |s, cx| {
            s.settings_open = true;
            cx.notify();
        });
        self.switch_tab(cx, MainTab::Settings);
    }
}

// —— 解压提交（工具栏 + argv 管线共用）——

/// 打开档案后按 dest 派生器提交解压（后台线程调用；R2-05 argv 管线）。
///
/// smart 语义：--auto-extract 强制 true；其余读 config（`force_smart=false`）。
pub(super) fn open_then_extract<F>(
    data: &std::sync::Mutex<crate::app::state::AppData>,
    path: &std::path::Path,
    dest_of: F,
    smart: Option<bool>,
) -> Result<u64, String>
where
    F: FnOnce(&std::path::Path) -> Result<std::path::PathBuf, String>,
{
    let dest = dest_of(path)?;
    open_then_extract_with_dest(data, path, dest, smart)
}

/// 打开档案后解压到指定 dest（共享 submit_extract_locked）。
pub(super) fn open_then_extract_with_dest(
    data: &std::sync::Mutex<crate::app::state::AppData>,
    path: &std::path::Path,
    dest: std::path::PathBuf,
    smart: Option<bool>,
) -> Result<u64, String> {
    {
        // 打开档案（失败即中止——解压前提）
        let mut d = data.lock().map_err(|_| "state 锁中毒".to_string())?;
        if d.archive.as_deref() != Some(path) {
            d.busy = true;
            let engine = d.engine.clone();
            let opened = d.main.open(&engine, path);
            d.busy = false;
            match opened {
                Ok(()) => {
                    d.archive = Some(path.to_path_buf());
                    // 右键解压家族（解压到当前目录/自动解压/解压到…）同样把档案装进
                    // 浏览会话 ⇒ 同属「打开档案」，一样进最近记录（用户口径）。
                    d.record_mru_open(path);
                }
                Err(hzip_error::HzipError::Password { .. }) => {
                    // 头加密档案：不预开浏览会话（避免与任务内的密码问询形成
                    // 双弹窗）。解压不依赖预打开——job 内 open_archive_retry 自问
                    // 密码（单一密码弹窗；进度窗时序由挂起机制保证，设计稿 11 §4.2）。
                }
                Err(e) => return Err(super::errors::open_failure_text(path, &e)),
            }
        }
    }
    submit_extract_locked(
        data,
        path,
        &dest,
        &crate::viewmodel::dialog_vm::SelectionArg::All,
        smart,
    )
}

/// 进度窗口开窗装配（T-PW5；工具栏/argv 意图/任务中心重开共用入口）。
///
/// config `[jobs].progress_window` 开 → 取行快照 + 装取消闭包（与任务中心
/// 取消同一通道：`jobs.cancel`）+ 装复选框回调（「完成后退出」走
/// `apply_setting` 五步流程——与设置页同一配置同一链路）→
/// `progress::open_progress_window`。
/// 主线程调用（`&mut App`——gpui 窗口创建亲和）；07 §4.4：AppData 触碰
/// 收口在 shell 层，views 只消费注入闭包。
pub(crate) fn open_progress_window(
    cx: &mut gpui_kit::App,
    data: &super::AppDataHandle,
    job_id: u64,
) {
    open_progress_window_with(cx, data, job_id, false)
}

/// 同上，但 `force = true` 时**无视** `[jobs].progress_window`。
///
/// 直接模式（右键「直接压缩/解压」）必须给用户一个可交互的输出窗口：它没有
/// 主界面兜底，配置把它关掉的话用户点完只会看到「什么都没发生」。
pub(crate) fn open_progress_window_with(
    cx: &mut gpui_kit::App,
    data: &super::AppDataHandle,
    job_id: u64,
    force: bool,
) {
    if !force && !hzip_config::Config::load().jobs.progress_window {
        return;
    }
    let row = data
        .lock()
        .ok()
        .and_then(|d| d.jobs.lock().ok().and_then(|j| j.row(job_id).cloned()));
    let Some(row) = row else {
        return; // 行不存在（已清除）
    };
    let data_c = data.clone();
    let data_exit = data.clone();
    crate::ui::views::progress::open_progress_window(
        cx,
        &row,
        move |id, cx| {
            if let Ok(d) = data_c.lock()
                && let Ok(mut jobs) = d.jobs.lock()
            {
                jobs.cancel(id);
            }
            let _ = cx;
        },
        move |checked, cx| {
            // 复选框 ↔ 设置页同一配置：走统一五步流程（store → save → effect
            // → SettingApplied 信号 → 设置面板即时同步）。
            if let Ok(mut d) = data_exit.lock() {
                d.apply_setting(
                    crate::viewmodel::SettingId::ExitOnFinish,
                    &crate::app::settings_registry::SettingValue::Bool(checked),
                );
            }
            let _ = cx;
        },
    );
}

/// 快捷解压参数（config `[extract]` 缺省 + 指定目标）。
///
/// `smart`：`Some` 覆盖配置——「智能解压」动词承诺的就是 smart 生效，不能
/// 让配置把它按下去（历史上留了「强制 true」的注释但代码读的是配置，属实现
/// 与承诺不符）；`None` = 尊重 `[extract].smart`。
///
/// `cfg` 由调用方注入（而非内部 `Config::load()`）：这样「参数窗 vs 快捷路径」
/// 的一致性测试能拿同一份 `Config::default()` 对拍，不受开发机配置影响。
pub(crate) fn quick_extract_vm(
    dest: Option<std::path::PathBuf>,
    smart: Option<bool>,
    cfg: &hzip_config::Config,
) -> crate::viewmodel::dialog_vm::ExtractDialogVm {
    use crate::viewmodel::dialog_vm::{ExtractDialogVm, codepage_from_str, conflict_from_str};
    ExtractDialogVm {
        dest,
        smart: smart.unwrap_or(cfg.extract.smart),
        best_effort: cfg.extract.best_effort,
        // 符号链接策略（跨平台档案释放场景；config [security].symlink）
        symlink: crate::viewmodel::dialog_vm::symlink_from_str(&cfg.security.symlink),
        conflict: conflict_from_str(&cfg.extract.conflict),
        codepage: codepage_from_str(cfg.extract.codepage.as_deref()),
        // 快捷路径无密码来源（config 无此项）：留空 → core 运行时询问链
        password: None,
        preview: Vec::new(),
    }
}

/// 任务密码的**会话缓存键**（用户口径 2026-09-29）。
///
/// 读档案类任务（解压/测试/编辑/转换）= **档案本体**——密码就是它的；
/// 压缩 = **产出档案**——参数窗里填的密码正是写给那个新档案的（建成后浏览/编辑
/// 它直接命中，不再问）。其它种类无密码语义。
pub(crate) fn password_cache_key(spec: &hzip_core::jobs::JobSpec) -> Option<std::path::PathBuf> {
    use hzip_core::jobs::JobKind;
    match spec.kind {
        JobKind::Compress => spec.out.clone(),
        JobKind::Extract
        | JobKind::Test
        | JobKind::EditSave
        | JobKind::EditBatch
        | JobKind::Convert => Some(spec.source.clone()),
        _ => None,
    }
}

/// **唯一解压提交点**：`vm` 已装配好（快捷路径由 [`quick_extract_vm`] 填，
/// 参数窗由用户填）。
///
/// 主界面工具栏 / argv 直接模式 / 参数窗三条入口都只经这里投递——参数**从哪来**
/// 可以不同，`JobSpec` 的构造与投递必须只有一份，否则两种工作模式的效果会漂移。
pub(crate) fn submit_extract(
    data: &std::sync::Mutex<crate::app::state::AppData>,
    archive: &std::path::Path,
    vm: &crate::viewmodel::dialog_vm::ExtractDialogVm,
    sel: &crate::viewmodel::dialog_vm::SelectionArg,
) -> Result<u64, String> {
    // 目标目录不存在则先创建（R-GUI3 #3：解压到新目录；参数窗手填目标同此）
    if let Some(dest) = &vm.dest
        && !dest.exists()
        && let Err(e) = std::fs::create_dir_all(dest)
    {
        return Err(format!("创建目标目录 {} 失败: {e}", dest.display()));
    }
    // 会话密码缓存复用（用户口径 2026-09-28）：参数窗没填密码时，用该档案已缓存
    // （且经预检验证）的密码作为 core 首个候选——成功即零弹窗，正是「该压缩包处于
    // 解密中状态，再解压不用再输密码」。无缓存则保持 None，走既有运行时询问链。
    let vm = {
        let mut vm = vm.clone();
        if vm.password.is_none()
            && let Ok(d) = data.lock()
            && let Some(pw) = d.cached_password(archive)
        {
            vm.password = Some(pw.clone());
        }
        vm
    };
    let spec = vm.build_spec(archive, sel).map_err(|e| e.to_string())?;
    submit_spec(data, spec)
}

// —— 编辑提交（T-MENU6：ChangeSet → EditBatch 后台任务）——

/// **唯一编辑提交点**（AppData 已持锁形态）：添加/删除/粘贴三条入口共用。
///
/// 收口在锁内是刻意的：唯一持有 `&mut AppData` 的调用者（确认对话框出口 /
/// 粘贴入口）已经站在数据面上，再走一次 `Mutex::lock` 只会自锁死。需要
/// 「未持锁」提交的调用方先加锁再调本函数（同 `submit_extract_locked` 惯例）。
///
/// `caps.edit` 前置由菜单禁用矩阵保证；此处再校验一次（防绕过菜单直达）。
/// 变更集为空 → Err（无变更不提交任务）。
pub(crate) fn submit_edit_batch_locked(
    d: &mut crate::app::state::AppData,
    archive: &std::path::Path,
    changes: hzip_format::ChangeSet,
    write_back: Option<hzip_core::services::WriteBackChain>,
) -> Result<u64, String> {
    if changes.changes.is_empty() {
        return Err("没有需要提交的变更".into());
    }
    if !d.caps_edit() {
        return Err(d.readonly_write_message());
    }
    let mut spec =
        hzip_core::jobs::JobSpec::new(hzip_core::jobs::JobKind::EditBatch, archive.to_path_buf());
    // 写回链的外层容器**可能是加密档案**（用户实测 2026-10-19：加密 `qcow2.zip`
    // 里删文件 ⇒ 外层重建读其它条目无密码 ⇒ 密码错）：密码取外层档案的会话缓存。
    spec.outer_password = write_back
        .as_ref()
        .and_then(|chain| d.cached_password(&chain.outer));
    spec.changes = Some(changes);
    spec.write_back = write_back;
    // 会话密码缓存（用户口径 2026-09-28「解密中」态）：该档案此前经密码门验证过的
    // 密码直接带上——已解锁的档案重命名/新建**不再重复问密码**。未缓存 → 服务层
    // 经 `JobRequest::Password` 弹窗询问（首次编辑加密档的正常路径）。
    spec.password = d.cached_password(archive);
    submit_spec_locked(d, spec)
}

/// 投递出口（已持 `&mut AppData` 版本；[`submit_spec`] 的锁内形态）。
pub(crate) fn submit_spec_locked(
    d: &mut crate::app::state::AppData,
    spec: hzip_core::jobs::JobSpec,
) -> Result<u64, String> {
    let engine = d.engine.clone();
    // 已知密码预登记（用户口径 2026-09-29）：任务**成功后**才进会话缓存，
    // 失败/取消丢弃——参数窗填过的密码也只需输一次。
    let known_pw = spec.password.clone();
    let known_key = password_cache_key(&spec);
    let job_id = d
        .jobs
        .lock()
        .map_err(|_| "jobs 锁中毒".to_string())?
        .submit(&engine, spec)
        .map_err(|e| e.to_string())?;
    if let (Some(key), Some(pw)) = (known_key, known_pw) {
        d.note_job_password(job_id, key, pw);
    }
    let jobs = d.jobs.clone();
    let router = d.router.clone();
    let tx = d.signal_tx.clone().ok_or("signal 通道未装配")?;
    crate::app::pump_host::spawn_job_pump(jobs, router, job_id, tx.clone());
    let _ = tx.try_send(crate::viewmodel::VmSignal::JobsChanged);
    Ok(job_id)
}

/// **唯一投递出口**：任务规格 → `jobs.submit` → 任务泵 → 回投刷新信号。
///
/// 压缩/解压/未来种类共用；返回 job_id 供日志，Err 带用户可读文案。
///
/// T-OPT 项10：本函数 = [`submit_spec_locked`] 的加锁包装（两处曾逐字重复
/// 「取 engine → 登记密码 → submit → 起泵 → 投信号」，改一处漏一处即隐患）。
fn submit_spec(
    data: &std::sync::Mutex<crate::app::state::AppData>,
    spec: hzip_core::jobs::JobSpec,
) -> Result<u64, String> {
    let Ok(mut d) = data.lock() else {
        return Err("state 锁中毒".into());
    };
    submit_spec_locked(&mut d, spec)
}

/// 锁内解压提交（R2-02 工具栏与 R2-05 argv 管线共用；后台线程调用）。
pub(crate) fn submit_extract_locked(
    data: &std::sync::Mutex<crate::app::state::AppData>,
    archive: &std::path::Path,
    dest: &std::path::Path,
    sel: &crate::viewmodel::dialog_vm::SelectionArg,
    smart: Option<bool>,
) -> Result<u64, String> {
    let vm = quick_extract_vm(
        Some(dest.to_path_buf()),
        smart,
        &hzip_config::Config::load(),
    );
    submit_extract(data, archive, &vm, sel)
}

// —— 压缩提交（右键「添加到…」/ argv 管线共用）——

/// 提交压缩任务（输出路径已定；调用方在**后台线程**）。
///
/// 与 [`submit_extract_locked`] 同构：锁 state → 装参数 → engine 提交 → 任务泵。
/// 参数取 `AppData::compress`（对话框与右键共用同一份参数模型，避免两套默认）。
/// **唯一压缩提交点**：`vm` 已装配好（快捷路径按格式派生，参数窗由用户填）。
///
/// 与 [`submit_extract`] 同款约定：入口可以多，构造与投递只有一份。
pub(crate) fn submit_compress(
    data: &std::sync::Mutex<crate::app::state::AppData>,
    inputs: &[std::path::PathBuf],
    vm: &crate::viewmodel::dialog_vm::CompressDialogVm,
) -> Result<u64, String> {
    if inputs.is_empty() {
        return Err("压缩输入为空".into());
    }
    let out = vm.out.clone().ok_or("未指定输出路径")?;
    // 输出目录可能是新建的（参数窗手填了尚不存在的目录）
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return Err(format!("创建输出目录 {} 失败: {e}", parent.display()));
    }
    let spec = vm.build_spec(inputs, &out).map_err(|e| e.to_string())?;
    submit_spec(data, spec)
}

/// 快捷压缩（固定格式 + 已定输出；右键「添加到 X.zip」）。
pub(crate) fn submit_compress_locked(
    data: &std::sync::Mutex<crate::app::state::AppData>,
    inputs: &[std::path::PathBuf],
    out: &std::path::Path,
    format: &str,
) -> Result<u64, String> {
    let vm = quick_compress_vm(format, out, &hzip_config::Config::load());
    submit_compress(data, inputs, &vm)
}

/// 快捷压缩参数（固定格式 + 已定输出；级别取配置缺省）。
///
/// 与 [`quick_extract_vm`] 同款：`cfg` 注入，便于与参数窗路径对拍一致性。
/// 级别必须取 `[defaults].level`——快捷入口没有调参处，硬编码 normal 会让
/// 「压缩设置」页的缺省形同虚设。
pub(crate) fn quick_compress_vm(
    format: &str,
    out: &std::path::Path,
    cfg: &hzip_config::Config,
) -> crate::viewmodel::dialog_vm::CompressDialogVm {
    use crate::viewmodel::dialog_vm::{CompressDialogVm, level_from_str};
    CompressDialogVm {
        format: Some(hzip_format::FormatId::from(format)),
        level: level_from_str(&cfg.defaults.level),
        out: Some(out.to_path_buf()),
        ..Default::default()
    }
}

// —— 设置页：文件关联 / 上下文菜单的系统侧执行（后台线程）——

/// 文件关联：把用户送到系统「设置 → 默认应用」页（`.{ext}` 需要在那儿选成 hzip）。
///
/// 「应用关联」后逐格式回读：哪些**没有**归我们（含原因），供弹窗如实告知。
///
/// 为什么要回读而不是只看 `apply_association` 的返回值：写注册表成功 ≠ 系统
/// 采用了我们（`UserChoice` 层可能清不掉、或被系统重建）。用户报障
/// 「点了应用、双击没变」正是这种情况——必须**逐项核对系统实况**再下结论。
fn assoc_not_taken(exts: &[String]) -> Vec<(String, hzip_platform::assoc::ExtAssocState)> {
    exts.iter()
        .map(|e| (e.clone(), hzip_platform::assoc::ext_state(e)))
        .filter(|(_, st)| !st.is_effective())
        .collect()
}

/// 「解除关联」后仍在归我们的格式（应清干净却没清掉的那些）。
fn assoc_still_ours(exts: &[String]) -> Vec<(String, hzip_platform::assoc::ExtAssocState)> {
    exts.iter()
        .map(|e| (e.clone(), hzip_platform::assoc::ext_state(e)))
        .filter(|(_, st)| st.is_effective())
        .collect()
}

/// 未生效项 → 弹窗正文（全部成功 → `None`）。
///
/// 文案逐项写明**原因与出路**：被系统「用户显式选择」压着（Windows）→ 说明那一层
/// 没清掉，请用「打开系统『默认应用』设置」；其余（系统里是别人/压根没默认）→
/// 说明写注册表失败或未刷新。
fn assoc_failure_message(
    failed: &[(String, hzip_platform::assoc::ExtAssocState)],
) -> Option<String> {
    if failed.is_empty() {
        return None;
    }
    let items: Vec<String> = failed
        .iter()
        .map(|(ext, st)| {
            let owner = st.owner_name().unwrap_or("(未知)");
            if st.blocked_by_other() {
                format!(".{ext}：系统里的「用户显式选择」仍指向 {owner}（受保护层未清除成功）")
            } else if st.effective.is_some() {
                format!(".{ext}：系统里仍是 {owner}")
            } else {
                format!(".{ext}：没有生效（系统里没有默认程序）")
            }
        })
        .collect();
    Some(crate::tr_with("assoc.failed.items", &[("items", &items.join("\n"))]).to_string())
}

/// 关联操作结束后的**弹窗决策**（纯逻辑，便于测试）。
///
/// - 写入报错 → **错误**级 + 原始原因（详情给全，用户/支持都能看懂）；
/// - 写入成功但系统没全接管 → **警告**级 + 逐项原因；
/// - 全部接管 → `None`（不打扰用户）。
fn assoc_result_dialog(
    err: Option<&str>,
    apply: bool,
    exts: &[String],
) -> Option<crate::viewmodel::message::MessageDialog> {
    use crate::viewmodel::message::MessageDialog;
    if let Some(e) = err {
        return Some(MessageDialog::error(crate::tr("assoc.failed.title"), e));
    }
    let failed = if apply {
        assoc_not_taken(exts)
    } else {
        assoc_still_ours(exts)
    };
    assoc_failure_message(&failed)
        .map(|items| MessageDialog::warning(crate::tr("assoc.failed.partial_title"), items))
}

/// 文件关联：应用 / 解除（`AssocSpec` 只携带关联语义，不再含 verb）。
fn run_assoc(apply: bool, data: &std::sync::Arc<std::sync::Mutex<crate::app::state::AppData>>) {
    let spec = {
        let Ok(d) = data.lock() else { return };
        hzip_platform::assoc::AssocSpec {
            extensions: d.settings.assoc.clone(),
            prog_id: PROG_ID,
            exe_path: std::env::current_exe().unwrap_or_default(),
        }
    };
    let res = if apply {
        hzip_platform::assoc::apply_association(&spec)
    } else {
        hzip_platform::assoc::remove_association(&spec)
    };
    match &res {
        Ok(()) if apply => {
            tracing::info!("文件关联已应用（{} 项扩展）", spec.extensions.len())
        }
        Ok(()) => tracing::info!("文件关联已解除"),
        Err(e) => tracing::error!("文件关联操作失败: {e}"),
    }
    // 失败原因上浮到状态行（同 `run_context_menu` 的走查 #13 结论）；成功清空
    // （`res` 后面还要用来决定弹窗正文，故先取出错误串）
    let err = res.as_ref().err().cloned();
    if let Ok(mut d) = data.lock() {
        d.assoc_error = err.clone();
    }
    // 系统实况已变：丢掉缓存——否则状态行会拿 TTL 内的旧结果，
    // 表现为「点了应用、状态行纹丝不动」（用户报障 2026-09-29）
    crate::ui::contracts::invalidate_assoc_status_cache();
    notify_settings_applied(data, crate::viewmodel::SettingId::Assoc);
    // **弹窗**告知结果（用户口径 2026-09-29：失败/没生效必须弹窗，不能只在状态行、
    // 更不能只在日志里）：写入报错 → 原样上浮；写入成功 → 逐项回读系统实况，
    // 有没接管的就列出来。
    let dialog = assoc_result_dialog(err.as_deref(), apply, &spec.extensions);
    if let Some(dialog) = dialog
        && let Ok(d) = data.lock()
        && let Some(tx) = &d.signal_tx
    {
        let _ = tx.try_send(crate::viewmodel::VmSignal::ShowMessage(dialog));
    }
}

/// 打开系统「默认应用设置」页（**批量**入口；失败原因上浮到状态行）。
fn run_open_app_settings(data: &std::sync::Arc<std::sync::Mutex<crate::app::state::AppData>>) {
    let res = hzip_platform::assoc::open_default_apps_settings();
    match &res {
        Ok(()) => tracing::info!("已打开系统默认应用设置"),
        Err(e) => tracing::error!("打开系统默认应用设置失败: {e}"),
    }
    if let Ok(mut d) = data.lock() {
        d.assoc_error = res.err();
    }
    crate::ui::contracts::invalidate_assoc_status_cache();
    notify_settings_applied(data, crate::viewmodel::SettingId::Assoc);
}

/// 上下文菜单：安装 / 清除，并把结果状态回写 AppData（状态行数据源）。
///
/// 走查 #13：失败**上浮到状态行**——旧实现失败只进日志，`query_ctx_menu_status`
/// 查回的仍是「未安装」，用户看到的是「点了应用、状态纹丝不动」且无从知道
/// 原因（如 Shell DLL 未随程序部署、未勾选任何项）。现失败时写
/// `Degraded { reason }`（状态行以警告色显示原因），成功路径照旧查系统实况。
fn run_context_menu(
    action: CtxOsAction,
    data: &std::sync::Arc<std::sync::Mutex<crate::app::state::AppData>>,
) {
    use hzip_platform::context_menu::backend;

    let spec = {
        let Some(d) = lock_recovered(data) else {
            return;
        };
        crate::ui::contracts::build_menu_spec(&d, std::env::current_exe().unwrap_or_default())
    };

    // 失败原因（Some = 失败）：回写阶段升格为 Degraded 状态
    let mut failure: Option<String> = None;
    match action {
        CtxOsAction::Install => match backend().install(&spec) {
            Ok(report) => tracing::info!(
                items = report.items,
                mode = ?report.mode,
                restart = report.restart_required,
                "右键菜单已安装"
            ),
            Err(e) => {
                tracing::error!("右键菜单安装失败: {e}");
                failure = Some(e.to_string());
            }
        },
        CtxOsAction::Uninstall => {
            if let Err(e) = backend().uninstall() {
                tracing::error!("右键菜单清除失败: {e}");
                failure = Some(e.to_string());
            }
        }
        // 恢复默认（T-SEL 后走查 #6）：先卸再装——注册表侧先回到干净态，
        // 再按恢复后的默认勾选重装；避免 OS 侧实际状态与恢复后配置不一致
        // （例如恢复把 ctx_extract/ctx_compress 恢复为默认、但注册表里
        // 仍是用户自定义的安装集）。
        CtxOsAction::Reinstall => {
            if let Err(e) = backend().uninstall() {
                tracing::warn!("右键菜单重装前清除失败（继续安装）: {e}");
            }
            match backend().install(&spec) {
                Ok(report) => tracing::info!(
                    items = report.items,
                    mode = ?report.mode,
                    restart = report.restart_required,
                    "右键菜单已按恢复后配置重装"
                ),
                Err(e) => {
                    tracing::error!("右键菜单重装失败: {e}");
                    failure = Some(e.to_string());
                }
            }
        }
    }

    // 系统实况已变：缓存失效（下一次查询重算 + 后台预热，审计 B7/B10）
    crate::ui::contracts::invalidate_assoc_status_cache();
    // 回写状态：成功 = 系统实况；失败 = Degraded（原因直达状态行）
    let status = failure.map_or_else(crate::ui::contracts::query_ctx_menu_status, |reason| {
        crate::app::state::CtxMenuStatus::Degraded { reason }
    });
    if let Some(mut d) = lock_recovered(data) {
        d.ctx_menu_status = Some(status);
        // 回投「已生效」信号 → 主线程泵重载已打开的面板。**必须有**：后台线程
        // 写 AppData 不会触发任何渲染，缺它就出现「清除完了状态行还写着已注册」
        // （实测）。面板 reload 时会优先消费这里的 Degraded（见
        // settings_layout::effective_ctx_status），成功态则重查系统实况。
        drop(d);
        notify_settings_applied(data, crate::viewmodel::SettingId::Assoc);
    }
}

/// AppData 锁（中毒恢复）：某线程持锁 panic 后 `lock()` 永久 Err——
/// 旧写法 `if let Ok(...)` 会把提交/消费/安装整条链路**静默**跳过
/// （走查 #13 实测症状：点击应用后日志一条记录都没有）。
/// AppData 是纯数据（无跨字段不变量），恢复优于静默瘫痪。
fn lock_recovered(
    data: &std::sync::Arc<std::sync::Mutex<crate::app::state::AppData>>,
) -> Option<std::sync::MutexGuard<'_, crate::app::state::AppData>> {
    Some(data.lock().unwrap_or_else(|e| e.into_inner()))
}

/// 后台系统侧效果（写注册表 / 装包）完成 → 回投 `SettingApplied`。
///
/// 后台线程直接写 `AppData` **不会**触发任何渲染：缺这一步就会出现「点『从系统
/// 清除』后注册表已干净、面板状态行仍写着『已注册 N 项』」（实测症状）。泵消费
/// 本信号后重载已打开的设置面板，状态行随之反映系统实况。
fn notify_settings_applied(
    data: &std::sync::Arc<std::sync::Mutex<crate::app::state::AppData>>,
    id: crate::viewmodel::SettingId,
) {
    if let Ok(d) = data.lock()
        && let Some(tx) = &d.signal_tx
    {
        let _ = tx.try_send(crate::viewmodel::VmSignal::SettingApplied(id));
    }
}

/// 右键菜单：事件 → 是否动操作系统（`Some(true)` 装、`Some(false)` 卸、`None` 不动）。
///
/// **只有「应用到系统」「从系统清除」进这里**。勾选/取消只写内存草稿
/// （`Commit::Manual` → `stage_ctx_draft`），面板以「有未应用的更改」提示待应用，
/// 绝不触碰注册表 / 稀疏包。这条契约是用户明确要求的工作方式：
/// 「应用」按钮点击前只改配置，不做系统应用。
///
/// （历史缺陷：曾在勾选时即时 `backend().sync()`，未点应用就已写系统。）
fn ctx_menu_os_action(event: &crate::ui::contracts::DialogEvent) -> Option<CtxOsAction> {
    match event {
        crate::ui::contracts::DialogEvent::SettingsContextMenuApply => Some(CtxOsAction::Install),
        crate::ui::contracts::DialogEvent::SettingsContextMenuClear => Some(CtxOsAction::Uninstall),
        // 恢复默认（T-SEL 后走查 #6）：配置已由 apply_reset 恢复默认——
        // OS 侧先卸再装，实际注册状态与恢复后配置保持一致
        crate::ui::contracts::DialogEvent::SettingsReset => Some(CtxOsAction::Reinstall),
        _ => None,
    }
}

/// OS 上下文菜单系统侧动作（`ctx_menu_os_action` 产物）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CtxOsAction {
    /// 安装（按当前配置构建 spec）
    Install,
    /// 全量卸载
    Uninstall,
    /// 先卸再装（恢复默认——与恢复后配置对齐）
    Reinstall,
}

/// 程序标识（`AssocSpec.prog_id`）。**单一来源在平台层**：
/// [`crate::ui::contracts::PROG_ID`] 就是 [`hzip_platform::assoc::PROG_ID`] 的
/// 再导出（右键菜单不用它；Linux/macOS 后端自持标识）。
use crate::ui::contracts::PROG_ID;

#[cfg(test)]
#[path = "tests/dispatch.rs"]
mod tests;
