//! VM 行 → UI 行派生（T-G20 EntryRow 派生 + T-G21 JobsRowUi 追加；纯函数可单测）。
//!
//! 大数一律格式化进 UI；大小/时间格式化职责从 T42 接续（本地时区，02 §1.2）。
//!
//! # 分节
//!
//! - `TreeRow → BrowseRow`（T-G20）
//! - `current_dir → crumbs`（T-G20）
//! - 排序视图重排（**不改 VM entries 顺序**，表格语义）
//! - 文件尾 `// ---- jobs view (T-G21) ----` 分节：JobRow → JobsRowUi（T-G21）

use crate::ui::contracts::{BrowseRow, Crumb, SortColumn, SortDir};
use crate::viewmodel::rows::{EntryRow, TreeRow};
use gpui_kit::SharedString;
use gpui_kit::component::tree::TreeItem;
use hzip_format::NameEncoding;

/// 字节数人读格式化（`{:.1} KiB/MiB/GiB`；0 → "0 B"）。
pub fn fmt_bytes(n: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = K * 1024;
    const G: u64 = M * 1024;
    if n >= G {
        format!("{:.1} GiB", n as f64 / G as f64)
    } else if n >= M {
        format!("{:.1} MiB", n as f64 / M as f64)
    } else if n >= K {
        format!("{:.1} KiB", n as f64 / K as f64)
    } else {
        format!("{n} B")
    }
}

/// 级别排序键（未知 `—` 恒排末 = -1；数字文本转数值）。
fn level_key(text: &str) -> f64 {
    let t = text.trim();
    if t.is_empty() || t == NO_VALUE {
        return -1.0;
    }
    t.parse::<f64>().unwrap_or(-1.0)
}

/// CRC 排序键（8 位十六进制文本 → 数值；`—` = 0）。
fn crc_key(text: &str) -> u32 {
    u32::from_str_radix(text.trim(), 16).unwrap_or(0)
}

/// 人类可读大小文本 → 可排序键（`12.4 KiB` → 12697.6；"—"/解析失败 → -1，
/// 恒排有效值之后——UI 展示层语义，T-G20）。
fn size_key(text: &str) -> f64 {
    let text = text.trim();
    if text.is_empty() || text == NO_VALUE {
        return -1.0;
    }
    let (num, unit) = text.split_once(' ').unwrap_or((text, "B"));
    let mult = match unit.to_ascii_lowercase().as_str() {
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "b" | "" => 1.0,
        _ => 1.0,
    };
    num.parse::<f64>().map(|n| n * mult).unwrap_or(-1.0)
}

/// 目录/无值占位（06 §4.1 原型：目录大小列 "—"）。
pub const NO_VALUE: &str = "—";

/// 类型列文本（R-GUI4 #2）：目录 → 文件夹；软链接 → 快捷方式；嵌套容器 →
/// 压缩包；其余按扩展名映射常见类型，未知扩展回退 "{EXT} 文件"、无扩展回退
/// "文件"（展示面本地化：`type.*`；扩展名本身是数据，不翻译）。
pub fn entry_type_label(name: &str, is_dir: bool, is_symlink: bool, is_container: bool) -> String {
    if is_dir {
        return crate::tr("type.folder");
    }
    if is_symlink {
        return crate::tr("type.shortcut");
    }
    if is_container {
        return crate::tr("type.archive");
    }
    // 档案扩展名（含 tgz/tbz2/txz/tzst 别名与 tar.gz 复合写法）→ 表驱动，
    // 不再维护第二份清单（曾漏 tgz：`.tgz` 显示成「TGZ 文件」）。
    if hzip_format::exts::is_archive_ext(name) {
        return crate::tr("type.compressed");
    }
    let Some(ext) = name.rsplit('.').next() else {
        return crate::tr("type.file");
    };
    if ext.is_empty() || ext.len() == name.len() {
        return crate::tr("type.file"); // 无扩展名（rsplit('.').next() == 整名）
    }
    match ext.to_ascii_lowercase().as_str() {
        "txt" | "log" | "md" | "rst" => crate::tr("type.text"),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "svg" => crate::tr("type.image"),
        "pdf" => crate::tr("type.pdf"),
        "exe" | "msi" | "bat" | "cmd" => crate::tr("type.executable"),
        "dll" | "so" | "dylib" => crate::tr("type.library"),
        "mp3" | "wav" | "flac" | "ogg" => crate::tr("type.audio"),
        "mp4" | "mkv" | "avi" | "mov" | "webm" => crate::tr("type.video"),
        "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "go" | "java" | "toml" | "json"
        | "yaml" | "yml" | "xml" | "html" | "css" | "sh" | "ps1" => crate::tr("type.source"),
        _ => crate::tr_with(
            "type.ext_file",
            &[("ext", ext.to_ascii_uppercase().as_str())],
        ),
    }
}

