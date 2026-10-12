//! Linux 后端：XDG 图标主题**查表**（不启动外部进程、不问 shell）。
//!
//! # 路线（与 Windows 后端的关系）
//!
//! Windows 侧向 shell 要图标（7-Zip / Bandizip 同款）；Linux 没有等价入口，
//! 图标按 freedesktop **图标主题**规范落成磁盘文件，故本后端是纯查表：
//! 扩展名 → MIME（复用 [`crate::assoc::mime`] 的既有表，与「打开方式」同一份）→
//! 图标名（`/` 换 `-`：`application/zip` → `application-zip`）→ 在主题目录里找
//! `<size>/<context>/<name>.png`。入参只需扩展名与种类。
//!
//! # 搜索面与顺序
//!
//! 主题搜索根（顺序即优先级）：`$XDG_DATA_HOME/icons`（缺省 `~/.local/share/icons`）
//! → `$XDG_DATA_DIRS` 各段 + `/icons`（缺省 `/usr/local/share/icons`、
//! `/usr/share/icons`）→ `~/.icons`（历史遗留路径，仍在用）。主题内：主题自身 →
//! `index.theme` 的 `Inherits=` 链 → `hicolor` 兜底。
//!
//! **不做桌面环境猜测**：`$XDG_CURRENT_DESKTOP` / GTK 主题名不保证存在、也不保证
//! 与用户实际看到的一致，猜错的后果是「拿到别人的图标」；固定按上述顺序在实际
//! 安装的主题集合里找，命中即返回。这与 7-Zip「扩展名 → 图标索引」同思路——
//! 宁可用主题默认，也不猜。
//!
//! # 硬限制（都是明确、可见的降级，不是静默失败）
//!
//! - 只接受 **`.png`**：本后端不引入 SVG 光栅化依赖，`.svg` 一律不查、不解码；
//!   找不到 PNG ⇒ `None` ⇒ 调用方回退自有图标。
//! - 档位取**最接近** [`ICON_PX`] 的 `NxN` 目录；解码尺寸与档位不一致时**不缩放**，
//!   原样返回实际尺寸（缩放、插值归调用方）。
//! - 全程无 panic：读不了的 `index.theme`、坏 PNG、缺目录一律走 `None`。
//!
//! # 为什么 `mime/` 与 `mimetypes/` 都查
//!
//! 任务卡按 `mime/` 约定，但 freedesktop 规范与真实主题（hicolor / Adwaita）用的是
//! `mimetypes/`。只查其一必然在一半的发行版上恒查空（⇒ 恒回退自有图标，静默失效），
//! 两者都试只是多一次 `stat`，故**按 `mime/` → `mimetypes/` 的顺序兼顾**；平铺档位
//! （`<size>/<name>.png`）同理。

use std::path::{Path, PathBuf};

use super::{ICON_PX, IconKind, IconRequest, SystemIcon};

/// 目录的固定 MIME（freedesktop 图标名 `inode-directory`）。
const DIR_MIME: &str = "inode-directory";

/// 主题链末端的兜底主题名：即使没有 `index.theme` 也按同一目录约定查找。
const FALLBACK_THEME: &str = "hicolor";

/// 主题内放 MIME 图标的上下文目录名（顺序 = 优先级；依据见模块文档末节）。
const CONTEXT_DIRS: [&str; 2] = ["mime", "mimetypes"];

/// 查系统图标；拿不到 → `None`（调用方回退自有图标）。
pub(super) fn lookup(req: &IconRequest<'_>) -> Option<SystemIcon> {
    lookup_in(&search_roots(), req)
}

/// 在给定搜索根下查图标（**可测入口**：根是显式入参，不读环境变量 ⇒ 注入合成
/// 主题树的用例彼此并行也不串扰）。
///
/// 只读磁盘：不启动进程、不写缓存、任何失败返回 `None`。
pub(super) fn lookup_in(roots: &[PathBuf], req: &IconRequest<'_>) -> Option<SystemIcon> {
    let name = icon_name(req)?;
    theme_chain(roots)
        .iter()
        .find_map(|theme| icon_in_theme(roots, theme, &name))
}

// ---------------------------------------------------------------- 搜索根

