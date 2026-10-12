use super::*;
use std::time::Duration;

/// 阈值语义：低于 150 ms 静默，150 ms 起 warn，500 ms 起 error。
#[test]
fn thresholds_are_half_and_full_second_scale() {
    assert_eq!(WARN_MS, 150);
    assert_eq!(ERROR_MS, 500);
    const { assert!(WARN_MS < ERROR_MS, "warn 必须先于 error 触发") };
}

/// 守卫是 RAII：构造即开始计时，`Drop` 判定（不 panic、不阻塞）。
#[test]
fn guard_measures_elapsed_and_drops_cleanly() {
    let lag = UiLag::new("测试块");
    assert!(lag.start.elapsed() < Duration::from_secs(1), "起点即刻");
    std::thread::sleep(Duration::from_millis(5));
    let elapsed = lag.start.elapsed();
    drop(lag);
    assert!(elapsed >= Duration::from_millis(5), "计时真的在走");
}
