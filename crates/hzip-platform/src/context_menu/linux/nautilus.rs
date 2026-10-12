//! GNOME / Nautilus 右键菜单（T-CM §5）。
//!
//! # 两条轨道
//!
//! | 轨道 | 落点 | 菜单位置 | 前提 |
//! |---|---|---|---|
//! | **nautilus-python 扩展** | `$XDG_DATA_HOME/nautilus-python/extensions/hzip.py` | 右键**顶层**「hzip」子菜单 | 用户已装 `nautilus-python` |
//! | Nautilus 脚本 | `$XDG_DATA_HOME/nautilus/scripts/hzip-*` | 右键 →「脚本」子菜单 | 无（Nautilus 内建） |
//!
//! 两条同时写入：装了 nautilus-python 的机器得到顶层菜单，未装的机器自动忽略
//! `.py`（Nautilus 不加载该目录），仍可用脚本轨。**脚本轨是保底，扩展轨是增强。**
//!
//! # 加载时机
//!
//! nautilus-python 扩展**无法热重载**（官方 README 明示），安装后需
//! `nautilus -q` 重启文件管理器；脚本目录新增文件同样需重启 Nautilus 才进菜单。
//! 故 [`install`] 返回 `restart_required = true`。

use std::path::PathBuf;

use super::script::{SCRIPT_PREFIX, script_dir};
use crate::context_menu::{MenuError, MenuErrorKind, MenuSpec, entry_for};

/// nautilus-python 扩展文件名。
const EXT_NAME: &str = "hzip.py";

/// nautilus-python 扩展目录（`$XDG_DATA_HOME/nautilus-python/extensions`）。
fn ext_dir() -> PathBuf {
    crate::dirs::data_dir()
        .parent()
        .unwrap_or(&crate::dirs::data_dir())
        .join("nautilus-python")
        .join("extensions")
}

/// Nautilus 脚本目录（`$XDG_DATA_HOME/nautilus/scripts`）。
fn scripts_dir() -> PathBuf {
    crate::dirs::data_dir()
        .parent()
        .unwrap_or(&crate::dirs::data_dir())
        .join("nautilus")
        .join("scripts")
}

/// 安装（脚本轨 + 扩展轨），返回写入条目数。
///
/// 前置：[`super::script::write_scripts`] 已写入转发脚本。
pub(super) fn install(spec: &MenuSpec) -> Result<usize, MenuError> {
    let dir = scripts_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("创建 {} 失败: {e}", dir.display()),
        )
    })?;
    remove_wrappers(&dir);

    let mut count = 0usize;
    for entry in spec.entries() {
        let Some(def) = entry_for(entry.key) else {
            continue;
        };
        // 档案类条目的脚本按扩展名过滤由 Nautilus 侧不可表达，故脚本一律写入；
        // 语义收窄靠扩展轨的 MIME 过滤（脚本轨属保底，宁可多不宁可漏）。
        let _ = def;
        let name = format!("{SCRIPT_PREFIX}{}", sanitize(&entry.label));
        let path = dir.join(&name);
        let target = script_dir().join(format!("{SCRIPT_PREFIX}{}", entry.key));
        let body = format!(
            "#!/bin/sh\n# hzip 转发（Nautilus 脚本轨）\nexec {}\n",
            super::script::sh_quote(&target.to_string_lossy())
        );
        std::fs::write(&path, body).map_err(|e| {
            MenuError::new(
                MenuErrorKind::Io,
                format!("写入 {} 失败: {e}", path.display()),
            )
        })?;
        super::script::set_executable(&path)?;
        count += 1;
    }

    install_extension(spec)?;
    Ok(count)
}

/// 卸载（两轨都清；幂等）。
pub(super) fn uninstall() -> Vec<String> {
    let mut errors = Vec::new();

    let dir = scripts_dir();
    if let Err(e) = remove_wrappers_checked(&dir) {
        errors.push(e);
    }

    let ext = ext_dir().join(EXT_NAME);
    if let Err(e) = std::fs::remove_file(&ext)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!("删除 {} 失败: {e}", ext.display()));
    }
    errors
}

/// 是否已安装（脚本轨或扩展轨任一在位）。
pub(super) fn installed() -> bool {
    if ext_dir().join(EXT_NAME).is_file() {
        return true;
    }
    let dir = scripts_dir();
    std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(SCRIPT_PREFIX))
            })
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------- 扩展轨

