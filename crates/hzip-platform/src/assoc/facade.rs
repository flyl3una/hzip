//! 文件关联的**平台分发门面**：每个入口按 `cfg` 转发到 `win`/`macos`/`linux` 后端。
//!
//! 类型与常量在 `api.rs`；模块路径（`crate::assoc::*`）由 `mod.rs` 的再导出保持不变。

use super::api::{AssocSpec, ExtAssocState};

/// 回读某扩展名的关联实况（设置页状态行 / 测试）。
///
/// 该平台的「双击落点」没被显式指派 → `effective = None`（系统会弹「打开方式」）。
pub fn ext_state(ext: &str) -> ExtAssocState {
    #[cfg(target_os = "windows")]
    {
        super::win::ext_state(ext)
    }
    #[cfg(target_os = "macos")]
    {
        super::macos::ext_state(ext)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        super::linux::ext_state(ext)
    }
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    {
        let _ = ext;
        ExtAssocState::default()
    }
}

/// **批量**回读关联实况（返回顺序 = 入参顺序；单扩展结果与 [`ext_state`] 逐字节相同）。
///
/// T-OPT B5.3：状态行一次要问「这一批勾选的扩展名各自归谁」——
/// macOS 后端每次查询要起 `plutil` 子进程（bundle id + 偏好 plist），逐扩展调用
/// 就是 N 对子进程；批量入口把它压成**一次快照**（[`super::macos::ext_states`]）。
/// Windows/Linux 侧退化为逐扩展调用（各自只是注册表读/文件读，无子进程）。
pub fn ext_states(exts: &[String]) -> Vec<(String, ExtAssocState)> {
    #[cfg(target_os = "macos")]
    {
        let names: Vec<&str> = exts.iter().map(String::as_str).collect();
        super::macos::ext_states(&names)
            .into_iter()
            .zip(exts.iter())
            .map(|(state, ext)| (ext.clone(), state))
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        exts.iter()
            .map(|ext| (ext.clone(), ext_state(ext)))
            .collect()
    }
}

/// 应用文件关联（各平台幂等覆盖：先清旧痕迹再写新勾选集）。
///
/// 语义 = 「让这些格式双击归本程序」：
/// - Windows：写 ProgId + `.<ext>\(Default)` + `OpenWithProgids` + `Capabilities`；
///   被 `UserChoice` 占着的格式**清掉那一层**（否则写不进去，
///   见 [`ExtAssocState::pinned`]）；最后发 `SHChangeNotify` 让 Explorer 立刻重读；
/// - Linux：写 `hzip.desktop` + `mimeapps.list` 的两个小节（**能真正改默认**）；
/// - macOS：`lsregister` 注册 bundle + 写 `LSHandlers`（**能真正改默认**）。
///
/// 失败原因原样返回（界面把它显示在状态行）——绝不吞掉。
pub fn apply_association(spec: &AssocSpec) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        super::win::apply_association(spec)
    }
    #[cfg(target_os = "macos")]
    {
        super::macos::apply_association(spec)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        super::linux::apply_association(spec)
    }
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    {
        let _ = spec;
        Err(UNSUPPORTED_PLATFORM.to_string())
    }
}

/// 解除文件关联（撤销 [`apply_association`] 的系统侧痕迹）。
///
/// 幂等：没装过也算成功。已生效的扩展名**归还给原来的程序**（Windows 按
/// `PrevDefault` 备份还原；Linux 从 `mimeapps.list` 删掉我们的条目，让系统回落）。
pub fn remove_association(spec: &AssocSpec) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        super::win::remove_association(spec)
    }
    #[cfg(target_os = "macos")]
    {
        super::macos::remove_association(spec)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        super::linux::remove_association(spec)
    }
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    {
        let _ = spec;
        Err(UNSUPPORTED_PLATFORM.to_string())
    }
}

/// 打开系统的「默认应用设置」（**批量**改默认的入口）。
///
/// 只有 Windows 有这样一个集中页面：Windows 11 深链到本程序的条目
/// （`ms-settings:defaultapps?registeredAppUser=hzip`，需 [`apply_association`]
/// 已写 `RegisteredApplications`）；Windows 10 打开「默认应用」页。
///
/// Linux（XDG）与 macOS 没有对应页面——那两个平台**程序自己就能改默认**
/// （写 `mimeapps.list` / `LSHandlers`），故返回 `Err` 说明不需要。
pub fn open_default_apps_settings() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        super::win::open_default_apps_settings()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(NO_SETTINGS_PAGE.to_string())
    }
}

/// 提权子进程入口：清理这些扩展名的 `UserChoice`（**只删键、不写值**）。
///
/// 由 `main.rs` 截获 [`super::api::ELEVATE_FLAG`] 后调用，随即退出——不建窗口、不参与单实例。
#[cfg(windows)]
pub fn clear_user_choice_batch(exts: &[String]) -> Result<(), String> {
    super::win::clear_user_choice_batch(exts)
}

/// 无后端平台（非 Windows/macOS/unix）的说明文案。
#[cfg(not(any(windows, target_os = "macos", unix)))]
const UNSUPPORTED_PLATFORM: &str = "当前平台暂不支持文件关联";

/// 非 Windows 平台没有「系统默认应用设置页」。
#[cfg(not(target_os = "windows"))]
const NO_SETTINGS_PAGE: &str = "该平台由本程序直接修改默认程序，无需系统设置页";
