//! Linux 右键菜单：转发脚本生成与落盘（GNOME / KDE / XFCE 共用）。
//!
//! # 为什么统一生成脚本
//!
//! 三套桌面机制的「选中项传递」方式各不相同（Nautilus 走环境变量、KDE/XFCE
//! 走命令行参数），且**提取类命令一次只接受一个档案**（`--extract-here` 的
//! 协议限制）。把占位符解释集中在一份 POSIX sh 脚本里，三套桌面只负责
//! 「把脚本挂到菜单上」，语义即可完全一致。
//!
//! 脚本同时支持两种输入：有参数时用参数（KDE/XFCE），无参数时读
//! `NAUTILUS_SCRIPT_SELECTED_FILE_PATHS`（Nautilus）。
//!
//! # 占位符
//!
//! - `%SEL`：每个选中项各启动一次进程（提取类——协议一次一个档案）；
//! - `%SELS`：一次进程、全部选中项作为参数（压缩类）。

use std::path::{Path, PathBuf};

use crate::context_menu::{MenuError, MenuErrorKind, MenuSpec, entry_for};

/// 生成脚本落在数据目录下的子目录名（`~/.local/share/hzip/context-menu`）。
pub(super) const SCRIPT_DIR: &str = "context-menu";

/// 脚本文件名前缀（卸载按此前缀清理）。
pub(super) const SCRIPT_PREFIX: &str = "hzip-";

/// 脚本目录（`$XDG_DATA_HOME/hzip/context-menu`）。
pub(super) fn script_dir() -> PathBuf {
    crate::dirs::data_dir().join(SCRIPT_DIR)
}

/// 单引号 POSIX sh 字面量转义（内含 `'` → `'\''`）。
pub(super) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 生成一个 verb 的转发脚本（纯函数，便于单测）。
///
/// `argv` 来自 [`crate::context_menu::entry_for`]；其中**恰好一个**元素是占位符
/// （`%SEL` 或 `%SELS`），其余按字面量引用。
pub(super) fn render_script(exe: &Path, key: &str, label: &str, argv: &[&str]) -> String {
    let exe_q = sh_quote(&exe.to_string_lossy());
    let per_file = argv.contains(&"%SEL");

    // 路径读取头部：参数优先，回退 Nautilus 环境变量。
    let mut s = String::new();
    s.push_str("#!/bin/sh\n");
    s.push_str(&format!("# hzip context menu: {label} ({key})\n"));
    s.push_str("# 本文件由 hzip 自动生成，手工修改会被覆盖。\n");
    s.push_str(&format!("exe={exe_q}\n"));
    s.push_str("if [ \"$#\" -eq 0 ]; then\n");
    s.push_str("  set --\n");
    s.push_str("  while IFS= read -r _f; do\n");
    s.push_str("    [ -n \"$_f\" ] && set -- \"$@\" \"$_f\"\n");
    s.push_str("  done <<HZIP_SEL_EOF\n");
    s.push_str("$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\n");
    s.push_str("HZIP_SEL_EOF\n");
    s.push_str("fi\n");
    s.push_str("[ \"$#\" -gt 0 ] || exit 0\n");

    // 命令体：逐元素渲染，占位符按模式展开。
    let args = |placeholder_value: &str| -> String {
        argv.iter()
            .map(|a| match *a {
                "%SEL" | "%SELS" => placeholder_value.to_string(),
                other => sh_quote(other),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };

    if per_file {
        // %SEL：每个档案各一次（提取类协议一次只收一个路径）
        s.push_str("for _path in \"$@\"; do\n");
        s.push_str(&format!("  \"$exe\" {}\n", args("\"$_path\"")));
        s.push_str("done\n");
    } else {
        // %SELS：一次进程接收全部路径（压缩类）
        s.push_str(&format!("exec \"$exe\" {}\n", args("\"$@\"")));
    }
    s
}

/// 写入全部勾选项的转发脚本，返回写入条数。
pub(super) fn write_scripts(spec: &MenuSpec) -> Result<usize, MenuError> {
    let dir = script_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("创建 {} 失败: {e}", dir.display()),
        )
    })?;

    // 幂等：先清旧脚本
    remove_scripts();

    let mut written = 0usize;
    for entry in spec.entries() {
        let Some(def) = entry_for(entry.key) else {
            continue;
        };
        let body = render_script(&spec.exe_path, entry.key, &entry.label, def.argv);
        let path = dir.join(format!("{SCRIPT_PREFIX}{}", entry.key));
        std::fs::write(&path, body).map_err(|e| {
            MenuError::new(
                MenuErrorKind::Io,
                format!("写入 {} 失败: {e}", path.display()),
            )
        })?;
        set_executable(&path)?;
        written += 1;
    }
    Ok(written)
}

/// 删除全部转发脚本（`context-menu` 目录下 `hzip-` 前缀）。
pub(super) fn remove_scripts() {
    let dir = script_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(SCRIPT_PREFIX) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    // 空目录一并清掉（不留空壳）
    let _ = std::fs::remove_dir(&dir);
}

/// 脚本绝对路径（KDE / XFCE 的 `Exec` 引用它）。
pub(super) fn script_path(key: &str) -> PathBuf {
    script_dir().join(format!("{SCRIPT_PREFIX}{key}"))
}

/// 置可执行位（unix mode 0755）。
#[cfg(unix)]
pub(super) fn set_executable(path: &Path) -> Result<(), MenuError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)
        .map_err(|e| {
            MenuError::new(
                MenuErrorKind::Io,
                format!("读取 {} 权限失败: {e}", path.display()),
            )
        })?
        .permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("设置 {} 可执行位失败: {e}", path.display()),
        )
    })
}

/// 非 unix（`cross-check` 语法校验期）无操作。
#[cfg(not(unix))]
pub(super) fn set_executable(_path: &Path) -> Result<(), MenuError> {
    Ok(())
}

#[cfg(test)]
#[path = "tests/script.rs"]
mod tests;
