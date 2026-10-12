//! 各格式的**面向用户的扩展名清单**（全仓唯一来源）。
//!
//! # 为什么需要这张表
//!
//! 扩展名清单此前散在四处，各自维护：`Signature::extensions`（格式签名）、
//! `hzip-core::services::browse::container_handler_id`（嵌套容器识别）、
//! GUI 的 `MainViewModel::is_container_name` 与 `binding::entry_type_label`
//! （文件类型展示）。漏掉别名 `tgz` 的后果很具体：
//!
//! - 右键菜单按 `archive_exts` 判定「目标是不是档案」以决定解压类条目显隐，
//!   而 `archive_exts` 取自签名扩展名集合 → `a.tgz` 不在其中 ⇒ **菜单里没有
//!   解压项**；`a.tar.gz` 却因为 `.gz` 命中而一切正常。用户看到的就是
//!   「tgz 不受支持，tar.gz 正常」这种自相矛盾。
//! - 文件关联页的候选清单同源 ⇒ 也没法勾选 `tgz`。
//!
//! # 与 [`Signature::extensions`](crate::Signature::extensions) 的分工
//!
//! | | 签名扩展名 | 本表 |
//! |---|---|---|
//! | 用途 | 内容嗅探失败后的**格式回退** | 文件关联、右键菜单档案判定、选择器过滤、嵌套容器识别 |
//! | 内容 | 可含 `part1.rar` 这类**文件名后缀**（不是扩展名） | 只列**扩展名**，含 `tgz` 别名与 `tar.gz` 复合写法 |
//!
//! 两张表服务的是不同问题，故不强行合并；但**凡面向用户枚举扩展名处一律用本表**。

/// 格式族的类别（文件关联页的分组依据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtKind {
    /// 独立归档容器（zip / 7z / rar / tar / cpio）。
    Archive,
    /// 容器 + 压缩层的复合归档（`tar.gz` / `cpio.gz` 族，含 `tgz` 等别名）。
    ///
    /// 名字里的 `Tar` 是历史沿用（该变体先服务 tar 族）：语义是「外层压缩层 +
    /// 内层容器」，cpio 族同样归此档；内层容器由 [`FormatExts::container`] 指明。
    TarComposite,
    /// 单流压缩（gz / bz2 / xz / zst）：解出的是**一个**文件，不是容器。
    Stream,
}

/// 一个格式族及其扩展名。
#[derive(Debug, Clone, Copy)]
pub struct FormatExts {
    /// 格式 id（写侧 [`FormatId`](hzip_error::FormatId) 字符串；复合格式含点，如 `tar.gz`）。
    pub format: &'static str,
    /// 读侧**进入**该文件（当容器下钻）时用的 handler id；`None` = 不可下钻，
    /// 只能整体解压（单流压缩解出单文件；rar 只读下钻未接线）。
    pub container: Option<&'static str>,
    /// 类别（文件关联页分组用）。
    pub kind: ExtKind,
    /// 扩展名（小写、不含点）。**顺序即展示顺序**；含点的复合写法（`tar.gz`）
    /// 只用于匹配（Windows 关联以最后一个点为准，故不做关联候选）。
    pub exts: &'static [&'static str],
}

