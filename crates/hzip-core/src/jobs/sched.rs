//! 调度纯函数（同键串行 / 异键并行）：资源键推导与可派发挑选。
//!
//! 从 `jobs/mod.rs` 迁出（T-STD：`mod.rs` 只做声明与再导出）；内容逐字未改。

use super::JobSpec;

/// 任务占用的**资源键**（源档案 + 输出档案，路径小写归一）。
///
/// 调度口径（用户 2026-09-23 要求）：**同一档案的任务串行、不同档案并行**——
/// 解压 a.zip 与重压 a.zip 不能并发（读写互踩），而 a.zip 与 b.zip 互不阻塞。
/// 归一：先 `canonicalize`（存在则取真实路径），失败回落原样路径——「输出档案
/// 尚不存在」是最常见情形，此时相对/绝对写法不同即视为不同资源（可接受）。
pub(super) fn resource_keys(spec: &JobSpec) -> Vec<String> {
    let mut keys = vec![norm_key(&spec.source)];
    if let Some(out) = &spec.out {
        keys.push(norm_key(out));
    }
    keys.sort();
    keys.dedup();
    keys
}

fn norm_key(p: &std::path::Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .to_ascii_lowercase()
}

/// 挑第一个**资源不冲突**的排队任务（保序：跳过被占的，取其后第一个可跑的）。
///
/// 返回下标；全被同键任务占着 → `None`（本轮不派发，等占用方收口时再调度）。
pub(super) fn pick_dispatchable(
    keys: &[Vec<String>],
    busy: &std::collections::HashSet<String>,
) -> Option<usize> {
    keys.iter()
        .position(|ks| ks.iter().all(|k| !busy.contains(k)))
}
