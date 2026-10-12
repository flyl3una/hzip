//! `AppShell` 渲染骨架（chrome 装配）与空页签占位——自 `shell/mod.rs` 拆分。

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, IntoElement, Render, SharedString, Window, div};

use crate::ui::constant::MENUBAR_H;
use crate::ui::contracts::{MainTab, ToolbarAction};
use crate::ui::spacing::sp;

use super::actions::{
    About, AddFiles, CheckUpdate, ClearMru, CopyFiles, DeleteFiles, Extract, FindFile, NewWindow,
    Open, OpenRecent, Paste, Quit, RefreshView, Reveal, SelectAll, SelectInvert, SetToolbarStyle,
    SetViewStyle, Settings, ShowJobs, TestIntegrity, ToggleJobs, ToggleTree,
};
use super::app_shell::AppShell;
use super::open_progress_window;

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // UI 线程滞后看门狗（见 `app::ui_lag`）：一帧超过阈值就留一条 warn/error，
        // 再现「界面卡住」时日志直接点名是哪一帧的哪段同步活。
        let _lag = crate::app::ui_lag::UiLag::new("主窗口渲染");
        // 行内编辑的「提交失败重新聚焦」要用窗口句柄（订阅回调无 Window 参数）
        self.last_window = Some(window.window_handle());
        // 无任何焦点（例如持有焦点的设置面板刚随页签销毁）→ 焦点自补到壳层：
        // 否则动作派发回落成「从根节点派发」，壳层 on_action 链收不到
        //（用户实测：频繁开关页签后菜单「设置…」点了没反应）。
        let shell_focus = self.focus_handle.clone();
        if window.focused(cx).is_none() {
            shell_focus.focus(window, cx);
        }
        // 先拷贝 state 字段（结束不可变借用），再建需要 &mut App 的元素
        let (
            busy,
            busy_text,
            archive_title,
            tab,
            entry_count,
            selection_count,
            suspect_count,
            sel_stats,
            job_pill,
            snapshot,
        ) = {
            let s = self.state.read(cx);
            // T-G53 内容插槽快照：ShellState 浏览字段 → 快照投影（只读渲染输入）
            let snapshot = s.to_snapshot();
            (
                s.busy,
                s.busy_text.clone(),
                s.archive_name.clone(),
                s.tab,
                s.entry_count,
                s.selection_count,
                s.suspect_count,
                s.sel_stats,
                s.job_pill.clone(),
                snapshot,
            )
        };
        // 先拷贝 state 字段 + 主题色（&App 不可变借用），再取 &mut cx 构建
        // 带 cx.listener 的 toolbar（borrow 冲突规避，T-G30 #3）。
        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let popover = cx.theme().popover;
        // 菜单投影注入（T-MENU1 MenuContext：has_archive/has_selection/勾选态；
        // epoch = ShellState.menu_epoch——MRU/视图偏好变化即重建快照）
        let menu_ctx = {
            let s = self.state.read(cx);
            // T-PERF3：MRU 与视图偏好读 ShellState.cfg 缓存（旧实现每帧
            // `Config::load()` 读盘+解析——动画帧预算杀手）；失效由泵在
            // SettingApplied/MenuEpochBump 时重读（见 refresh_cfg）
            let cfg = s.cfg.clone();
            // T-MENU6 caps_edit 投影 / MRU：**走 ShellState 快照**（T-PERF5）。
            //
            // 旧实现在这里 `data.lock()` + `caps_edit()`：后者要**嗅探档案**
            // （开文件读头）——而本块在 `render` 里，等于**每帧一次文件 IO +
            // 每帧一次 AppData 锁**。慢盘/网络盘上直接表现为界面卡顿，且与任何
            // 持锁的后台任务（解出条目、预览）互相拖累。现在两者都由泵的快照
            // 投影带入（`caps_edit` 另在 AppData 侧按档案路径记忆化）。
            let caps_edit = s.caps_edit;
            let mru = s.mru.clone();
            crate::ui::bar::menu_bar::MenuContext {
                has_archive: !archive_title.is_empty(),
                has_selection: s.selection_count > 0,
                caps_edit,
                mru,
                show_tree: cfg.ui.show_tree,
                show_jobs: cfg.ui.show_jobs,
                toolbar_style: crate::ui::bar::menu_bar::toolbar_style_of(&cfg.ui.toolbar_style),
                view_style: crate::ui::bar::menu_bar::view_style_of(&cfg.ui.view_style),
                // T-MENU8：剪贴板空 → 粘贴灰（内部剪贴板为进程级资源）
                // 走快照投影（审计 B14：渲染帧不做跨模块查询）
                has_clipboard: s.has_clipboard,
            }
        };
        let menu_epoch = self.state.read(cx).menu_epoch;
        self.menu_bar.update(cx, |bar, mbcx| {
            bar.sync(&menu_ctx, menu_epoch, mbcx);
        });
        // 菜单条容器：固定高度 flex_shrink_0，防止占满剩余空间把工具栏挤到底部。
        let menubar = div()
            .flex_shrink_0()
            .h(MENUBAR_H)
            .border_b_1()
            .border_color(border)
            .bg(popover)
            .child(self.menu_bar.clone());
        let busy_bar = crate::ui::widgets::status::busy_bar(busy, &busy_text, cx);
        // 页签组（工具栏**右槽**；页签集是派生的，见 `ShellState::present_tabs`）：
        // 空集（刚启动：无档案、show_jobs 关、未开设置）= 不渲染任何页签。
        // 点击 → switch_tab；关闭钮 → close_tab（WeakEntity 升级调用，'static
        // 无 render 借用问题）。
        let show_jobs = self.state.read(cx).show_jobs;
        let settings_open = self.state.read(cx).settings_open;
        let shell_handle = cx.entity();
        // 无页签 = 内容区空态（刚启动：没开档案、没开任务中心、没开设置）
        let tabs_empty = self.state.read(cx).present_tabs().is_empty();
        let tabs_slot: Option<gpui_kit::AnyElement> = {
            let s = self.state.read(cx);
            let tabs = s.present_tabs();
            let active = s.tab;
            let badge = s.jobs_badge;
            (!tabs_empty).then(|| {
                let on_click: crate::ui::bar::tab_bar::TabClick = std::rc::Rc::new({
                    let shell = shell_handle.downgrade();
                    move |target: MainTab, cx: &mut gpui_kit::App| {
                        let _ =
                            shell.update(cx, |this: &mut AppShell, cx| this.switch_tab(cx, target));
                    }
                });
                let on_close: crate::ui::bar::tab_bar::TabClose = std::rc::Rc::new({
                    let shell = shell_handle.downgrade();
                    move |target: MainTab, cx: &mut gpui_kit::App| {
                        let _ =
                            shell.update(cx, |this: &mut AppShell, cx| this.close_tab(cx, target));
                    }
                });
                div()
                    .debug_selector(|| "main-tabs-wrap".into())
                    .child(crate::ui::bar::tab_bar::tab_row(
                        &tabs, active, badge, on_click, on_close, cx,
                    ))
                    .into_any_element()
            })
        };
        let status_bar = crate::ui::widgets::status::status_bar(
            entry_count,
            selection_count,
            suspect_count,
            sel_stats,
            &job_pill,
            cx,
        );
        // toolbar（emit 闭包借用随构造释放）先行构建；child 顺序按 06 §4.1：
        // 标题栏 → 菜单 → 工具栏 → busy 条 → 页签 → 内容 → 状态栏（R-GUI1-01 §1.2
        // 修复：旧写法把 toolbar 追加链尾导致渲染在状态栏之下）。
        // 工具栏动作一律走 `dispatch_toolbar_ui`（「添加/删除」要实体上下文
        // 装配确认对话框——`dispatch_toolbar` 只有 `&mut App`，见其文档）。
        let emit: crate::ui::contracts::ToolbarEmit = std::rc::Rc::new(cx.listener(
            |this: &mut Self, action: &ToolbarAction, window, cx| {
                this.dispatch_toolbar_ui(window, cx, action.clone());
            },
        ));
        let (props, toolbar_style) = {
            let s = self.state.read(cx);
            (
                crate::ui::bar::toolbar::ToolbarProps {
                    enable: s.toolbar_enable,
                    mru: &s.mru,
                    columns: &s.columns,
                },
                // T-PERF3：读 cfg 缓存（旧：每帧 Config::load() 读盘）
                crate::ui::bar::menu_bar::toolbar_style_of(&s.cfg.ui.toolbar_style),
            )
        };
        let toolbar = crate::ui::bar::toolbar::toolbar(&props, toolbar_style, &emit, tabs_slot, cx);
        let action_chain = v_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .key_context("Main")
            // 壳层可聚焦（= 焦点恢复目标 + 动作派发的兜底起点，见字段文档）
            .track_focus(&shell_focus)
            .on_action(cx.listener(|this, _: &Open, window, cx| {
                // T-MENU1 修正：菜单「打开」直连工具栏同路径（on_toolbar 槽在
                // runtime 装配中是 no-op default——经它派发等于空转，实测缺陷）
                this.dispatch_toolbar(window, cx, ToolbarAction::Open);
            }))
            .on_action(cx.listener(|this, _: &Settings, window, cx| {
                // 菜单「设置…」直连工具栏同路径（R-GUI1-03 §2.2；
                // R-GUI4 #4：事件现场窗口直传——active_window 链废弃）
                this.dispatch_toolbar(window, cx, ToolbarAction::Settings);
            }))
            .on_action(cx.listener(|this, _: &TestIntegrity, window, cx| {
                // T-MENU1 修正：同 Open——直连 dispatch_toolbar（no-op 槽修复）
                this.dispatch_toolbar(window, cx, ToolbarAction::TestIntegrity);
            }))
            .on_action(cx.listener(|this, _: &RefreshView, window, cx| {
                // T-MENU1 修正：同 Open——直连 dispatch_toolbar（no-op 槽修复）
                this.dispatch_toolbar(window, cx, ToolbarAction::Refresh);
            }))
            .on_action(cx.listener(|this, _: &ToggleJobs, _, cx| {
                // 任务中心显隐开关 → 注册表五步流程（落盘 + 快照刷新）
                // 与设置面板同源——双入口同一事件流（同 ToggleTree）。
                if let Some(data) = this.data.as_ref()
                    && let Ok(mut d) = data.lock()
                {
                    let cur = d.settings.show_jobs;
                    d.apply_setting(
                        crate::viewmodel::SettingId::ShowJobs,
                        &crate::app::settings_registry::SettingValue::Bool(!cur),
                    );
                }
                let _ = cx;
            }))
            .on_action(cx.listener(|this, _: &ShowJobs, _, cx| {
                // 任务中心页签切换（ctrl-j 同义）。任务中心关闭时 switch_tab
                // 内部 no-op——页签不存在（显隐走「视图 ▸ 任务中心」开关）。
                let target = if this.state.read(cx).tab == MainTab::Jobs {
                    MainTab::Browse
                } else {
                    MainTab::Jobs
                };
                this.switch_tab(cx, target);
            }))
            .on_action(cx.listener(|_this, _: &Quit, _, cx| {
                // T-GUI T34：菜单退出 → gpui 退出（窗口关闭链走 graceful_shutdown）
                cx.quit();
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                // T-GUI T34：全选（VM 过滤后可见条目；回投刷新选择计数）
                if let Some(data) = this.data.as_ref()
                    && let Ok(mut d) = data.lock()
                {
                    d.main.select_all(false);
                }
                let _ = cx;
            }))
            .on_action(cx.listener(|this, _: &SelectInvert, _, cx| {
                // T-GUI T34：反选（T34 新增 VM select_invert；可见条目互换）
                if let Some(data) = this.data.as_ref()
                    && let Ok(mut d) = data.lock()
                {
                    d.main.select_invert();
                }
                let _ = cx;
            }))
            .on_action(cx.listener(|this, _: &Extract, window, cx| {
                // T-MENU1：菜单「解压文件…」直连工具栏解压同路径（选中预填
                // 语义同源——dispatch_toolbar Extract 分支处理选中回退全部）
                this.dispatch_toolbar(window, cx, ToolbarAction::Extract);
            }))
            .on_action(cx.listener(|this, _: &Reveal, _, _| {
                // T-MENU1：在文件管理器显示（hzip-platform reveal；select=true）。
                // `reveal_in_file_manager` 走 ShellExecuteW / 资源管理器 IPC——会等
                // 系统响应，慢盘/网络路径上可达百毫秒级；挪后台线程（用户口径
                // 2026-09-24：UI 线程不做可能 >0.5 s 的系统调用）。
                let archive = this
                    .data
                    .as_ref()
                    .and_then(|data| data.lock().ok().and_then(|d| d.archive.clone()));
                if let Some(archive) = archive {
                    std::thread::spawn(move || {
                        if let Err(e) = hzip_platform::open::reveal_in_file_manager(&archive, true)
                        {
                            tracing::warn!("reveal 失败: {e}");
                        }
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &FindFile, window, cx| {
                // T-MENU1：查找文件（Ctrl+F）→ 聚焦浏览页过滤框；
                // 无档案 no-op——菜单项已 disabled，正常路径到不了这里
                this.focus_browse_filter(window, cx);
            }))
            .on_action(cx.listener(|_this, _: &About, window, cx| {
                // T-MENU1：About 对话框（版本 + 开源许可；用户口径 2026-10-09
                // 去掉格式支持清单与仓库地址）
                crate::ui::dialogs::open_about_dialog(window, cx);
            }))
            .on_action(cx.listener(|_this, _: &CheckUpdate, window, cx| {
                // T-MENU1：检查更新占位（T43 挂账——与设置页同源文案提示）
                use gpui_kit::component::WindowExt as _;
                window.push_notification(
                    gpui_kit::component::notification::Notification::new()
                        .message(crate::tr("menu.help.update_unavailable"))
                        .id::<crate::ui::bar::menu_bar::MenuEpochMarker>(),
                    cx,
                );
            }))
            .on_action(cx.listener(|this, _: &NewWindow, _, cx| {
                // T-MENU10：新窗口 = 独立 AppData + 独立信号通道（同一工厂）。
                // 未注入工厂（headless）→ 静默 no-op。
                if let Some(factory) = this.on_new_window.clone() {
                    factory(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &AddFiles, window, cx| {
                // T-MENU6：多选 → ChangeSet(Append) → 确认对话框 → EditBatch
                this.open_add_files(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteFiles, window, cx| {
                // T-MENU7：选中 → ChangeSet(Delete) → 确认对话框 → EditBatch
                this.open_delete_files(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CopyFiles, window, cx| {
                // T-MENU8：选中 → 物化 TempArea → 写内部剪贴板（+系统单文件尽力）
                this.copy_selection(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Paste, window, cx| {
                // T-MENU8：内部剪贴板（优先）→ 外部 CF_HDROP → 批量 Append
                this.paste_files(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTree, _, cx| {
                // T-MENU5：树型目录开关 → 注册表五步流程（落盘 + 快照刷新）
                // 与设置面板同源——双入口同一事件流。
                if let Some(data) = this.data.as_ref()
                    && let Ok(mut d) = data.lock()
                {
                    let cur = d.settings.show_tree;
                    d.apply_setting(
                        crate::viewmodel::SettingId::ShowTree,
                        &crate::app::settings_registry::SettingValue::Bool(!cur),
                    );
                }
                let _ = cx;
            }))
            .on_action(cx.listener(|this, action: &SetToolbarStyle, _, cx| {
                // T-MENU5：工具栏三态（菜单入口 → 注册表五步流程）
                if let Some(data) = this.data.as_ref()
                    && let Ok(mut d) = data.lock()
                {
                    d.apply_setting(
                        crate::viewmodel::SettingId::ToolbarStyle,
                        &crate::app::settings_registry::SettingValue::Text(action.value.clone()),
                    );
                }
                let _ = cx;
            }))
            .on_action(cx.listener(|this, action: &SetViewStyle, _, cx| {
                // T-MENU5：视图样式（icon/list 灰——菜单装配层 disabled）
                if let Some(data) = this.data.as_ref()
                    && let Ok(mut d) = data.lock()
                {
                    d.apply_setting(
                        crate::viewmodel::SettingId::ViewStyle,
                        &crate::app::settings_registry::SettingValue::Text(action.value.clone()),
                    );
                }
                let _ = cx;
            }))
            .on_action(cx.listener(|this, action: &OpenRecent, _, cx| {
                // T-MENU4：最近文件点击 → open 流程（与 Open 同源）+ MRU 置顶。
                // 文件不存在 → tracing 错误 + 从 MRU 移除（epoch 递增收起项）。
                let path = std::path::PathBuf::from(&action.path);
                if !path.is_file() {
                    tracing::error!("最近文件已不存在: {}", action.path);
                    if let Some(data) = this.data.as_ref()
                        && let Ok(mut d) = data.lock()
                    {
                        d.remove_mru(&action.path);
                        if let Some(tx) = &d.signal_tx {
                            let _ = tx.try_send(crate::viewmodel::VmSignal::MenuEpochBump);
                        }
                    }
                    return;
                }
                this.open_archive_path(cx, path);
            }))
            .on_action(cx.listener(|this, _: &ClearMru, _, _| {
                // T-MENU4：清空所有历史记录（清 + 落盘 + epoch 递增收起列表）
                if let Some(data) = this.data.as_ref()
                    && let Ok(mut d) = data.lock()
                {
                    d.clear_mru();
                    if let Some(tx) = &d.signal_tx {
                        let _ = tx.try_send(crate::viewmodel::VmSignal::MenuEpochBump);
                    }
                }
            }))
            .child(
                TitleBar::new().child(
                    h_flex()
                        .gap(sp(2.))
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(crate::tr("shell.app_title")),
                        )
                        .children(if archive_title.is_empty() {
                            None
                        } else {
                            Some(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(format!("— {archive_title}"))),
                            )
                        }),
                ),
            )
            .child(menubar)
            .child(toolbar)
            .child(busy_bar)
            .child(div().flex_1().min_h_0().children(if tabs_empty {
                // 无任何页签 → 空态提示（用户口径：刚启动一个页签都没有）
                Some(tabs_empty_state(cx))
            } else {
                match tab {
                    // 任务中心（页签可见时才走此支）
                    MainTab::Jobs if show_jobs => {
                        // T-GUI T31：批量操作上行复用 on_dialog_event 链
                        //（AppData 消费面）。&mut App 由按钮点击现场传入（同
                        // tab_row 模式），WeakEntity 升级后直调 handlers。
                        let shell = shell_handle.downgrade();
                        let on_batch = std::rc::Rc::new(
                            move |e: crate::ui::contracts::DialogEvent, cx: &mut gpui_kit::App| {
                                let _ = shell.update(cx, |this: &mut AppShell, _| {
                                    (this.handlers.on_dialog_event)(e)
                                });
                            },
                        );
                        // T-PW5 重开入口：双击活跃任务行 → 窗口已关则重开
                        //（on_reopen 直调 dispatch 装配口——AppData 触碰收口 shell 层）。
                        let data_reopen = self.data.clone();
                        let on_reopen: crate::ui::views::jobs::ReopenHandler =
                            std::rc::Rc::new(move |job_id: u64, cx: &mut gpui_kit::App| {
                                if let Some(data) = data_reopen.as_ref() {
                                    open_progress_window(cx, data, job_id);
                                }
                            });
                        Some(crate::ui::views::jobs::jobs_root(
                            cx,
                            window,
                            &snapshot,
                            &self.handlers,
                            on_batch,
                            Some(on_reopen),
                        ))
                    }
                    // 设置页（页签存在才有此支）：面板实体在「设置」动作里懒建
                    //（`SettingsPanel::new_entity`，需 window）；未建则空态兜底。
                    MainTab::Settings if settings_open => Some(
                        self.settings_panel
                            .clone()
                            .map(|panel| {
                                // 上下左右留白（用户口径：设置页铺满内容区太拥挤）
                                // + 纵向可滚（原 Dialog 的滚动体随弹层下线）。
                                // 内边距挂**外层**：面板根带 `min_h_full`，padding 与它
                                // 同元素会让外框高 = 100% + padding 而溢出。
                                div()
                                    .size_full()
                                    .debug_selector(|| "settings-host".into())
                                    .p(crate::ui::spacing::sp(4.))
                                    .overflow_y_scrollbar()
                                    .child(panel)
                                    .into_any_element()
                            })
                            .unwrap_or_else(|| tabs_empty_state(cx)),
                    ),
                    // 浏览页；`Jobs`/`Settings` 落到这里 = 对应页签已被关（页签不存在）
                    // 而状态残留的兜底——内容恒为浏览页（`ShellState::apply` 已修态，
                    // 此处仅防御）
                    MainTab::Browse | MainTab::Jobs | MainTab::Settings => {
                        let shell_weak = cx.weak_entity();
                        Some(crate::ui::views::browse::browse_root(
                            window, cx, &snapshot, shell_weak,
                        ))
                    }
                }
            }))
            .child(status_bar);
        // Dialog/Sheet/Notification 层**不再由内容视图手工挂载**（gpui-kit
        // 0.7.1）。0.6 时代 `Root::render` 不自行挂层，须根内容视图逐层手工补挂
        // （sheet / dialog / notification 三个挂载函数）；0.7.1 改了契约：这些
        // 挂载函数整族从 kit 删掉（整树 0 命中、无同名替代），改由
        // `component::init` 注册的 `WindowState` 插件在 `gpui_base::Root::render`
        // 内自动挂载（gpui-component-0.7.1/src/root.rs:21 注册、:461 插件 overlay、
        // :483 `WindowStateLayers` 挂 sheet/dialog/notification 三层；
        // gpui-base-0.7.1/src/root.rs:288 把 plugin view 作为 overlay 叠上）。
        // 手工调用已无法编译——也不必：只要 `component::init` 跑过（`run_ui`
        // 已保证），层就随 Root 自动出现。
        action_chain.into_any_element()
    }
}

/// 无页签时的内容区占位（刚启动：还没打开档案、也没开任务中心/设置）。
fn tabs_empty_state(cx: &App) -> gpui_kit::AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(crate::tr("shell.no_tabs")),
        )
        .into_any_element()
}
