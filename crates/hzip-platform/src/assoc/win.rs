//! Windows 文件关联后端（注册表）：ProgId 键树 + `.<ext>` 默认值 + 打开方式列表。
//!
//! 本文件是跨平台门面 [`super`] 的 Windows 实现。
//!
//! # 注册表布局（只写 HKCU ⇒ 免管理员）
//!
//! ```text
//! HKCU\Software\Classes\<prog_id>
//!     (Default) = "hzip 压缩档案"
//!     DefaultIcon = "<exe>,0"
//!     shell\open\command = "\"<exe>\" --open \"%1\""
//!     PrevDefault\<ext> = <被本程序顶掉的旧 ProgId>（空串 = 旧值本就不存在）
//! HKCU\Software\Classes\.<ext>                      (Default) = <prog_id>   ← 双击落点
//! HKCU\Software\Classes\.<ext>\OpenWithProgids      <prog_id> = ""         ← 「打开方式」列表
//! HKCU\Software\hzip\Capabilities\FileAssociations  .<ext> = <prog_id>
//! HKCU\Software\RegisteredApplications                hzip = "Software\\hzip\\Capabilities"
//! ```
//!
//! `PrevDefault` 是**顶替备份**：写 `.<ext>\(Default)` 会覆盖别人的默认程序，
//! 解除关联时按它还原（还原后若旧值本就不存在则删掉该值，让系统回落 HKLM 层）。
//!
//! `Capabilities` + `RegisteredApplications` 让 hzip 出现在「设置 → 默认应用 →
//! 按应用设置默认值」里（用户可在那儿一次性改全部扩展名），也是
//! `IApplicationAssociationRegistration::SetAppAsDefault` 的前置——但本模块**不调**
//! 它：Win8 起该 API 在「当前默认程序的发行者不同」时返回 `E_ACCESSDENIED`
//! （MSDN Remarks 原文），从 Bandizip/7-Zip 手里抢默认注定失败。
//!
//! # 双击到底归谁（系统的解析优先级）
//!
//! ```text
//! ① FileExts\.<ext>\UserChoice\ProgId        ← Win8+ 用户显式选过，最高优先，哈希保护
//! ② HKCU\Software\Classes\.<ext>\(Default)  ← 本模块写的位（无 ① 时生效）
//! ③ HKLM\Software\Classes\.<ext>\(Default)  ← 机器级安装（HKLM 与 HKCU 是合并视图）
//! ```
//!
//! ①存在时**任何程序都写不进去**（哈希由系统内部算；SetUserFTA 那类改哈希的玩法
//! 自 Win10 21H1 起失效），`SetAppAsDefault` 发行者不同即 `E_ACCESSDENIED`。
//! 故本模块的策略是：该写的都写（②③ 备好）→ [`ext_state`] 回读实况、界面如实告知
//! `pinned` → 用户用一次 [`open_with_dialog`]（「打开方式」对话框）点掉那一格。

use super::api::{
    AssocSpec, ELEVATE_FLAG, ExtAssocState, PROG_ID, PROG_ID_DESC, format_verb_command,
};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

const CLASSES: &str = "Software\\Classes";
/// 顶替备份子键（值名 = 扩展名，值 = 被顶掉的旧 ProgId；空串 = 旧值不存在）。
const PREV_SUBKEY: &str = "PrevDefault";
/// 「打开方式」列表清理的备份子键（值名 = 扩展名，值 = 被摘掉的 ProgId 名单）。
///
/// 见 [`prune_open_with`]：Windows 解析「没人显式选过」的扩展名时，会把**每用户**
/// `FileExts\.<ext>\OpenWithProgids` 里的其它程序当作当前默认（实测 2026-09-29），
/// 压过我们写进 `HKCU\Software\Classes\.<ext>` 的默认值。故经典模式要把这些竞争者
/// 摘掉（备份在此，解除关联时原样还回去）。
const PREV_OPEN_WITH_SUBKEY: &str = "PrevOpenWith";
/// 本程序在 `RegisteredApplications` 里的名字（也用于 `Capabilities` 路径）。
const REGISTERED_APP_NAME: &str = "hzip";
/// `Capabilities` 键路径（HKCU\Software\<REGISTERED_APP_NAME>\Capabilities）。
const CAPABILITIES_PATH: &str = "Software\\hzip\\Capabilities";
/// 用户显式默认（Win8+）——`FileExts\.<ext>\UserChoice`。
const FILE_EXTS: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts";

fn open_classes() -> Result<RegKey, String> {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(CLASSES, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("打开 HKCU\\{CLASSES} 失败: {e}"))
}

/// 读某键的默认值（空串/无值 → None）。
fn default_value(key: &RegKey) -> Option<String> {
    key.get_value::<String, _>("")
        .ok()
        .filter(|s| !s.is_empty())
}

