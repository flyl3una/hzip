//! KDE Plasma / Dolphin 右键菜单（T-CM §5）：KIO service menu。
//!
//! # 落点
//!
//! `$XDG_DATA_HOME/kio/servicemenus/hzip-*.desktop`（KDE Frameworks ≥ 5.85）。
//! 旧版（< 5.85）为 `$XDG_DATA_HOME/kservices5/ServiceMenus/`，且需额外
//! `ServiceTypes=KonqPopupMenu/Plugin`——本实现**同时写入两处**，旧版机器也能用。
//!
//! # 为什么拆两个文件
//!
//! KDE 的 `MimeType` 是**文件级**而非动作级：一个 `.desktop` 里的全部 `Actions`
//! 共用同一组 MIME。而解压项只该出现在档案上、压缩项应出现在任意文件/文件夹上。
//! 故拆为：
//!
//! | 文件 | MimeType | 动作 |
//! |---|---|---|
//! | `hzip-extract.desktop` | 档案 MIME 列表 | 解压类（`ArchiveFile`） |
//! | `hzip-compress.desktop` | `inode/directory;application/octet-stream;` | 压缩类（`Any` / 背景） |
//!
//! 两者共用 `X-KDE-Submenu=hzip` → Dolphin 里合并为一个「hzip」子菜单。
//!
//! # 用户目录需可执行位
//!
//! KDE 官方文档明示：装在非标准路径（用户数据目录）的 servicemenu 必须
//! `chmod +x` 才被认可。

use std::path::PathBuf;

use super::script::script_path;
use crate::context_menu::{MenuError, MenuErrorKind, MenuScope, MenuSpec, entry_for};

/// 顶层子菜单名（两个文件共用 → 合并为一个子菜单）。
const SUBMENU: &str = "hzip";

/// 解压类 servicemenu 文件名。
const EXTRACT_FILE: &str = "hzip-extract.desktop";

/// 压缩类 servicemenu 文件名。
const COMPRESS_FILE: &str = "hzip-compress.desktop";

/// 档案 MIME 类型（与 `hzip-format` 支持的扩展名对齐）。
const ARCHIVE_MIMES: &str = "application/zip;\
application/x-7z-compressed;\
application/vnd.rar;\
application/x-rar;\
application/x-tar;\
application/gzip;\
application/x-gzip;\
application/x-bzip2;\
application/x-xz;\
application/zstd;";

/// 「任意文件 + 文件夹」MIME 类型（KDE 官方：`application/octet-stream` = 所有文件）。
const ANY_MIMES: &str = "inode/directory;application/octet-stream;";

/// 当前与旧版 servicemenu 目录（`(目录, 是否需要旧版键)`）。
fn service_dirs() -> Vec<(PathBuf, bool)> {
    let base = crate::dirs::data_dir();
    let share = base.parent().unwrap_or(&base);
    vec![
        (share.join("kio").join("servicemenus"), false),
        (share.join("kservices5").join("ServiceMenus"), true),
    ]
}

/// 安装（两处目录各写两个文件），返回条目数。
pub(super) fn install(spec: &MenuSpec) -> Result<usize, MenuError> {
    let extract = render_file(spec, MenuScope::ArchiveFile, false)?;
    let compress = render_file(spec, MenuScope::Any, false)?;
    let extract_old = render_file(spec, MenuScope::ArchiveFile, true)?;
    let compress_old = render_file(spec, MenuScope::Any, true)?;

    for (dir, legacy) in service_dirs() {
        std::fs::create_dir_all(&dir).map_err(|e| {
            MenuError::new(
                MenuErrorKind::Io,
                format!("创建 {} 失败: {e}", dir.display()),
            )
        })?;
        let (ef, cf) = if legacy {
            (&extract_old, &compress_old)
        } else {
            (&extract, &compress)
        };
        write_executable(&dir.join(EXTRACT_FILE), ef)?;
        write_executable(&dir.join(COMPRESS_FILE), cf)?;
    }
    Ok(spec.resolved().count())
}

/// 卸载（两处目录的 `hzip-*.desktop` 全清；幂等）。
pub(super) fn uninstall() -> Vec<String> {
    let mut errors = Vec::new();
    for (dir, _) in service_dirs() {
        for name in [EXTRACT_FILE, COMPRESS_FILE] {
            let path = dir.join(name);
            if let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                errors.push(format!("删除 {} 失败: {e}", path.display()));
            }
        }
    }
    errors
}

/// 是否已安装（任一目录的任一文件在位）。
pub(super) fn installed() -> bool {
    service_dirs()
        .iter()
        .any(|(dir, _)| dir.join(EXTRACT_FILE).is_file() || dir.join(COMPRESS_FILE).is_file())
}

/// 生成一个 servicemenu 文件（纯函数，便于单测）。
///
/// `want` 决定纳入哪些 scope 的条目；`legacy` 决定是否写旧版专属键。
pub(super) fn render_file(
    spec: &MenuSpec,
    want: MenuScope,
    legacy: bool,
) -> Result<String, MenuError> {
    let picked: Vec<_> = spec
        .resolved()
        .filter(|(_, def)| def.scopes.contains(&want))
        .filter(|(e, _)| entry_for(e.key).is_some())
        .collect();

    if picked.is_empty() {
        return Err(MenuError::new(
            MenuErrorKind::Spec,
            format!("scope {:?} 无可写条目", want),
        ));
    }

    let mime = match want {
        MenuScope::ArchiveFile => ARCHIVE_MIMES,
        _ => ANY_MIMES,
    };

    let mut out = String::new();
    out.push_str("[Desktop Entry]\n");
    out.push_str("Type=Service\n");
    out.push_str(&format!("Name={SUBMENU}\n"));
    out.push_str("Icon=hzip\n");
    out.push_str(&format!("MimeType={mime}\n"));
    if legacy {
        out.push_str("ServiceTypes=KonqPopupMenu/Plugin\n");
    }
    let actions: Vec<&str> = picked.iter().map(|(e, _)| e.key).collect();
    out.push_str(&format!("Actions={};\n", actions.join(";")));
    out.push_str(&format!("X-KDE-Submenu={SUBMENU}\n"));
    out.push_str("X-KDE-Priority=TopLevel\n");

    for (entry, def) in picked {
        out.push_str(&format!("\n[Desktop Action {}]\n", entry.key));
        out.push_str(&format!("Name={}\n", desktop_escape(&entry.label)));
        out.push_str("Icon=hzip\n");
        // Exec 指向转发脚本（脚本内部实现 %SEL / %SELS 语义）
        let script = script_path(entry.key);
        out.push_str(&format!(
            "Exec={} %F\n",
            desktop_exec_quote(&script.to_string_lossy())
        ));
        let _ = def;
    }
    Ok(out)
}

/// `.desktop` 值的转义（`\` `\n` `\t` `\r` 具义；其余原样）。
fn desktop_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// `Exec` 中的可执行文件引用：含空格等需双引号包裹，`"` 反斜杠转义。
fn desktop_exec_quote(s: &str) -> String {
    if s.chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '$' | ';'))
    {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// 写入并置可执行位（KDE 对用户目录下的 servicemenu 要求可执行）。
fn write_executable(path: &std::path::Path, body: &str) -> Result<(), MenuError> {
    std::fs::write(path, body).map_err(|e| {
        MenuError::new(
            MenuErrorKind::Io,
            format!("写入 {} 失败: {e}", path.display()),
        )
    })?;
    super::script::set_executable(path)
}

#[cfg(test)]
#[path = "tests/dolphin.rs"]
mod tests;