/// RFC3339 UTC → 本地时区 `YYYY-MM-DD HH:mm`（解析失败原样返回）。
pub fn fmt_mtime(rfc3339: &str) -> String {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    let Ok(dt) = OffsetDateTime::parse(rfc3339, &Rfc3339) else {
        return rfc3339.to_string();
    };
    let local =
        dt.replace_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC));
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    local.format(&fmt).unwrap_or_else(|_| rfc3339.to_string())
}

/// 编码列文本（`NameEncoding` → "UTF-8"/"GBK"/…；确定编码 → 编码名；未知 "—"）。
pub fn encoding_label(row: &EntryRow) -> String {
    match &row.name_encoding {
        NameEncoding::Utf8 => "UTF-8".to_string(),
        NameEncoding::Suspect(enc) => match enc {
            hzip_format::Encoding::Gbk => "GBK".to_string(),
            hzip_format::Encoding::Big5 => "Big5".to_string(),
            hzip_format::Encoding::ShiftJis => "Shift-JIS".to_string(),
            hzip_format::Encoding::EucKr => "EUC-KR".to_string(),
            hzip_format::Encoding::Cp1252 => "CP1252".to_string(),
            hzip_format::Encoding::Utf8 => "UTF-8".to_string(),
            // #[non_exhaustive] 兜底
            _ => NO_VALUE.to_string(),
        },
        NameEncoding::Unknown => NO_VALUE.to_string(),
        // #[non_exhaustive] 兜底
        _ => NO_VALUE.to_string(),
    }
}

// —— 新五列文本派生（TreeRow/EntryRow 双源共用；纯函数）——

/// 压缩级别列文本（None = 格式不存储级别 → `—`）。
pub fn level_text(level: Option<u8>) -> String {
    level
        .map(|l| l.to_string())
        .unwrap_or_else(|| NO_VALUE.to_string())
}

/// 加密算法列文本（None = 未加密/未声明 → `—`；未加密行直接 `—`，不显示「无」）。
pub fn encryption_text(encrypted: bool, enc: Option<&str>) -> String {
    if !encrypted {
        return NO_VALUE.to_string();
    }
    enc.unwrap_or(NO_VALUE).to_string()
}

/// CRC 校验列文本（8 位大写十六进制；None → `—`）。
pub fn crc_text(crc: Option<u32>) -> String {
    crc.map(|c| format!("{c:08X}"))
        .unwrap_or_else(|| NO_VALUE.to_string())
}

/// 文件属性列文本（按 只读/隐藏/系统/可执行 顺序拼 i18n 标记，空格分隔；
/// 全无 → `—`。可执行未知（None）不显示——Unix mode 缺失时不猜测）。
pub fn attrs_text(attrs: hzip_format::EntryAttrs) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if attrs.read_only {
        parts.push(crate::tr("attr.read_only").leak());
    }
    if attrs.hidden {
        parts.push(crate::tr("attr.hidden").leak());
    }
    if attrs.system {
        parts.push(crate::tr("attr.system").leak());
    }
    if attrs.executable == Some(true) {
        parts.push(crate::tr("attr.executable").leak());
    }
    if parts.is_empty() {
        NO_VALUE.to_string()
    } else {
        parts.join(" ")
    }
}