/// 图标主题搜索根（顺序即优先级，见模块文档）。
fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    // `dirs::data_dir()`：Linux = `$XDG_DATA_HOME`，缺省 `~/.local/share`。
    if let Some(data_home) = dirs::data_dir() {
        roots.push(data_home.join("icons"));
    }
    let data_dirs = match std::env::var_os("XDG_DATA_DIRS").filter(|v| !v.is_empty()) {
        Some(v) => std::env::split_paths(&v).collect::<Vec<_>>(),
        None => vec![
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ],
    };
    roots.extend(data_dirs.into_iter().map(|dir| dir.join("icons")));
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".icons"));
    }
    roots
}

// ---------------------------------------------------------------- 主题链

/// 图标名：目录恒为 `inode-directory`；其余按扩展名查 MIME 再把 `/` 换成 `-`。
/// 无扩展名、未知扩展名 ⇒ `None`（调用方取自有默认图标）。
fn icon_name(req: &IconRequest<'_>) -> Option<String> {
    if req.kind == IconKind::Dir {
        return Some(DIR_MIME.to_owned());
    }
    let mime = crate::assoc::mime::mime_of_ext(req.ext?)?;
    Some(mime.replace('/', "-"))
}

/// 主题遍历顺序：逐根扫顶层主题，每个主题贡献「自身 → `Inherits=` 链」；
/// 最后补 `hicolor` 兜底。同名只保留首次出现（防环、防重复扫盘）。
fn theme_chain(roots: &[PathBuf]) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    for root in roots {
        for name in top_level_themes(root) {
            push_theme_chain(roots, &name, &mut chain);
        }
    }
    if !chain.iter().any(|theme| theme == FALLBACK_THEME) {
        chain.push(FALLBACK_THEME.to_owned());
    }
    chain
}

/// 某根的直接子目录里能当主题用的（含有 `index.theme`）；排序只为遍历确定。
fn top_level_themes(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.join("index.theme").is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names
}

/// 把 `theme` 及其 `Inherits=` 链压入顺序表；已在表中 ⇒ 直接返回（环 / 菱形继承）。
fn push_theme_chain(roots: &[PathBuf], theme: &str, chain: &mut Vec<String>) {
    if chain.iter().any(|seen| seen == theme) {
        return;
    }
    chain.push(theme.to_owned());
    for parent in inherits(roots, theme) {
        push_theme_chain(roots, &parent, chain);
    }
}

/// 主题声明的 `Inherits=`（主题未安装 / 无 `index.theme` ⇒ 空）。
fn inherits(roots: &[PathBuf], theme: &str) -> Vec<String> {
    theme_dir(roots, theme)
        .map(|dir| ThemeMeta::read(&dir).inherits)
        .unwrap_or_default()
}

/// 主题目录：取**第一个**含该主题的搜索根（与「根序即优先级」一致）。
fn theme_dir(roots: &[PathBuf], theme: &str) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join(theme))
        .find(|p| p.is_dir())
}

// ---------------------------------------------------------------- index.theme

/// `index.theme` 里本后端消费的两个键。
struct ThemeMeta {
    /// `Directories=` 声明的档位名（只留 `NxN` 形状）。
    sizes: Vec<String>,
    /// `Inherits=` 声明的父主题名。
    inherits: Vec<String>,
}

impl ThemeMeta {
    /// 读并解析 `<theme_dir>/index.theme`；文件缺失/不可读/编码非 UTF-8 ⇒ 全空
    /// （于是该主题退化为「扫实际档位目录、无继承」，而不是报错）。
    fn read(theme_dir: &Path) -> Self {
        let text = std::fs::read_to_string(theme_dir.join("index.theme")).unwrap_or_default();
        let mut sizes = Vec::new();
        let mut inherits = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            // 跳过空行、注释与 `[Icon Theme]` 这类节头。
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "Directories" => sizes = size_names(value),
                "Inherits" => inherits = comma_list(value),
                _ => {}
            }
        }
        Self { sizes, inherits }
    }

    /// 主题内待查的档位目录名（`NxN`），按与 [`ICON_PX`] 的接近度排序（最近者在前）。
    ///
    /// 主题声明了 `Directories=` 就以声明为准（与 GTK 一致：未声明的目录不是有效
    /// 档位）；没声明（`hicolor` 等连 `index.theme` 都没有的兜底主题）则以实际存在
    /// 的 `NxN` 子目录为准——否则兜底主题会一个档位都扫不到。
    fn size_dirs(&self, theme_dir: &Path) -> Vec<String> {
        let mut names = if self.sizes.is_empty() {
            actual_size_dirs(theme_dir)
        } else {
            self.sizes.clone()
        };
        names.sort_by_key(|name| size_rank(name));
        names.dedup();
        names
    }
}

