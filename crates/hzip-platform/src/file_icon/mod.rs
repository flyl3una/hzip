//! 系统文件类型图标（三平台门面；T-ICON）。
//!
//! # 语义：与操作系统文件管理器**逐类型一致**
//!
//! 按「扩展名 + 种类」向操作系统查询**该类型应有的图标**：
//!
//! | 平台 | 取图 | 尺寸 |
//! |---|---|---|
//! | Windows | `SHGetFileInfoW(SHGFI_ICON｜SHGFI_USEFILEATTRIBUTES[｜SHGFI_LINKOVERLAY])` → HICON | 32（`ICON_PX`） |
//! | macOS | 扩展名 → `UTType` → `NSWorkspace.iconForContentType:` → `NSBitmapImageRep` | 32（`ICON_PX`） |
//! | Linux | XDG 图标主题查表（MIME → 主题图标文件） | 主题档位 |
//!
//! # 两条硬约定
//!
//! 1. **不触碰文件系统**：入参只有扩展名与种类 ⇒ 压缩包**内**条目（无真实路径）
//!    与真实文件走同一条路。依据：7-Zip `SysIconUtils.cpp` 原注释
//!    「we use SHGFI_USEFILEATTRIBUTES, and (res != 0) is expected for main cases,
//!    even if there are no such file. / So we can use SHGFI_USEFILEATTRIBUTES for
//!    any case.」
//! 2. **可能慢、可能失败**：shell 图标查询会加载 shell 扩展、读关联注册表。
//!    调用方**必须**在后台线程调用并缓存；失败返回 `None` 由调用方回退自有图标
//!    （本模块绝不 panic、绝不返回空白占位）。
//!
//! # 与两个先例的对照（2026-10-10 源码/二进制逆向核对）
//!
//! | | 7-Zip | Bandizip | 本模块 |
//! |---|---|---|---|
//! | 取图 | `SHGetFileInfoW` + `SYSICONINDEX` + 系统图像列表 | `SHGetFileInfoW`（`ICON`/`TYPENAME`，含 PIDL 通道） | `SHGetFileInfoW(SHGFI_ICON)` |
//! | 档内条目 | `SHGFI_USEFILEATTRIBUTES` | 同 | 同（入参只有扩展名 ⇒ 天然如此） |
//! | 尺寸 | 16 / 32 两档 | 32 级 + 自绘缩放 | 32 单档，调用方缩放 |
//! | 缓存 | 扩展名表（≤65536，二分） | 逐次调用 | **GUI 侧**按 `(kind, ext)` 缓存（见 `hzip-gui::ui::widgets::file_icon`） |
//!
//! 两个先例都**没有**使用 COM（`IImageList`/`IShellItemImageFactory`）⇒ 本模块也不引。
//!
//! # cross-check 范围（本机非目标平台的语法校验）
//!
//! [`linux`] 后端是纯 Rust（无平台 crate 依赖）⇒ 挂 `feature = "cross-check"`
//! 被拉进本机构建做语法校验；[`win`] 与 [`macos`] 都依赖 target 门控的平台 crate
//! （`windows-sys` / `objc2*`），**只按各自 `target_os` 门控**（与
//! [`crate::assoc`]/[`crate::context_menu`] 的平台后端同款）。

// Windows 后端**只按 target 门控**：它依赖 target 门控的 `windows-sys` 与
// `crate::winutil`，非 Windows 主机上开 `cross-check` 也编不过 ⇒ 不挂 cross-check
// （与 `assoc`/`context_menu` 的 Windows 后端同款约定；cross-check 的语义是
// 「在本机强制编译**其它**平台后端」）。
#[cfg(target_os = "windows")]
pub(crate) mod win;

#[cfg(any(all(unix, not(target_os = "macos")), feature = "cross-check"))]
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub(crate) mod linux;

// macOS 后端**只按 target 门控**：它依赖 target 门控的 `objc2*` 家族，非 macOS
// 主机上开 `cross-check` 也编不过 ⇒ 与 [`win`] 同款，不挂 cross-check。
#[cfg(target_os = "macos")]
pub(crate) mod macos;

mod api;

// 公开面在此再导出：类型与取图入口的定义在 `api.rs`（`mod.rs` 只放声明与再导出，
// 见 §模块结构；布局检查器 L1 为红线）。
pub use api::{ICON_PX, IconKind, IconRequest, SystemIcon, lookup};
