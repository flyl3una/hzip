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

/// 打开失败的人读文案（**非档案要说得像人话**）。
///
/// 用户口径 2026-10-26：右键菜单对**任意文件**都给出「打开/解压」（扩展名不是
/// 证据，`disk.img` / 无扩展名的文件可能就是 zip）。因此「这文件不是压缩包」是
/// 一种**常规结局**，不能只回 `HZ-2001: 格式错误 (unknown): bad signature`——
/// 用户读不懂，也不知道 hzip 是怎么判断的。
///
/// 只有「魔数不匹配」走专用文案；`Password` / `Truncated` / 语义性错误保持
/// 原样（那些情况下 hzip 确实认出了格式，只是别的地方有问题）。
pub(crate) fn open_failure_text(path: &std::path::Path, e: &hzip_error::HzipError) -> String {
    if let hzip_error::HzipError::Format {
        kind: hzip_error::FormatErrorKind::BadSignature,
        ..
    } = e
    {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        return crate::tr_with("dlg.not_archive", &[("name", &name)]);
    }
    hz_reason(e)
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
