//! `file_icon` 的测试钩子（`#[cfg(test)]`；由 `file_icon.rs` 的 `#[path]` 引入）。
//!
//! 为什么单列文件：业务文件里不得内联测试模块（`build/checks/layout-conventions.py`
//! L2 红线）。钩子是本模块的子模块 ⇒ `use super::*` 即可访问缓存的私有面。
//!
//! 生产路径**不调**这里任何函数；它们只为让状态机断言与线程调度解耦。

use super::*;

/// 装配一个可注入 `source` 的缓存（开关初值 `true`，不读配置）。
///
/// **不启后台线程**：测试一律用 [`drain_pending_for_test`] 同步排空，状态断言
/// 才与线程调度无关；要覆盖真线程路径的用例自行调 `spawn_worker`。
pub(crate) fn init_with_source(cx: &mut App, tx: Option<Arc<SignalTx>>, source: IconSource) {
    let _ = install(cx, tx, source, true);
}

/// 同步排空待处理键并写缓存，返回真正探测的键数。
///
/// 生产路径由后台线程消费队列（`spawn_worker`），**不调**本函数；这里只是把
/// 同一份 `probe_one` 搬到调用线程上跑，让状态机断言无需等线程/超时。
pub(crate) fn drain_pending_for_test(cx: &App) -> usize {
    let Some(cache) = cx.try_global::<FileIconCache>() else {
        return 0;
    };
    let mut probed = 0;
    while let Some(key) = {
        let mut st = lock(&cache.inner.state);
        st.pending.pop_front()
    } {
        if probe_one(&cache.inner, &key) {
            probed += 1;
        }
    }
    probed
}

/// 某键的缓存状态（`"absent" | "wanted" | "ready" | "missing"`）。
pub(crate) fn state_of_for_test(cx: &App, key: (IconKind, Option<String>)) -> &'static str {
    let Some(cache) = cx.try_global::<FileIconCache>() else {
        return "absent";
    };
    let st = lock(&cache.inner.state);
    match st.entries.get(&key) {
        Some(Entry::Wanted) => "wanted",
        Some(Entry::Ready { .. }) => "ready",
        Some(Entry::Missing) => "missing",
        None => "absent",
    }
}
