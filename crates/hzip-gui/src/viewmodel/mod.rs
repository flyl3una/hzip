//! ViewModel 层（02 §11.1 分层；T41 独占）。
//!
//! # 分层规则（02 §11.1 原文，红线）
//!
//! gpui/gpui_kit 组件内**零业务逻辑**；ViewModel 不 import gpui/gpui_kit 类型（纯数据
//! [`rows`] 等），保证 `cargo test -p hzip-gui --lib` 无需图形环境。
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`rows`] | 纯数据行：EntryRow / JobRow / FailureRow（零 gpui/gpui_kit import） |
//! | [`main_vm`] | MainViewModel：条目缓存 / 过滤 / 树构建 / 选择集 / refresh |
//! | [`jobs_vm`] | JobsViewModel：队列行状态机 / 进度聚合 / 取消 |
//! | [`dialog_vm`] | 压缩/解压对话框参数 → JobSpec 构建 |
//! | [`router`] | 密码三态 / 冲突会话（全部应用）请求-应答状态机 |
//! | [`pump`] | 事件消费循环（channel select + 10 Hz flush + VmNotifier 回调） |
//! | [`setting_id`] | 设置项标识（T-SET 注册表键；跨层共享载荷，无顺序语义） |
//!
//! # 请求-应答契约
//!
//! 密码/冲突请求来自 core `JobHandle.requests()`（容量 1，02 §9.11）：
//! worker 上抛 → pump 路由进 [`router`] 弹窗状态机 → 用户提交 →
//! `handle.answer(req_id, resp)`；120 s 决策超时（core 侧 `REQUEST_TIMEOUT_SECS`，
//! HZ-5002）→ 任务 Failed → GUI 消费终态复位弹窗（02 §11.3）。
//!
//! 出处：02 §11.1/§11.3/§9.11、附录 B.2。

pub mod density;
pub mod dialog_vm;
pub mod jobs_vm;
pub mod main_vm;
/// 标准消息弹窗载荷（级别/标题/详情；UI 层统一渲染）
pub mod message;
pub mod pump;
pub mod router;
pub mod rows;
pub mod setting_id;

#[cfg(test)]
#[path = "tests/testutil.rs"]
pub(crate) mod testutil;

pub use dialog_vm::{CompressDialogVm, ExtractDialogVm};
pub use hzip_error::PasswordTarget;
pub use jobs_vm::JobsViewModel;
pub use main_vm::{Filter, MainViewModel};
pub use pump::{VmNotifier, VmPump, VmSignal};
pub use router::{ConflictSession, DialogRouter, GateAction, OpenForm, PasswordDialogState};
pub use rows::{EntryRow, FailureRow, JobRow, JobRowOutcome, JobRowState, RecentEntry};
pub use setting_id::SettingId;
