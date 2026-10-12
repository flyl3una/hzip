//! 构建脚本：把 `hzip.rc` 编译进 `hzip.exe` 的资源段（程序图标）。
//!
//! 图标取用与资源 ID 的完整理由见 `crates/hzip-gui/build.rs::embed_windows_icon`
//! （同一份 `crates/hzip-gui/assets/logo/hzip.ico`，同一 ID 1 约定）：CLI 自己没有窗口，但 exe
//! 仍会出现在资源管理器、快捷方式、控制台宿主里，图标应与 GUI 一致。
//!
//! 非 Windows 目标上 `embed_resource::compile` 是 no-op。

fn main() {
    const RC: &str = "hzip.rc";
    const ICO: &str = "../hzip-gui/assets/logo/hzip.ico";
    println!("cargo:rerun-if-changed={RC}");
    println!("cargo:rerun-if-changed={ICO}");
    println!("cargo:rerun-if-changed=build.rs");
    let _ = embed_resource::compile(RC, embed_resource::NONE);
}
