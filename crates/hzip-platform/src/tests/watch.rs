use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[test]
fn debounce_merges_burst() {
    let dir = crate::testutil::temp_dir("watch");
    let file = dir.path().join("a.txt");
    std::fs::write(&file, b"v1").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = count.clone();
    let file2 = file.clone();
    let _w = FileWatch::new(
        &file,
        Box::new(move |p| {
            assert_eq!(p, file2);
            count2.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();

    // 500 ms 内连续 5 次修改 → 只触发 1 次
    for i in 2..=6 {
        std::fs::write(&file, format!("v{i}")).unwrap();
        std::thread::sleep(Duration::from_millis(80));
    }
    std::thread::sleep(Duration::from_millis(900)); // 等待去抖窗口过期
    let after_burst = count.load(Ordering::SeqCst);
    assert_eq!(after_burst, 1, "连续修改应合并为 1 次回调");

    // 间隔 > 500 ms 的两次修改 → 再触发 1 次（累计 2 次）
    std::fs::write(&file, "v7").unwrap();
    std::thread::sleep(Duration::from_millis(900));
    let after_second = count.load(Ordering::SeqCst);
    assert_eq!(after_second, 2, "独立保存应分别触发");
}
