//! 文件列表剪贴板（T-MENU8；B3 降级线）。
//!
//! # Spike 结论（T-MENU3；0.3.4 首查，0.3.8 复核结论未变）
//!
//! | 方向 | 能力 | 证据 |
//! |---|---|---|
//! | 读 CF_HDROP（外部 → hzip） | **可用** | `gpui-pre-windows/src/clipboard.rs::read_from_clipboard` 枚举 `CF_HDROP`（0.3.8 clipboard.rs:115）并经 `DragQueryFileW`（:134）还原路径，产出 `ClipboardEntry::ExternalPaths`（:272） |
//! | 写 CF_HDROP（hzip → 外部） | **不可用** | 同文件 `write_to_clipboard` 对 `ClipboardEntry::ExternalPaths` 是**空臂**（`=> {}`，0.3.8 clipboard.rs:81）——gpui 只写文本/图片 |
//!
//! 故按 README B3 默认降级：**内部剪贴板**（hzip 内复制→粘贴闭环）+
//! **系统剪贴板只读**（外部资源管理器复制 → hzip 粘贴可用）。
//!
//! **写侧已收口（T-MENU8 后续）**：hzip 复制 → 外部粘贴不再挂账——gpui 不写
//! `CF_HDROP`，改由本模块 [`set_system_file_list`] 自绘 win32 写入
//! （`OpenClipboard` + `DROPFILES` 结构 + `CF_UNICODETEXT` 附带文本清单），
//! 与资源管理器「复制」产物等价，外部粘贴可直接消费。读侧维持 gpui 能力 +
//! [`system_file_list`] 适配点不变。
//!
//! # 内部剪贴板语义
//!
//! 进程级单例（`OnceLock<Mutex<Vec<PathBuf>>>`）而非 AppData 字段：剪贴板是
//! **跨窗口共享**的进程资源（T-MENU10 多窗口下必须一致），且复制时机在
//! 后台线程（物化）与粘贴时机在 UI 线程——用进程静态量避免把生命周期
//! 绑到某个 AppData 上。
//!
//! 物化路径本身即载荷（TempArea 稳定路径 `{session}/{sha256前8}/{安全名}`，
//! TC-SEC-015 同源净化）——粘贴侧不需要再知道「来自哪个档案」。
//! 多段子路径（目录递归展开，`sub/b.txt` 形态）经 [`materialize_entry`]
//! 落地，中间目录自动创建。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 进程级内部剪贴板（复制 → 物化路径清单）。
fn internal() -> &'static Mutex<Vec<PathBuf>> {
    static CELL: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(Vec::new()))
}

/// 复制物化区（进程级 TempArea 会话；与内部剪贴板同生命周期）。
///
/// 与编辑会话的 TempArea 语义不同：**不随作用域清理**——复制产出的文件必须
/// 活到粘贴那一刻（可能跨多次 UI 交互）。故用进程静态量持有；退出时由
/// `TempArea` 的孤儿清理兜底（`cleanup_orphans`，02 §5.3.5）。
fn copy_area() -> Option<&'static crate::temp_area::TempArea> {
    static CELL: OnceLock<Option<crate::temp_area::TempArea>> = OnceLock::new();
    CELL.get_or_init(|| crate::temp_area::TempArea::new_session(None).ok())
        .as_ref()
}

/// 物化一个条目流到复制区（`{area}/{sha256前8}/{安全名}`），返回落地路径。
///
/// `None` = 复制区不可用（临时目录创建失败）——调用方据此报错，不静默吞。
pub fn materialize_for_copy(
    archive: &Path,
    entry_name: &str,
    is_dir: bool,
    bytes: impl std::io::Read,
) -> Option<PathBuf> {
    let area = copy_area()?;
    let safe = safe_materialize_name(entry_name, is_dir);
    let name = crate::temp_area::EntryNameSafe::sanitize(&safe).ok()?;
    area.materialize(archive, &name, bytes).ok()
}

/// 写入内部剪贴板（覆盖；空清单 = 清空）。
pub fn set_internal(paths: Vec<PathBuf>) {
    if let Ok(mut g) = internal().lock() {
        *g = paths;
    }
}

