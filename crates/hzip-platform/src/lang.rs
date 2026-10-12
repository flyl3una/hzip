//! 系统界面语言探测（T-ICON 拍板3：语言「跟随系统」需要真值）。
//!
//! # 契约
//!
//! - 返回 BCP-47 近似的语言标签（`"zh-CN"` / `"en-US"` …）；探测不到 → `None`。
//! - **只读探测，不改进程状态**（locale 的落地由 GUI 侧的 `init_locale` 负责）。
//! - 不 panic（环境变量缺失/非法 → `None`）。
//!
//! # 三平台来源
//!
//! | 平台 | 来源 |
//! |---|---|
//! | Windows | `GetUserDefaultLocaleName`（用户区域/显示语言，与资源管理器「类型」列同源） |
//! | macOS / Linux | `LC_ALL` → `LC_MESSAGES` → `LANG`（`zh_CN.UTF-8` → `zh-CN`），空值/`C`/`POSIX` → `None` |
//!
//! # 已知取舍
//!
//! macOS **不读** `AppleLanguages` 偏好（那要 `core-foundation` 依赖，且常与
//! `LC_*` 不一致）；unix 侧统一走 POSIX 环境变量，与同机终端程序看到的语言
//! 保持一致——「跟随系统」在 unix 上的实际语义就是「跟随登录环境」。

/// 系统界面语言标签；探测不到 → `None`。
pub fn system_language() -> Option<String> {
    platform::detect()
}

/// Windows：`GetUserDefaultLocaleName`（用户 locale，非安装语言）。
///
/// 选它而不是 `GetUserDefaultUILanguage`：后者给的是 LCID/语言 ID，需再查表
/// 才能得到 BCP-47 标签；前者直接产出 `"zh-CN"` 形态且与 shell 同一数据源。
#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    /// `LOCALE_NAME_MAX_LENGTH`（`winnls.h`）：含 NUL 的语言标签容量上限。
    const LOCALE_NAME_MAX_LENGTH: usize = 85;

    /// 探测用户默认 locale；API 返回 0（失败）→ `None`。
    pub(super) fn detect() -> Option<String> {
        let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];
        // SAFETY：缓冲区由本函数独占且长度如实传入，API 至多在容量内写
        // UTF-16 + NUL。返回值的长度语义按文档「含 NUL 的字符数」，但这里
        // 只拿它判失败，真正的串长以缓冲区里第一个 NUL 为准（更稳）。
        let written = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if written == 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let tag = String::from_utf16_lossy(&buf[..end]);
        (!tag.is_empty()).then_some(tag)
    }
}

/// unix（Linux/macOS）：按 POSIX 优先级取环境变量。
#[cfg(unix)]
mod platform {
    /// 优先级同 POSIX：`LC_ALL` 覆盖一切，`LC_MESSAGES` 管消息语言，`LANG` 兜底。
    ///
    /// 前一个变量取到「空 / `C` / `POSIX`」**不算命中**——那表示「未设置」的
    /// 等价物，必须继续看下一个，否则 `LANG=zh_CN.UTF-8` 会被 `LC_ALL=C` 抹掉。
    const VARS: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANG"];

    /// 第一个能给出语言标签的变量；全部不可用 → `None`。
    pub(super) fn detect() -> Option<String> {
        VARS.iter().find_map(|name| {
            let raw = std::env::var(*name).ok()?;
            normalize(&raw)
        })
    }

    /// `zh_CN.UTF-8` → `zh-CN`；空 / `C` / `POSIX` → `None`。
    ///
    /// `C.UTF-8` 这类「C locale + codeset」与裸 `C` 语义相同，故先剥 codeset
    /// 再判定——否则会返回一个并非语言的 `"C"`。
    fn normalize(raw: &str) -> Option<String> {
        // GNU locale 形如 `language[_territory][.codeset][@modifier]`：语言标签
        // 只要 `.`/`@` 前一段（codeset 与 modifier 都不携带语言信息）。
        let base = raw.trim().split(['.', '@']).next().unwrap_or_default();
        if base.is_empty() || base == "C" || base == "POSIX" {
            return None;
        }
        Some(base.replace('_', "-"))
    }
}

/// 未覆盖的目标平台（如 wasm）：视为「拿不到」，由调用方回退产品默认语言。
#[cfg(not(any(windows, unix)))]
mod platform {
    pub(super) fn detect() -> Option<String> {
        None
    }
}
