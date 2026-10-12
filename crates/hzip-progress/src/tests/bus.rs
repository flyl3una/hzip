use super::*;
use crate::event::{JobStats, Outcome};

fn started() -> ProgressEvent {
    ProgressEvent::JobStarted {
        job_id: 1,
        total_bytes: 1000,
        total_entries: 10,
    }
}

fn bytes(delta: u64) -> ProgressEvent {
    ProgressEvent::Bytes { delta }
}

fn finished() -> ProgressEvent {
    ProgressEvent::JobFinished {
        job_id: 1,
        outcome: Outcome::Done(JobStats::new(1000, 10)),
    }
}

#[test]
fn broadcast_to_all_subscribers() {
    let bus = ProgressBus::new();
    let mut subs: Vec<_> = (0..3).map(|_| bus.subscribe()).collect();
    let n = 200u64;
    bus.send(started());
    for i in 0..n {
        bus.send(bytes(i + 1));
    }
    bus.send(finished());

    for sub in subs.iter_mut() {
        let mut got = Vec::new();
        while let Some(ev) = sub.try_recv() {
            got.push(ev);
        }
        // 同序副本：Started 首、Finished 尾（Bytes 可能被剥离但边界必达）
        assert!(matches!(
            got.first(),
            Some(ProgressEvent::JobStarted { .. })
        ));
        assert!(matches!(
            got.last(),
            Some(ProgressEvent::JobFinished { .. })
        ));
    }
    assert_eq!(bus.subscriber_count(), 3);
}

#[test]
fn slow_subscriber_no_block() {
    let bus = ProgressBus::new();
    let slow = bus.subscribe();
    let _fast = bus.subscribe();
    // 猛发 10× 容量 Bytes（不读 slow）——send 必须全部立即返回
    for _ in 0..(BUS_CHANNEL_CAP * 10) {
        bus.send(bytes(1));
    }
    bus.send(finished());
    // slow 队列中必然存在 JobFinished
    let mut saw_finished = false;
    let mut byte_count = 0usize;
    while let Some(ev) = slow.try_recv() {
        match ev {
            ProgressEvent::Bytes { .. } => byte_count += 1,
            ProgressEvent::JobFinished { .. } => saw_finished = true,
            _ => {}
        }
    }
    assert!(saw_finished, "JobFinished 必达");
    assert!(
        byte_count <= BUS_CHANNEL_CAP,
        "Bytes 数量 ≤ 容量（旧事件被剥离）"
    );
}

#[test]
fn disconnected_cleanup() {
    let bus = ProgressBus::new();
    {
        let sub = bus.subscribe();
        bus.send(bytes(1));
        drop(sub);
    }
    // drop 后继续 send 正常、无 panic
    bus.send(bytes(2));
    bus.send(started());
    // 重建订阅仍可用
    let s2 = bus.subscribe();
    bus.send(finished());
    let mut got = Vec::new();
    while let Some(ev) = s2.try_recv() {
        got.push(ev);
    }
    assert!(!got.is_empty());
}

#[test]
fn recv_after_bus_drop() {
    let bus = ProgressBus::new();
    let sub = bus.subscribe();
    bus.send(finished());
    drop(bus);
    // 已发事件仍可消费；通道断开后 recv 返回 None 不 panic
    let ev = sub.recv().expect("已发送的 JobFinished 应可接收");
    assert!(matches!(ev, ProgressEvent::JobFinished { .. }));
}

#[test]
fn recv_timeout_trichotomy() {
    use std::time::Duration;
    let bus = ProgressBus::new();
    let sub = bus.subscribe();
    // 空通道仍活 → Timeout（可继续等，而非误判退出）
    assert!(matches!(
        sub.recv_timeout(Duration::from_millis(10)),
        RecvTimeout::Timeout
    ));
    // 有事件 → Event
    bus.send(started());
    assert!(matches!(
        sub.recv_timeout(Duration::from_millis(500)),
        RecvTimeout::Event(ProgressEvent::JobStarted { .. })
    ));
    // 总线 drop → Closed（不阻塞不 panic）
    drop(bus);
    assert!(matches!(
        sub.recv_timeout(Duration::from_millis(10)),
        RecvTimeout::Closed
    ));
}