/// `TreeRow`（当前目录行）→ [`BrowseRow`]（展示文本在本层格式化）。
pub fn browse_row_from_tree_row(row: &TreeRow) -> BrowseRow {
    let suspect = matches!(row_enc_suspect(row), Some(true));
    BrowseRow {
        entry_id: row.entry_id.map(|id| id as u64),
        path: row.path.clone(),
        name: row.name.clone(),
        is_dir: row.is_dir,
        is_container: row.is_container,
        encrypted: row.encrypted,
        suspect,
        size_text: if row.is_dir {
            if row.size_raw > 0 {
                fmt_bytes(row.size_raw)
            } else {
                NO_VALUE.to_string()
            }
        } else {
            fmt_bytes(row.size_raw)
        },
        // 压缩后未知（0 = 未计算）→ 空占位（R-GUI4 #2：不显示误导性 0B）
        packed_text: if row.size_packed > 0 {
            fmt_bytes(row.size_packed)
        } else {
            NO_VALUE.to_string()
        },
        // TreeRow 无编码字段（编码判定归 EntryRow 全量派生路径）
        encoding_text: NO_VALUE.to_string(),
        mtime_text: row
            .mtime
            .as_deref()
            .map(fmt_mtime)
            .unwrap_or_else(|| NO_VALUE.to_string()),
        // 软链接标记照实传播（TreeRow.is_symlink）：右栏列表要画 Link 图标、
        // 类型列要写「软链接」——写死 false 会让两处都失声（左栏树行本就不含
        // 链接语义，值恒 false，无需分别处理）。
        is_symlink: row.is_symlink,
        symlink_target: None,
        // 合成容器层（组合档内层容器：`init.cpio`）→ 类型列显**格式名**
        // （用户口径 2026-09-24：显示 `cpio` 而不是「文件夹」）；其余行走通用映射。
        type_text: row.container_format.clone().unwrap_or_else(|| {
            entry_type_label(&row.name, row.is_dir, row.is_symlink, row.is_container)
        }),
        level_text: level_text(row.compression_level),
        encryption_text: encryption_text(row.encrypted, row.encryption),
        crc_text: crc_text(row.crc32),
        attrs_text: attrs_text(row.attrs),
    }
}

/// TreeRow 无编码字段——可疑判定经 EntryRow（VM entries 查表）：
/// 这里以 `None` 占位（T-G20 接线时由 AppData 传入 EntryRow 映射）。
fn row_enc_suspect(_row: &TreeRow) -> Option<bool> {
    None
}

/// `EntryRow`（VM 条目）→ [`BrowseRow`]（右栏完整派生；来自 VM entries 顺序）。
pub fn browse_row_from_entry_row(row: &EntryRow) -> BrowseRow {
    let suspect = matches!(row.name_encoding, NameEncoding::Suspect(_));
    BrowseRow {
        entry_id: Some(row.id as u64),
        path: row.name.clone(),
        name: row.name.rsplit('/').next().unwrap_or(&row.name).to_string(),
        is_dir: row.is_dir,
        is_container: false,
        encrypted: row.encrypted,
        suspect,
        size_text: if row.is_dir {
            NO_VALUE.to_string()
        } else {
            fmt_bytes(row.size_raw)
        },
        packed_text: if row.size_packed > 0 {
            fmt_bytes(row.size_packed)
        } else {
            NO_VALUE.to_string() // 未知压缩后大小 → 空占位（R-GUI4 #2）
        },
        encoding_text: encoding_label(row),
        mtime_text: row
            .mtime
            .as_deref()
            .map(fmt_mtime)
            .unwrap_or_else(|| NO_VALUE.to_string()),
        // 软链接标记 + 目标（R-GUI1-02 §2.4；format 层 EntryInfo 上浮）
        is_symlink: row.is_symlink,
        symlink_target: row.symlink_target.clone(),
        type_text: entry_type_label(&row.name, row.is_dir, row.is_symlink, false),
        level_text: level_text(row.compression_level),
        encryption_text: encryption_text(row.encrypted || row.header_encrypted, row.encryption),
        crc_text: crc_text(row.crc32),
        attrs_text: attrs_text(row.attrs),
    }
}