/// 逗号分隔列表 → 去空白、丢空段。
fn comma_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `Directories=` 条目形如 `32x32/mimetypes`、`scalable/apps`；只保留 `NxN` 档位名。
fn size_names(value: &str) -> Vec<String> {
    comma_list(value)
        .into_iter()
        .filter_map(|entry| {
            let head = entry.split('/').next().unwrap_or_default().to_owned();
            size_px(&head).map(|_| head)
        })
        .collect()
}

/// 主题目录下实际存在的 `NxN` 子目录名。
fn actual_size_dirs(theme_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(theme_dir) else {
        return names;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && size_px(name).is_some()
        {
            names.push(name.to_owned());
        }
    }
    names
}

/// `NxN` 档位名 → 边长（`32x32` → 32）；非该形状（`scalable`、`symbolic`、
/// `128x128@2`）⇒ `None`。
fn size_px(name: &str) -> Option<u32> {
    let (w, h) = name.split_once('x')?;
    if w != h {
        return None;
    }
    let n: u32 = w.parse().ok()?;
    (n > 0).then_some(n)
}

/// 档位排序键：与 [`ICON_PX`] 的距离优先；同距取**更大**档（缩小优于放大）。
fn size_rank(name: &str) -> (u32, std::cmp::Reverse<u32>) {
    let px = size_px(name).unwrap_or(0);
    (px.abs_diff(ICON_PX), std::cmp::Reverse(px))
}

// ---------------------------------------------------------------- 档位内查找

/// 在单个主题里找 `<name>.png`，命中即解码返回。
///
/// 顺序：档位由近及远 → `<size>/{mime,mimetypes}/<name>.png` → 平铺
/// `<size>/<name>.png`；所有档位都不中再退到主题根 `{mime,mimetypes}/<name>.png`。
/// 逐档退让是刻意的：只查「最接近」那一个档位会漏掉「主题只在一个偏远档位里
/// 提供该图标」的情形，而命中即返回保证最近档位优先。
fn icon_in_theme(roots: &[PathBuf], theme: &str, name: &str) -> Option<SystemIcon> {
    let dir = theme_dir(roots, theme)?;
    let file = format!("{name}.png");
    for size in ThemeMeta::read(&dir).size_dirs(&dir) {
        let size_dir = dir.join(size);
        for context in CONTEXT_DIRS {
            if let Some(icon) = decode(&size_dir.join(context).join(&file)) {
                return Some(icon);
            }
        }
        if let Some(icon) = decode(&size_dir.join(&file)) {
            return Some(icon);
        }
    }
    CONTEXT_DIRS
        .iter()
        .find_map(|context| decode(&dir.join(context).join(&file)))
}

/// 读 PNG → **预乘 BGRA**（gpui `RenderImage` 直通格式）；任何失败 ⇒ `None`。
///
/// `image` 给的是**未预乘** RGBA8，故逐像素乘 alpha（`+127` 四舍五入）并换通道到
/// BGRA。预乘不是可选的美化：`A=0` 处颜色必须归零，否则半透明边缘会渗出黑边/白边。
/// 解码尺寸与档位不一致时**不缩放**（插值归调用方）。
fn decode(path: &Path) -> Option<SystemIcon> {
    let bytes = std::fs::read(path).ok()?;
    let rgba = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut premultiplied_bgra = Vec::with_capacity(rgba.len());
    for pixel in rgba.pixels() {
        let [r, g, b, a] = pixel.0;
        let alpha = u32::from(a);
        premultiplied_bgra.extend_from_slice(&[
            premul(b, alpha),
            premul(g, alpha),
            premul(r, alpha),
            a,
        ]);
    }
    Some(SystemIcon {
        width,
        height,
        premultiplied_bgra,
        type_name: None,
    })
}

/// `通道 × alpha / 255`（`+127` 四舍五入）——结果必 ≤ alpha，满足预乘不变式。
fn premul(channel: u8, alpha: u32) -> u8 {
    ((u32::from(channel) * alpha + 127) / 255) as u8
}

#[cfg(test)]
#[path = "tests/linux.rs"]
mod tests;
