//! 卷集自动发现（02 §8.3 图 / T26 §4.5）：新命名 `.partN.rar` glob / 旧命名
//! `.rar/.r00/.r01` 扩展名序列探测 → 缺失卷 HZ-2005 附卷名。
//!
//! # 算法（T26 §4.5，含防误报落地）
//!
//! 1. **新命名**（文件名含 `.partN.rar`）：
//!    - 首卷归一：用户给 `.partN(N>1).rar` → 先定位 `.part1.rar`（缺失 → 2005
//!      报 part1）；从 part1 出发；
//!    - 同目录手写过滤 `{base}.part*.rar`，N 按**数值序**（`part2`、`part10`，
//!      非字典序）；
//!    - `1..=max(S)` 连续校验：**中间缺口**（如 part2 缺而 part3 在）→ 2005 报
//!      `{base}.part{n}.rar`（此类缺口是确定的——unrar 必将需要中间卷）。
//!      `max+1` 之后的「未来卷」无法预知是否被内容需要，留待 unrar 处理期
//!      EOpen（reader 以 `next_expected_name` 报 2005），**不在此误报**；
//! 2. **旧命名**：首卷 `.rar` + `base.r00, base.r01, …` 连续存在性探测，
//!    **首个缺失即截断**为该上界；
//!    - 用户给续卷 `.rNN(N>0)` → 归一首卷 `base.rar`（缺失 → 2005 报 base.rar）；
//!    - **0 个续卷且首卷即 `.rar` → 单卷退化**（返回 `[first]`，防误报）：不在此
//!      报 r00 缺失——若内容真需要下一卷，unrar 处理期自然 EOpen，由 reader 以
//!      `next_expected_name(first)` 报 2005（02 §8.3 F 分支延迟语义）；
//! 3. glob 实现 `std::fs::read_dir` + 手写过滤，**不引第三方**（05 §1.2 纪律）。
//!
//! # 命名风格判定不唯魔数版本（T26 §3.2）
//!
//! rar5 可用旧命名、rar4 可见新命名——本层纯按**文件名形态 + 同目录实际存在性**
//! 判定，与魔数版本解耦。reader 以 unrar 卷标志/处理期 EOpen 兜底。

use std::fs;
use std::path::{Path, PathBuf};

use hzip_error::{FormatErrorKind, HzipError};

/// 解析新命名：`{base}.part{NN}.rar` → `Some((base, N))`。
///
/// 纯文件名形态判定（不触碰文件系统）：扩展名 `rar`、file_stem 形如
/// `{base}.part{N}`（N 纯数字、可零填充），N 取数值。
pub(crate) fn new_style_n(path: &Path) -> Option<(String, u32)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if ext != "rar" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let (base, num) = stem.rsplit_once(".part")?;
    if base.is_empty() || num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((base.to_string(), num.parse().ok()?))
}

/// 是否新命名形态（`*.partN.rar`）。
pub(crate) fn is_new_style_name(path: &Path) -> bool {
    new_style_n(path).is_some()
}