/// 读 `HKCU\Software\Classes\.<ext>` 的默认值（我们的生效位）。
fn read_user_default(classes: &RegKey, ext: &str) -> Option<String> {
    classes
        .open_subkey_with_flags(format!(".{ext}"), KEY_READ)
        .ok()
        .and_then(|k| default_value(&k))
}

/// 读顶替备份表（值名 = ext；空串 = 旧值本就不存在）。
///
/// 返回 `None` = 无备份子键（从未顶替过）；`Some(map)` 里缺某 ext = 该 ext
/// 未顶替过（与「顶替了但旧值不存在」区分——后者是 `Some("")`）。
fn read_prev_defaults(
    classes: &RegKey,
    prog_id: &str,
) -> Option<std::collections::HashMap<String, String>> {
    let key = classes
        .open_subkey_with_flags(format!("{prog_id}\\{PREV_SUBKEY}"), KEY_READ)
        .ok()?;
    let mut map = std::collections::HashMap::new();
    for (name, _) in key.enum_values().filter_map(|v| v.ok()) {
        map.insert(
            name.clone(),
            key.get_value::<String, _>(&name).unwrap_or_default(),
        );
    }
    Some(map)
}

/// 清理某扩展 OpenWithProgids 下的 prog_id 值。
fn remove_open_with(classes: &RegKey, ext: &str, prog_id: &str, errors: &mut Vec<String>) {
    let path = format!(r".{ext}\OpenWithProgids");
    match classes.open_subkey_with_flags(&path, KEY_READ | KEY_WRITE) {
        Ok(k) => {
            if let Err(e) = k.delete_value(prog_id)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                errors.push(format!("删除 {path}\\{prog_id} 失败: {e}"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => errors.push(format!("打开 {path} 失败: {e}")),
    }
}

/// 注册表清理（apply 幂等前置 + remove 共用）。
fn clear_association(prog_id: &str, extensions: &[String]) -> Vec<String> {
    let mut errors = Vec::new();
    let classes = match open_classes() {
        Ok(k) => k,
        Err(e) => return vec![e],
    };
    // ProgId 键整树删除
    if let Err(e) = classes.delete_subkey_all(prog_id)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!("删除 ProgId {prog_id} 失败: {e}"));
    }
    // 每扩展 OpenWithProgids 移除 prog_id 值
    for ext in extensions {
        remove_open_with(&classes, ext, prog_id, &mut errors);
    }
    // 右键菜单 verb 不在此清理：归 `context_menu::backend().uninstall()`——
    // 二者作用域不同（本模块只碰 ProgId 与 OpenWithProgids），混在一起会让
    // 「解除关联」误删用户仍想保留的右键菜单项，反之亦然。
    errors
}

/// 写 ProgId 根 + `shell\open`（双击打开）+ `DefaultIcon`。
fn write_prog_id(classes: &RegKey, spec: &AssocSpec, errors: &mut Vec<String>) {
    let exe = spec.exe_path.to_string_lossy();
    let prog = match classes.create_subkey(spec.prog_id) {
        Ok((k, _)) => k,
        Err(e) => {
            errors.push(format!("创建 {} 失败: {e}", spec.prog_id));
            return;
        }
    };
    if let Err(e) = prog.set_value("", &PROG_ID_DESC) {
        errors.push(format!("写 ProgId 描述失败: {e}"));
    }
    // DefaultIcon：Explorer 用它画这类文件的图标。
    //
    // **必须 `create_subkey`**：ProgId 刚被 `clear_association` 清过（子键不存在），
    // 而 `open_subkey_with_flags` 只打开、不创建 ⇒ 旧写法静默跳过，`DefaultIcon`
    // 永远写不出来 ⇒ 文件图标退化成「未知类型」⇔ 用户看到的就是「这个格式没关联上」
    // （`.zip` 那类 Windows 自带图标的格式反而显得正常——2026-09-29 用户报障
    //「zip 关联上了、7z/tgz 没有」的真因之一）。
    match prog.create_subkey("DefaultIcon") {
        Ok((icon, _)) => {
            if let Err(e) = icon.set_value("", &format!("{exe},0")) {
                errors.push(format!("写 DefaultIcon 失败: {e}"));
            }
        }
        Err(e) => errors.push(format!("创建 DefaultIcon 失败: {e}")),
    }
    let open_cmd = format_verb_command(&exe, &["--open", "%1"]);
    if let Err(e) = write_shell_command(&prog, "open", &open_cmd) {
        errors.push(e);
    }
    // `shell\open\FriendlyAppName`：「打开方式」对话框里显示的**程序名**。
    // 不写它，对话框只能拿 ProgId 的默认值（本机 Bandizip 那种「ZIP 压缩文件」
    // 类型描述）当名字——用户要在一串类型描述里找程序名。
    if let Ok(open) = prog.open_subkey_with_flags(r"shell\open", KEY_WRITE)
        && let Err(e) = open.set_value("FriendlyAppName", &REGISTERED_APP_NAME)
    {
        errors.push(format!("写 shell\\open\\FriendlyAppName 失败: {e}"));
    }
}

/// 写 `shell\<verb>\command`（本模块只用 `open`；浏览器的 verb 归 `context_menu`）。
fn write_shell_command(prog: &RegKey, verb: &str, cmd: &str) -> Result<(), String> {
    // 子键只拼一次：错误文案里的完整键路径由它派生，注册与报错指向同一字符串。
    let shell_key = format!(r"shell\{verb}");
    let shell = prog
        .create_subkey(&shell_key)
        .and_then(|(k, _)| k.create_subkey("command"))
        .map(|(k, _)| k)
        .map_err(|e| format!("创建 {shell_key}\\command 失败: {e}"))?;
    shell
        .set_value("", &cmd)
        .map_err(|e| format!("写 {shell_key}\\command 失败: {e}"))
}

/// 每扩展 OpenWithProgids 追加 prog_id（只追加不独占，不写默认值）。
fn write_open_with(classes: &RegKey, ext: &str, prog_id: &str, errors: &mut Vec<String>) {
    // 子键只拼一次：错误文案里的完整键路径由它派生，注册与报错指向同一字符串。
    let sub = format!(r".{ext}\OpenWithProgids");
    match classes.create_subkey(&sub) {
        Ok((k, _)) => {
            if let Err(e) = k.set_value(prog_id, &"") {
                errors.push(format!("写 {CLASSES}\\{sub}\\{prog_id} 失败: {e}"));
            }
        }
        Err(e) => errors.push(format!("创建 {CLASSES}\\{sub} 失败: {e}")),
    }
}

/// 每用户「打开方式」列表（`FileExts\.<ext>\OpenWithProgids`）——**Windows 实际拿它
/// 当默认**，故经典模式必须在这里也只剩我们一个候选。
///
/// # 实测（2026-09-29，Win10 19045）
///
/// 本机 `.7z`/`.tgz`/`.tar` 的 `HKCU\Software\Classes\.<ext>` 与 `HKLM` 都已是
/// `hzip.Archive`、`UserChoice` 也不存在，但资源管理器与 `ShellExecute` 仍把
/// **Bandizip** 当默认（双击打不开、类型显示「7Z 压缩文件」）；把该列表里的
/// `Bandizip.<ext>` 摘掉并通知 shell 后，**立刻**归我们。
/// （`.zip` 之所以「看起来正常」，是因为它列表里 Windows 自带的 `CompressedFolder`
/// 打头，解析落到了我们写的 Classes 默认值上。）
///
/// # 做法
///
/// 摘掉除 `prog_id` 之外的条目（名单**备份**到 `<ProgId>\PrevOpenWith\<ext>`，
/// 解除关联时还原），并保证 `prog_id` 在里面。
fn prune_open_with(prog: &RegKey, ext: &str, prog_id: &str, errors: &mut Vec<String>) {
    let path = format!("{FILE_EXTS}\\.{ext}\\OpenWithProgids");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = match hkcu.create_subkey(&path) {
        Ok((k, _)) => k,
        Err(e) => {
            errors.push(format!("创建 {path} 失败: {e}"));
            return;
        }
    };
    let backup = match prog.create_subkey(PREV_OPEN_WITH_SUBKEY) {
        Ok((k, _)) => k,
        Err(e) => {
            errors.push(format!("创建 {PREV_OPEN_WITH_SUBKEY} 失败: {e}"));
            return;
        }
    };
    let mut removed: Vec<String> = Vec::new();
    for (name, _) in key.enum_values().filter_map(|v| v.ok()) {
        if name == prog_id {
            continue;
        }
        removed.push(name.clone());
        if let Err(e) = key.delete_value(&name) {
            errors.push(format!("删除 {path}\\{name} 失败: {e}"));
        }
    }
    if !removed.is_empty() {
        let joined = removed.join(",");
        if let Err(e) = backup.set_value(ext, &joined) {
            errors.push(format!("写 {PREV_OPEN_WITH_SUBKEY}\\{ext} 失败: {e}"));
        }
        tracing::info!(ext, removed = %joined, "经典模式：已摘掉每用户「打开方式」列表里的竞争者");
    }
    if let Err(e) = key.set_value(prog_id, &"") {
        errors.push(format!("写 {path}\\{prog_id} 失败: {e}"));
    }
}

/// 还原 [`prune_open_with`] 摘掉的条目（解除关联时调用）。
fn restore_open_with(prog: &RegKey, ext: &str, prog_id: &str, errors: &mut Vec<String>) {
    let path = format!("{FILE_EXTS}\\.{ext}\\OpenWithProgids");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let names: Vec<String> = prog
        .open_subkey_with_flags(PREV_OPEN_WITH_SUBKEY, KEY_READ)
        .ok()
        .and_then(|k| k.get_value::<String, _>(ext).ok())
        .map(|s| {
            s.split(',')
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    match hkcu.create_subkey(&path) {
        Ok((key, _)) => {
            for name in names {
                if let Err(e) = key.set_value(&name, &"") {
                    errors.push(format!("还原 {path}\\{name} 失败: {e}"));
                }
            }
            if let Err(e) = key.delete_value(prog_id)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                errors.push(format!("删除 {path}\\{prog_id} 失败: {e}"));
            }
        }
        Err(e) => errors.push(format!("打开 {path} 失败: {e}")),
    }
}

/// 写 `.<ext>\(Default) = prog_id`（双击落点）+ 首次顶替时备份旧值。
///
/// `prev`：本次 apply 之前读到的备份表（`clear_association` 会连备份子键
/// 一起删掉，故必须在删除前读出、写回；**不**用当前值重新备份——二次 apply
/// 时当前值已是我们自己，重新备份会把 `hzip.Archive` 记成「旧值」，
/// 解除关联后就留下一个指向已删 ProgId 的死默认）。
fn write_ext_defaults(
    classes: &RegKey,
    spec: &AssocSpec,
    prev: Option<&std::collections::HashMap<String, String>>,
    errors: &mut Vec<String>,
) {
    let prog = match classes.open_subkey_with_flags(spec.prog_id, KEY_READ | KEY_WRITE) {
        Ok(k) => k,
        Err(e) => {
            errors.push(format!("打开 {} 失败: {e}", spec.prog_id));
            return;
        }
    };
    let backup = match prog.create_subkey(PREV_SUBKEY) {
        Ok((k, _)) => k,
        Err(e) => {
            errors.push(format!("创建 {PREV_SUBKEY} 失败: {e}"));
            return;
        }
    };
    // `clear_association` 连备份子键一起删了，故先把**既有备份全部重放**回来：
    // 那张表是「我们顶替过哪些扩展名」的唯一台账（解除关联据此清理，含用户后来
    // 取消勾选、本次不在 spec 里的格式——丢掉它们会在系统里留下指向我们的死默认）。
    if let Some(prev_map) = prev {
        for (ext, old) in prev_map {
            if let Err(e) = backup.set_value(ext, old) {
                errors.push(format!("重放 {PREV_SUBKEY}\\{ext} 失败: {e}"));
            }
        }
    }
    for ext in &spec.extensions {
        // 旧值：已备份过就沿用，否则记录「顶替前」的值（None → 空串 = 本不存在）
        let recorded = match prev.and_then(|m| m.get(ext)) {
            Some(v) => v.clone(),
            None => read_user_default(classes, ext).unwrap_or_default(),
        };
        if let Err(e) = backup.set_value(ext, &recorded) {
            errors.push(format!("写 {PREV_SUBKEY}\\{ext} 失败: {e}"));
        }
        match classes.create_subkey(format!(".{ext}")) {
            Ok((k, _)) => {
                if let Err(e) = k.set_value("", &spec.prog_id) {
                    errors.push(format!("写 .{ext} 默认值失败: {e}"));
                }
            }
            Err(e) => errors.push(format!("创建 .{ext} 失败: {e}")),
        }
    }
}

/// 清掉某扩展 `UserChoice` 层的失败原因（提权与否在此分流）。
enum ClearOutcome {
    /// 已清（含「本来就没有」）
    Done,
    /// 被 DACL 拒（极少数机器/策略额外 Deny 了 DELETE）⇒ 需提权兜底
    Denied,
    /// 其它失败（如实上报）
    Failed(String),
}

/// 删掉某扩展的 `UserChoice` 层（**经典模式**：应用关联时直接接管默认）。
///
/// # 为什么「直接删键」而不先打开探一下
///
/// `UserChoice` 的 DACL 里有一条 **`Deny <当前用户> SetValue`**（系统加的保护），
/// 而删键走的是父键上的 **DELETE** 权限（DACL 里 `Allow <当前用户> FullControl`
/// 覆盖）⇒ **删键不需要管理员**。
///
/// 但若像早期实现那样先 `open_subkey_with_flags(path, KEY_READ | KEY_WRITE)`
/// 探存在性，那个 `KEY_WRITE` 请求会**当场**被 `Deny SetValue` 拒掉
/// （`os error 5 拒绝访问`）——用户看到的就是「点了应用关联报错没有权限」。
/// 本机实测（`FileExts\.zip\UserChoice`，2026-09-29）：
/// `KEY_READ` 可开 ✓ / `KEY_READ|KEY_WRITE` → 拒绝访问 ✗ / `delete_subkey_all` ✓。
///
/// 故本函数**只在父键上操作**，绝不带写标志打开 `UserChoice` 本身。
fn delete_user_choice(parent: &RegKey, ext: &str) -> ClearOutcome {
    let path = format!("{FILE_EXTS}\\.{ext}\\UserChoice");
    match parent.delete_subkey_all(&path) {
        Ok(()) => {
            tracing::info!(
                ext,
                "经典模式：已清除 FileExts 的用户显式选择（UserChoice）"
            );
            ClearOutcome::Done
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClearOutcome::Done,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => ClearOutcome::Denied,
        Err(e) => ClearOutcome::Failed(format!("清除 {path} 失败: {e}")),
    }
}

/// 批量清理 `UserChoice`（**提权子进程的入口**）：只删键、不写任何值。
///
/// 由 `hzip-gui --hzip-elevate-clear-user-choice=<ext,ext,…>` 调用（见 `main.rs`），
/// 供 [`apply_association`] 在遇到 DACL 额外拒绝 DELETE 的机器上兜底提权。
pub(super) fn clear_user_choice_batch(exts: &[String]) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut errors = Vec::new();
    for ext in exts {
        match delete_user_choice(&hkcu, ext) {
            ClearOutcome::Done => {}
            ClearOutcome::Denied => {
                errors.push(format!("清除 .{ext} 的 UserChoice 被拒绝（权限不足）"))
            }
            ClearOutcome::Failed(m) => errors.push(m),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// 提权兜底：`ShellExecuteW("runas", 本程序, 内部开关)`，用户在 UAC 上点一次确认。
///
/// 仅当直删被 DACL 拒绝时才走（正常机器永远不需要）。用户取消 UAC ⇒
/// `ShellExecute` 返回值 ≤ 32 ⇒ 如实上报（状态行显示失败原因，**不当成功**）。
fn relaunch_elevated(exe: &std::path::Path, exts: &[String]) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if std::env::var_os("HZIP_DRY_RUN").is_some() {
        tracing::info!(?exts, "HZIP_DRY_RUN=1：跳过 UAC 提权子进程");
        return Ok(());
    }
    let params = format!("{ELEVATE_FLAG}{}", exts.join(","));
    let op = crate::winutil::wide_str("runas");
    let file = crate::winutil::wide_str(&exe.to_string_lossy());
    let params_w = crate::winutil::wide_str(&params);
    // SAFETY: 三个字符串活过调用；ShellExecuteW 不持有它们。
    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            params_w.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if ret as isize <= 32 {
        Err(format!(
            "需要管理员权限才能清除这些格式的系统默认（UAC 被取消或失败，ShellExecute={ret:?}）"
        ))
    } else {
        tracing::info!(?exts, "已提权清理 UserChoice");
        Ok(())
    }
}

/// 按备份还原 `.<ext>\(Default)`（**只在当前值仍是本程序时**还原——用户
/// 期间自己改过默认程序就尊重用户的选择，别把人家改回去）。
fn restore_ext_defaults(
    classes: &RegKey,
    prog_id: &str,
    prev: Option<&std::collections::HashMap<String, String>>,
    extensions: &[String],
    errors: &mut Vec<String>,
) {
    for ext in extensions {
        let current = read_user_default(classes, ext);
        if current.as_deref() != Some(prog_id) {
            continue; // 不是我们写的：不动
        }
        let restored = prev.and_then(|m| m.get(ext)).cloned().unwrap_or_default();
        match classes.open_subkey_with_flags(format!(".{ext}"), KEY_WRITE) {
            Ok(k) => {
                let r = if restored.is_empty() {
                    // 旧值本不存在（或从未备份）→ 删掉，让系统回落到 HKLM 层
                    match k.delete_value("") {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                            Err(format!("删除 .{ext} 默认值失败: {e}"))
                        }
                        _ => Ok(()),
                    }
                } else {
                    k.set_value("", &restored)
                        .map_err(|e| format!("还原 .{ext} 默认值失败: {e}"))
                };
                if let Err(e) = r {
                    errors.push(e);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => errors.push(format!("打开 .{ext} 失败: {e}")),
        }
    }
}

/// 注册 `RegisteredApplications` + `Capabilities`（HKCU，免管理员）。
///
/// 让 hzip 出现在「设置 → 默认应用 → 按应用设置默认值」，并让
/// `SetAppAsDefault` 至少认得这个应用名（发行者不匹配时仍会
/// `E_ACCESSDENIED`，那是系统策略，不是本函数的问题）。
fn write_registered_app(spec: &AssocSpec, errors: &mut Vec<String>) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let caps = hkcu.create_subkey(CAPABILITIES_PATH).map(|(k, _)| k);
    let caps = match caps {
        Ok(k) => k,
        Err(e) => {
            errors.push(format!("创建 {CAPABILITIES_PATH} 失败: {e}"));
            return;
        }
    };
    let exe = spec.exe_path.to_string_lossy().to_string();
    if let Err(e) = caps.set_value("ApplicationName", &REGISTERED_APP_NAME) {
        errors.push(format!("写 ApplicationName 失败: {e}"));
    }
    if let Err(e) = caps.set_value("ApplicationDescription", &PROG_ID_DESC) {
        errors.push(format!("写 ApplicationDescription 失败: {e}"));
    }
    if let Err(e) = caps.set_value("ApplicationIcon", &format!("{exe},0")) {
        errors.push(format!("写 ApplicationIcon 失败: {e}"));
    }
    match caps.create_subkey("FileAssociations") {
        Ok((fa, _)) => {
            for ext in &spec.extensions {
                if let Err(e) = fa.set_value(format!(".{ext}"), &spec.prog_id) {
                    errors.push(format!("写 FileAssociations\\.{ext} 失败: {e}"));
                }
            }
        }
        Err(e) => errors.push(format!("创建 FileAssociations 失败: {e}")),
    }
    match hkcu.create_subkey("Software\\RegisteredApplications") {
        Ok((ra, _)) => {
            if let Err(e) = ra.set_value(REGISTERED_APP_NAME, &CAPABILITIES_PATH) {
                errors.push(format!("写 RegisteredApplications 失败: {e}"));
            }
        }
        Err(e) => errors.push(format!("打开 RegisteredApplications 失败: {e}")),
    }
}

/// 注销 `RegisteredApplications` 值 + `Capabilities` 子树（不动 `HKCU\Software\hzip` 本身）。
fn remove_registered_app(errors: &mut Vec<String>) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(ra) = hkcu.open_subkey_with_flags("Software\\RegisteredApplications", KEY_WRITE)
        && let Err(e) = ra.delete_value(REGISTERED_APP_NAME)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!(
            "删除 RegisteredApplications\\{REGISTERED_APP_NAME} 失败: {e}"
        ));
    }
    if let Ok(hzip) = hkcu.open_subkey_with_flags("Software\\hzip", KEY_WRITE)
        && let Err(e) = hzip.delete_subkey_all("Capabilities")
        && e.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!("删除 Capabilities 失败: {e}"));
    }
}

