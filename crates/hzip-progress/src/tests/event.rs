use super::*;

fn started(total: u64) -> ProgressEvent {
    ProgressEvent::JobStarted {
        job_id: 1,
        total_bytes: total,
        total_entries: 1,
    }
}

fn finished(outcome: Outcome) -> ProgressEvent {
    ProgressEvent::JobFinished { job_id: 1, outcome }
}

// JobTotal 顺序规范：Started 之后、Finished 之前恰一次；补报值覆盖 total
#[test]
fn job_total_order_rules() {
    let total = ProgressEvent::JobTotal {
        job_id: 1,
        total_bytes: 9,
        total_entries: 3,
    };
    let mut v = OrderValidator::new();
    assert!(v.push(&total).is_err(), "先于 JobStarted 非法");
    assert!(v.push(&started(0)).is_ok());
    assert!(v.push(&total).is_ok());
    assert!(v.push(&total).is_err(), "恰一次");
    assert!(
        v.push(&finished(Outcome::Done(JobStats::new(0, 0))))
            .is_ok()
    );
    // 守恒判定消费补报后的 total（0 != 9 → 不守恒）
    assert!(!v.bytes_conserved());
}

#[test]
fn order_validator_accepts_legal_sequence() {
    let events = [
        started(100),
        ProgressEvent::EntryStarted {
            entry: "a.txt".into(),
            bytes: 100,
        },
        ProgressEvent::Bytes { delta: 40 },
        ProgressEvent::Bytes { delta: 60 },
        ProgressEvent::EntryDone {
            entry: "a.txt".into(),
            bytes: 100,
            crc_ok: true,
        },
        finished(Outcome::Done(JobStats::new(100, 1))),
    ];
    let mut v = OrderValidator::new();
    for ev in &events {
        v.push(ev).unwrap();
    }
    v.finish().unwrap();
    assert!(v.bytes_conserved());
}

#[test]
fn order_validator_rejects_illegal_sequences() {
    // 缺 Started
    let mut v = OrderValidator::new();
    assert!(v.push(&ProgressEvent::Bytes { delta: 1 }).is_err());
    // Started 两次
    let mut v = OrderValidator::new();
    v.push(&started(10)).unwrap();
    assert!(v.push(&started(10)).is_err());
    // Finished 后再有事件
    let mut v = OrderValidator::new();
    v.push(&started(10)).unwrap();
    v.push(&finished(Outcome::Done(JobStats::new(10, 1))))
        .unwrap();
    assert!(
        v.push(&ProgressEvent::EntryStarted {
            entry: "x".into(),
            bytes: 1,
        })
        .is_err()
    );
    // EntryDone 无配对 EntryStarted
    let mut v = OrderValidator::new();
    v.push(&started(10)).unwrap();
    assert!(
        v.push(&ProgressEvent::EntryDone {
            entry: "x".into(),
            bytes: 1,
            crc_ok: true,
        })
        .is_err()
    );
    // finish 缺 JobFinished
    let mut v = OrderValidator::new();
    v.push(&started(10)).unwrap();
    assert!(v.finish().is_err());
}

#[test]
fn bytes_conservation() {
    let events = [
        started(1000),
        ProgressEvent::EntryStarted {
            entry: "a".into(),
            bytes: 1000,
        },
        ProgressEvent::Bytes { delta: 300 },
        ProgressEvent::Bytes { delta: 700 },
        ProgressEvent::EntryDone {
            entry: "a".into(),
            bytes: 1000,
            crc_ok: true,
        },
        finished(Outcome::Done(JobStats::new(1000, 1))),
    ];
    let mut v = OrderValidator::new();
    for ev in &events {
        v.push(ev).unwrap();
    }
    v.finish().unwrap();
    assert!(v.bytes_conserved());
    assert_eq!(v.bytes_sum(), 1000);
}

#[test]
fn validator_rejects_unpaired_entry() {
    let events = [
        started(10),
        ProgressEvent::EntryStarted {
            entry: "a".into(),
            bytes: 10,
        },
    ];
    let mut v = OrderValidator::new();
    for ev in &events {
        v.push(ev).unwrap();
    }
    assert!(v.finish().is_err(), "未配对 EntryStarted 必须报错");
}
