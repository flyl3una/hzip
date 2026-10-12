//! 取消令牌（02 §4.2；TC-CORE-011）。
//!
//! [`CancellationToken`] 以「原子标志 + 代际计数 + 父链」实现幂等取消与父子级联：
//!
//! - `is_cancelled()` 只读自身 state 的低 1 位（一条 relaxed load + 位与），满足
//!   格式层 `ctx.checkpoint()?` 单次开销 < 5 ns 的要求（路径上无锁/分配/遍历）；
//! - `cancel()` 幂等：CAS 置位 + 代际递增，随后沿 `children`（Weak，防父→子强环）
//!   向下级联；父取消 → 子自动取消，子取消**不影响**父（单向）；
//! - `child()` 构造时若父已取消，新子直接以已取消态构造（防止「逃逸子」）；
//! - 代际计数供等待者以「代际变化」为唤醒条件（Condvar 休眠，避免轮询烧 CPU）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use hzip_error::HzipError;

/// 协作式取消令牌（02 §4.2 冻结签名）。
#[derive(Debug)]
pub struct CancellationToken(Arc<CancelInner>);

#[derive(Debug)]
struct CancelInner {
    /// 低 1 位 = cancelled 标志；高位 = 代际计数（每次 cancel 递增 2）。
    state: AtomicU64,
    /// 父链：子令牌持有父的强引用（父存活期 ⊇ 子，02 §4.2 单向级联的
    /// 结构性保证——父不持有子强引用，故子必须反向持有父防其提前释放）。
    /// 字段本身不在查询路径上读取（取消沿 `children` 下行），属防悬垂持有。
    #[allow(dead_code)]
    parent: Option<CancellationToken>,
    /// 子令牌弱引用表：cancel 时主动向下级联。
    children: Mutex<Vec<Weak<CancelInner>>>,
    /// 等待组：代际变化时 `notify_all` 唤醒。
    cv: Condvar,
    cv_lock: Mutex<()>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// 根令牌（无父）。
    pub fn new() -> Self {
        Self(Arc::new(CancelInner {
            state: AtomicU64::new(0),
            parent: None,
            children: Mutex::new(Vec::new()),
            cv: Condvar::new(),
            cv_lock: Mutex::new(()),
        }))
    }

    /// 子令牌：父取消 → 子自动取消；子取消不影响父。
    ///
    /// 构造时父已取消 → 新子直接处于取消态（防逃逸子，02 §4.2）。
    pub fn child(&self) -> Self {
        let inner = Arc::new(CancelInner {
            state: AtomicU64::new(0),
            parent: Some(self.clone()),
            children: Mutex::new(Vec::new()),
            cv: Condvar::new(),
            cv_lock: Mutex::new(()),
        });
        if let Ok(mut guard) = self.0.children.lock() {
            guard.retain(|w| w.strong_count() > 0);
            guard.push(Arc::downgrade(&inner));
        }
        if self.is_cancelled() {
            // 竞态兜底：父刚取消，本子直接置取消态
            CancellationToken(Arc::clone(&inner)).cancel();
        }
        CancellationToken(inner)
    }

    /// 幂等取消：置取消位 + 代际递增 + 唤醒等待者 + 向下级联。
    pub fn cancel(&self) {
        let inner = &self.0;
        // ① 幂等置位 + 代际递增（CAS 循环）
        loop {
            let cur = inner.state.load(Ordering::Relaxed);
            if cur & 1 == 1 {
                break; // 已取消 → 幂等返回
            }
            let want = cur.wrapping_add(2) | 1; // 代际 +1（进 2），置取消位
            if inner
                .state
                .compare_exchange_weak(cur, want, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        // ② 唤醒等待者（持锁 notify，避免丢失唤醒）
        if let Ok(guard) = inner.cv_lock.lock() {
            inner.cv.notify_all();
            drop(guard);
        }
        // ③ 向下级联：遍历 children 中的存活 Weak，逐个 cancel
        if let Ok(mut guard) = inner.children.lock() {
            let mut keep: Vec<Weak<CancelInner>> = Vec::with_capacity(guard.len());
            for w in guard.drain(..) {
                if let Some(child) = w.upgrade() {
                    CancellationToken(child).cancel();
                    keep.push(w);
                }
            }
            *guard = keep;
        }
    }

    /// 是否已取消：单条 relaxed load + 位与（< 5 ns 热路径，无锁无遍历）。
    pub fn is_cancelled(&self) -> bool {
        self.0.state.load(Ordering::Relaxed) & 1 == 1
    }

    /// 检查点：已取消则 `Err(HzipError::Cancelled)`（02 §4.2 语义）。
    pub fn throw_if_cancelled(&self) -> Result<(), HzipError> {
        if self.is_cancelled() {
            Err(HzipError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// 等待取消，至多阻塞 `timeout`。已取消立即返回 `true`；
    /// 超时未取消返回 `false`。基于代际变化唤醒（Condvar），不轮询。
    pub fn wait_cancelled(&self, timeout: Duration) -> bool {
        let inner = &self.0;
        if inner.state.load(Ordering::Acquire) & 1 == 1 {
            return true;
        }
        let mut guard = match inner.cv_lock.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let cur = inner.state.load(Ordering::Acquire);
            if cur & 1 == 1 {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let (g, _) = inner
                .cv
                .wait_timeout_while(guard, deadline - now, |_| {
                    inner.state.load(Ordering::Acquire) & 1 == 0
                })
                .unwrap_or_else(|p| p.into_inner());
            guard = g;
        }
    }

    /// 内部代际值（测试断言用：幂等取消只跃迁一次）。
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.0.state.load(Ordering::Relaxed) >> 1
    }
}

impl Clone for CancellationToken {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl PartialEq for CancellationToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CancellationToken {}

#[cfg(test)]
#[path = "tests/token.rs"]
mod tests;
