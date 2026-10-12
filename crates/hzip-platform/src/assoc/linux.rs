//! Linux（XDG）文件关联后端：`.desktop` + `mimeapps.list`。
//!
//! # 两件东西
//!
//! 1. **`$XDG_DATA_HOME/applications/hzip.desktop`**（缺省
//!    `~/.local/share/applications/`）—— 让「hzip」出现在文件管理器的
//!    「打开方式」列表里；`MimeType=` 列出我们认领的全部 MIME。按桌面条目规范
//!    加上可执行位（KDE/旧版实现对此有要求，见 `context_menu::linux::dolphin`）。
//! 2. **`$XDG_CONFIG_HOME/mimeapps.list`**（缺省 `~/.config/mimeapps.list`）——
//!    `[Default Applications]`（**双击落点**）+ `[Added Associations]`
//!    （「打开方式」里的优先项）。这是 freedesktop 的关联配置文件，也是
//!    `xdg-mime default` 自己会改的那个文件。
//!
//! # 与 Windows 的根本差别
//!
//! `mimeapps.list` 是**普通文本文件**：没有哈希保护、没有用户显式选择层
//! ⇒ 程序能真正把默认改过来。故本后端产出的 [`ExtAssocState`] 里
//! `pinned` 恒为 `false`（世界上没有「Linux 上改不动的默认程序」这回事）。
//!
//! # 只动自己的条目
//!
//! 改写 `mimeapps.list` 是**行级手术**：我们认领的那些 MIME 的条目被替换/删除，
//! 其余小节、键、注释、空行一字不动（用户手改的其它关联不能被我们吃掉）。
//!
//! # 回读顺序（XDG 规范）
//!
//! `$XDG_CONFIG_HOME/mimeapps.list` → `$XDG_CONFIG_DIRS/*/mimeapps.list`（缺省
//! `/etc/xdg`）→ 旧位置 `$XDG_DATA_HOME/applications/mimeapps.list` →
//! `$XDG_DATA_DIRS/*/applications/mimeapps.list`。先命中者为准（系统级条目
//! 说明默认是发行版/管理员设的：`ours=false, pinned=false`——我们改得动）。
//!
//! # 测试
//!
//! 本模块的路径解析**只读 `$XDG_*` 环境变量**（不借 `dirs` 的平台默认值），
//! 故单元测试给出临时目录即可隔离运行——连 `feature = "cross-check"` 下的
//! Windows 宿主也能跑同一批用例（路径只是普通文件路径）。

use std::path::{Path, PathBuf};

use super::mime::mime_of;
use super::{AssocSpec, ExtAssocState};

/// 我们注册的桌面条目文件名（`mimeapps.list` 里的标识）。
pub(super) const DESKTOP_ID: &str = "hzip.desktop";

/// 桌面条目里显示的程序名。
const DESKTOP_NAME: &str = "hzip";

/// `[Default Applications]` 小节名（双击落点）。
const SEC_DEFAULT: &str = "Default Applications";

/// `[Added Associations]` 小节名（「打开方式」里的优先项）。
const SEC_ADDED: &str = "Added Associations";

// ===================== 路径解析（XDG） =====================