pub(super) fn apply_association(spec: &AssocSpec) -> Result<(), String> {
    // 经典模式（用户口径 2026-09-29，现在是**唯一**行为）：先清掉系统的「用户
    // 显式选择」层（`FileExts\.<ext>\UserChoice`）——它压过下面所有写入，不清掉
    // 的话 `.<ext>\(Default)` 写了也不生效（「点了应用、双击还是老程序」）。
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut classic_errors = Vec::new();
    let mut denied: Vec<String> = Vec::new();
    for ext in &spec.extensions {
        match delete_user_choice(&hkcu, ext) {
            ClearOutcome::Done => {}
            ClearOutcome::Denied => denied.push(ext.clone()),
            ClearOutcome::Failed(m) => classic_errors.push(m),
        }
    }
    if !denied.is_empty()
        && let Err(e) = relaunch_elevated(&spec.exe_path, &denied)
    {
        classic_errors.push(e);
    }
    // 幂等覆盖：先清旧 verbs 再写新勾选集。备份子键挂在 ProgId 下、会被
    // 一起清掉，故先读出（见 `write_ext_defaults` 的 prev 参数说明）。
    let prev = open_classes()
        .ok()
        .and_then(|c| read_prev_defaults(&c, spec.prog_id));
    let mut errors = clear_association(spec.prog_id, &spec.extensions);
    let classes = match open_classes() {
        Ok(k) => k,
        Err(e) => return Err(format!("{e}; {}", errors.join("; "))),
    };
    write_prog_id(&classes, spec, &mut errors);
    write_ext_defaults(&classes, spec, prev.as_ref(), &mut errors);
    // 每用户「打开方式」列表：Windows 拿它当默认（压过 Classes），必须只剩我们
    if let Ok(prog) = classes.open_subkey_with_flags(spec.prog_id, KEY_READ | KEY_WRITE) {
        for ext in &spec.extensions {
            prune_open_with(&prog, ext, spec.prog_id, &mut errors);
        }
    }
    for ext in &spec.extensions {
        write_open_with(&classes, ext, spec.prog_id, &mut errors);
    }
    write_registered_app(spec, &mut errors);
    errors.splice(0..0, classic_errors);
    // Explorer 不会自己发现关联变了：发一条 `SHCNE_ASSOCCHANGED`，否则要等
    // 注销/重启才看得到效果。先做**线程级 COM 初始化**——本函数跑在
    // `std::thread::spawn` 的裸线程上、没有套间，而 shell 调用在无套间线程上可能
    // 静默失效（「写了注册表但资源管理器没反应」的常见成因）。
    ensure_com_initialized();
    crate::context_menu::win::notify_shell();
    refresh_shell_caches();
    if errors.is_empty() {
        tracing::info!(
            prog_id = spec.prog_id,
            exts = spec.extensions.len(),
            "文件关联已写入 HKCU"
        );
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// 让资源管理器**刷新关联缓存**（best-effort）。
///
/// `SHChangeNotify` 之后，Explorer 的类型/图标缓存有时仍是旧的（用户报障
/// 2026-09-29：注册表已对、资源管理器重启后仍显示旧程序）。`ie4uinit.exe -show`
/// 是系统自带的缓存刷新入口（安装程序普遍这么用）——不弹窗、不需管理员，
/// 失败只记日志（多数机器其实用不到）。
fn refresh_shell_caches() {
    if std::env::var_os("HZIP_DRY_RUN").is_some() {
        tracing::info!("HZIP_DRY_RUN=1：跳过 ie4uinit 缓存刷新");
        return;
    }
    match std::process::Command::new("ie4uinit.exe")
        .arg("-show")
        .status()
    {
        Ok(s) if s.success() => tracing::info!("已请求刷新 shell 关联/图标缓存（ie4uinit -show）"),
        Ok(s) => tracing::warn!(code = ?s.code(), "ie4uinit 返回非零（忽略）"),
        Err(e) => tracing::info!("未调用 ie4uinit（{e}；忽略）"),
    }
}

/// 当前线程的 COM 套间初始化（幂等；仅本线程首次生效）。
///
/// 本后端的入口都跑在 `std::thread::spawn` 的裸线程上（`dispatch.rs` 后台段），
/// 而 `SHChangeNotify` 这类 shell 调用在**无套间**线程上可能静默失效，故调用前
/// 先补这一步（与 `crate::context_menu::win` 同款做法）。
fn ensure_com_initialized() {
    use std::cell::Cell;
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    thread_local! {
        static DONE: Cell<bool> = const { Cell::new(false) };
    }
    DONE.with(|d| {
        if !d.get() {
            // SAFETY: 仅当前线程首次调用；返回值忽略（重复初始化返回 S_FALSE、
            // RPC_E_CHANGED_MODE 表示套间已是别的模式——都不影响本用途）。
            unsafe {
                CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
            }
            d.set(true);
        }
    });
}

pub(super) fn remove_association(spec: &AssocSpec) -> Result<(), String> {
    let prog_id = spec.prog_id;
    // 先还原 `.ext` 默认值（需读备份），再清键树
    let mut errors = Vec::new();
    if let Ok(classes) = open_classes() {
        let prev = read_prev_defaults(&classes, prog_id);
        // 清理范围 = 当前勾选集 **∪ 顶替台账**：用户可能先取消勾选、再点「解除
        // 关联」——只看勾选集会把此前接管过的格式漏在系统里（双击仍归我们，用户
        // 以为「解除了却没解除」）。台账（`PrevDefault`）记着我们顶替过的每一个
        // 扩展名，是「清干净」的唯一依据。
        let mut all: Vec<String> = spec.extensions.clone();
        if let Some(map) = &prev {
            for ext in map.keys() {
                if !all.contains(ext) {
                    all.push(ext.clone());
                }
            }
        }
        restore_ext_defaults(&classes, prog_id, prev.as_ref(), &all, &mut errors);
        if let Ok(prog) = classes.open_subkey_with_flags(prog_id, KEY_READ | KEY_WRITE) {
            for ext in &all {
                restore_open_with(&prog, ext, prog_id, &mut errors);
            }
        }
        errors.extend(clear_association(prog_id, &all));
    }
    remove_registered_app(&mut errors);
    ensure_com_initialized();
    crate::context_menu::win::notify_shell();
    if errors.is_empty() {
        tracing::info!(prog_id, "文件关联已从 HKCU 移除");
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// 读某键的默认值（HKLM 只读；无权限/无键 → None）。
fn machine_default(ext: &str) -> Option<String> {
    RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(format!("Software\\Classes\\.{ext}"), KEY_READ)
        .ok()
        .and_then(|k| default_value(&k))
}

/// ProgId 的**可读程序名**（状态行要显示「仍由 X 打开」的那个 X）。
///
/// 取值顺序（都是 Windows 自己的约定，按「像程序名」的程度排）：
///
/// 1. `<ProgId>\shell\open\FriendlyAppName`——shell 自己在「打开方式」里显示的
///    就是它（实测本机 `Bandizip.zip` → `Bandizip`）；
/// 2. `<ProgId>\Application\ApplicationName`——ProgId 约定的应用名键；
/// 3. `<ProgId>\(Default)`——**类型**名而非程序名，故排在后面：本机
///    `Bandizip.zip` 的默认值是「ZIP 压缩文件」，直接拿去显示就成了
///    「.zip 仍由 ZIP 压缩文件 打开」（用户实测后反馈的坑）；
/// 4. 都没有 → 回落 ProgId 原文（界面自己处理 `None`）。
///
/// HKCU 与 HKLM 是合并视图：先查 HKCU，再查 HKLM。
fn prog_id_name(prog_id: &str) -> Option<String> {
    use winreg::enums::HKEY_LOCAL_MACHINE;

    let classes = format!("Software\\Classes\\{prog_id}");
    let open = format!("{classes}\\shell\\open");
    let app = format!("{classes}\\Application");
    // HKCU 与 HKLM 是合并视图：先查 HKCU，再查 HKLM。
    for hive in [
        RegKey::predef(HKEY_CURRENT_USER),
        RegKey::predef(HKEY_LOCAL_MACHINE),
    ] {
        let read = |path: &str, value: &str| -> Option<String> {
            hive.open_subkey_with_flags(path, KEY_READ)
                .ok()
                .and_then(|k| k.get_value::<String, _>(value).ok())
                .filter(|s| !s.is_empty())
        };
        if let Some(name) = read(&open, "FriendlyAppName") {
            return Some(name);
        }
        if let Some(name) = read(&app, "ApplicationName") {
            return Some(name);
        }
        if let Some(name) = read(&classes, "") {
            return Some(name);
        }
    }
    None
}

/// 双击实况（跨平台泛化模型，见 [`super::ExtAssocState`]）。
///
/// 解析优先级（系统的真实规则，见 [`super`] 模块文档）：`UserChoice`（哈希
/// 保护、程序改不动）> `HKCU\\Software\\Classes\\.<ext>` > `HKLM\\...`。
pub(super) fn ext_state(ext: &str) -> ExtAssocState {
    let user_choice = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(format!("{FILE_EXTS}\\.{ext}\\UserChoice"), KEY_READ)
        .ok()
        .and_then(|k| k.get_value::<String, _>("ProgId").ok())
        .filter(|s| !s.is_empty());
    let user_classes_default = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(CLASSES, KEY_READ)
        .ok()
        .and_then(|c| read_user_default(&c, ext));
    let machine = machine_default(ext);

    // UserChoice 存在 ⇒ 它就是双击落点，且**程序改不动**（pinned）
    let (effective, pinned) = match user_choice {
        Some(id) => (Some(id), true),
        None => (user_classes_default.or(machine), false),
    };
    let effective_name = effective.as_deref().and_then(prog_id_name);
    let ours = effective.as_deref() == Some(PROG_ID);
    ExtAssocState {
        effective,
        effective_name,
        ours,
        pinned,
    }
}

/// 打开「设置 → 默认应用」页（用户在那儿把某个扩展名选成 hzip）。
///
/// # 为什么不是「打开方式」对话框
///
/// ① **Windows 10 起系统不允许程序改默认打开方式**：`SHOpenWithDialog` 的
/// `OAIF_ALLOW_REGISTRATION`/`OAIF_FORCE_REGISTRATION`/`OAIF_HIDE_REGISTRATION`
/// 一律被忽略（MSDN OPENASINFO Remarks 原文：「The Open With dialog box can no
/// longer be used to change the default program used to open a file extension」）；
/// `SetAppAsDefault` 在发行者不同时返回 `E_ACCESSDENIED`；`UserChoice` 是哈希
/// 保护的（SetUserFTA 那类改哈希的玩法自 Win10 21H1 起失效）。**唯一受支持的
/// 途径就是让用户本人去系统设置里点一次。**
/// ② 只传类名（`pcszFile = NULL`）在 Win10 上直接报 `E_INVALIDARG`
/// （实测 0x80070057）——即便不报错，上面 ① 也说明它办不成事。
///
/// 故本函数只做「把用户送到能做这件事的地方」：
/// - Windows 11（build ≥ 22000）：`ms-settings:defaultapps?registeredAppUser=hzip`
///   ——直接落在 hzip 的条目上，本程序注册的**全部**扩展名列在一屏里，逐个点选即可
///   （需 `RegisteredApplications` 里已写 `hzip`，由 [`apply_association`] 负责）；
/// - Windows 10：`ms-settings:defaultapps`（页面里用「按文件类型选择默认应用」）。
///
/// 非阻塞：`ShellExecuteW` 起的是设置应用（另一个进程），本函数立刻返回。
pub(super) fn open_default_apps_settings() -> Result<(), String> {
    let uri = if is_windows_11() {
        "ms-settings:defaultapps?registeredAppUser=hzip"
    } else {
        "ms-settings:defaultapps"
    };
    let op: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let target = crate::winutil::wide_str(uri);
    // SAFETY: 两个字符串都是 NUL 终止的 UTF-16 缓冲；ShellExecuteW 不持有它们。
    // 返回值 ≤ 32 = 失败（HINSTANCE 与错误码共用返回值，这是该 API 的历史约定）。
    let ret = unsafe {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if ret as isize <= 32 {
        Err(format!(
            "打开系统「默认应用」设置失败: ShellExecute={ret:?}"
        ))
    } else {
        tracing::info!("已打开系统默认应用设置（{uri}）");
        Ok(())
    }
}

/// 是否 Windows 11（build ≥ 22000；决定 `ms-settings` 深链是否可用）。
///
/// build 号的读取/解析归 [`crate::context_menu::win::windows_build_number`]
/// （与右键菜单后端同一份实现，见其文档）；读不到 = 非 Win11。
fn is_windows_11() -> bool {
    // build 号读不到（键缺失 / 值非数字）= 非 Win11
    let build = crate::context_menu::win::windows_build_number();
    build.is_some_and(|b| b >= 22000)
}

#[cfg(test)]
#[path = "tests/win.rs"]
mod tests;