/// 读内部剪贴板（克隆；空 = 无内容）。
pub fn internal_paths() -> Vec<PathBuf> {
    internal().lock().map(|g| g.clone()).unwrap_or_default()
}

/// 内部剪贴板是否为空。
pub fn internal_is_empty() -> bool {
    internal().lock().map(|g| g.is_empty()).unwrap_or(true)
}

/// 清空内部剪贴板（测试隔离/退出清理）。
pub fn clear_internal() {
    if let Ok(mut g) = internal().lock() {
        g.clear();
    }
}

/// 读系统剪贴板的文件列表（Windows `CF_HDROP`）。
///
/// gpui 平台层已实现该读取（见模块文档 Spike 结论），但**不暴露公开 API**
/// （`ClipboardItem::entries` 只能经 `App::read_from_clipboard` 拿，而该调用
/// 需要 `&mut App`——粘贴近线可用）。本函数因此**不做平台 IO**，只做
/// 「把平台读到的外部路径交给粘贴侧」的适配点：非 Windows 恒空
/// （B3 降级线；macOS/Linux 的文件列表剪贴板挂账）。
///
/// 调用方（shell 粘贴入口）持 `&mut App` 时走 gpui `cx.read_from_clipboard()`
/// 并自行提取 `ExternalPaths`（GUI 层收口，见 `ui/shell` 粘贴入口）；本函数
/// 供**无 App 上下文**的路径（测试/后台）返回空清单，保证
/// 「粘贴无内容 → 菜单灰」语义成立。
pub fn system_file_list() -> Vec<PathBuf> {
    Vec::new()
}

/// 写系统剪贴板的文件列表（Windows `CF_HDROP`；hzip → 外部方向收口，T-MENU8）。
///
/// gpui 平台层不写 `CF_HDROP`（见模块文档 Spike 结论），故自绘 win32：
/// `DROPFILES` 结构 + 每路径 UTF-16 单 NUL 结尾、列表尾双 NUL，与资源管理器
/// 「复制」产物等价；再附带 `CF_UNICODETEXT`（`\r\n` 连接、尾无换行）作为
/// 不识别文件列表的接收方的文本降级。
///
/// 返回 `false` 的情形（调用方据此降级为「仅内部剪贴板」，不 panic）：
/// 非 Windows（B3 降级线）；`paths` 为空（无意义写入）；
/// `OpenClipboard` 重试 5 次 × 20ms 仍失败（剪贴板被其它进程持有）；
/// `EmptyClipboard`/`GlobalAlloc`/`GlobalLock`/`SetClipboardData` 任一步失败
/// （`tracing::warn!` 带 `io::Error::last_os_error()`）。
///
/// # 内存所有权（win32 语义）
///
/// `SetClipboardData` 成功后 `hMem` 所有权移交系统，**禁止 `GlobalFree`**
/// （系统粘贴或剪贴板被替换时释放）；失败路径则由本函数释放，不泄漏。
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn set_system_file_list(paths: &[PathBuf]) -> bool {
    // 非 Windows：B3 降级线恒 false（与 `system_file_list` 恒空对称）。
    #[cfg(not(windows))]
    {
        false
    }
    #[cfg(windows)]
    {
        set_system_file_list_win(paths)
    }
}

/// Windows 实现（独立函数保证非 Windows 编译单元零平台符号引用）。
#[cfg(windows)]
fn set_system_file_list_win(paths: &[PathBuf]) -> bool {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};

    if paths.is_empty() {
        return false;
    }

    // OpenClipboard 可能因其它进程持有剪贴板而失败（ERROR_ACCESS_DENIED 等），
    // 重试 5 次 × 20ms 消化瞬时竞争。
    let mut opened = false;
    for attempt in 0..5 {
        unsafe {
            // 参数为 HWND（裸指针类型别名），无剪贴板属主传 null。
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
        }
        if attempt < 4 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    if !opened {
        let err = std::io::Error::last_os_error();
        tracing::warn!("OpenClipboard 重试 5 次仍失败: {err}");
        return false;
    }

    // CloseClipboard 由后续所有步骤共用（成功/失败都必须走）。
    let result = write_clipboard_payload(paths);
    unsafe {
        CloseClipboard();
    }
    result
}

