//! 文件关联（三平台门面）：把「勾选的扩展名」变成操作系统里真实生效的默认程序。
//!
//! # 职责边界（T-CM §7）
//!
//! | 本模块 | [`crate::context_menu`] |
//! |---|---|
//! | 默认程序（双击打开）+ 「打开方式」列表 | 右键菜单 verb / 稀疏包 / 服务菜单 |
//!
//! 二者作用域刻意分开：「解除关联」不该顺手删掉用户的右键菜单项，反之亦然。
//!
//! # 三平台机制总览
//!
//! | | Windows | Linux（XDG） | macOS（LaunchServices） |
//! |---|---|---|---|
//! | 注册「谁能打开」 | `HKCU\Software\Classes\<ProgId>` + `OpenWithProgids` | `$XDG_DATA_HOME/applications/hzip.desktop` | app bundle 的 `CFBundleDocumentTypes` + `lsregister` |
//! | 双击落点 | `.<ext>\(Default)`（**但被 `UserChoice` 压着**） | `$XDG_CONFIG_HOME/mimeapps.list` `[Default Applications]` | `LSHandlers`（`com.apple.launchservices.secure`） |
//! | 程序能否直接改默认 | **否**（`UserChoice` 哈希保护；只能让用户点一次） | **能**（改文本文件即可） | **能**（写 `LSHandlers` 即可） |
//! | 需管理员 | 否（只写 HKCU） | 否 | 否 |
//! | 后端文件 | [`win`] | [`linux`] | [`macos`] |
//!
//! 「程序能否直接改默认」这一列是**本模块的核心事实**，也是
//! [`ExtAssocState::pinned`] 的来处：Windows 上点「应用关联」后若某格式仍被别人
//! 占着，界面必须如实说「改不动，请点一次『打开方式』」，而不是假装成功。
//!
//! # 为什么不做「强制抢默认」的花招
//!
//! 改 `UserChoice` 的哈希（SetUserFTA 那类玩法）自 Win10 21H1 起失效，
//! 且属绕过系统安全策略；`IApplicationAssociationRegistration::SetAppAsDefault`
//! 在发行者不同时 `E_ACCESSDENIED`（见 [`win`] 内长注释）。故 Windows 侧的正道是
//! 「把该写的都写好」，再清掉那层受保护的 `UserChoice`（[`apply_association`]
//! 直接做，被 DACL 拒时提权重来，用户无需再去系统设置里点一次）；
//! [`open_default_apps_settings`] 是「把用户送到系统里唯一能做这件事的地方」
//! 的兜底入口。
//!
//! # 后端隔离与 `cross-check`
//!
//! 各后端互不引用，只在「非目标平台」这一编译条件下被 `feature = "cross-check"`
//! 拉进来做语法/类型校验（本机无法运行，但至少不会腐坏）。与
//! [`crate::context_menu`] 同款约定。

mod api;
mod facade;

#[cfg(target_os = "windows")]
pub(crate) mod win;

/// ext → MIME 类型表（Linux 后端消费；macOS 走「按扩展名」的 LSHandlers，
/// 不需要 UTI/MIME 表）。表本身恒编译——[`mime_supports`] 是跨平台一致性
/// 断言的入口（`hzip-gui` 用它验证「文件关联候选 ⊆ MIME 表」）。
///
/// `pub(crate)` 仅为让 `file_icon::linux` 也能按扩展名查 MIME（T-ICON 图标名
/// 与「打开方式」共用同一张表，避免两处表漂移；模块内的 `pub(super)` 入口
/// 跨不到 `file_icon`）。
pub(crate) mod mime;

// `cross-check` 在本机（非目标平台）编译这些后端只为语法校验：其中绝大多数符号
// 不会被本机引用，故仅在「非目标平台」这一编译条件下豁免死代码。与
// [`crate::context_menu`] 同款（见该模块 mod.rs 的说明）。
#[cfg(any(target_os = "macos", feature = "cross-check"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod macos;

#[cfg(any(all(unix, not(target_os = "macos")), feature = "cross-check"))]
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub(crate) mod linux;

pub use api::{AssocSpec, ExtAssocState, PROG_ID, mime_supports};
pub use facade::{
    apply_association, ext_state, ext_states, open_default_apps_settings, remove_association,
};

#[cfg(windows)]
pub use api::ELEVATE_FLAG;
#[cfg(windows)]
pub use facade::clear_user_choice_batch;

// `api` 里的 verb 命令串格式化只有 `tests/mod_rs.rs`（`use super::*`）取用——
// 非测试构建无引用方，故只随测试编译再导出（否则 `-D warnings` 会以 unused import 拦下）。
#[cfg(test)]
use api::format_verb_command;

#[cfg(test)]
#[path = "tests/mod_rs.rs"]
mod tests;
