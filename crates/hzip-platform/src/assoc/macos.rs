//! macOS 文件关联后端（LaunchServices）。
//!
//! # 两件事
//!
//! 1. **注册**：`lsregister -f <AppBundle>`——让系统知道这个包能打开哪些类型。
//!    前提是**发行形态是 `.app` bundle** 且其 `Info.plist` 里声明了
//!    `CFBundleDocumentTypes`（打包产物的事，运行期无法补）。裸二进制
//!    （`cargo run` 出来的 `target/debug/hzip-gui`）没有 bundle ⇒ 明确报错并说明
//!    原因，而不是「点了没反应」。
//! 2. **默认**：写 `~/Library/Preferences/com.apple.LaunchServices/
//!    com.apple.launchservices.secure.plist` 的 `LSHandlers` 数组，元素形如
//!    `{LSHandlerContentTagClass=public.filename-extension; LSHandlerContentTag=zip;
//!    LSHandlerRoleAll=com.hzip.mac}`——按**扩展名**声明（class = filename-extension），
//!    故不需要 UTI 表。这是 LaunchServices 认的经典形式，也是 `duti` 之外的
//!    零依赖写法。
//!
//! # 为什么用 `defaults write` 而不是直接写 plist 文件
//!
//! `cfprefsd` 缓存着这份偏好：直接改文件会被它覆盖回去。`defaults` 走的就是
//! cfprefsd 的正门。代价是它**整体替换** `LSHandlers`——所以我先把现有数组读
//! 回来（`plutil -convert json`）、合并我们的条目、再整段写回：别的程序留在
//! 这份表里的关联不会被我们吃掉（这是本后端最容易出人命的地方，见
//! [`merge_handlers`] 的测试）。
//!
//! # 生效时机
//!
//! LaunchServices 会在下次解析打开方式时读到新表；Finder 的图标/右键可能仍用
//! 缓存（macOS 常见现象，重登录或 `killall Finder` 后一致）。本后端**不**杀
//! 用户进程，只在日志里说明。
//!
//! # 可测性
//!
//! 本机（Windows/Linux）无法运行 macOS 命令，故逻辑被切成两半：
//! **纯函数**（合并/回读/参数拼装）在 `cross-check` 下照常编译并被单元测试
//! 覆盖；**副作用**（`lsregister`/`plutil`/`defaults` 子进程）在
//! `HZIP_DRY_RUN=1` 时只打印不执行（与 `context_menu::macos` 同款约定）。

use std::path::{Path, PathBuf};

use super::{AssocSpec, ExtAssocState};

/// LaunchServices 偏好的域名（`plist` 在 `~/Library/Preferences/` 下同名子目录）。
const DOMAIN: &str = "com.apple.LaunchServices/com.apple.launchservices.secure";

/// `LSHandlers` 键名（数组；每个元素描述「某类型由谁打开」）。
const KEY_HANDLERS: &str = "LSHandlers";

/// 按扩展名声明时用的 tag class。
const TAG_CLASS_EXT: &str = "public.filename-extension";

/// `lsregister` 的绝对路径（`Support/` 里的私有工具，Apple 未公开但长期稳定）。
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// 打包兜底：`com.hzip.mac` 是发行包约定的 bundle id（见 `context_menu::macos::APP_ID`）。
/// 只有读不到 `Info.plist` 的 `CFBundleIdentifier` 时才用它。
const FALLBACK_BUNDLE_ID: &str = "com.hzip.mac";

/// 是否干跑（测试/无 macOS 的开发机评审用）。
fn dry_run() -> bool {
    std::env::var_os("HZIP_DRY_RUN").is_some()
}

// ===================== bundle 探测 =====================

/// 从可执行文件路径推出 `.app` bundle 根（无 → `None`）。
///
/// `…/hzip.app/Contents/MacOS/hzip-gui` → `…/hzip.app`。
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for comp in exe.components() {
        out.push(comp);
        if let std::path::Component::Normal(name) = comp
            && name.to_string_lossy().ends_with(".app")
        {
            return Some(out);
        }
    }
    None
}

