//! XFCE / Thunar 右键菜单（T-CM §5）：UCA（自定义动作）。
//!
//! # 落点
//!
//! `$XDG_CONFIG_HOME/Thunar/uca.xml`——**必须读-改-写合并**：用户可能已有
//! 自己的自定义动作，覆盖文件会毁掉它们。本实现按 `<unique-id>` 前缀
//! `hzip-` 增删，其余节点原样保留。
//!
//! # 为什么手写 XML 而不是引 XML 库
//!
//! UCA 文件的结构固定且扁平（`<actions>` 下若干 `<action>`，每个含固定子元素）。
//! 引 XML crate 会为平台收口层增加依赖面（红线 4 的成本），而按 `<action>`
//! 块切分 + 前缀匹配已足够安全：**不解析**未知节点内容、只做整体搬运。
//!
//! # 缩进与转义
//!
//! Thunar 自己写出的文件缩进不统一；我们保留**其他**节点的原始文本（含缩进），
//! 只替换自己那一段，故不会因为重排缩进产生无意义的 diff。

use std::path::PathBuf;

use super::script::script_path;
use crate::context_menu::{MenuError, MenuErrorKind, MenuSpec};

/// 动作 `<unique-id>` 前缀（卸载按此识别）。
const ID_PREFIX: &str = "hzip-";

/// 子菜单名（多个动作归入同一子菜单）。
const SUBMENU: &str = "hzip";

/// `uca.xml` 路径（`$XDG_CONFIG_HOME/Thunar/uca.xml`）。
fn uca_path() -> PathBuf {
    crate::dirs::config_dir()
        .parent()
        .unwrap_or(&crate::dirs::config_dir())
        .join("Thunar")
        .join("uca.xml")
}

/// 安装（读-改-写合并），返回写入条目数。
pub(super) fn install(spec: &MenuSpec) -> Result<usize, MenuError> {
    let path = uca_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            MenuError::new(
                MenuErrorKind::Io,
                format!("创建 {} 失败: {e}", dir.display()),
            )
        })?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = merge(&existing, spec)?;
    write_atomic(&path, merged.as_bytes()).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("写入 {} 失败: {e}", path.display()),
        )
    })?;
    Ok(spec.resolved().count())
}

/// 卸载（按 `hzip-` 前缀删除动作节点，其余原样保留）。
pub(super) fn uninstall() -> Vec<String> {
    let path = uca_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let stripped = strip_ours(&existing);
    if let Err(e) = write_atomic(&path, stripped.as_bytes()) {
        return vec![format!("写入 {} 失败: {e}", path.display())];
    }
    Vec::new()
}

/// 同目录临时文件 + 原子替换（Thunar 可能正在读该文件，避免半写）。
fn write_atomic(path: &std::path::Path, body: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    let tmp = dir.join(format!(".uca.hzip-{}", std::process::id()));
    std::fs::write(&tmp, body)?;
    match crate::fs_util::atomic_replace(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// 是否已安装（文件中存在 `hzip-` 动作）。
pub(super) fn installed() -> bool {
    std::fs::read_to_string(uca_path())
        .map(|s| s.contains(&format!("<unique-id>{ID_PREFIX}")))
        .unwrap_or(false)
}

// ---------------------------------------------------------------- 合并逻辑

/// 读-改-写合并（纯函数，便于单测）。
pub(super) fn merge(existing: &str, spec: &MenuSpec) -> Result<String, MenuError> {
    let stripped = strip_ours(existing);
    let block = render_actions(spec)?;

    let body = if let Some(close) = stripped.rfind("</actions>") {
        // 在 </actions> 前插入（保留用户既有节点与其缩进原貌）
        let (head, tail) = stripped.split_at(close);
        format!("{head}{block}{tail}")
    } else if stripped.trim().is_empty() {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n{block}</actions>\n")
    } else {
        // 文件存在但不是合法 UCA（无 </actions>）：不擅自改写，报错让用户处理
        return Err(MenuError::new(
            MenuErrorKind::Spec,
            format!(
                "{} 不是可识别的 Thunar UCA 文件（缺少 </actions>）",
                uca_path().display()
            ),
        ));
    };
    Ok(body)
}

/// 删除全部 `hzip-` 动作（按 `<action>` 块整体移除；其余文本原样搬运）。
pub(super) fn strip_ours(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<action>") {
        let Some(end_rel) = rest[start..].find("</action>") else {
            break;
        };
        let end = start + end_rel + "</action>".len();
        let block = &rest[start..end];
        if block.contains(&format!("<unique-id>{ID_PREFIX}")) {
            // 丢弃该块（连同其前的空白，避免残留空行）
            out.push_str(rest[..start].trim_end_matches([' ', '\t', '\r', '\n']));
        } else {
            out.push_str(&rest[..end]);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// 渲染全部动作 XML（纯函数）。
pub(super) fn render_actions(spec: &MenuSpec) -> Result<String, MenuError> {
    let mut picked = Vec::new();
    for (entry, def) in spec.resolved() {
        picked.push((entry, def));
    }
    if picked.is_empty() {
        return Err(MenuError::new(MenuErrorKind::Spec, "无可写条目"));
    }

    let mut out = String::new();
    for (entry, def) in picked {
        let script = script_path(entry.key);
        // 解压类只对档案扩展名出现；压缩类对任意文件 / 文件夹出现
        let is_archive = def
            .scopes
            .iter()
            .all(|s| matches!(s, crate::context_menu::MenuScope::ArchiveFile));
        out.push_str("  <action>\n");
        out.push_str("    <icon>hzip</icon>\n");
        out.push_str(&format!("    <name>{}</name>\n", xml_escape(&entry.label)));
        out.push_str(&format!("    <submenu>{}</submenu>\n", xml_escape(SUBMENU)));
        out.push_str(&format!(
            "    <unique-id>{ID_PREFIX}{}</unique-id>\n",
            xml_escape(entry.key)
        ));
        out.push_str(&format!(
            "    <command>{}</command>\n",
            xml_escape(&format!("{} %F", shell_quote(&script.to_string_lossy())))
        ));
        if is_archive {
            out.push_str(&format!(
                "    <patterns>{}</patterns>\n",
                xml_escape(&archive_patterns(spec))
            ));
            out.push_str("    <other-files/>\n");
        } else {
            out.push_str("    <directories/>\n");
            out.push_str("    <other-files/>\n");
        }
        out.push_str("  </action>\n");
    }
    Ok(out)
}

/// 档案扩展名 glob 模式串（`*.zip;*.7z;…`）。
fn archive_patterns(spec: &MenuSpec) -> String {
    if spec.archive_exts.is_empty() {
        return "*".to_string();
    }
    spec.archive_exts
        .iter()
        .map(|e| format!("*.{e}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// XML 文本转义。
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Thunar 命令中的路径引用（含空格 → 单引号；`'` 用 `'\''` 收起）。
fn shell_quote(s: &str) -> String {
    if s.chars()
        .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '$' | '\\' | ';'))
    {
        format!("'{}'", s.replace('\'', r"'\''"))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
#[path = "tests/thunar.rs"]
mod tests;