/// 浏览行视图排序（**不改 VM entries 顺序**——排序作用于派生出的展示行副本；
/// 目录恒排前（表格语义，对齐 06 §4.1 原型目录置顶）。
pub fn sort_browse_rows(rows: &mut [BrowseRow], col: SortColumn, dir: SortDir) {
    let ord = |a: &BrowseRow, b: &BrowseRow| -> std::cmp::Ordering {
        match col {
            SortColumn::Name => a.name.cmp(&b.name),
            SortColumn::Type => a.type_text.cmp(&b.type_text),
            SortColumn::Size => size_key(&a.size_text).total_cmp(&size_key(&b.size_text)),
            SortColumn::Packed => size_key(&a.packed_text).total_cmp(&size_key(&b.packed_text)),
            SortColumn::Encoding => a.encoding_text.cmp(&b.encoding_text),
            SortColumn::Mtime => a.mtime_text.cmp(&b.mtime_text),
            // 级别：数值序（文本 "6" 按数字比；未知 "—" 恒排末）
            SortColumn::Level => level_key(&a.level_text).total_cmp(&level_key(&b.level_text)),
            // 加密算法：文本字典序（"—" 归序即可，无需特殊处理）
            SortColumn::Encryption => a.encryption_text.cmp(&b.encryption_text),
            // CRC：数值序（十六进制文本转数值比；"—" = 0 与未声明同序）
            SortColumn::Crc => crc_key(&a.crc_text).cmp(&crc_key(&b.crc_text)),
            // 属性：按标记数（文本长度近似——同标记集长度相同）
            SortColumn::Attrs => a.attrs_text.cmp(&b.attrs_text),
        }
    };
    rows.sort_by(|a, b| {
        // 目录恒排前（06 §4.1 原型）；is_dir=false > true → reverse 后目录在前
        a.is_dir.cmp(&b.is_dir).reverse().then_with(|| {
            let o = ord(a, b);
            if dir == SortDir::Desc { o.reverse() } else { o }
        })
    });
}

#[cfg(test)]
#[path = "tests/binding.rs"]
mod tests;

// ---- jobs view (T-G21) ----
// 本分节归 T-G21（binding.rs 共享冲突点：文件尾 append，降低合并冲突面）。

/// 任务状态语义色分组（T-GUI T02：binding 层只出语义，色值由渲染侧
/// 读 `cx.theme()`——danger/success/warning/primary/muted_foreground）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStateSemantic {
    /// 次要信息（队列中）
    Neutral,
    /// 进行中（运行中）
    Accent,
    /// 需注意（等待输入/取消中/部分完成）
    Warning,
    /// 失败/已取消
    Danger,
    /// 成功
    Success,
}