/// 打开剪贴板后的载荷写入（`CF_HDROP` + `CF_UNICODETEXT`）。
///
/// 任一步失败：告警 + 释放尚未移交的 `hMem`（成功移交的不可再碰）→ false。
#[cfg(windows)]
fn write_clipboard_payload(paths: &[PathBuf]) -> bool {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
    use windows_sys::Win32::UI::Shell::DROPFILES;

    unsafe {
        if EmptyClipboard() == 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!("EmptyClipboard 失败: {err}");
            return false;
        }

        // ---- CF_HDROP：DROPFILES 头 + 每路径 UTF-16 单 NUL，列表尾双 NUL ----
        use std::os::windows::ffi::OsStrExt;
        let mut utf16: Vec<u16> = Vec::new();
        for p in paths {
            let mut enc: Vec<u16> = p.as_os_str().encode_wide().collect();
            enc.push(0); // 单 NUL 结尾
            utf16.extend_from_slice(&enc);
        }
        utf16.push(0); // 列表尾双 NUL
        let header_size = std::mem::size_of::<DROPFILES>();
        let total = header_size + utf16.len() * std::mem::size_of::<u16>();

        let hdrop = alloc_global(total);
        if hdrop.is_null() {
            let err = std::io::Error::last_os_error();
            tracing::warn!("GlobalAlloc(CF_HDROP, {total}) 失败: {err}");
            return false;
        }
        let locked = GlobalLock(hdrop) as *mut u8;
        if locked.is_null() {
            let err = std::io::Error::last_os_error();
            tracing::warn!("GlobalLock(CF_HDROP) 失败: {err}");
            GlobalFree(hdrop);
            return false;
        }
        // DROPFILES 是 repr(C, packed(1))，逐字段写避免对齐问题。
        let df = locked as *mut DROPFILES;
        (*df).pFiles = header_size as u32; // 文件列表紧随结构体
        (*df).pt = Default::default();
        (*df).fNC = 0;
        (*df).fWide = 1; // UTF-16
        std::ptr::copy_nonoverlapping(
            utf16.as_ptr() as *const u8,
            locked.add(header_size),
            utf16.len() * std::mem::size_of::<u16>(),
        );
        GlobalUnlock(hdrop);
        // 成功 → 所有权归系统（禁 GlobalFree）；失败 → 自行释放防泄漏。
        if SetClipboardData(CF_HDROP as u32, hdrop).is_null() {
            let err = std::io::Error::last_os_error();
            tracing::warn!("SetClipboardData(CF_HDROP) 失败: {err}");
            GlobalFree(hdrop);
            return false;
        }

        // ---- CF_UNICODETEXT：路径 \r\n 连接（尾无换行），文本降级 ----
        let text = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\r\n");
        let mut text16: Vec<u16> = text.encode_utf16().collect();
        text16.push(0); // NUL 结尾
        let text_bytes = text16.len() * std::mem::size_of::<u16>();

        let htext = alloc_global(text_bytes);
        if htext.is_null() {
            let err = std::io::Error::last_os_error();
            tracing::warn!("GlobalAlloc(CF_UNICODETEXT, {text_bytes}) 失败: {err}");
            return false;
        }
        let locked = GlobalLock(htext) as *mut u8;
        if locked.is_null() {
            let err = std::io::Error::last_os_error();
            tracing::warn!("GlobalLock(CF_UNICODETEXT) 失败: {err}");
            GlobalFree(htext);
            return false;
        }
        std::ptr::copy_nonoverlapping(text16.as_ptr() as *const u8, locked, text_bytes);
        GlobalUnlock(htext);
        if SetClipboardData(CF_UNICODETEXT as u32, htext).is_null() {
            let err = std::io::Error::last_os_error();
            tracing::warn!("SetClipboardData(CF_UNICODETEXT) 失败: {err}");
            GlobalFree(htext);
            return false;
        }
        true
    }
}

/// GMEM_MOVEABLE 分配（可移动内存块，剪贴板约定的分配标志）。
#[cfg(windows)]
fn alloc_global(bytes: usize) -> windows_sys::Win32::Foundation::HGLOBAL {
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc};
    unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
}

