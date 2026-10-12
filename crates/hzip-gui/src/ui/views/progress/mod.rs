//! 压缩/解压进度窗口（独立 OS 窗口；T-PW）。
//!
//! 参考 Bandizip / 7-Zip 形态：新窗口显示进度与文件细节，非 Dialog——主窗口
//! 浏览/其他任务不受影响。每任务一窗（`job_id` 键控注册表防重复开）。
//!
//! # 布局（T-PW README §3）
//!
//! 概要行（源 → 目标）→ 大号百分比 → kit `Progress` → 状态+统计行（左：Badge.dot
//! 语义色 + 状态标签；右：速度/剩余/条数——同行左右分布）→ 当前条目 → 输出区
//! （Tabs：结果/成功/失败 + 只读 Textarea，**自适应高度**吃满剩余空间）→
//! 按钮行（左「完成后自动关闭窗口」复选框 / 右取消=danger）。
//!
//! # 窗口生命周期（T-PW README §5）
//!
//! - **关窗 ≠ 取消**：✕/Esc 只关窗（任务继续；任务中心可见）；
//!   「取消」按钮经注入的取消回调（与任务中心取消同一通道）。
//! - 终局 `Done`/`Cancelled` → 勾选「完成后退出」时**排定延迟关窗**
//!   （进度到 100% 后停留 `[jobs].exit_delay_ms` 再关；排定在
//!   [`dispatch_rows`]、到点执行在 [`tick_pending_closes`]——终局后无行信号，
//!   分发门控会停，必须靠泵的兜底 tick 驱动）；未勾选恒停留。
//!   `Failed`/`PartialDone` → 恒停留展示失败明细，取消按钮变「关闭」。
//! - 「完成后自动关闭窗口」复选框与设置-解压设置「完成后退出」是**同一配置**
//!   （`[jobs].exit_on_finish`）：勾选经注入回调走 `apply_setting` 五步流程
//!   落盘；勾上后全部任务完成 → 等待 `[jobs].exit_delay_ms`（默认 1 s，仅
//!   配置可调）→ 进程退出（`pump_host.rs`）。
//! - 数据面 [`ProgressWindow::apply`]：行数据纯拷贝，文本/颜色全部渲染侧派生。
//!
//! # 偏差登记
//!
//! 窗口根视图为 `ProgressWindow` 实体（preview.rs 同模式），未包
//! `component::Root`——本窗口无对话框/notification 层需求；若后续需要在
//! 进度窗口弹对话框，改回 Root 包裹（单点改动）。
//!
//! # 尺寸策略（T-PW README §2 含偏差登记）
//!
//! gpui-pre 0.3.8（`platform.rs:2438` 的 `WindowOptions`）仍无 `window_max_size`
//! ——min 420×320 原生硬约束（`window_min_size`），max 720×640 布局软约束
//! （内容列 `max_w/max_h` 居中，拖大时两侧留主题背景色）。
//! （0.3.4 → 0.3.8 该结构字段集未变，17 个字段逐一对照相同。）
//!
//! # 子模块（T-STD：本文件只做声明与再导出）
//!
//! | 文件 | 内容 |
//! |---|---|
//! | `window` | 窗口实体与渲染：`ProgressWindow` / `Render` / 渲染侧派生件 |
//! | `registry` | Global 注册表：开窗 / 判活 / 退场 / 延迟关窗截止时刻 |
//! | `pump` | 泵分发：灌行 / 延迟关窗排定与到点检查 / 语言切换重建 |
//!
//! 业务项迁出后，本模块以 `pub use` / `pub(crate) use` 保持原公开面
//! （`crate::ui::views::progress::X` 路径零改动）。

mod file_list;
mod pump;
mod registry;
mod window;

pub use crate::init_locale;
pub use pump::{dispatch_rows, is_close_on_terminal, relocalize, tick_pending_closes};
pub use registry::open_progress_window;
pub use window::{CancelHandler, ExitToggleHandler, ProgressWindow};

// —— 原 `mod.rs` 内 `pub(crate)` 项：跨文件后仍以同档可见性再导出 ——
pub(crate) use registry::exit_on_finish_effective;

// —— 目录内共享（`pump` 经 `super::` 消费；初始化时机/顺序未变）——
use registry::{ProgressGlobal, close_slot, live_slots};

// 渲染侧派生的可测面（`tests/mod_rs.rs` 经 `use super::*` 引用；业务期只在
// `window.rs` 内用）——仅测试引用，非测试构建不导入 → 免 unused 告警。
#[cfg(test)]
use window::{active_state_line, stat_line};

#[cfg(test)]
#[path = "tests/mod_rs.rs"]
mod tests;