/// 任务行 UI 投影（T-G21 + R-GUI4 #4 列扩展：任务/输入/输出/行为/格式/
/// 开始时间/耗时/大小/进度/状态/操作）。
#[derive(Debug, Clone)]
pub struct JobsRowUi {
    /// 任务 ID
    pub job_id: u64,
    /// 标题（解压 Goose-win32-x64.zip——行为 + 源对象名）
    pub title: String,
    /// 输入对象（档案/源显示名）
    pub input_text: String,
    /// 输出对象（目标目录/产物；无 → "—"）
    pub output_text: String,
    /// 行为（解压/压缩/测试/重命名…）
    pub action_text: String,
    /// 终局失败原因**原文**（`HZ-xxxx: …`；None = 无失败）。
    ///
    /// 用户口径 2026-09-29：任务中心要能看出**具体**原因——此前只有逐条目
    /// `failures`，编辑/整档类任务为空 ⇒ 单元格空白。
    pub final_line: Option<String>,
    /// 档案格式（zip/7z/…；空 → "—"）
    pub format_text: String,
    /// 开始时间（HH:MM:SS；未开始 → "—"）
    pub started_text: String,
    /// 耗时（12s / 1m 05s；运行中现算）
    pub elapsed_text: String,
    /// 大小（total_bytes；0 → "—"）
    pub size_text: String,
    /// 状态标签（运行中/队列/等待输入/取消中/完成/部分完成/失败/取消）
    pub state_label: String,
    /// 状态语义（渲染侧据此读 `cx.theme()` 语义色；binding 层不持色值，
    /// T-GUI T02：颜色决策收编主题 token）
    pub state_semantic: JobStateSemantic,
    /// 进度 0..1；`None` = **未知总量**（渲染侧走不确定态：滑动条 + 「…」，不得
    /// 冒充 0%——用户实测缺陷 2026-09-24：解压大文件时进度条恒显示 0% 而非未知）
    pub progress: Option<f32>,
    /// 是否活跃（取消按钮显隐：仅 Queued/Running/WaitingInput/Cancelling）
    pub is_active: bool,
    /// 失败明细（下钻行：条目名 + i18n key）
    pub failures: Vec<(String, String)>,
}

/// kind 展示文案（本地化查表；与 T42 约定一致）。
pub fn job_kind_label(kind: &str) -> String {
    match kind {
        "extract" => crate::tr("job.kind_extract"),
        "compress" => crate::tr("job.kind_compress"),
        "test" => crate::tr("job.kind_test"),
        "convert" => crate::tr("job.kind_convert"),
        "edit_save" => crate::tr("job.kind_save"),
        // 重命名/新建/批量变更（EditBatch）——任务中心「行为」列
        "edit" => crate::tr("job.kind_edit"),
        // 打开档案/进入容器（非任务；记录形态见 jobs_vm::record_open）
        "open" => crate::tr("job.kind_open"),
        // 复制选中条目（非任务；记录形态见 jobs_vm::record_op）
        "copy" => crate::tr("job.kind_copy"),
        // 刷新（重读档案；非任务）
        "refresh" => crate::tr("job.kind_refresh"),
        _ => crate::tr("job.kind_unknown"),
    }
}

/// 开始时间 → `HH:MM:SS`（本地时区）。
fn fmt_started(at: std::time::SystemTime) -> String {
    let dt: time::OffsetDateTime = at.into();
    format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second())
}

