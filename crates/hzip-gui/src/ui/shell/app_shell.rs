//! `AppShell` 结构体 + 生命周期与页签（构造/快照观察/设置页签/开关页签）
//! ——自 `shell/mod.rs` 拆分。

use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::{App, Entity};
// 仅 `dispatch_action_for_test`（cfg(test)）用到——非测试构建下不加会报 unused。
#[cfg(test)]
use gpui_kit::Window;

use crate::ui::bar::menu_bar::MenuBar;
use crate::ui::contracts::{AppStateUiSnapshot, MainTab, ShellHandlers};

#[cfg(test)]
use super::actions::NewWindow;
use super::runtime::AppDataHandle;
use super::state::ShellState;
use super::windows;

/// 主窗口 chrome（标题栏/菜单/工具栏/页签/状态栏 + 内容插槽）。
pub struct AppShell {
    /// 渲染状态
    pub state: Entity<ShellState>,
    /// 菜单条实体（T-G51：打开态持久 + PopupMenu 弹出）
    pub menu_bar: Entity<MenuBar>,
    /// 动作上行注入口（T-G30 填充）
    pub handlers: ShellHandlers,
    /// AppData 句柄（工具栏动作直接落地：打开档案/完整性测试）
    pub data: Option<AppDataHandle>,
    /// 行内名称编辑的输入框实体（重命名/新建文件夹；用户口径 2026-09-28）。
    ///
    /// 由 [`Self::begin_name_edit_ui`] 建、[`Self::end_name_edit_ui`] 销毁；
    /// 浏览页渲染侧经 `browse::set_name_input` 取用（同一实体）。
    pub name_edit_input: Option<Entity<gpui_kit::component::input::InputState>>,
    /// 行内编辑输入框的事件订阅（Enter 提交 / 失焦提交；保活防退订）
    pub(super) name_edit_sub: Option<gpui_kit::Subscription>,
    /// 最近一次渲染的主窗口句柄（提交失败要重新聚焦输入框——订阅回调里没有
    /// `Window` 参数，只能走句柄）。
    pub(super) last_window: Option<gpui_kit::AnyWindowHandle>,
    /// SignalTx（动作完成后回投 UI 刷新）
    pub signal_tx: Option<Arc<crate::ui::contracts::SignalTx>>,
    /// ShellState 观察订阅（notify 联动 AppShell 重渲；保活防退订）
    _observe_state: Option<gpui_kit::Subscription>,
    /// 新窗口工厂（T-MENU10；`run_ui` 注入：独立 AppData + 通道 + 注册表登记）
    ///
    /// `None` = 未注入（headless/测试装配）——「新窗口」Action 静默 no-op。
    pub(super) on_new_window: Option<std::rc::Rc<windows::NewWindowFn>>,
    /// 设置页实体（设置页签内容；2026-09-23 设置由弹层改为**页签**）。
    ///
    /// 懒建：首次点「设置」时装配（`SettingsPanel::new_entity`），关闭页签即丢弃
    /// （下次开重建——面板活读投影本来就是每开重建的语义）。
    pub(super) settings_panel:
        Option<gpui_kit::Entity<crate::ui::views::settings_layout::SettingsPanel>>,
    /// 设置面板实体订阅（保活防退订）。
    ///
    /// 用途（用户实测缺陷 2026-09-24：在设置里打开「任务中心」后，页签栏里
    /// **不出现**该页签，直到下一次别的信号才补上）：页签集依赖 `ui.show_jobs`，
    /// 而该值平时靠「AppData 发信号 → 主线程泵批量派生快照 → ShellState.apply」
    /// 这条**异步**链回流；面板自己的 tick 只重画面板，不带页签。这里挂一个
    /// 实体订阅——面板每次 notify（设置项一改就 notify）即把 `show_jobs` 从
    /// AppData 同步进 ShellState，与 `switch_tab`/`close_tab` 的「本地先行」
    /// 同一口径（事实源仍是 AppData，不复制状态）。
    pub(super) _observe_settings_panel: Option<gpui_kit::Subscription>,
    /// 壳层自身的焦点句柄（**焦点恢复目标**）。
    ///
    /// 为什么必须有：动作派发（菜单项/快捷键）从**当前焦点节点**沿渲染树向上冒泡；
    /// 一个都没有时会回落成「从根节点派发」——根节点没有祖先，挂在壳层 div 上的
    /// `on_action` 链**整条收不到**（实测缺陷：关掉设置页签时，持有焦点的面板实体
    /// 被销毁 ⇒ 焦点变空 ⇒ 之后菜单「选项 ▸ 设置…」点了没反应）。壳层可聚焦后，
    /// gpui 的 focus-lost 恢复会把它当作最近的存活祖先；渲染帧里再无焦点时也会
    /// 自补到它身上（见 `render`）。
    pub(super) focus_handle: gpui_kit::FocusHandle,
}

