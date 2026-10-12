use super::*;

fn generation_of(t: &CancellationToken) -> u64 {
    t.generation()
}

#[test]
fn cancel_idempotent() {
    let t = CancellationToken::new();
    assert!(!t.is_cancelled());
    t.cancel();
    assert!(t.is_cancelled());
    let g1 = generation_of(&t);
    t.cancel(); // 第二次：幂等
    t.cancel();
    assert!(t.is_cancelled());
    assert_eq!(generation_of(&t), g1, "代际只跃迁一次");
    assert!(matches!(t.throw_if_cancelled(), Err(HzipError::Cancelled)));
}

#[test]
fn cascade_parent_to_child() {
    let root = CancellationToken::new();
    let child = root.child();
    let grand = child.child();
    assert!(!child.is_cancelled() && !grand.is_cancelled());
    root.cancel();
    assert!(child.is_cancelled());
    assert!(grand.is_cancelled(), "孙级也应级联取消");
}

#[test]
fn cancel_child_does_not_affect_parent() {
    let root = CancellationToken::new();
    let child = root.child();
    child.cancel();
    assert!(child.is_cancelled());
    assert!(!root.is_cancelled(), "子取消不影响父");
}

#[test]
fn child_after_parent_cancelled() {
    let root = CancellationToken::new();
    root.cancel();
    let late = root.child();
    assert!(late.is_cancelled(), "父已取消后创建的子应立即取消");
}

#[test]
fn cancel_while_creating_children() {
    use std::sync::Arc as StdArc;
    let root = StdArc::new(CancellationToken::new());
    let root2 = StdArc::clone(&root);
    let stop = StdArc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = StdArc::clone(&stop);
    let maker = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            let c = root2.child();
            drop(c);
        }
    });
    // 主线程并发 cancel
    for _ in 0..200 {
        root.cancel();
    }
    stop.store(true, Ordering::Relaxed);
    let _ = maker.join();
    // 所有通过 root 产生的存活子最终取消（root 已取消）
    assert!(root.is_cancelled());
    let c = root.child();
    assert!(c.is_cancelled());
}

#[test]
fn wait_cancelled_wakes() {
    let t = CancellationToken::new();
    let t2 = t.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        t2.cancel();
    });
    assert!(t.wait_cancelled(Duration::from_secs(5)), "取消应唤醒等待者");
    let _ = h.join();
}

#[test]
fn wait_cancelled_timeout() {
    let t = CancellationToken::new();
    assert!(
        !t.wait_cancelled(Duration::from_millis(30)),
        "超时未取消返回 false"
    );
}
