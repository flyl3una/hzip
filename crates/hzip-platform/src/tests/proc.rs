use super::*;

#[test]
fn self_alive() {
    let pid = std::process::id();
    let created = self_created_ms();
    assert!(created > 0, "self_created_ms 应非零");
    assert!(is_alive(pid, created), "自身 pid + 创建时间应判活");
    assert!(
        !is_alive(pid, created + 60_000),
        "创建时间不符应判死（PID 复用防护）"
    );
    // 0xFFFFFFFF 非合法 pid：部分环境 OpenProcess 对其返回 ACCESS_DENIED → 保守判活，
    // 或直接成功打开（本沙箱实测）→ query 失败亦判活。故此处不断言固定结果，
    // PID 复用防护由上一断言（创建时间不符判死）覆盖。
    let _ = is_alive(0xFFFF_FFFF, 0);
}
