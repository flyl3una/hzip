//! 原生文件/目录选择器（T-G52；04 §3.1 收口例外：UI 原生对话框走 rfd）。
//!
//! rfd 的同步 API 为**阻塞调用**：消费方（app.rs Open 管道）必须经
//! `smol::unblock` 置于后台线程，禁止在 UI 线程直呼。

use std::path::PathBuf;

use hzip_core::engine::Engine;

use crate::ui::contracts::supported_open_extensions;

/// 打开档案文件选择框（阻塞；扩展清单由 Engine 注册表读能力 handler 派生）。
///
/// 过滤器两档：「hzip 支持的档案」（注册表扩展并集）+「所有文件」（`*`）。
pub fn pick_archive(engine: &Engine) -> Option<PathBuf> {
    let exts = supported_open_extensions(engine);
    rfd::FileDialog::new()
        .set_title(crate::tr("picker.open_title"))
        .add_filter(crate::tr("picker.filter_supported"), &exts)
        .add_filter(crate::tr("picker.filter_all"), &["*"])
        .pick_file()
}

/// 保存文件框（阻塞；右键「添加到压缩文件…」选输出档案路径）。
///
/// `suggest` = 建议文件名/路径（源名派生，见 `dialog_vm::default_out_path`）——
/// 预填后用户只改需要改的部分，避免从空白开始手打档案名。
pub fn pick_save(suggest: &std::path::Path) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new().set_title(crate::tr("picker.save_title"));
    if let Some(dir) = suggest.parent() {
        dlg = dlg.set_directory(dir);
    }
    if let Some(name) = suggest.file_name() {
        dlg = dlg.set_file_name(name.to_string_lossy());
    }
    dlg.save_file()
}

/// 目录选择框（阻塞；T-G54 后续接线：设置/解压目标目录）。
pub fn pick_directory() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
}

/// 多选文件（阻塞；T-MENU6「添加文件…」入口）。
///
/// 返回选中文件清单（顺序 = 用户选择序；取消 → 空 vec）。
pub fn pick_files() -> Vec<PathBuf> {
    rfd::FileDialog::new()
        .set_title(crate::tr("picker.add_files_title"))
        .pick_files()
        .unwrap_or_default()
}

/// 可执行文件选择框（阻塞；R2-03 查看器页「浏览…」用——图像查看器/编辑器路径）。
///
/// 过滤器两档：「可执行文件」（Windows `exe`）+「所有文件」（`*`）兜底；
/// 非 Windows 无扩展名约定，直接 `pick_file`。
pub fn pick_executable() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        rfd::FileDialog::new()
            .set_title(crate::tr("picker.executable_title"))
            .add_filter(crate::tr("picker.filter_executable"), &["exe"])
            .add_filter(crate::tr("picker.filter_all"), &["*"])
            .pick_file()
    }
    #[cfg(not(target_os = "windows"))]
    {
        rfd::FileDialog::new()
            .set_title(crate::tr("picker.executable_title"))
            .pick_file()
    }
}

#[cfg(test)]
#[path = "tests/file_picker.rs"]
mod tests;