impl AppShell {
    /// 创建 shell（`handlers` 由 T-G30 填充；T-G10 自测用 `ShellHandlers::default()`）。
    pub fn new(cx: &mut App, handlers: ShellHandlers) -> Self {
        Self::with_data(cx, handlers, None, None)
    }

    /// 带数据面构造（T-G30 集成路径：工具栏动作可直接落地）。
    pub fn with_data(
        cx: &mut App,
        handlers: ShellHandlers,
        data: Option<AppDataHandle>,
        signal_tx: Option<Arc<crate::ui::contracts::SignalTx>>,
    ) -> Self {
        let state = cx.new(|_| ShellState::new());
        // 菜单条实体（T-G51：替代平台菜单双通道——打开态持久 + PopupMenu 弹出）。
        let menu_bar = MenuBar::new(cx);
        Self {
            menu_bar,
            state,
            handlers,
            data,
            name_edit_input: None,
            name_edit_sub: None,
            last_window: None,
            signal_tx,
            _observe_state: None,
            on_new_window: None,
            settings_panel: None,
            _observe_settings_panel: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// 注入新窗口工厂（run_ui 装配；见字段文档）。
    pub fn set_new_window_factory(&mut self, f: std::rc::Rc<windows::NewWindowFn>) {
        self.on_new_window = Some(f);
    }

    /// 测试专用：直接派发一个动作（走与 `on_action` 链相同的处理分支）。
    ///
    /// 生产路径由 gpui 的动作链派发；本方法供 headless 测试验证
    /// 「动作 → 处理器」接线，避免依赖键盘/菜单驱动。
    #[cfg(test)]
    pub(crate) fn dispatch_action_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut App,
        action: &NewWindow,
    ) {
        let _ = action;
        if let Some(factory) = self.on_new_window.clone() {
            factory(cx);
        }
        let _ = window;
    }

    /// 观察 ShellState（泵/开关页签 notify state 后联动重渲 AppShell）。
    ///
    /// Entity::update 内 notify 只标脏该实体自身；AppShell render 读 state
    /// 但自身未被标脏 → 浏览数据刷新不生效（R-GUI1-02 实测根因）。
    /// 需在成为 Entity 后挂 observe——run_ui open_window 闭包内调用。
    pub fn observe_state(&mut self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        self._observe_state = Some(cx.observe(&state, |_, _, cx| cx.notify()));
    }
    pub fn update_from_state(&mut self, cx: &mut App, snap: &AppStateUiSnapshot) {
        self.state.update(cx, |s, cx| {
            // T-PERF2：未变化不 notify——apply 的变化检测就是重渲门控
            //（旧实现无条件 notify，检测成本照付、收益为零；任务运行时
            // 10 Hz 进度 tick 从此不再触发全窗口重渲）
            if s.apply(snap) {
                cx.notify();
            }
        });
    }
    /// 配置缓存刷新（T-PERF3）：设置落盘 / MRU·菜单数据变化后由泵调用——
    /// 渲染帧的 MenuContext/工具栏样式读 `ShellState.cfg` 缓存，读盘只发生
    /// 在这条低频信号路径，而非 60fps 渲染帧。
    pub fn refresh_cfg(&mut self, cx: &mut App) {
        self.state.update(cx, |s, cx| {
            s.cfg = Arc::new(hzip_config::Config::load());
            cx.notify();
        });
    }
    /// 切换页签（状态胶囊点击联动入口）。
    ///
    /// **不存在即不切**（页签集是派生的，见 `ShellState::present_tabs`）：切向
    /// 一个看不见的页会让内容落在隐藏页上。任务中心是唯一例外——它是「打开/
    /// 关闭」语义的页签（`ui.show_jobs`），故切向 Jobs 时顺带把它打开并落盘
    ///（状态胶囊点一下 = 打开任务中心；与视图菜单勾选同一事实源）。
    pub fn switch_tab(&mut self, cx: &mut App, tab: MainTab) {
        if !self.state.read(cx).tab_present(tab) {
            if tab != MainTab::Jobs {
                return;
            }
            // 任务中心未开：落盘 + 本地先行（泵批次到达前的即时性）
            if let Some(data) = self.data.as_ref()
                && let Ok(mut d) = data.lock()
            {
                d.apply_setting(
                    crate::viewmodel::SettingId::ShowJobs,
                    &crate::app::settings_registry::SettingValue::Bool(true),
                );
            }
            self.state.update(cx, |s, cx| {
                s.show_jobs = true;
                cx.notify();
            });
        }
        self.state.update(cx, |s, cx| {
            if s.tab != tab {
                s.tab = tab;
                cx.notify();
            }
        });
        (self.handlers.on_tab)(tab);
    }

    /// 关闭页签（页签栏关闭钮唯一入口）。
    ///
    /// | 页签 | 关闭语义 |
    /// |---|---|
    /// | 浏览 | **暂隐**（档案仍开着、非破坏性）；再开档案自动复位 |
    /// | 任务中心 | 取消 `ui.show_jobs`（与视图菜单勾选同一事实源） |
    /// | 设置 | 关掉设置页，面板实体丢弃（下次开重建） |
    ///
    /// 关掉的是当前页签时落到**右邻**（其次左邻）；全关 = 空态。
    pub fn close_tab(&mut self, cx: &mut App, tab: MainTab) {
        let (present, active) = {
            let s = self.state.read(cx);
            (s.present_tabs(), s.tab)
        };
        let ix = present.iter().position(|t| *t == tab);
        match tab {
            MainTab::Browse => {
                self.state.update(cx, |s, cx| {
                    s.browse_hidden = true;
                    cx.notify();
                });
            }
            MainTab::Settings => {
                self.settings_panel = None;
                self._observe_settings_panel = None;
                self.state.update(cx, |s, cx| {
                    s.settings_open = false;
                    cx.notify();
                });
            }
            MainTab::Jobs => {
                if let Some(data) = self.data.as_ref()
                    && let Ok(mut d) = data.lock()
                {
                    d.apply_setting(
                        crate::viewmodel::SettingId::ShowJobs,
                        &crate::app::settings_registry::SettingValue::Bool(false),
                    );
                }
                self.state.update(cx, |s, cx| {
                    s.show_jobs = false;
                    cx.notify();
                });
            }
        }
        if active != tab {
            return;
        }
        // 关的是当前页 → 落到右邻，无则左邻，再无 → 无页签（空态）
        let remaining: Vec<MainTab> = self
            .state
            .read(cx)
            .present_tabs()
            .into_iter()
            .filter(|t| *t != tab)
            .collect();
        let next = ix
            .and_then(|i| {
                remaining
                    .get(i)
                    .copied()
                    .or_else(|| remaining.last().copied())
            })
            .or_else(|| remaining.first().copied());
        if let Some(next) = next {
            self.state.update(cx, |s, cx| {
                if s.tab != next {
                    s.tab = next;
                    cx.notify();
                }
            });
            (self.handlers.on_tab)(next);
        }
    }
}
