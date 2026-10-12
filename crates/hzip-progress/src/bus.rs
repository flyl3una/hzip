//! 进度事件总线（02 §4.3/§4.1；TC-CORE-010）。
//!
//! [`ProgressBus`] 为多订阅者广播：每个订阅者一条有界 crossbeam 通道（独立队列，
//! 02 §4.1「crossbeam 广播通道」的扇出形态）。`send` 非阻塞、永不 panic：
//! 满槽时**剥离最旧事件**为关键事件让路——`JobStarted`/`EntryDone`/`JobFinished`
//! 等边界事件绝不静默丢弃（任何订阅者最终必须观察到 `JobFinished`，否则
//! CLI/GUI 挂死）。`Bytes` 类普通事件精度可牺牲（合并语义由 core/throttle 承担）。
//!
//! `ProgressEvent` 实现 `Clone`（HzipError 已 Clone），广播即逐槽 clone。
//!
//! 满槽剥离的实现：每槽除投递 `Sender` 外持一个**弹出 `Receiver`**（clone 自同
//! 一通道）——满时用它 `try_recv()` 弹掉一条最旧事件（crossbeam 多消费者竞争，
//! 弹出者与订阅者竞争，实际弹掉的是队首已入队的最旧事件），再重试投递。

use std::sync::RwLock;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError, bounded};

use crate::event::ProgressEvent;

/// 每订阅者通道容量（内部常量；容纳 10 Hz 消费者正常积压）。
const BUS_CHANNEL_CAP: usize = 256;

#[derive(Debug)]
struct Slot {
    tx: Sender<ProgressEvent>,
    /// 满槽剥离用：与订阅者竞争消费队首。
    popper: Receiver<ProgressEvent>,
    alive: bool,
}

/// 多订阅者广播总线（02 §4.1）。
#[derive(Debug, Default)]
pub struct ProgressBus {
    slots: RwLock<Vec<Slot>>,
}

/// 单订阅者接收端（每订阅者独立有界队列）。
#[derive(Debug)]
pub struct EventReceiver {
    rx: Receiver<ProgressEvent>,
}

/// 限时接收三分态（[`EventReceiver::recv_timeout`]）。
///
/// `try_recv` 把「无事件」与「通道关闭」都塌成 `None`，调用方无法据此决定
/// 「继续等」还是「退出」——限时等待需要区分。
#[derive(Debug)]
pub enum RecvTimeout {
    /// 收到事件。
    Event(ProgressEvent),
    /// 时限内无事件（通道仍活）。
    Timeout,
    /// 通道关闭（总线已 drop）。
    Closed,
}

impl EventReceiver {
    /// 阻塞接收；通道关闭后返回 `None`。
    pub fn recv(&self) -> Option<ProgressEvent> {
        self.rx.recv().ok()
    }

    /// 非阻塞接收；无事件返回 `None`，关闭后亦返回 `None`。
    pub fn try_recv(&self) -> Option<ProgressEvent> {
        match self.rx.try_recv() {
            Ok(ev) => Some(ev),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    /// 限时接收（三分态区分「无事件」与「已关闭」）。
    ///
    /// 供消费者做**静默期检查点**：只在事件到达时排空请求会饿死发生在
    /// 「上个事件之后、下个事件之前」的问询——解压早期密码门（头加密/条目
    /// 加密的首问）正落在这个无事件窗口，靠事件触发的排空会一直拖到 worker
    /// 侧 120 s 决策超时（hz-5002）。
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> RecvTimeout {
        match self.rx.recv_timeout(timeout) {
            Ok(ev) => RecvTimeout::Event(ev),
            Err(RecvTimeoutError::Timeout) => RecvTimeout::Timeout,
            Err(RecvTimeoutError::Disconnected) => RecvTimeout::Closed,
        }
    }
}

impl ProgressBus {
    /// 空总线。
    pub fn new() -> Self {
        Self {
            slots: RwLock::new(Vec::new()),
        }
    }

    /// 新增订阅者（独立有界队列）。
    pub fn subscribe(&self) -> EventReceiver {
        let (tx, rx) = bounded::<ProgressEvent>(BUS_CHANNEL_CAP);
        let popper = rx.clone(); // 弹出端与订阅者同通道竞争
        if let Ok(mut guard) = self.slots.write() {
            guard.retain(|s| s.alive); // 清扫 dead 槽
            guard.push(Slot {
                tx,
                popper,
                alive: true,
            });
        }
        EventReceiver { rx }
    }

    /// 广播事件：非阻塞、永不 panic。满槽时剥离最旧事件让关键事件挤入。
    pub fn send(&self, ev: ProgressEvent) {
        let urgent = is_urgent(&ev);
        let slots = match self.slots.read() {
            Ok(g) => g,
            Err(_) => return, // 读写锁中毒：静默放弃（永不 panic 承诺）
        };
        for slot in slots.iter().filter(|s| s.alive) {
            push_slot(slot, ev.clone(), urgent);
        }
    }

    /// 活跃订阅者数（测试/诊断）。
    pub fn subscriber_count(&self) -> usize {
        self.slots
            .read()
            .map(|g| g.iter().filter(|s| s.alive).count())
            .unwrap_or(0)
    }
}

/// 边界事件：满槽时优先保证其可达（02 §4.1 关键事件不丢）。
fn is_urgent(ev: &ProgressEvent) -> bool {
    matches!(
        ev,
        ProgressEvent::JobStarted { .. }
            | ProgressEvent::EntryDone { .. }
            | ProgressEvent::JobFinished { .. }
    )
}

/// 单槽投递：满则经 `popper` 剥离最旧（关键事件剥到能进为止；普通事件剥到有空位）。
/// 通道容量 ≥ 1 保证循环终止。
fn push_slot(slot: &Slot, ev: ProgressEvent, urgent: bool) {
    loop {
        match slot.tx.try_send(ev.clone()) {
            Ok(()) => return,
            Err(TrySendError::Full(_)) => {
                // 满：先尝试剥离一条最旧
                if slot.popper.try_recv().is_err() {
                    // 剥不动（订阅者正并发消费/竞态）：关键事件用零超时再抢一次，
                    // 抢不到即放弃（避免活锁）；普通事件直接放弃本轮。
                    if urgent
                        && slot
                            .tx
                            .send_timeout(ev.clone(), std::time::Duration::ZERO)
                            .is_ok()
                    {
                        return;
                    }
                    return;
                }
                // 成功剥离一条：重试投递（容量≥1，循环必终止）
                match slot.tx.try_send(ev.clone()) {
                    Ok(()) => return,
                    Err(TrySendError::Disconnected(_)) => return,
                    Err(TrySendError::Full(_)) => continue,
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                // 订阅者已 drop：静默返回（dead 槽由 subscribe 清扫）
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/bus.rs"]
mod tests;