/// 耗时 → `12s` / `1m 05s` / `1h 02m`。
fn fmt_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// `JobRow → JobsRowUi`（列扩展投影；数据面快照驱动，不持 VM 句柄）。
pub fn jobs_row_ui(row: &crate::viewmodel::JobRow) -> JobsRowUi {
    use crate::viewmodel::JobRowState as S;
    let (state_label, state_semantic): (String, JobStateSemantic) = match row.state {
        S::Queued => (crate::tr("job.state_queued"), JobStateSemantic::Neutral),
        S::Running => (crate::tr("job.state_running"), JobStateSemantic::Accent),
        S::WaitingInput => (
            crate::tr("job.state_waiting_input"),
            JobStateSemantic::Warning,
        ),
        S::Cancelling => (crate::tr("job.state_cancelling"), JobStateSemantic::Warning),
        S::Done => (crate::tr("job.state_done"), JobStateSemantic::Success),
        S::PartialDone => (crate::tr("job.state_partial"), JobStateSemantic::Warning),
        S::Failed => (crate::tr("job.state_failed"), JobStateSemantic::Danger),
        S::Cancelled => (crate::tr("job.state_cancelled"), JobStateSemantic::Danger),
    };
    // 耗时：终局定格 elapsed；运行中现算（10 Hz 泵刷新期自然推进）
    let elapsed = row.elapsed.or_else(|| {
        row.started_at
            .and_then(|s| std::time::SystemTime::now().duration_since(s).ok())
    });
    let action_text = job_kind_label(row.kind);
    let final_line = row.final_line.as_ref().map(|fl| {
        if fl.code != 0 {
            format!("HZ-{:04}: {}", fl.code, fl.message)
        } else {
            fl.message.clone()
        }
    });
    JobsRowUi {
        job_id: row.job_id,
        final_line,
        title: format!("{action_text} {}", row.src),
        input_text: row.src.clone(),
        output_text: row.dest.clone().unwrap_or_else(|| NO_VALUE.to_string()),
        action_text,
        format_text: if row.format.is_empty() {
            NO_VALUE.to_string()
        } else {
            row.format.clone()
        },
        started_text: row
            .started_at
            .map(fmt_started)
            .unwrap_or_else(|| NO_VALUE.to_string()),
        elapsed_text: elapsed
            .map(fmt_elapsed)
            .unwrap_or_else(|| NO_VALUE.to_string()),
        size_text: if row.total_bytes > 0 {
            fmt_bytes(row.total_bytes)
        } else {
            NO_VALUE.to_string()
        },
        state_label,
        state_semantic,
        progress: row.progress.map(|p| p as f32),
        is_active: !crate::viewmodel::jobs_vm::is_terminal(row.state),
        failures: {
            let mut f: Vec<(String, String)> = row
                .failures
                .iter()
                .map(|f| (f.entry.clone(), f.message.clone()))
                .collect();
            // 无逐条目明细时用终局摘要行当失败原因（用户口径 2026-09-24：
            // 「打开失败要在任务中心记录比较明确的失败原因」——打开类记录
            // 与「快速失败」的任务（compress 顶层失败等）都只有终局摘要，
            // 此前它只进进度窗口文件列表，任务中心一片空白）。
            if f.is_empty()
                && let Some(fl) = &row.final_line
            {
                f.push((fl.entry.clone(), fl.message.clone()));
            }
            f
        },
    }
}

// ---- browse tree (T-G53) ----
// 平铺 TreeRow（VM nav_rows 快照）→ 嵌套 gpui_base TreeItem（左栏 gpui-kit Tree 数据面）。
// 纯函数可单测；TreeItem 为 gpui_kit::component::tree 再导出类型（binding 层例外登记：
// 红线只约束 viewmodel/，本文件属 ui/ 允许 import gpui_kit）。

/// 树行元数据（R-GUI2-04 §3.2——图标判定数据面）。
///
/// gpui-kit `TreeItem::is_folder()` = `!children.is_empty()`：折叠目录/空
/// 目录无子节点 → 误判文件（空目录图标坑，§3.3）。本元数据随喂树同批
/// 替换，渲染层按 `is_dir` 判图标，不经 `entry.is_folder()`。
#[derive(Debug, Clone)]
pub struct TreeRowMeta {
    /// 节点在**所属会话**内的路径（`""` = 该层档案根）
    pub path: String,
    /// 是否目录（图标 Folder/FolderOpen 判定源）
    pub is_dir: bool,
    /// 是否文件型容器（图标 FolderZip 判定源——压缩包不用文件图标）
    pub is_container: bool,
}

/// 树节点 id 编码（层号 + 本层路径）——跨嵌套层唯一，且可解回路由目标。
///
/// 为什么 id ≠ path：嵌套展示下同名的 `docs/` 可能内外两层各有一份，widget 的
/// 展开/选中态与 `tree_meta` 都按 id 存 ⇒ 撞车会串层。
pub fn tree_item_id(level: usize, path: &str) -> String {
    format!("{level}:{path}")
}

/// [`tree_item_id`] 的逆：`"{level}:{path}"` → `(level, path)`。
///
/// 只切**第一个** `:`——层号必然全数字，路径里即便含 `:`（Linux 文件名允许）
/// 也不会把层号读错。格式不对 → None（宁可不动，也不误路由到别的会话）。
pub fn split_tree_id(id: &str) -> Option<(usize, &str)> {
    let (lvl, path) = id.split_once(':')?;
    Some((lvl.parse().ok()?, path))
}

