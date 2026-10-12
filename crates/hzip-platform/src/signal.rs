//! 信号 → 取消令牌桥接（02 §5.3.9；TC-WIN-009 / TC-CLI-015 支撑）。
//!
//! 平台差异：
//!
//! | 平台 | 捕获 | 第二次信号 |
//! |---|---|---|
//! | Windows | `SetConsoleCtrlHandler`：`CTRL_C_EVENT` / `CTRL_CLOSE_EVENT` | 第二次 CTRL_C 移除 handler 后返回 FALSE（默认处理即终止） |
//! | POSIX | `libc::sigaction`：SIGINT / SIGTERM | 第二次恢复 `SIG_DFL` 后 `raise(sig)` 重入默认处理 |
//!
//! 桥接只到根令牌；后续状态迁移（JobManager → Cancelled → 退出码 130）由 core 完成
//! （02 §4.2），本模块不含业务逻辑。信号 handler 全局静态、单实例安装（重复安装
//! 返回错误或替换，rustdoc 声明）。
//!
//! # 占位交付（任务卡 §2 注）
//!
//! `install_cancel_handler` 消费 `hzip_progress::CancellationToken`（T10 同波并行）。
//! **T10 未合入**（本交付时 `hzip-progress` 仍为空骨架），本模块以签名占位 +
//! `todo!()` + `#[ignore]` 测试交付（05 §1.2 允许占位、禁止静默砍功能）；
//! T10 合入后按冻结签名补全：
//!
//! ```rust,ignore
//! pub fn install_cancel_handler(root: hzip_progress::CancellationToken) -> Box<dyn Fn()>;
//! ```
//!
//! 冻结签名见 02 §5.3.9；禁止改动 hzip-progress 公开签名。

/// 安装取消桥接（02 §5.3.9 冻结签名）。
///
/// **占位**：T10（hzip-progress）合入后签名变为
/// `install_cancel_handler(root: CancellationToken) -> Box<dyn Fn()>`；返回的
/// `Box<dyn Fn()>` 是卸载器（恢复原 handler），Engine 退出时调用。
pub fn install_cancel_handler(_root: ()) -> Box<dyn Fn()> {
    todo!("T10 hzip-progress 合入后按冻结签名补全（02 §5.3.9；任务卡 §2 占位）")
}

#[cfg(test)]
#[path = "tests/signal.rs"]
mod tests;
