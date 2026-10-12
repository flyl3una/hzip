//! 页面视图（T-GUI T10 归位：页面装配自 layout/ 迁入）。
//!
//! - [`browse`]：浏览页数据装配 + 表格委托（布局见 [`browse_layout`]）
//! - [`jobs`]：任务中心页内容区
//! - [`open_progress`]：「正在打开档案」进度窗（打开耗时超门槛才弹）
//! - [`preview`]：预览独立窗口
//! - [`settings_layout`]：设置面板骨架（Tabs/状态行/打开路径）
//! - [`settings_render`]：设置面板注册表驱动渲染器（页装配 + 叶子控件）
//! - [`progress`]：压缩/解压进度独立窗口（T-PW）

pub mod browse;
pub mod browse_layout;
pub mod jobs;
pub mod op_window;
pub mod open_progress;
pub mod preview;
pub mod progress;
pub mod prompt_window;
pub mod settings_layout;
pub mod settings_render;