/// nautilus-python 扩展源码（生成：内嵌 spec JSON）。
///
/// 目标 API：Nautilus ≥ 43（`get_file_items(self, files)` 新签名）。
pub(super) fn render_extension(spec: &MenuSpec) -> String {
    let items: Vec<serde_json::Value> = spec
        .resolved()
        .map(|(entry, def)| {
            serde_json::json!({ "key": entry.key, "label": entry.label, "args": def.argv })
        })
        .collect();
    let archive_exts: Vec<&String> = spec.archive_exts.iter().collect();
    let doc = serde_json::json!({
        "exe": spec.exe_path.to_string_lossy(),
        "archive_exts": archive_exts,
        "items": items,
    });
    let literal = py_string_literal(&doc.to_string());

    format!(
        r#"# hzip context menu (generated; do not edit)
# 需要 nautilus-python（Nautilus >= 43）。未安装时本文件不会被加载，
# 右键菜单仍可由 ~/.local/share/nautilus/scripts/ 下的脚本轨提供。
import json
import os
import subprocess

from gi.repository import GObject, Nautilus

SPEC = json.loads({literal})


def _expand(args, paths):
    """把 %SEL / %SELS 占位符展开成若干次 argv。"""
    if any(a == "%SEL" for a in args):
        # 逐档案：每个选中项各一次进程
        return [[p if a in ("%SEL", "%SELS") else a for a in args] for p in paths]
    # 一次性：全部选中项作为多个独立参数
    out = []
    for a in args:
        if a == "%SELS":
            out.extend(paths)
        else:
            out.append(a)
    return [out]


def _selected(files):
    paths = []
    for f in files:
        loc = f.get_location()
        p = loc.get_path() if loc is not None else None
        if p:
            paths.append(p)
    return paths


class HzipMenuProvider(GObject.GObject, Nautilus.MenuProvider):
    def _build(self, items, paths):
        top = Nautilus.MenuItem(name="hzip::root", label="hzip", tip="hzip 压缩 / 解压")
        sub = Nautilus.Menu()
        top.set_submenu(sub)
        for item in items:
            if item["key"].startswith("extract") or item["key"] == "open_hzip":
                if not any(
                    p.lower().endswith("." + e.lower()) for p in paths for e in SPEC["archive_exts"]
                ):
                    continue
            mi = Nautilus.MenuItem(
                name="hzip::" + item["key"], label=item["label"], tip=item["label"]
            )
            mi.connect("activate", self._on_activate, item, paths)
            sub.append_item(mi)
        return top

    def _on_activate(self, _menu, item, paths):
        for argv in _expand(item["args"], paths):
            try:
                subprocess.Popen([SPEC["exe"]] + argv)
            except OSError:
                pass

    def get_file_items(self, files):
        paths = _selected(files)
        if not paths:
            return []
        return [self._build(SPEC["items"], paths)]

    def get_background_items(self, current_folder):
        loc = current_folder.get_location()
        p = loc.get_path() if loc is not None else None
        if not p:
            return []
        items = [i for i in SPEC["items"] if "directory" in i["key"] or i["key"].startswith("add")]
        return [self._build(items, [p])]
"#
    )
}

/// 写入 nautilus-python 扩展（目录不存在则创建；不装 nautilus-python 的机器会忽略它）。
fn install_extension(spec: &MenuSpec) -> Result<(), MenuError> {
    let dir = ext_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("创建 {} 失败: {e}", dir.display()),
        )
    })?;
    let path = dir.join(EXT_NAME);
    std::fs::write(&path, render_extension(spec)).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("写入 {} 失败: {e}", path.display()),
        )
    })
}

/// Python 双引号字符串字面量（转义 `\` / `"` / 换行）。
fn py_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// 文件名安全化（Nautilus 用文件名做菜单项展示）。
fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' => '_',
            other => other,
        })
        .collect()
}

/// 清脚本轨（`hzip-` 前缀）。
fn remove_wrappers(dir: &std::path::Path) {
    let _ = remove_wrappers_checked(dir);
}

/// 清脚本轨（带错误上报）。
fn remove_wrappers_checked(dir: &std::path::Path) -> Result<(), String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(SCRIPT_PREFIX)
            && let Err(err) = std::fs::remove_file(e.path())
        {
            return Err(format!("删除 {} 失败: {err}", e.path().display()));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/nautilus.rs"]
mod tests;