/// 旧命名续卷扩展名 `.rNN`（`r0`/`r00`/`r01`/`r9`…）→ `Some(N)`。
/// 首卷 `.rar` 不在此列（N=0 的 `.r00` 是第一个续卷）。
pub(crate) fn old_continuation_number(ext_lower: &str) -> Option<u32> {
    let rest = ext_lower.strip_prefix('r')?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// 卷缺失错误（附卷文件名，02 附录 A HZ-2005：错误信息含卷名）。
pub(crate) fn missing_volume(name: String) -> HzipError {
    HzipError::Format {
        format: hzip_error::FormatId::from("rar"),
        kind: FormatErrorKind::MissingVolume(name),
    }
}

/// 首卷所在目录（`Path::parent` 为空的相对名 → `"."`）。
fn parent_dir(first: &Path) -> PathBuf {
    first
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 扫描目录内匹配命名形态的卷（手写过滤 + 数值解析，返回值按 n 升序）。
///
/// `accept`：文件名 → 卷号（非卷返回 None）。
fn scan_dir<F>(dir: &Path, accept: F) -> Vec<(u32, PathBuf)>
where
    F: Fn(&str) -> Option<u32>,
{
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(n) = accept(name) {
            out.push((n, ent.path()));
        }
    }
    out.sort_by_key(|(n, _)| *n);
    out
}

fn new_volume_name(base: &str, n: u32) -> String {
    format!("{base}.part{n}.rar")
}

/// 新命名分卷发现（T26 §4.5 步骤 2）。
fn discover_new_volumes(first: &Path) -> Result<Vec<PathBuf>, HzipError> {
    // 调用方入口 discover_multipart 已按 is_new_style_name 分派；此处再判一次
    // 以省 expect（不 panic：非新命名 → 单卷退化放行）。
    let Some((base, n)) = new_style_n(first) else {
        return Ok(vec![first.to_path_buf()]);
    };
    let dir = parent_dir(first);
    // 首卷归一：非 .part1 先定位 part1（缺失 → 报 part1）
    let part1 = if n == 1 {
        first.to_path_buf()
    } else {
        let p1 = dir.join(new_volume_name(&base, 1));
        if !p1.exists() {
            return Err(missing_volume(new_volume_name(&base, 1)));
        }
        p1
    };

    let prefix = format!("{base}.part");
    let suffix = ".rar";
    let found = scan_dir(&dir, |name| {
        let rest = name.strip_prefix(&prefix)?;
        let digits = rest.strip_suffix(suffix)?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u32>().ok()
    });
    // found: (n, path) 数值升序；n≥1 属本档案新命名卷
    let found: Vec<(u32, PathBuf)> = found.into_iter().filter(|(n, _)| *n >= 1).collect();
    if found.is_empty() {
        // 目录里连 part1 自身都没扫到（改名/IO 异常）——按实际传入文件退化
        return Ok(vec![part1]);
    }
    let max = found.last().map(|(n, _)| *n).unwrap_or(1);
    // 连续校验 1..=max：中间缺口（确定需要，未到「未来卷」未知区）→ 2005
    for want in 1..=max {
        if !found.iter().any(|(n, _)| *n == want) {
            return Err(missing_volume(new_volume_name(&base, want)));
        }
    }
    Ok(found.into_iter().map(|(_, p)| p).collect())
}

/// 旧命名 base（剥扩展名）：`base.rar` / `base.rNN` → 无后缀 base 路径 + 续卷号
/// （首卷 `.rar` 续卷号为 None）。
fn old_base(first: &Path) -> (PathBuf, Option<u32>) {
    let file_name = first
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = first
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let (stem, cont) = match ext.as_deref() {
        Some("rar") => {
            let stem = file_name.trim_end_matches(".rar").to_string();
            (stem, None)
        }
        Some(e) => {
            let stem = file_name.trim_end_matches(&format!(".{e}")).to_string();
            (stem, old_continuation_number(e))
        }
        None => (file_name, None),
    };
    let base = first.with_file_name(stem);
    (base, cont)
}

/// 旧命名续卷探测：`base.r00, base.r01, …` 连续存在性（首个缺失即止）。
pub(crate) fn old_continuations(base_no_ext: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for n in 0u32.. {
        let cand = base_no_ext.with_extension(format!("r{n:02}"));
        if cand.exists() {
            out.push(cand);
        } else {
            break;
        }
    }
    out
}

/// 旧命名分卷发现（T26 §4.5 步骤 3，防误报落地见模块文档）。
fn discover_old_volumes(first: &Path) -> Result<Vec<PathBuf>, HzipError> {
    let (base, cont) = old_base(first);
    let dir = parent_dir(first);
    // base 在 dir 下的完整路径（续卷探测基准）
    let base_in_dir = dir.join(
        base.file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(base.as_path())),
    );
    let _ = &base; // base 仅作 file_name 载体（含相对路径场景）；续卷探测用 base_in_dir
    if cont.is_some() {
        // 用户给了续卷：首卷应为 base.rar
        let first_vol = base_in_dir.with_extension("rar");
        if !first_vol.exists() {
            let name = first_vol
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "base.rar".to_string());
            return Err(missing_volume(name));
        }
        let conts = old_continuations(&base_in_dir);
        let mut vols = vec![first_vol];
        vols.extend(conts);
        return Ok(vols);
    }
    // 首卷即 .rar：探测续卷（0 个 → 单卷退化，防误报，不在此报 r00）
    let conts = old_continuations(&base_in_dir);
    let mut vols = vec![first.to_path_buf()];
    vols.extend(conts);
    Ok(vols)
}

/// 分卷集合发现总入口（T26 §4.5 / 02 §8.3 F 分支）。
///
/// 返回依序卷集（`volumes[0]` = 逻辑首卷——unrar 只认它）。确定性缺口（新命名
/// 中间缺卷 / 给了续卷但首卷缺失）在此报 HZ-2005 附卷名；其余「内容超出既有卷」
/// 场景延迟到 unrar 处理期 EOpen，由 reader 以 [`next_expected_name`] 报 2005。
pub(crate) fn discover_multipart(first: &Path) -> Result<Vec<PathBuf>, HzipError> {
    if is_new_style_name(first) {
        discover_new_volumes(first)
    } else {
        discover_old_volumes(first)
    }
}

/// 由最后已知卷名推导「下一期待卷」文件名（reader 处理期 EOpen → 2005 用）。
///
/// - 新命名 `base.partN.rar` → `base.part{N+1}.rar`；
/// - 旧命名 `.rar` 首卷 → `base.r00`；`base.rNN` → `base.r{NN+1}`（宽度对齐当前名）。
pub(crate) fn next_expected_name(last: &Path) -> String {
    if let Some((base, n)) = new_style_n(last) {
        return new_volume_name(&base, n + 1);
    }
    let (base, cont) = old_base(last);
    let stem_name = base
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match cont {
        Some(n) => {
            let ext = last
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            let width = ext.trim_start_matches('r').len().max(1);
            format!("{stem_name}.r{:0width$}", n + 1, width = width)
        }
        None => format!("{stem_name}.r00"),
    }
}

#[cfg(test)]
#[path = "tests/volume.rs"]
mod tests;