/// 支持的格式与扩展名（**顺序即文件关联页展示顺序**）。
///
/// 增删格式时**只需改这里**：文件关联候选、右键菜单档案判定、打开选择器
/// 过滤、嵌套容器识别、文件类型展示全部随之更新。
pub const FORMAT_EXTS: &[FormatExts] = &[
    FormatExts {
        format: "zip",
        container: Some("zip"),
        kind: ExtKind::Archive,
        exts: &["zip"],
    },
    FormatExts {
        format: "7z",
        container: Some("7z"),
        kind: ExtKind::Archive,
        exts: &["7z"],
    },
    FormatExts {
        format: "rar",
        container: None,
        kind: ExtKind::Archive,
        exts: &["rar"],
    },
    FormatExts {
        format: "tar",
        container: Some("tar"),
        kind: ExtKind::Archive,
        exts: &["tar"],
    },
    FormatExts {
        format: "tar.gz",
        container: Some("tar"),
        kind: ExtKind::TarComposite,
        exts: &["tgz", "tar.gz"],
    },
    FormatExts {
        format: "tar.bz2",
        container: Some("tar"),
        kind: ExtKind::TarComposite,
        exts: &["tbz", "tbz2", "tar.bz2"],
    },
    FormatExts {
        format: "tar.xz",
        container: Some("tar"),
        kind: ExtKind::TarComposite,
        exts: &["txz", "tar.xz"],
    },
    FormatExts {
        format: "tar.zst",
        container: Some("tar"),
        kind: ExtKind::TarComposite,
        exts: &["tzst", "tar.zst"],
    },
    // cpio（只读引入格式，02 §8.7）：裸档 + 与压缩层的组合形态。组合形态全部
    // 列出——嗅探按**内容**装配解码链（任何压缩层都通），UI 侧若漏列就会出现
    // 「引擎解得开、右键菜单却不认」的自相矛盾（`tgz` 的历史缺陷同型）。
    FormatExts {
        format: "cpio",
        container: Some("cpio"),
        kind: ExtKind::Archive,
        exts: &["cpio"],
    },
    FormatExts {
        format: "cpio.gz",
        container: Some("cpio"),
        kind: ExtKind::TarComposite,
        exts: &["cpio.gz"],
    },
    FormatExts {
        format: "cpio.bz2",
        container: Some("cpio"),
        kind: ExtKind::TarComposite,
        exts: &["cpio.bz2"],
    },
    FormatExts {
        format: "cpio.xz",
        container: Some("cpio"),
        kind: ExtKind::TarComposite,
        exts: &["cpio.xz"],
    },
    FormatExts {
        format: "cpio.zst",
        container: Some("cpio"),
        kind: ExtKind::TarComposite,
        exts: &["cpio.zst"],
    },
    FormatExts {
        format: "gz",
        container: None,
        kind: ExtKind::Stream,
        exts: &["gz"],
    },
    FormatExts {
        format: "bz2",
        container: None,
        kind: ExtKind::Stream,
        exts: &["bz2"],
    },
    FormatExts {
        format: "xz",
        container: None,
        kind: ExtKind::Stream,
        exts: &["xz"],
    },
    FormatExts {
        format: "zst",
        container: None,
        kind: ExtKind::Stream,
        exts: &["zst"],
    },
];

/// 组合档的**内层容器层名**（`init.cpio.gz` → `init.cpio`；`a.tgz` → `a.tar`）。
///
/// # 用途（用户口径 2026-09-24）
///
/// 浏览视图把组合档画成两层：外层是磁盘上的文件名（`init.cpio.gz`），下一层是
/// **内层容器**（`init.cpio`），展开它才见容器里的结构——Bandizip / 7-Zip 同款
/// 观感。此前的实现把两层压平（直接显示 cpio 里的条目），用户看到的就是「少了
/// cpio 这一层」。
///
/// # 命名规则
///
/// 「剥掉全部已知层后缀」得到的**词干** + 内层容器扩展名：
/// `a.tar.gz` → `a.tar`、`a.tgz` → `a.tar`、`init.cpio.gz` → `init.cpio`。
/// 逐层剥（不是只剥最后一个后缀）是为了让别名（`tgz`/`tbz2`）也归一成标准名。
///
/// 返回 `None` = **无压缩层**（单容器 `zip`/`tar`/`cpio` 本身，或单流 `x.txt.gz`）
/// → 调用方不加层（单流解出来就是一个文件条目，本来就没有「容器层」可言）。
pub fn composite_inner_name(name: &str) -> Option<String> {
    let (container, codec) = container_and_codec(name)?;
    let _ = codec?; // 无压缩层的单容器不加层
    let stem = strip_known_suffixes(name);
    if stem.is_empty() {
        return None;
    }
    let ext = components(container).next()?;
    Some(format!("{stem}.{ext}"))
}

