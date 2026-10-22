//! AppShell 主窗口骨架（T-G10 chrome 实体 + T-G30 run_ui 装配；06 §4.1）。
//!
//! T-GUI T11 拆分：单文件 → 四块。
//!
//! | 文件 | 职责 |
//! |---|---|
//! | [`mod`]（本文件） | AppShell（chrome 装配渲染）+ ShellState |
//! | [`dispatch`] | dispatch_toolbar + submit_extract_locked（业务分发） |
//! | [`keymap`] | 14 条可配置键位（`[keys]` 配置 ↔ gpui keymap 的唯一事实源） |
//! | [`runtime`] | run_ui + 泵循环 + LaunchIntent/args（启动序列） |
//! | [`prompt`] | open_prompt_dialog + answer_prompt（请求-应答） |
//!
//! # 渲染分区（06 §4.1 自上而下）
//!
//! 自绘标题栏（`TitleBar`）→ 菜单条（T-G51 自绘触发器 + PopupMenu，菜单项 → Action 路由）→
//! 工具栏（图标 Button 行）→ busy 条 → 页签行（Tab）→ 内容插槽（T-G20/T-G21
//! 承接）→ 状态栏（计数 + 任务胶囊）。
//!
//! # 数据纪律（T-G10 §6.2）
//!
//! 本卡不接任何 VM；一切数据来自 [`AppStateUiSnapshot`]（T-G30 导出）。

mod actions;
mod app_shell;
mod clipboard_ops;
pub(crate) mod dispatch;
mod errors;
pub(crate) mod external_edits;
mod file_ops;
pub(crate) mod keymap;
mod open_flow;
mod prompt;
mod render;
mod runtime;
mod session;
mod state;
pub mod windows;

pub(crate) use dispatch::open_progress_window;
pub use runtime::{AppDataHandle, ArgvParse, LaunchArgs, run_ui};

// 参数窗（`ui::views::op_window`）用的提交/开窗入口：`dispatch` 本身不对外
// （它是 shell 内部实现），故按需再导出这三个——它们正是「唯一提交点 +
// 唯一进度窗入口」，参数窗与主界面共用同一份。
pub(crate) use dispatch::{open_progress_window_with, submit_compress, submit_extract};
// 泵生效分派的可测面（泵循环内联在 `run_ui` 异步闭包，测试不可达；
// 仅测试引用，非测试构建不导入 → 免 unused 告警）
#[cfg(test)]
pub(crate) use runtime::{apply_ui_effects, batch_requested};

// —— T-STD：业务项已移入同目录子模块，此处再导出保持外部路径不变 ——

pub use app_shell::AppShell;
pub use state::ShellState;
// `actions!` 生成的 21 个单元动作（原为 `pub struct`，路径 `crate::ui::shell::<动作>`）
pub use actions::{
    About, AddFiles, CheckUpdate, ClearMru, CopyFiles, DeleteFiles, Extract, FindFile, NewWindow,
    Open, Paste, Quit, RefreshView, Reveal, SelectAll, SelectInvert, Settings, ShowJobs,
    TestIntegrity, ToggleJobs, ToggleTree,
};
// 载荷动作（原为 `pub(crate)`：菜单条/渲染按 `crate::ui::shell::<动作>` 取用）
pub(crate) use actions::{OpenRecent, SetToolbarStyle, SetViewStyle};

pub(crate) use errors::hz_reason;
pub(crate) use open_flow::{OpenFlow, open_archive_task, run_open_flow};
pub(crate) use session::{
    SESSION_SUPERSEDED, spawn_browse_reopen, spawn_rows_rebuild, start_session_switch,
};

#[cfg(test)]
#[path = "tests/mod_rs.rs"]
mod tests;

// 键位表用例（T-KEYS）：三态语义 + 「与 kit 共用一个 keymap」的 install 纪律。
// 与 `tests/mod_rs.rs` 分开挂：本模块的断言面是纯函数 + 真实 `cx.key_bindings()`，
// 不涉及 AppShell 渲染，合进 mod_rs 只会让那个文件继续膨胀。
#[cfg(test)]
#[path = "tests/keymap.rs"]
mod keymap_tests;
