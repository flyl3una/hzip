//! 增删/改名/新建文件操作（确认对话框出口、行内名称编辑）——自 `shell/mod.rs` 拆分。

use gpui_kit::prelude::*;
use gpui_kit::{Entity, Window};

use crate::ui::contracts::ToolbarAction;

use super::app_shell::AppShell;
use super::errors::unique_new_folder_name;

impl AppShell {
    /// 打开「添加文件…」流程（T-MENU6）：后台多选 → 组装 ChangeSet →
    /// 确认对话框（列条数 + 同名冲突提示）。
    ///
    /// 确认后经 `AppData::commit_edit_batch` 提交后台 `EditBatch` 任务
    /// （唯一编辑提交点；对话框出口见 `confirm_handler`）。
    pub fn begin_add_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.data.clone() else {
            return;
        };
        let shell = cx.weak_entity();
        cx.spawn_in(window, async move |_this, cx| {
            use gpui_kit::component::WindowExt as _;
            // ① 后台多选（rfd 阻塞调用必须离 UI 线程）
            let picked = cx
                .background_executor()
                .spawn(async { smol::unblock(crate::app::file_picker::pick_files).await })
                .await;
            if picked.is_empty() {
                tracing::info!("添加文件：用户取消选择");
                return;
            }
            tracing::info!(count = picked.len(), "添加文件：已选择文件");
            let proposed = {
                let Ok(d) = data.lock() else {
                    return;
                };
                // 「添加到档案内当前路径」（用户口径 2026-09-29）+ T-MENU8 粘贴
                // 同口径：目标名由 `paste_target_name` 组装（`current_dir` 前缀，
                // `/` 分隔防 Windows `\`），`append_changeset` 按目标全名判冲突。
                let cur = d.main.current_dir().to_string();
                let entries: Vec<(std::path::PathBuf, String)> = picked
                    .iter()
                    .filter_map(|p| {
                        let file = p.file_name()?.to_string_lossy().into_owned();
                        Some((p.clone(), crate::app::state::paste_target_name(&cur, &file)))
                    })
                    .collect();
                crate::app::state::append_changeset(&entries, |n| {
                    d.main.entries().iter().any(|e| e.name == n)
                })
            };
            let (changes, conflicts) = proposed;
            if changes.changes.is_empty() {
                return;
            }
            let count = changes.changes.len();
            // ③ 暂存载荷（AppData 持句柄；确认出口经同一句柄提交）+ 主线程开确认对话框
            let archive = {
                let Ok(d) = data.lock() else {
                    return;
                };
                d.archive.clone()
            };
            let Some(archive) = archive else {
                return;
            };
            if let Ok(mut d) = data.lock() {
                d.stage_edit_batch(archive, changes);
            }
            let _ = cx.update(|window, cx| {
                let _ = shell.update(cx, |shell: &mut AppShell, cx| {
                    let on = shell.confirm_handler();
                    window.open_dialog(cx, move |_, _window, cx| {
                        crate::ui::dialogs::add_files_confirm_dialog(cx, count, conflicts, {
                            let on = on.clone();
                            move |e| on(e)
                        })
                    });
                });
            });
        })
        .detach();
    }

    /// 打开删除确认（T-MENU7）：右栏选中 → `Change::Delete(entry_id)` 批量。
    pub fn begin_delete_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let Some(data) = self.data.as_ref() else {
            return;
        };
        let (changes, count) = {
            let Ok(d) = data.lock() else {
                return;
            };
            // T-MENU7：selection() 源自 BTreeSet（去重升序）→ 变更集确定
            // T-SEL 走查 #1：隐式目录选择经子树展开（选目录 = 删其全部内容）
            let cs = crate::app::state::delete_changeset(&d.main.effective_selection_ids());
            let count = cs.changes.len();
            (cs, count)
        };
        if count == 0 {
            return;
        }
        if let Ok(mut d) = data.lock() {
            let Some(archive) = d.archive.clone() else {
                return;
            };
            d.stage_edit_batch(archive, changes);
        }
        let on = self.confirm_handler();
        window.open_dialog(cx, move |_, _window, cx| {
            crate::ui::dialogs::delete_files_confirm_dialog(cx, count, {
                let on = on.clone();
                move |e| on(e)
            })
        });
    }

    /// 确认对话框事件出口（`DialogEvent` → 提交/取消；T-MENU6/7）。
    ///
    /// 事件循环收敛在 shell 层（与 `answer_prompt` 同款）：对话框闭包只拿
    /// `Fn(DialogEvent)`，故确认出口经 `AppData` 句柄直落提交点
    /// （`commit_edit_batch` → 唯一编辑提交点）。
    pub(super) fn confirm_handler(&self) -> std::rc::Rc<dyn Fn(crate::ui::contracts::DialogEvent)> {
        use crate::ui::contracts::DialogEvent;
        let data = self.data.clone();
        let tx = self.signal_tx.clone();
        std::rc::Rc::new(move |event: DialogEvent| {
            let Some(data) = data.as_ref() else {
                return;
            };
            match event {
                DialogEvent::EditAddAccept { .. } | DialogEvent::EditDeleteAccept => {
                    if let Ok(mut d) = data.lock() {
                        match d.commit_edit_batch() {
                            Ok(job_id) => {
                                tracing::info!("提交批量编辑任务 {job_id}");
                                if let Some(tx) = tx.as_ref() {
                                    let _ = tx.try_send(crate::viewmodel::VmSignal::JobsChanged);
                                    // 用户可见出口（2026-09-15 用户裁定）：编辑流程
                                    // 复用压缩/解压的进度窗形态——提交即开窗跟踪，
                                    // 任务中心只是后台记录。窗口线程亲和 → 经
                                    // OpenProgress 信号由主线程泵开窗（强制显示）。
                                    let _ = tx
                                        .try_send(crate::viewmodel::VmSignal::OpenProgress(job_id));
                                }
                            }
                            Err(e) => {
                                tracing::error!("批量编辑提交失败: {e}");
                            }
                        }
                    }
                }
                DialogEvent::EditConflictSkip
                | DialogEvent::EditConflictCancel
                | DialogEvent::EditAddCancel
                | DialogEvent::EditDeleteCancel => {
                    // 取消/跳过：丢弃暂存载荷（不提交）
                    if let Ok(mut d) = data.lock() {
                        d.discard_edit_batch();
                    }
                }
                _ => {}
            }
        })
    }

    /// 打开「添加文件…」的第一触点（菜单 Action 入口；对话框在本方法内开）。
    pub fn open_add_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_add_files(window, cx);
    }

    /// 打开删除确认的第一触点（菜单 Action 入口）。
    pub fn open_delete_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_delete_files(window, cx);
    }

    /// 浏览页右键菜单/双击文件动作路由（T-SEL3）。
    ///
    /// 入口：[`crate::ui::views::browse::BrowseHold::command`]（闭包直调——
    /// 菜单项 on_click 带 window/cx；**不走 `dispatch_action` 焦点链**，
    /// T-PERF §8 教训：弹层关闭后焦点可能残留在已移除元素上派发落空）。
    /// 五个现成动作直连内部方法（与菜单 Action/工具栏同路径）；
    /// 「编辑」与「打开」同口径（走查 #8：打开该文件——预览形态走内置
    /// 预览窗、外部形态走系统默认编辑器）；新建文件夹/重命名为 T-SEL6
    /// 占位（菜单项禁用，误触发 no-op）。
    pub fn browse_command(
        &mut self,
        cmd: crate::ui::views::browse::BrowseCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::views::browse::BrowseCommand;
        match cmd {
            BrowseCommand::ExtractAll => {
                self.dispatch_toolbar(window, cx, ToolbarAction::ExtractAll);
            }
            BrowseCommand::ExtractSel => {
                self.dispatch_toolbar(window, cx, ToolbarAction::Extract);
            }
            // 走查 #8：编辑 = 打开该文件（指定程序/系统默认程序；用户口径 2026-09-28）
            BrowseCommand::Open | BrowseCommand::Edit => self.open_selection_entry(cx),
            // 「打开方式…」：系统询问对话框选程序（用户口径 2026-09-28）
            BrowseCommand::OpenWith => self.open_selection_entry_with_picker(cx),
            // 新建文件夹/重命名（用户口径 2026-09-28）：进入**行内名称编辑态**
            // （Enter/失焦提交 → 后台重写档案）
            BrowseCommand::NewFolder => self.begin_new_folder(window, cx),
            BrowseCommand::Rename => self.begin_rename(window, cx),
            BrowseCommand::Delete => self.open_delete_files(window, cx),
            BrowseCommand::AddFiles => self.open_add_files(window, cx),
            // 复制/粘贴（T-MENU8 双向剪贴板）：与 Ctrl+C/Ctrl+V 同一处理函数
            // （copy_selection/paste_files），仅入口不同（右键菜单闭包直调）。
            BrowseCommand::Copy => self.copy_selection(window, cx),
            BrowseCommand::Paste => self.paste_files(window, cx),
            BrowseCommand::SelectAll => {
                // T-SEL2：与 Ctrl+A 快捷键**同一方法**（可见行全选 + 回投信号）
                // ——旧实现两入口一处发信号一处不发，行为长期不一致。
                let _ = self.select_all_visible(cx);
            }
        }
    }

    /// 行内改名入口（右键「重命名」）：以当前**单选**条目（文件/目录）为对象。
    ///
    /// 无有效单选（多选/无选/容器层）→ 不开编辑（菜单已按可用性禁用，此处
    /// 兜底）。
    fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.data.clone() else {
            return;
        };
        let target = {
            let Ok(d) = data.lock() else {
                return;
            };
            let dir = d.main.current_dir().to_string();
            let ids = d.main.selection();
            let dirs = d.main.dir_selection();
            let row = if ids.len() == 1 && dirs.is_empty() {
                let id = ids[0];
                d.dir_rows.iter().find(|r| r.entry_id == Some(id))
            } else if ids.is_empty() && dirs.len() == 1 {
                d.dir_rows.iter().find(|r| r.path == dirs[0])
            } else {
                None
            };
            row.filter(|r| !r.is_container)
                .map(|r| (dir, r.name.clone()))
        };
        let Some((dir, from)) = target else {
            return;
        };
        self.begin_name_edit_ui(window, cx, dir, from.clone(), Some(from));
    }

    /// 新建文件夹入口（右键「新建文件夹」）：在当前目录建草稿行 + 编辑态。
    fn begin_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.data.clone() else {
            return;
        };
        let Some(dir) = data.lock().ok().map(|d| d.main.current_dir().to_string()) else {
            return;
        };
        let initial = unique_new_folder_name(&data, &dir);
        self.begin_name_edit_ui(window, cx, dir, initial, None);
    }

    /// 建立行内名称编辑（输入框实体 + 事件订阅 + 数据面 + 聚焦）。
    ///
    /// `from` = 改名源条目名末段（`None` = 新建文件夹草稿行）；
    /// `initial` = 输入初值。
    fn begin_name_edit_ui(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        dir: String,
        initial: String,
        from: Option<String>,
    ) {
        let Some(data) = self.data.clone() else {
            return;
        };
        // 已有编辑（重复触发）→ 先收尾（提交前一次）
        if self.name_edit_input.is_some() {
            self.end_name_edit_ui(cx);
        }
        let epoch = data.lock().map(|d| d.session_epoch()).unwrap_or(0);
        let input =
            cx.new(|cx| gpui_kit::component::input::InputState::new(window, cx).clean_on_escape());
        input.update(cx, |s, cx| s.set_value(&initial, window, cx));
        // 事件：Enter → 提交；失焦 → 提交（用户口径 2026-09-28「回车或者脱离
        // 聚焦后保存生效」）
        let sub = cx.subscribe(
            &input,
            move |this: &mut Self,
                  _input: Entity<gpui_kit::component::input::InputState>,
                  ev: &gpui_kit::component::input::InputEvent,
                  cx: &mut Context<Self>| {
                match ev {
                    gpui_kit::component::input::InputEvent::Change => {}
                    gpui_kit::component::input::InputEvent::PressEnter { .. }
                    | gpui_kit::component::input::InputEvent::Blur => {
                        this.commit_name_edit_ui(true, cx);
                    }
                    gpui_kit::component::input::InputEvent::Focus => {}
                }
            },
        );
        self.name_edit_sub = Some(sub);
        self.name_edit_input = Some(input.clone());
        {
            let Ok(mut d) = data.lock() else {
                return;
            };
            d.begin_name_edit(crate::app::state::NameEdit {
                dir,
                from,
                text: initial,
                session_epoch: epoch,
            });
        }
        // 浏览页渲染侧共享同一实体（草稿行/改名行的名称列换成它）
        crate::ui::views::browse::set_name_input(cx, Some(input.clone()));
        // 聚焦 + **全选**（改名惯例：资源管理器 F2 那样整段可覆盖——只聚焦不全选
        // 的话，用户直接打字会**追加**在原名后面）
        use gpui_kit::Focusable as _;
        let fh = input.read(cx).focus_handle(cx);
        fh.focus(window, cx);
        input.update(cx, |st, cx| st.select_all(window, cx));
        if let Some(tx) = self.signal_tx.as_ref() {
            let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
        }
        cx.notify();
    }

    /// 提交行内名称（`commit=true`）/ 仅收尾。
    ///
    /// 校验失败（重名/非法名）→ **保持编辑态**让用户改 + toast 文案。
    fn commit_name_edit_ui(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(input) = self.name_edit_input.clone() else {
            return;
        };
        let text = input.read(cx).value().to_string();
        let Some(data) = self.data.clone() else {
            return;
        };
        if commit {
            let res = data
                .lock()
                .map(|mut d| d.commit_name_edit(&text))
                .unwrap_or_else(|_| Err(String::new()));
            match res {
                Ok(_) => {}
                Err(msg) => {
                    if !msg.is_empty()
                        && let Some(tx) = self.signal_tx.as_ref()
                    {
                        let _ = tx.try_send(crate::viewmodel::VmSignal::Notify(
                            crate::viewmodel::pump::NotifyEvent {
                                title: crate::tr("browse.rename.failed"),
                                body: msg,
                                force_system: false,
                            },
                        ));
                    }
                    // 保持编辑态 + 重新聚焦（失焦提交失败时焦点已丢）
                    if let Some(handle) = self.last_window {
                        let _ = handle.update(cx, |_, window, cx| {
                            use gpui_kit::Focusable as _;
                            let fh = input.read(cx).focus_handle(cx);
                            fh.focus(window, cx);
                        });
                    }
                    cx.notify();
                    return;
                }
            }
        } else if let Ok(mut d) = data.lock() {
            d.cancel_name_edit();
        }
        self.end_name_edit_ui(cx);
    }

    /// 收尾行内编辑（清输入框 + 数据面已由调用方处理 + 通知渲染）。
    fn end_name_edit_ui(&mut self, cx: &mut Context<Self>) {
        self.name_edit_input = None;
        self.name_edit_sub = None;
        crate::ui::views::browse::set_name_input(cx, None);
        if let Some(tx) = self.signal_tx.as_ref() {
            let _ = tx.try_send(crate::viewmodel::VmSignal::EntriesChanged);
        }
        cx.notify();
    }

    /// 文件列表**空白区左键** → 取消选中（用户口径 2026-09-28：与资源管理器一致）。
    ///
    /// 由表格容器（`browse_layout` 的 `table-wrap`）在「本次左键没落在行上」时调用；
    /// 行内点击被行自己的处理器拦下（`last_hit_row`）。
    pub(crate) fn clear_selection_now(&mut self) {
        // 与行内选择**同一条上行**（`on_selection_changed`）：生产接线里它会
        // `select_rows(&[], &[])` 并在 VM 侧回投 `EntriesChanged`，于是快照一刷
        // 行高亮/状态栏统计同步归零——不另开清理通道，避免两套真相。
        (self.handlers.on_selection_changed)(crate::ui::contracts::SelectionUpdate {
            ids: Vec::new(),
            dirs: Vec::new(),
        });
    }

    /// 密码门放行后的**重跑入口**（主线程泵调用；用户口径 2026-09-28）。
    ///
    /// 门内后台线程已验证密码并缓存 → 取出待执行动作后原样重跑对应入口
    /// （`OpenSelection` → [`Self::open_selection_entry`]；`OpenContainer` →
    /// [`Self::open_container`]），此时密码缓存命中，直接放行原操作。
    ///
    /// `OpenArchive` 不进本槽——头加密档案的重开由 [`begin_browse_gate_reopen`]
    /// 在门内一步做完（含装会话/MRU）。
    pub(crate) fn retry_pending_gate_action(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self
            .data
            .as_ref()
            .and_then(|d| d.lock().ok().and_then(|d| d.take_pending_gate_action()))
        else {
            return;
        };
        tracing::info!(
            action = action.kind(),
            "密码门放行：泵取出待执行动作，重跑原操作"
        );
        match action {
            crate::viewmodel::GateAction::OpenArchive => {}
            crate::viewmodel::GateAction::OpenSelection { id, name, form } => {
                self.open_one_entry(id, name, form, cx)
            }
            crate::viewmodel::GateAction::OpenContainer(path) => self.open_container(&path, cx),
        }
    }
}