/// `$HOME`（测试与 XDG 默认值共用；`dirs` 只作最后兜底）。
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// XDG 基准目录：环境变量为**绝对路径**时才采信（规范要求忽略相对路径）。
fn xdg(env: &str, fallback: &str) -> PathBuf {
    std::env::var_os(env)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

fn xdg_data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

fn xdg_config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_CONFIG_DIRS`（缺省 `/etc/xdg`）：**多路径**（冒号分隔）。
fn xdg_config_dirs() -> Vec<PathBuf> {
    xdg_list("XDG_CONFIG_DIRS", "/etc/xdg")
}

/// `$XDG_DATA_DIRS`（缺省 `/usr/local/share:/usr/share`）。
fn xdg_data_dirs() -> Vec<PathBuf> {
    xdg_list("XDG_DATA_DIRS", "/usr/local/share:/usr/share")
}

fn xdg_list(env: &str, fallback: &str) -> Vec<PathBuf> {
    let raw = std::env::var(env).unwrap_or_else(|_| fallback.to_string());
    raw.split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect()
}

/// 我们写的桌面条目路径。
pub(super) fn desktop_path() -> PathBuf {
    xdg_data_home().join("applications").join(DESKTOP_ID)
}

/// 我们写的关联配置路径（freedesktop 用户级 mimeapps.list）。
pub(super) fn mimeapps_path() -> PathBuf {
    xdg_config_home().join("mimeapps.list")
}

// ===================== 桌面条目 =====================

/// 桌面条目的 `Exec` 参数转义（Desktop Entry Spec §Exec：含空格等需整体加引号，
/// 内部的 `"` `` ` `` `$` `\` 以反斜杠转义）。
fn quote_exec(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for ch in path.chars() {
        if matches!(ch, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// 渲染 `hzip.desktop`（`mimes` = 本次认领的 MIME，顺序即展示顺序）。
fn render_desktop(exe: &Path, mimes: &[&str]) -> String {
    let mut mime_line = String::new();
    for m in mimes {
        mime_line.push_str(m);
        mime_line.push(';');
    }
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name={DESKTOP_NAME}\n\
         Comment=hzip 压缩档案\n\
         Exec={exec} --open %f\n\
         Terminal=false\n\
         Categories=Utility;Archiver;\n\
         MimeType={mime_line}\n",
        exec = quote_exec(&exe.to_string_lossy()),
    )
}

/// 写桌面条目（含可执行位；KDE 与部分旧实现的硬要求）。
fn write_desktop(exe: &Path, mimes: &[&str]) -> Result<(), String> {
    let path = desktop_path();
    let dir = path
        .parent()
        .ok_or_else(|| format!("桌面条目路径异常（无父目录）: {}", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    std::fs::write(&path, render_desktop(exe, mimes))
        .map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)) {
            // 非阻断：多数桌面不要求可执行位（只有 KDE/旧版实现要求）
            tracing::warn!(path = %path.display(), "设置桌面条目可执行位失败: {e}");
        }
    }
    Ok(())
}

/// 刷新桌面数据库（best-effort：工具不存在/失败只记日志——`mimeapps.list`
/// 优先于缓存，多数桌面无需它也能生效）。
fn update_desktop_database() {
    if std::env::var_os("HZIP_DRY_RUN").is_some() {
        tracing::info!("HZIP_DRY_RUN=1：跳过 update-desktop-database");
        return;
    }
    let dir = xdg_data_home().join("applications");
    match std::process::Command::new("update-desktop-database")
        .arg(&dir)
        .status()
    {
        Ok(s) if s.success() => tracing::info!(dir = %dir.display(), "桌面数据库已刷新"),
        Ok(s) => tracing::warn!(status = ?s.code(), "update-desktop-database 返回非零（忽略）"),
        Err(e) => tracing::info!("未调用 update-desktop-database（{e}；忽略）"),
    }
}

// ===================== mimeapps.list（INI 行级手术） =====================

/// INI 的一行（无法识别的行原样保留，绝不丢用户内容）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    /// `[小节名]`
    Section(String),
    /// `键=值`
    Entry { key: String, value: String },
    /// 注释 / 空行 / 不合法行
    Raw(String),
}

fn parse_ini(text: &str) -> Vec<Line> {
    text.lines()
        .map(|l| {
            let t = l.trim();
            if let Some(inner) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                Line::Section(inner.trim().to_string())
            } else if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
                Line::Raw(l.to_string())
            } else if let Some((k, v)) = t.split_once('=') {
                Line::Entry {
                    key: k.trim().to_string(),
                    value: v.trim().to_string(),
                }
            } else {
                Line::Raw(l.to_string())
            }
        })
        .collect()
}

fn render_ini(lines: &[Line]) -> String {
    let mut out = String::new();
    for l in lines {
        match l {
            Line::Section(s) => out.push_str(&format!("[{s}]\n")),
            Line::Entry { key, value } => out.push_str(&format!("{key}={value}\n")),
            Line::Raw(r) => {
                out.push_str(r);
                out.push('\n');
            }
        }
    }
    out
}

/// 小节体的范围 `[start, end)`（`end` = 下一小节行或文件尾）。
fn section_range(lines: &[Line], name: &str) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|l| matches!(l, Line::Section(s) if s == name))?
        + 1;
    let end = lines[start..]
        .iter()
        .position(|l| matches!(l, Line::Section(_)))
        .map(|i| start + i)
        .unwrap_or(lines.len());
    Some((start, end))
}

/// 小节内某键的值（不存在 → `None`）。
fn entry_value(lines: &[Line], section: &str, key: &str) -> Option<String> {
    let (start, end) = section_range(lines, section)?;
    lines[start..end].iter().find_map(|l| match l {
        Line::Entry { key: k, value } if k == key => Some(value.clone()),
        _ => None,
    })
}