/// 逐层剥全部已知扩展名（大小写不敏感；最长后缀优先），返回词干。
///
/// 只剥「表里认得的」扩展名 ⇒ `我的档案.cpio.gz` → `我的档案`；剥不动就停。
fn strip_known_suffixes(name: &str) -> String {
    let mut out = name.to_string();
    loop {
        let lower = out.to_ascii_lowercase();
        let mut hit: Option<usize> = None;
        for ext in all_exts() {
            let dot_len = ext.len() + 1;
            if lower.len() > dot_len
                && lower.ends_with(ext)
                && lower.as_bytes()[lower.len() - dot_len] == b'.'
                && hit.is_none_or(|h| ext.len() > h)
            {
                hit = Some(ext.len());
            }
        }
        let Some(len) = hit else { break };
        out.truncate(out.len() - len - 1);
    }
    out
}

/// `name` 是否以 `ext` 结尾且**落在点边界**上（`b.zip.txt` / `notzip` 均不命中）。
fn ends_with_ext(name_lower: &str, ext: &str) -> bool {
    name_lower.len() > ext.len()
        && name_lower.ends_with(ext)
        && name_lower.as_bytes()[name_lower.len() - ext.len() - 1] == b'.'
}

/// 命中的格式族（**最长后缀优先**：`a.tar.gz` → `tar.gz` 族而非 `gz` 族）。
///
/// 大小写不敏感；只按**文件名**匹配（传路径亦可，等价）。
pub fn find(name: &str) -> Option<&'static FormatExts> {
    let lower = name.to_ascii_lowercase();
    let mut best: Option<(&'static FormatExts, usize)> = None;
    for e in FORMAT_EXTS {
        for ext in e.exts {
            if ends_with_ext(&lower, ext) && best.is_none_or(|(_, len)| ext.len() > len) {
                best = Some((e, ext.len()));
            }
        }
    }
    best.map(|(e, _)| e)
}

/// 全部扩展名（表序；含 `tar.gz` 这类复合写法）。用于打开选择器过滤与
/// 右键菜单的 `archive_exts`（档案判定能力集）。
pub fn all_exts() -> impl Iterator<Item = &'static str> {
    FORMAT_EXTS.iter().flat_map(|e| e.exts.iter().copied())
}

/// 可做 **Windows 文件关联**的扩展名（表序）：只保留单点扩展名。
///
/// Windows 的关联查找以**最后一个点**为准（`.tar.gz` 实际走 `.gz`），
/// 故复合写法（`tar.gz`）不列入候选——用户勾 `tgz`/`gz` 即可覆盖。
pub fn assoc_exts() -> impl Iterator<Item = &'static str> {
    all_exts().filter(|e| !e.contains('.'))
}

/// 是否为我们认得的档案扩展名（文件类型展示 / 档案判定用）。
pub fn is_archive_ext(name: &str) -> bool {
    find(name).is_some()
}

/// 把该文件当**容器**下钻时用的 handler id（`tar.gz`/`tgz` → `tar`）。
///
/// 返回 `None` = 不可下钻（单流压缩、rar）或不是已知档案。
/// 调用方仍应确认该 handler 已注册（feature 裁剪时可能未编入）。
pub fn container_handler(name: &str) -> Option<&'static str> {
    container_and_codec(name).map(|(c, _)| c)
}

/// 容器 handler + **需先解的压缩层**（`tar.gz`/`tgz` → `("tar", Some("gz"))`；
/// `zip` → `("zip", None)`）。
///
/// 组合归档条目在档案内是**压缩后**的字节（`.tgz` 条目 = gzip 流），
/// 故下钻时得先按 `codec` 解一层再交容器 handler；`codec` 为 `None` = 条目流
/// 就是容器字节本身。
///
/// 返回 `None` = 不可下钻（单流压缩、rar）或不是已知档案。
pub fn container_and_codec(name: &str) -> Option<(&'static str, Option<&'static str>)> {
    let e = find(name)?;
    let container = e.container?;
    // 复合格式 id 的最后一层即压缩编码（`tar.bz2` → `bz2`）；单层格式无 codec
    let codec = components(e.format).nth(1);
    Some((container, codec))
}

/// 格式 id 的组成层（`tar.gz` → `["tar", "gz"]`；`zip` → `["zip"]`）。
///
/// 用于按「本构建实际注册了哪些 handler」裁剪格式清单。
pub fn components(format: &str) -> impl Iterator<Item = &str> {
    format.split('.')
}

#[cfg(test)]
#[path = "tests/exts.rs"]
mod tests;
