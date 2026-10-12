//! 窗口级外壳条（T-GUI T10 归位：原 layout/ 的菜单/工具栏/页签；目录名用
//! bar，避免 chrome 与浏览器混淆）。
//!
//! - [`menu_bar`]：菜单条（T-G51 实体化 PopupMenu；T-GUI T21 → AppMenuBar）
//! - [`toolbar`]：工具栏（5 钮 + 右侧页签槽）
//! - [`tab_bar`]：页签行

pub mod menu_bar;
pub mod tab_bar;
pub mod toolbar;