/// 物化名安全化（条目末段 + TC-SEC-015 净化；目录补 `_dir` 防与同名文件碰撞）。
///
/// 与 core 编辑服务 `entry_safe_name` 同语义（本函数供 GUI 复制路径物化，
/// 不经 core 编辑会话）。
pub fn safe_materialize_name(entry_name: &str, is_dir: bool) -> String {
    let mut base = entry_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(entry_name)
        .to_string();
    if base.is_empty() || base == "." || base == ".." {
        base = "entry".into();
    }
    // 控制字符/穿越片段剥离（与 EntryNameSafe 同判据的更宽容预处理：
    // 物化名只求「可写且不越界」，非法则截断重命名）
    base = base.replace(['\0', '/', '\\'], "_").replace("..", "__");
    if is_dir { format!("{base}_dir") } else { base }
}

/// 物化目标路径（`{root}/{sha256(档案)[..8]}/{安全名}`；TempArea 同构布局）。
///
/// 本函数只**计算**路径不建目录（`TempArea::materialize` 负责落盘）。
pub fn materialize_target(root: &Path, archive: &Path, safe_name: &str) -> PathBuf {
    let hash = short_hash(archive);
    root.join(hash).join(safe_name)
}

/// 物化一个**多段相对路径**条目到复制区（契约 B；目录递归展开 T-MENU8）。
///
/// `rel` 可含 `/` 多段（如 `sub/b.txt`），逐段经 [`safe_materialize_name`]
/// 同源净化；叶子段按 `is_dir` 沿用 `_dir` 后缀防碰撞，中间目录段同样净化
/// 但不加后缀（目录与文件同名碰撞由叶子后缀规则兜底）。落地根复用
/// [`materialize_target`] 同布局：`{copy_area}/{archive_hash8}/…`，中间目录
/// 自动 `fs::create_dir_all`。
///
/// 返回 `None`（调用方据此报错，不静默吞）：
/// 复制区不可用（临时目录创建失败）；`rel` 为空/纯分隔符；
/// 任一段净化后为空（全非法字符段）。
///
/// 目录条目本身不写内容（空目录落地即目录），`bytes` 仅对文件叶消费——
/// 但本函数**总会读尽** `bytes`（目录时丢弃），保证流语义一致（避免调用方
/// 侧残留半读流的歧义）。
pub fn materialize_entry(
    archive: &Path,
    rel: &str,
    is_dir: bool,
    bytes: impl std::io::Read,
) -> Option<PathBuf> {
    let area = copy_area()?;
    let rel = rel.trim_matches(['/', '\\']);
    if rel.is_empty() {
        return None;
    }
    // 逐段净化：中间目录段不加 `_dir` 后缀；叶子段按 is_dir 沿用规则。
    let segs_raw: Vec<&str> = rel.split(['/', '\\']).collect();
    let last = segs_raw.len() - 1;
    let segments: Vec<String> = segs_raw
        .iter()
        .enumerate()
        .map(|(i, seg)| safe_materialize_name(seg, i == last && is_dir))
        .collect();

    // 落地根：{copy_area}/{archive_hash8}/（materialize_target 同布局）。
    let mut target = area.session_dir().join(short_hash(archive));
    for seg in &segments[..segments.len() - 1] {
        target = target.join(seg);
    }
    std::fs::create_dir_all(&target).ok()?;
    let leaf = segments.last()?;
    target = target.join(leaf);

    if is_dir {
        std::fs::create_dir_all(&target).ok()?;
        // 目录无内容可写，但读尽流保持语义一致。
        let mut sink = std::io::sink();
        let mut bytes = bytes;
        let _ = std::io::copy(&mut bytes, &mut sink);
        Some(target)
    } else {
        let mut out = std::fs::File::create(&target).ok()?;
        std::io::copy(&mut { bytes }, &mut out).ok()?;
        Some(target)
    }
}

/// 档案路径短哈希（sha256 前 8 字符；与 TempArea `archive_hash` 同布局）。
fn short_hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..crate::consts::TEMP_ENTRY_HASH_LEN.min(hex.len())].to_owned()
}

#[cfg(test)]
#[path = "tests/clipboard.rs"]
mod tests;
