//! 失败文案（带 HZ 码）与「新建文件夹」初值命名——自 `shell/mod.rs` 拆分。

use super::runtime::AppDataHandle;

/// `HzipError` → 人读失败文案（**带 HZ 码**）。
///
/// 打开/进入容器的失败会出现在两处：进度窗状态行与任务中心失败原因。带码的文案让
/// 用户报障能直接对上错误码表（与 CLI 的「错误 HZ-xxxx: …」同口径），也让任务中心
/// 记录满足「比较明确的失败原因」（用户口径 2026-09-24）。
pub(crate) fn hz_reason(e: &hzip_error::HzipError) -> String {
    format!("HZ-{:04}: {e}", e.code())
}

/// 「新建文件夹」初值：`新建文件夹`／`新建文件夹 (2)`…（避开当前目录既有同名）。
pub(super) fn unique_new_folder_name(data: &AppDataHandle, dir: &str) -> String {
    let base = crate::tr("browse.new_folder.default");
    let Ok(d) = data.lock() else {
        return base;
    };
    if dir != d.main.current_dir() {
        return base;
    }
    let taken: std::collections::HashSet<String> = d
        .main
        .dir_children()
        .iter()
        .map(|r| r.name.clone())
        .collect();
    if !taken.contains(&base) {
        return base;
    }
    for n in 2..1000 {
        let cand = format!("{base} ({n})");
        if !taken.contains(&cand) {
            return cand;
        }
    }
    base
}