/// bundle 的 `CFBundleIdentifier`（读不到 → [`FALLBACK_BUNDLE_ID`]）。
fn bundle_id(bundle: &Path) -> String {
    let plist = bundle.join("Contents/Info.plist");
    let out = run_capture(
        "plutil",
        &["-convert", "json", "-o", "-", &plist.to_string_lossy()],
    );
    let Ok(text) = out else {
        return FALLBACK_BUNDLE_ID.to_string();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("CFBundleIdentifier")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| FALLBACK_BUNDLE_ID.to_string())
}

// ===================== LSHandlers 纯逻辑 =====================

/// 一个 handler 条目（保留未知键——别人的条目可能带我们没见过的键）。
type Handler = serde_json::Map<String, serde_json::Value>;

fn handler_tag(h: &Handler) -> Option<&str> {
    h.get("LSHandlerContentTag").and_then(|v| v.as_str())
}

fn handler_tag_class(h: &Handler) -> Option<&str> {
    h.get("LSHandlerContentTagClass").and_then(|v| v.as_str())
}

fn handler_role(h: &Handler) -> Option<&str> {
    h.get("LSHandlerRoleAll")
        .or_else(|| h.get("LSHandlerRoleViewer"))
        .or_else(|| h.get("LSHandlerRoleEditor"))
        .and_then(|v| v.as_str())
}

/// 是否「按扩展名、且 tag = `ext`」的条目。
fn is_ext_handler(h: &Handler, ext: &str) -> bool {
    handler_tag_class(h) == Some(TAG_CLASS_EXT) && handler_tag(h) == Some(ext)
}

/// 我们的条目（扩展名 + 角色）。
fn our_handler(ext: &str, our_id: &str) -> Handler {
    let mut h = Handler::new();
    h.insert(
        "LSHandlerContentTagClass".into(),
        serde_json::Value::String(TAG_CLASS_EXT.into()),
    );
    h.insert(
        "LSHandlerContentTag".into(),
        serde_json::Value::String(ext.to_string()),
    );
    // RoleAll：所有角色（查看/编辑）都归我们——与「双击归谁」的诉求一致。
    h.insert(
        "LSHandlerRoleAll".into(),
        serde_json::Value::String(our_id.to_string()),
    );
    h
}

/// 合并：认领的扩展名由我们接管（同名旧条目被替换掉），其余条目**原样保留**。
///
/// `LSHandlers` 是「类型 → 处理者」的**列表**，同一扩展名出现多条时以先出现者为准；
/// 故这里既删旧条目又把它顶到最前，避免被别人的残留条目压住。
fn merge_handlers(existing: &[Handler], exts: &[String], our_id: &str) -> Vec<Handler> {
    let mut out: Vec<Handler> = existing
        .iter()
        .filter(|h| !exts.iter().any(|e| is_ext_handler(h, e)))
        .cloned()
        .collect();
    let mut ours: Vec<Handler> = exts.iter().map(|e| our_handler(e, our_id)).collect();
    ours.append(&mut out);
    ours
}

/// 摘除我们的条目（`remove` 用；`exts` 为空表示「凡指向我们的都摘掉」）。
fn strip_handlers(existing: &[Handler], exts: &[String], our_id: &str) -> Vec<Handler> {
    existing
        .iter()
        .filter(|h| {
            let ours_role = handler_role(h) == Some(our_id);
            let ours_ext = exts.is_empty() || exts.iter().any(|e| is_ext_handler(h, e));
            !(ours_role && ours_ext)
        })
        .cloned()
        .collect()
}

/// 「某扩展名当前归谁」的判定（纯函数，供 [`ext_state`] 与测试共用）。
fn state_of(existing: &[Handler], ext: &str, our_id: &str) -> ExtAssocState {
    let effective = existing
        .iter()
        .find(|h| is_ext_handler(h, ext))
        .and_then(handler_role)
        .map(str::to_string);
    let ours = effective.as_deref() == Some(our_id);
    ExtAssocState {
        effective,
        // 角色名/bundle 展示名要问系统（几十毫秒），状态行不强依赖 ⇒ 不查
        effective_name: None,
        ours,
        // LaunchServices 的默认程序是**我们写得了的**（改 LSHandlers 即可）
        pinned: false,
    }
}