/// 写入/覆盖某键的值：小节或键不存在则创建（小节追加在文件尾）。
fn set_entry(lines: &mut Vec<Line>, section: &str, key: &str, value: String) {
    match section_range(lines, section) {
        Some((start, end)) => {
            let hit = lines[start..end]
                .iter()
                .position(|l| matches!(l, Line::Entry { key: k, .. } if k == key));
            match hit {
                Some(i) => {
                    lines[start + i] = Line::Entry {
                        key: key.to_string(),
                        value,
                    }
                }
                None => lines.insert(
                    end,
                    Line::Entry {
                        key: key.to_string(),
                        value,
                    },
                ),
            }
        }
        None => {
            if !lines.is_empty() && !matches!(lines.last(), Some(Line::Raw(r)) if r.is_empty()) {
                lines.push(Line::Raw(String::new()));
            }
            lines.push(Line::Section(section.to_string()));
            lines.push(Line::Entry {
                key: key.to_string(),
                value,
            });
        }
    }
}

/// `;` 分隔的值列表（过滤空项）。
fn split_list(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 值列表 → 存储形式（非空时补尾随 `;`，符合 freedesktop 惯例）。
fn join_list(items: &[String]) -> String {
    if items.is_empty() {
        String::new()
    } else {
        format!("{};", items.join(";"))
    }
}

/// 从某键的值列表里删掉一项；列表空了就删掉整条键（让系统回落，而不是留
/// 一个空值——空值会让「打开方式」列表里出现空白项）。
fn strip_from_entry(lines: &mut Vec<Line>, section: &str, key: &str, item: &str) {
    let Some((start, end)) = section_range(lines, section) else {
        return;
    };
    let Some(i) = lines[start..end]
        .iter()
        .position(|l| matches!(l, Line::Entry { key: k, .. } if k == key))
    else {
        return;
    };
    let idx = start + i;
    let Line::Entry { value, .. } = &lines[idx] else {
        return;
    };
    let mut items = split_list(value);
    items.retain(|x| x != item);
    if items.is_empty() {
        lines.remove(idx);
    } else {
        lines[idx] = Line::Entry {
            key: key.to_string(),
            value: join_list(&items),
        };
    }
}

/// 读关联配置（用户级优先，其次系统级/旧位置；都无 → 空）。
///
/// 只收集我们**用得到**的两个小节（`[Default Applications]` / `[Added
/// Associations]`）——回读只问「这个 MIME 的默认是谁」（见 [`ext_state`]）；
/// 其余小节/注释在**改写**时由 [`parse_ini`] 原样保留（那条路径不经过这里），
/// 故不必在此复刻全文。
///
/// 多文件合并是「先命中者优先」（XDG 查找顺序）：同一 `(小节, 键)` 只认第一份
/// 定义——用户级 `~/.config/mimeapps.list` 压过 `/etc/xdg` 与旧位置。
fn read_mimeapps() -> Vec<Line> {
    let mut candidates = vec![mimeapps_path()];
    candidates.extend(
        xdg_config_dirs()
            .into_iter()
            .map(|d| d.join("mimeapps.list")),
    );
    candidates.push(xdg_data_home().join("applications/mimeapps.list"));
    candidates.extend(
        xdg_data_dirs()
            .into_iter()
            .map(|d| d.join("applications/mimeapps.list")),
    );

    // (小节名, 该小节的键值；键唯一、先命中者胜)
    let mut sections: [(&str, Vec<(String, String)>); 2] =
        [(SEC_DEFAULT, Vec::new()), (SEC_ADDED, Vec::new())];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines = parse_ini(&text);
        for (name, entries) in sections.iter_mut() {
            let Some((start, end)) = section_range(&lines, name) else {
                continue;
            };
            for line in &lines[start..end] {
                if let Line::Entry { key, value } = line
                    && !entries.iter().any(|(k, _)| k == key)
                {
                    entries.push((key.clone(), value.clone()));
                }
            }
        }
    }

    let mut out = Vec::new();
    for (name, entries) in sections {
        if entries.is_empty() {
            continue;
        }
        out.push(Line::Section(name.to_string()));
        out.extend(
            entries
                .into_iter()
                .map(|(key, value)| Line::Entry { key, value }),
        );
    }
    out
}

// ===================== 对外三件事 =====================

