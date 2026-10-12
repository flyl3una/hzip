//! 自研线程池（02 §9.2 节点 D；T30 §1 裁定）。
//!
//! 语义：**长生命周期任务 + 受控并发 + 每任务可取消**，非数据并行 steal
//! 调度（rayon 归 T33 压缩管道内部）。池通道用 rendezvous（`bounded(0)`）——
//! 队列本体（含门控回退）在 JobManager，本池只接收「已获准运行」的票。
//!
//! worker 数 = `min(max(2, 核数), 4)`（[`JOB_POOL_SIZE`]，附录 B.2），可被
//! `EngineConfig.pool_size` 覆盖（测试用小池）。panic 隔离在 job 粒度
//! （JobManager 侧 catch_unwind），worker 线程本身不因任务 panic 退出。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Sender, bounded};
use hzip_progress::CancellationToken;
use hzip_progress::JobId;

/// 物理 worker 数：`min(max(2, 核数), 4)`（02 附录 B.2 `JOB_POOL_SIZE`）。
///
/// `available_parallelism` 非 const——以惰性求值提供（`OnceLock`，进程内
/// 首次调用确定）。
pub fn job_pool_size() -> usize {
    use std::sync::OnceLock;
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| match std::thread::available_parallelism() {
        Ok(n) => n.get().clamp(2, 4),
        Err(_) => 2,
    })
}

/// 关停轮询间隔（worker 检查 closed 标志）。
const SHUTDOWN_POLL: Duration = Duration::from_millis(20);

/// 池内任务票（已获准运行；携带 job_id 供诊断）。
type Ticket = (JobId, Box<dyn FnOnce() + Send + 'static>);

/// 自研线程池。
pub(crate) struct Pool {
    tx: Sender<Ticket>,
    handles: Mutex<Vec<JoinHandle<()>>>,
    /// 关闭标志（shutdown 置位；worker 轮询退出）。实例级——多 Engine
    /// 实例互不影响。
    closed: Arc<AtomicBool>,
}

impl Pool {
    /// 构建池（worker 数；root 取消不终止 worker——取消是协作式，正在执行
    /// 的任务自行在检查点退出，worker 继续取下一票）。
    pub(crate) fn new(size: usize, _root: &CancellationToken) -> Arc<Self> {
        let (tx, rx) = bounded::<Ticket>(0); // rendezvous：队列在 JobManager
        let closed = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::with_capacity(size);
        for i in 0..size {
            let rx = rx.clone();
            let closed_c = Arc::clone(&closed);
            let h = std::thread::Builder::new()
                .name(format!("hzip-worker-{i}"))
                .spawn(move || {
                    loop {
                        // 关闭检查：closed 置位后不再接票
                        if closed_c.load(Ordering::Acquire) {
                            break;
                        }
                        match rx.recv_timeout(SHUTDOWN_POLL) {
                            Ok((_job_id, job)) => {
                                // panic 隔离在 job 粒度（JobManager catch_unwind
                                // 包裹），此处防御性再包一层——worker 线程永不
                                // 因任务 panic 退出。
                                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                });
            let h = match h {
                Ok(h) => h,
                Err(e) => panic!("spawn worker thread: {e}"),
            };
            handles.push(h);
        }
        Arc::new(Self {
            tx,
            handles: Mutex::new(handles),
            closed,
        })
    }

    /// 提交任务执行（阻塞至 worker 接票——rendezvous 语义）。
    pub(crate) fn execute(&self, job_id: JobId, job: impl FnOnce() + Send + 'static) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let _ = self.tx.send((job_id, Box::new(job)));
    }

    /// 关停：置位 + join 全部 worker（等当前任务结束；取消由 Engine 侧先发）。
    pub(crate) fn shutdown(self: &Arc<Self>) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return; // 幂等
        }
        let handles = self
            .handles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .collect::<Vec<_>>();
        for h in handles {
            let _ = h.join();
        }
    }
}
