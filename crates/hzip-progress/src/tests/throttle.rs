use super::*;
use crate::event::{EntryName, JobStats, Outcome};

fn started(total: u64) -> ProgressEvent {
    ProgressEvent::JobStarted {
        job_id: 1,
        total_bytes: total,
        total_entries: 10,
    }
}

fn entry_done(name: &str, bytes: u64) -> ProgressEvent {
    ProgressEvent::EntryDone {
        entry: EntryName::from(name),
        bytes,
        crc_ok: true,
    }
}

fn finished() -> ProgressEvent {
    ProgressEvent::JobFinished {
        job_id: 1,
        outcome: Outcome::Done(JobStats::new(0, 0)),
    }
}

// JobTotal 补报：骨架期 total=0 → 补报后立即产出带真实 total 的快照
// （GUI 进度条 0%→100% 跳变根因修复：total 恒 0 时进度恒 None）
#[test]
fn job_total_supplies_missing_total() {
    let mut t = Throttle::new();
    let s0 = t.push(&started(0)).unwrap();
    assert_eq!(s0.total, 0, "骨架期 JobStarted total=0");
    let s1 = t
        .push(&ProgressEvent::JobTotal {
            job_id: 1,
            total_bytes: 100_000,
            total_entries: 10,
        })
        .unwrap();
    assert_eq!(s1.total, 100_000, "补报立即生效并冲刷快照");
    assert_eq!(s1.done, 0);
    // 后续条目边界冲刷沿用补报 total
    let s2 = t.push(&entry_done("a.txt", 40_000)).unwrap();
    assert_eq!(s2.total, 100_000);
    assert_eq!(s2.done, 40_000);
    t.push(&finished());
}

#[test]
fn window_merge_10hz() {
    // 预热期 500ms 会使 EWMA 无速度，但快照仍应按窗口输出。
    // 用 200ms 内密集 Bytes 验证「合并 ≤ 1 快照」由窗口控制。
    let mut t = Throttle::new();
    t.push(&started(100_000));
    // 强迫经过一个窗口：把 last_flush 前移不可行，直接投喂并推进时间
    std::thread::sleep(Duration::from_millis(120));
    let mut snapshots = 0;
    for _ in 0..1000 {
        if t.push(&ProgressEvent::Bytes { delta: 100 }).is_some() {
            snapshots += 1;
        }
    }
    assert!(snapshots >= 1, "跨窗口应触发至少一次冲刷");
    assert_eq!(t.done(), 100_000);
    // 101ms 后再投喂一条 → 第二条窗口冲刷
    std::thread::sleep(Duration::from_millis(120));
    assert!(t.push(&ProgressEvent::Bytes { delta: 1 }).is_some());
    assert_eq!(t.done(), 100_001);
}

#[test]
fn boundary_events_flush_immediately() {
    let mut t = Throttle::new();
    t.push(&started(1000));
    // EntryDone 在窗口未到期时也立即冲刷
    let snap = t
        .push(&entry_done("a.txt", 500))
        .expect("EntryDone 立即冲刷");
    assert_eq!(snap.done, 500);
    // JobFinished 终局冲刷
    let snap = t.push(&finished()).expect("JobFinished 冲刷");
    assert_eq!(snap.done, 500);
    // 结束后不再有快照
    assert!(t.push(&ProgressEvent::Bytes { delta: 1 }).is_none());
}

#[test]
fn ewma_convergence() {
    // 恒速输入：≥10 窗口后误差 < 5%
    let mut e = SpeedEwma::new();
    // 跳过预热期
    std::thread::sleep(Duration::from_millis(520));
    let mut last = None;
    for _ in 0..15 {
        last = e.sample(1000, Duration::from_millis(10)); // 恒速 100_000 B/s? 不——delta/秒 = 1000/0.01 = 100k
        std::thread::sleep(Duration::from_millis(2));
    }
    let v = last.expect("预热后应输出速度");
    assert!(
        (v - 100_000.0).abs() / 100_000.0 < 0.05,
        "恒速收敛误差 <5%，实际 {v}"
    );
}

#[test]
fn speed_mutation_tracks() {
    let mut e = SpeedEwma::new();
    std::thread::sleep(Duration::from_millis(520));
    for _ in 0..10 {
        e.sample(10, Duration::from_millis(10)); // 1000 B/s
        std::thread::sleep(Duration::from_millis(2));
    }
    let before = e.current().unwrap();
    for _ in 0..6 {
        e.sample(100, Duration::from_millis(10)); // 10000 B/s（10× 突变）
        std::thread::sleep(Duration::from_millis(2));
    }
    let after = e.current().unwrap();
    assert!(
        after > before * 2.0,
        "10× 突变后应跟踪到 2× 区间：before {before}, after {after}"
    );
}

#[test]
fn warmup_and_eta_rules() {
    // 预热期速度 None
    let mut e = SpeedEwma::new();
    assert!(e.sample(100, Duration::from_millis(10)).is_none());
    // eta 规则
    assert_eq!(eta_from(100, 100, Some(1.0)), Some(Eta::AlmostDone));
    assert_eq!(eta_from(0, 10, None), None);
    // 剩余 <10s → AlmostDone（done=0,total=50,速度=10B/s → 5s）
    assert_eq!(eta_from(0, 50, Some(10.0)), Some(Eta::AlmostDone));
    // >24h → None（total 巨大、速度小）
    assert_eq!(eta_from(0, u64::MAX / 2, Some(1.0)), None);
    // 速度 0 防除零
    assert_eq!(eta_from(0, 100, Some(0.0)), None);
}
