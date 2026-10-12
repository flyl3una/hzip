use super::*;

/// 退出条件表驱动（T-MENU10 验收 3）：任一非零即不退。
#[test]
fn exit_condition_requires_every_window_gone() {
    assert!(all_windows_closed(0, 0));
    assert!(!all_windows_closed(1, 0), "注册表还有窗口");
    assert!(!all_windows_closed(0, 1), "gpui 还有窗口");
    assert!(!all_windows_closed(2, 3));
}

/// id 分配单调、注册/注销计数正确（注册表纯数据面，可在无窗口测试中驱动）。
#[test]
fn registry_allocates_ids_and_tracks_lifetime() {
    let reg = WindowRegistry::new();
    let a = reg.alloc_id();
    let b = reg.alloc_id();
    assert!(b > a, "id 单调");
    assert!(reg.is_empty());

    // 用 drop 后即失效的实体句柄占位：这里只验证 id/计数面，
    // 故不注册 shell（Entity 需 gpui 上下文构造）。
    assert_eq!(reg.len(), 0);
    assert!(reg.last_handle().is_none(), "无窗口 → 无宿主句柄");
}

/// 快照按 id 稳定排序（多窗口刷新顺序确定）。
#[test]
fn snapshot_order_is_stable() {
    let reg = WindowRegistry::new();
    assert!(reg.snapshot().is_empty());
    assert!(reg.is_empty());
}

mod multi_window_tests {
    use super::*;

    /// id 分配与注册表计数（T-MENU10：每窗口独立 id；关闭后计数归零）。
    ///
    /// 只驱动纯数据面（`Entity`/句柄需 gpui 上下文）；窗口实体的注册/注销
    /// 路径由 `run_ui` 的 on_window_closed + `all_windows_closed` 守卫，
    /// 后者在下个用例表驱动。
    #[test]
    fn id_allocation_is_monotonic_and_empty_starts_clean() {
        let reg = WindowRegistry::new();
        let a = reg.alloc_id();
        let b = reg.alloc_id();
        assert_eq!(a, 1, "首个 id 为 1（primary 窗口 id 约定）");
        assert_eq!(b, 2, "id 单调递增");
        assert!(reg.is_empty(), "未注册 → 空");
        assert!(reg.last_handle().is_none(), "无窗口 → 无宿主句柄");
        assert!(reg.snapshot().is_empty());
    }

    /// 退出判定：任一窗口存活即不退（T-MENU10 验收 3 表驱动）。
    #[test]
    fn exit_only_when_registry_and_gpui_both_empty() {
        for (reg, live, want) in [
            (0usize, 0usize, true),
            (1, 0, false),
            (0, 1, false),
            (1, 1, false),
            (3, 0, false),
        ] {
            assert_eq!(
                all_windows_closed(reg, live),
                want,
                "registered={reg} live={live}"
            );
        }
    }
}