/// 树元数据表：key = [`TreeItem`]`.id` = `"{level}:{path}"`（跨嵌套层唯一）。
pub type TreeMeta = std::collections::HashMap<SharedString, TreeRowMeta>;

/// 平铺树行 → 嵌套 [`TreeItem`] + 元数据表（R-GUI2-04 §3.2 改造）。
///
/// 输入是 VM 铺平后的「可见行」序列（DFS 序、depth 缩进）。派生规则：
/// - **文件行跳过**（左栏树只显示目录及子目录——需求 #5；文件不进树）；
/// - 栈式折叠：行 depth > 栈顶 depth → 作为栈顶节点 child；
/// - `expanded=false` 的目录行不含子（懒展开——VM 只铺已展开子树）；
/// - `id` = 节点路径（VM `TreeRow.path`，树内唯一）；
/// - 返回 `(items, meta)`：meta 与 items 同批替换进 [`BrowseHold`]（滞后
///   一帧会出现图标闪错——旧 id 查不到 meta 回落文件图标，可接受一帧）。
pub fn tree_items_from_nav(rows: &[TreeRow]) -> (Vec<TreeItem>, TreeMeta) {
    /// 栈帧：节点 + 深度（折叠用；depth 与 TreeRow.depth 同标尺）
    struct Frame {
        item: TreeItem,
        depth: usize,
    }

    let mut roots: Vec<TreeItem> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut meta: TreeMeta = std::collections::HashMap::new();

    for row in rows {
        // 目录进树；**容器（内层压缩包）也进树**——它要当子目录展示，
        // 且必须可展开（展开后挂的是内层会话的行）。其余文件不进树（§3.2）。
        if !row.is_dir && !row.is_container {
            continue;
        }
        // id 前缀层号：嵌套展示下同名目录可能内外各有一份（`docs/`），
        // 光靠 path 做 id 会撞车（widget 的展开/选中态与 meta 都按 id 存）。
        let id = SharedString::from(tree_item_id(row.level, &row.path));
        meta.insert(
            id.clone(),
            TreeRowMeta {
                path: row.path.clone(),
                is_dir: row.is_dir,
                is_container: row.is_container,
            },
        );
        // 懒展开语义：VM 快照只铺已展开目录的子行；TreeItem 内部 expanded 位
        // 从 TreeRow.expanded 传播（VM 是唯一事实源——键盘/点击 toggle 触发
        // TreeEvent → on_tree_toggle 回写 VM，下次快照 set_items 同步）。
        let item = TreeItem::new(id, row.name.clone()).expanded(row.expanded);
        // 弹栈：深度不高于当前行 → 闭合，回挂父链
        while let Some(top) = stack.last() {
            if top.depth < row.depth {
                break;
            }
            let frame = match stack.pop() {
                Some(frame) => frame,
                None => break,
            };
            match stack.last_mut() {
                Some(parent) => {
                    let child = parent.item.clone().child(frame.item);
                    parent.item = child;
                }
                None => {
                    roots.push(frame.item);
                }
            }
        }
        stack.push(Frame {
            item,
            depth: row.depth,
        });
    }
    while let Some(frame) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => {
                let child = parent.item.clone().child(frame.item);
                parent.item = child;
            }
            None => {
                roots.push(frame.item);
            }
        }
    }
    (roots, meta)
}

/// 右栏 `..` 行可见性（根目录无 `..`；非根目录恒给——T-G53 表格首行约定）。
///
/// 渲染层从快照判定（不持 VM）：`crumbs.len() > 1` ⇔ 非档案根
/// （crumbs[0] = 档案根段，见 `crumbs_from_current_dir`）。
pub fn show_parent_row(crumbs: &[Crumb]) -> bool {
    crumbs.len() > 1
}