/// 旧式 plist 字面量（`defaults write -array` 的参数形式）。
///
/// `{k=v;k2="含 空格";}`；值统一加引号并转义 `\` 与 `"`——`defaults` 的解析器
/// 接受带引号的字符串、拒绝裸 `;`/`=`，统一加引号最省心。
fn handler_literal(h: &Handler) -> String {
    let mut parts: Vec<String> = h
        .iter()
        .map(|(k, v)| {
            let raw = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{k}={}", quote_plist(&raw))
        })
        .collect();
    parts.sort();
    // 老式 plist 字典的惯例是**尾随分号**（`{k=v;}`）：`defaults` 两种都吃，
    // 但带上更贴近 Apple 自家工具的输出，也便于肉眼比对 `defaults read`。
    if parts.is_empty() {
        "{}".to_string()
    } else {
        format!("{{{};}}", parts.join(";"))
    }
}

fn quote_plist(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if matches!(ch, '"' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// `defaults write` 的完整参数（`handlers` 空 → `defaults delete`）。
fn write_args(handlers: &[Handler]) -> Vec<String> {
    if handlers.is_empty() {
        return vec![
            "delete".to_string(),
            DOMAIN.to_string(),
            KEY_HANDLERS.to_string(),
        ];
    }
    let mut args = vec![
        "write".to_string(),
        DOMAIN.to_string(),
        KEY_HANDLERS.to_string(),
        "-array".to_string(),
    ];
    args.extend(handlers.iter().map(handler_literal));
    args
}

// ===================== 副作用 =====================

/// 跑一条命令（干跑时只打印）。成功 = 退出码 0。
fn run(tool: &str, args: &[&str]) -> Result<(), String> {
    if dry_run() {
        tracing::info!(tool, ?args, "HZIP_DRY_RUN=1：跳过外部命令");
        return Ok(());
    }
    let out = std::process::Command::new(tool)
        .args(args)
        .output()
        .map_err(|e| format!("执行 {tool} 失败: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{tool} 失败（exit={:?}）: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// 跑一条命令并取 stdout。
fn run_capture(tool: &str, args: &[&str]) -> Result<String, String> {
    if dry_run() {
        tracing::info!(tool, ?args, "HZIP_DRY_RUN=1：跳过外部命令（无输出）");
        return Err("dry-run".to_string());
    }
    let out = std::process::Command::new(tool)
        .args(args)
        .output()
        .map_err(|e| format!("执行 {tool} 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{tool} 失败（exit={:?}）: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 读回 `LSHandlers`（无表/无偏好 → 空数组）。
fn read_handlers() -> Result<Vec<Handler>, String> {
    if dry_run() {
        return Ok(Vec::new());
    }
    let plist = prefs_plist();
    if !plist.exists() {
        return Ok(Vec::new());
    }
    let text = run_capture(
        "plutil",
        &["-convert", "json", "-o", "-", &plist.to_string_lossy()],
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 LaunchServices 偏好失败: {e}"))?;
    let arr = value
        .get(KEY_HANDLERS)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .filter_map(|v| match v {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        })
        .collect())
}

/// `~/Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist`。
fn prefs_plist() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join("Library/Preferences")
        .join(format!("{DOMAIN}.plist"))
}

/// 写回 `LSHandlers`（走 `defaults`，cfprefsd 才认）。
fn write_handlers(handlers: &[Handler]) -> Result<(), String> {
    let args = write_args(handlers);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run("defaults", &refs)
}

// ===================== 对外三件事 =====================

/// 关联实况快照：[`state_of`] 的全部输入，**读一次**即可对任意多个扩展名求值。
///
/// 逐扩展名的判定只依赖这两份数据。若每个扩展名各读一次，配置里勾十几个格式
/// 就是十几对 `plutil` 子进程（bundle `Info.plist` + 偏好 plist 各一次）。
struct Snapshot {
    /// 我们自己的 bundle id（[`state_of`] 用它判 `ours`）。
    our_id: String,
    /// `LSHandlers` 现有条目。
    handlers: Vec<Handler>,
}

/// 读一次快照（bundle id 探测 + `LSHandlers` 回读）。
fn snapshot() -> Result<Snapshot, String> {
    Ok(Snapshot {
        our_id: our_bundle_id(),
        handlers: read_handlers()?,
    })
}

/// **批量**回读：读一次、逐扩展查表（[`ext_state`] 的单元素特例即它）。
///
/// 读失败 → 逐个返回默认态（与单扩展版同款：状态行不报错，只在日志里说明）。
pub(super) fn ext_states(exts: &[&str]) -> Vec<ExtAssocState> {
    match snapshot() {
        Ok(snap) => {
            let mut out = Vec::with_capacity(exts.len());
            for ext in exts {
                out.push(state_of(&snap.handlers, ext, &snap.our_id));
            }
            out
        }
        Err(e) => {
            tracing::warn!("回读 LaunchServices 关联失败: {e}");
            vec![ExtAssocState::default(); exts.len()]
        }
    }
}

/// 回读某扩展名的关联实况（无 bundle/无表 → 默认态）。
pub(super) fn ext_state(ext: &str) -> ExtAssocState {
    ext_states(std::slice::from_ref(&ext))
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// 我们自己的 bundle id（无 bundle → 兜底常量，此时新写的条目也不会指向真实包）。
fn our_bundle_id() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    match bundle_of(&exe) {
        Some(b) => bundle_id(&b),
        None => FALLBACK_BUNDLE_ID.to_string(),
    }
}

/// 注册（`lsregister -f`）+ 写 `LSHandlers`。
pub(super) fn apply_association(spec: &AssocSpec) -> Result<(), String> {
    let bundle = bundle_of(&spec.exe_path).ok_or_else(|| {
        format!(
            "当前可执行文件不在 .app 包内（{}）：macOS 只能为 App 包注册文件关联",
            spec.exe_path.display()
        )
    })?;
    let our_id = bundle_id(&bundle);
    if spec.extensions.is_empty() {
        return Err("没有勾选任何格式".to_string());
    }

    let existing = read_handlers()?;
    let merged = merge_handlers(&existing, &spec.extensions, &our_id);
    write_handlers(&merged)?;
    run(LSREGISTER, &["-f", &bundle.to_string_lossy()])?;
    tracing::info!(
        bundle = %bundle.display(),
        bundle_id = %our_id,
        exts = spec.extensions.len(),
        "文件关联已写入 LaunchServices（Finder 生效可能需重登录）"
    );
    Ok(())
}

/// 撤销：摘掉指向我们的 handler + 重新注册（让系统忘掉旧条目）。
///
/// `exts` 传**空** = 「凡指向我们的都摘」：用户可能先取消勾选再点解除，只看勾选集
/// 会把此前接管过的类型留在 `LSHandlers` 里（双击仍归我们）。[`strip_handlers`]
/// 的空集语义正是这个。
pub(super) fn remove_association(spec: &AssocSpec) -> Result<(), String> {
    let our_id = our_bundle_id();
    let existing = read_handlers()?;
    let rest = strip_handlers(&existing, &[], &our_id);
    write_handlers(&rest)?;
    if let Some(bundle) = bundle_of(&spec.exe_path) {
        // 重新注册让 LaunchServices 重读（失败不阻断——条目已经摘掉了）
        if let Err(e) = run(LSREGISTER, &["-f", &bundle.to_string_lossy()]) {
            tracing::warn!("重新注册 bundle 失败（忽略）: {e}");
        }
    }
    tracing::info!("文件关联已从 LaunchServices 移除");
    Ok(())
}

#[cfg(test)]
#[path = "tests/macos.rs"]
mod tests;
