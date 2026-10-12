//! 退出钩子（T42 exit；02 §11.2 G + §5.3.9 宽限）。
//!
//! 窗口关闭/退出：取消全部任务（`Engine::cancel_all` 级联）→ 等活跃泵线程
//! 落账 ≤ `CANCEL_GRACE_MS`(2 s) → TempArea 清理 → 返回退出。
//! 钩子内不启动新任务/不保存档案（§5.3.9）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hzip_core::engine::Engine;
use hzip_platform::temp_area::TempArea;

use crate::viewmodel::jobs_vm::{JobsViewModel, is_terminal};

/// 取消宽限（02 §5.3.9 `CANCEL_GRACE_MS=2000`；T42 consts 登记）。
pub const CANCEL_GRACE_MS: u64 = 2000;

/// 执行退出钩子：取消全部 → 等活跃任务落账 ≤2 s → 清理 TempArea。
pub fn graceful_shutdown(
    engine: &Engine,
    jobs: &Arc<Mutex<JobsViewModel>>,
    temp: Option<&TempArea>,
) {
    // 配置落盘收口（2026-09-24 审计 B6）：设置变更走**异步**写（后台合并写），
    // 退出前必须等它写完，否则最后一次设置可能丢。上限 `FLUSH_TIMEOUT`，超时
    // 只记 warn 不阻塞退出。
    if !hzip_config::Config::flush_pending_saves() {
        tracing::warn!("退出前配置落盘超时（可能有最后一次设置未落盘）");
    }
    engine.cancel_all();
    // 泵线程以 JobFinished 落账退出（pump_host）；宽限内轮询活跃行收敛。
    // 等待以固定小步 sleep 轮询实现（不 Join 泵线程——泵自然退出；
    // 宽限尽则进程退出自然终止，取消已尽力）。
    let deadline = std::time::Instant::now() + Duration::from_millis(CANCEL_GRACE_MS);
    loop {
        let active = jobs
            .lock()
            .map(|j| j.rows().iter().any(|r| !is_terminal(r.state)))
            .unwrap_or(false);
        if !active {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if let Some(t) = temp {
        let _ = t.cleanup();
    }
}

#[cfg(test)]
#[path = "tests/exit.rs"]
mod tests;
