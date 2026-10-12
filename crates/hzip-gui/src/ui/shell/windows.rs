//! 主窗口注册表（T-MENU10：多窗口架构的信号路由与退出语义）。
//!
//! # 设计裁定（设计 §5.1 三问）
//!
//! | 问题 | 裁定 |
//! |---|---|
//! | `AppData` 共享还是独立 | **每窗口独立**（独立浏览态/任务路由/设置投影）——仅共享 `Engine`（线程池只建一次）；单实例锁是**进程级**，同进程多窗口不冲突（`instance.rs`） |
//! | `signal_tx` 单接收端怎么办 | **每窗口一条信号通道**（独立 `SignalTx` 存于该窗口的 `AppData.signal_tx`）——任务提交/设置变更自然回投到发起窗口；泵按窗口 id 分发快照 |
//! | 退出语义 | 关单窗口 ≠ 退进程；**全部窗口关**才走既有退出链（`exit_on_finish` 判定在「最后窗口」，`QuitMode::LastWindowClosed` 与之一致） |
//!
//! # 为什么注册表要独立成模块
//!
//! 泵线程（后台）不能触碰 `Entity`（gpui 线程亲和），只能触碰进程级共享量；
//! 窗口实体注册/注销是**主线程**动作，泵消费时经注册表快照取句柄。故注册表
//! 必须是 `Arc<Mutex<..>>` 的纯数据结构（本模块），生命周期与进程同。
//!
//! 「本次信号属于哪个窗口」由**通道归属**回答，不需要在信号载荷里塞
//! window_id：`VmSignal` 是 `Copy` 小枚举，且所有生产者（`AppData` 的
//! `dispatch_effect` / `submit_spec` / 泵 `JobsChanged`）都持有**自己窗口的**
//! `SignalTx`。给信号加 window_id 反而要求每个生产者都知道 id。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gpui_kit::Entity;

use crate::ui::shell::{AppDataHandle, AppShell};

/// 开窗工厂签名（T-MENU10）：给定数据面与通道，开一个主窗口并登记注册表。
///
/// 类型别名收口两处 `type_complexity`（定义点 + 自引用槽点）。
pub type SpawnWindowFn =
    dyn Fn(&mut gpui_kit::App, AppDataHandle, Arc<crate::ui::contracts::SignalTx>);

/// 自引用槽：工厂需要在已构建的 shell 上注入自己（闭包不能引用自身）。
pub type SpawnWindowSlot = std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<SpawnWindowFn>>>>;

/// 无参开窗（注入到 `AppShell` 的「新窗口」入口签名）。
pub type NewWindowFn = dyn Fn(&mut gpui_kit::App);

/// 单窗口条目（注册表值）。
pub struct WindowEntry {
    /// 窗口 id（进程内单调；注册表键）
    pub id: u64,
    /// 该窗口的 shell 实体（泵经此刷新快照）
    pub shell: Entity<AppShell>,
    /// 该窗口的数据面（独立浏览态/任务/设置投影）
    pub data: AppDataHandle,
    /// 窗口句柄（弹窗宿主解析/判活）
    pub handle: gpui_kit::AnyWindowHandle,
}

/// 主窗口注册表（进程级；主线程写，泵线程读）。
#[derive(Default)]
pub struct WindowRegistry {
    windows: Mutex<HashMap<u64, WindowEntry>>,
    next_id: Mutex<u64>,
}

impl WindowRegistry {
    /// 新建空注册表。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 分配窗口 id（单调；不写注册表——窗口构造完成后再 [`Self::register`]）。
    pub fn alloc_id(&self) -> u64 {
        let mut n = self.next_id.lock().unwrap_or_else(|p| p.into_inner());
        *n += 1;
        *n
    }

    /// 注册窗口（主线程；窗口实体构造完成后调用）。
    pub fn register(&self, entry: WindowEntry) {
        let mut w = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        w.insert(entry.id, entry);
    }

    /// 注销窗口（主线程；`on_window_closed` 调用）。返回注销后剩余窗口数。
    ///
    /// 按 `WindowId` 匹配（`on_window_closed` 回调给的是 id，不是句柄——
    /// 窗口此刻可能已不可 update）。
    pub fn unregister_window_id(&self, window_id: gpui_kit::WindowId) -> usize {
        let mut w = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        w.retain(|_, e| e.handle.window_id() != window_id);
        w.len()
    }

    /// 全部窗口快照（泵每 tick 取一次；`Entity` 是廉价句柄克隆）。
    ///
    /// 返回 `(id, shell, data)`——**判活不在此处**（泵在自己的主线程上下文
    /// 里用 `cx.windows()` 过滤；注册表不依赖 gpui 运行时状态）。
    pub fn snapshot(&self) -> Vec<(u64, Entity<AppShell>, AppDataHandle)> {
        let w = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<_> = w
            .iter()
            .map(|(id, e)| (*id, e.shell.clone(), e.data.clone()))
            .collect();
        out.sort_by_key(|(id, _, _)| *id); // 稳定序（多窗口刷新顺序确定）
        out
    }

    /// 存活窗口数。
    pub fn len(&self) -> usize {
        self.windows.lock().map(|w| w.len()).unwrap_or_default()
    }

    /// 是否无窗口（退出判定输入）。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 最近注册的窗口句柄（弹窗宿主解析兜底；无窗口 → None）。
    pub fn last_handle(&self) -> Option<gpui_kit::AnyWindowHandle> {
        let w = self.windows.lock().ok()?;
        w.values().max_by_key(|e| e.id).map(|e| e.handle)
    }
}

/// 全部窗口是否已关闭（T-MENU10 退出语义；纯函数，可单测）。
///
/// `registered` = 注册表存活数，`live_windows` = gpui 当前窗口数。
/// 两者都为 0 才判定「全部窗口已关」——注册表可能滞后一帧（窗口关闭回调
/// 与 `cx.windows()` 更新有先后），取交集最保守：任一非零即视为「还有窗口」，
/// 避免误判提前走退出链（02 §5.3.9 优雅退出不可回退）。
pub fn all_windows_closed(registered: usize, live_windows: usize) -> bool {
    registered == 0 && live_windows == 0
}

#[cfg(test)]
#[path = "tests/windows.rs"]
mod tests;