/// 回读某扩展名的关联实况（`pinned` 恒 false，见模块文档）。
pub(super) fn ext_state(ext: &str) -> ExtAssocState {
    let Some(mime) = mime_of(ext) else {
        return ExtAssocState::default();
    };
    let lines = read_mimeapps();
    let effective =
        entry_value(&lines, SEC_DEFAULT, mime).and_then(|v| split_list(&v).into_iter().next());
    let effective_name = effective.as_deref().and_then(desktop_name);
    let ours = effective.as_deref() == Some(DESKTOP_ID);
    ExtAssocState {
        effective,
        effective_name,
        ours,
        pinned: false,
    }
}

/// 桌面条目的 `Name=`（用于把标识显示成人能读的名字）。
fn desktop_name(id: &str) -> Option<String> {
    let mut dirs = vec![xdg_data_home().join("applications")];
    dirs.extend(xdg_data_dirs().into_iter().map(|d| d.join("applications")));
    for dir in dirs {
        let Ok(text) = std::fs::read_to_string(dir.join(id)) else {
            continue;
        };
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("Name=") {
                let name = name.trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// 应用关联：写桌面条目 + `mimeapps.list` 的两个小节 + 刷新桌面数据库。
pub(super) fn apply_association(spec: &AssocSpec) -> Result<(), String> {
    let mimes: Vec<&'static str> = spec.extensions.iter().filter_map(|e| mime_of(e)).collect();
    if mimes.is_empty() {
        return Err("没有可用的扩展名（勾选的格式不在 MIME 表内）".to_string());
    }
    write_desktop(&spec.exe_path, &mimes)?;

    let path = mimeapps_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines = parse_ini(&text);
    for mime in &mimes {
        // 双击落点：直接覆盖（这正是「应用关联」的语义）
        set_entry(&mut lines, SEC_DEFAULT, mime, format!("{DESKTOP_ID};"));
        // 「打开方式」优先项：我们置顶，保留用户原有的其它项
        let mut items = split_list(&entry_value(&lines, SEC_ADDED, mime).unwrap_or_default());
        items.retain(|x| x != DESKTOP_ID);
        items.insert(0, DESKTOP_ID.to_string());
        set_entry(&mut lines, SEC_ADDED, mime, join_list(&items));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    }
    std::fs::write(&path, render_ini(&lines))
        .map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
    update_desktop_database();
    tracing::info!(
        exts = spec.extensions.len(),
        file = %path.display(),
        "文件关联已写入（XDG 桌面条目 + mimeapps.list）"
    );
    Ok(())
}

/// 解除关联：删桌面条目 + 从 `mimeapps.list` 摘掉我们的条目（系统自动回落）。
///
/// **清理范围是「凡指向我们的」**，不是眼前的勾选集：用户可能先取消勾选、再点
/// 「解除关联」——只看勾选集就会把此前接管过的 MIME 留在配置里（双击仍归我们，
/// 用户以为「解除了却没解除」）。故这里摘的是**所有**值列表含 [`DESKTOP_ID`] 的
/// 条目（配置规模 = 用户自己的关联数，代价可忽略）。
pub(super) fn remove_association(_spec: &AssocSpec) -> Result<(), String> {
    let mut errors = Vec::new();
    let desktop = desktop_path();
    if let Err(e) = std::fs::remove_file(&desktop)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!("删除 {} 失败: {e}", desktop.display()));
    }

    let path = mimeapps_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        let mut lines = parse_ini(&text);
        for section in [SEC_DEFAULT, SEC_ADDED] {
            let Some((start, end)) = section_range(&lines, section) else {
                continue;
            };
            // 先收集、后删除（避免边遍历边改）
            let keys: Vec<String> = lines[start..end]
                .iter()
                .filter_map(|l| match l {
                    Line::Entry { key, value }
                        if split_list(value).iter().any(|x| x == DESKTOP_ID) =>
                    {
                        Some(key.clone())
                    }
                    _ => None,
                })
                .collect();
            for key in keys {
                strip_from_entry(&mut lines, section, &key, DESKTOP_ID);
            }
        }
        if let Err(e) = std::fs::write(&path, render_ini(&lines)) {
            errors.push(format!("写 {} 失败: {e}", path.display()));
        }
    }
    update_desktop_database();
    if errors.is_empty() {
        tracing::info!("文件关联已解除（桌面条目 + mimeapps.list 条目）");
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
#[path = "tests/linux.rs"]
mod tests;
