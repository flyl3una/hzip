//! 机型识别（macOS `hw.model`）与触控栏机型的辅助功能逃生判定。
//!
//! # 契约
//!
//! - [`machine_model`]：读 `hw.model`（macOS 走 `sysctl(3)`）；非 macOS、读取
//!   失败、结果为空 → `None`。只读探测，不改进程状态。
//! - [`model_has_touch_bar`]：**纯函数**（无 IO、无平台调用）——机型串 → 是否
//!   带触控栏，任意平台（含 Windows）可单测。
//! - [`is_touch_bar_machine`]：`model` 为 `None`（非 macOS / 读不到）⇒ `false`
//!   ——「拿不到」不得升级成「关掉辅助功能」。
//! - [`a11y_fallback_requested`]：环境变量 [`A11Y_ENV`] 值为 `off`（大小写不
//!   敏感）⇒ `true`；未设置/其它任何值 ⇒ `false`（默认走正常辅助功能路径）。
//! - 全部函数不 panic。
//!
//! # 判定规则表（`model_has_touch_bar`）
//!
//! | 机型串 | 判定 | 依据 |
//! |---|---|---|
//! | `MacBookPro13,2` `MacBookPro13,3` | 带触控栏 | Late 2016 四口 TB3 机型（`13,3` 是 15" 板号） |
//! | `MacBookPro13,1` | 否 | Late 2016 13" 两口机型 = 功能键版，**无** Touch Bar |
//! | `MacBookPro14,2` `MacBookPro14,3` | 带触控栏 | Mid 2017 四口 TB3 机型 |
//! | `MacBookPro14,1` | 否 | Mid 2017 13" 两口机型 = 功能键版，**无** Touch Bar |
//! | `MacBookPro15,x`（全 `x`） | 带触控栏 | 2018–2019 全系标配 Touch Bar |
//! | `MacBookPro16,x`（全 `x`） | 带触控栏 | 16" 2019 / 13" 2020 全系标配 Touch Bar |
//! | `MacBookPro17,1` | 带触控栏 | 13" MacBook Pro (M1, 2020)：Touch Bar 为该机型标配（Apple「Identify your MacBook Pro model」108052 / EveryMac 13" M1 2020「Touch Bar」能力栏） |
//! | `Mac14,7` | 带触控栏 | 13" MacBook Pro (M2, 2022)：Apple 108052 明列 Model Identifier = `Mac14,7`，该机型仍带 Touch Bar（Touch Bar 产品线 2023 年终结） |
//! | `MacBookPro18,1` `18,3`（M1 Pro/Max 14"/16"）/ `Mac14,2`（M2 Air）/ `Mac14,5` `14,9`（M2 Pro/Max 14"/16"）/ `Mac14,13` `14,14`（Mac Studio）/ `Mac15,x`（M3 全系）/ `MacBookAir*` / `iMac*` / 空串 / 垃圾串 | 否 | 非触控栏机型（同族的 `Mac` 编号里既有带也有不带，无法按世代整体判定） |
//!
//! `13,x` / `14,x` 里除 `,1` 之外的小号（含将来可能出现、当前不存在的 `13,4`）
//! 与只有单一小号的 `17,x` 一律按**带触控栏**处理：这些世代里已知的无触控栏
//! 机型只有 `13,1` / `14,1` 两款，**误判**只会让一个并无此缺陷的机型在用户
//! **显式**要求关辅助功能时真的关掉（用户自选，无害），**漏判**却会让逃生开关
//! 在真机上静默失效（有害）。方向上宁可多报——**唯一例外是 `Mac` 编号族**：
//! `Mac14,x` / `Mac15,x` 以不带触控栏者为主（Air / Pro 14"·16" / mini / Studio），
//! 故只认已知带触控栏的那一个标识 `Mac14,7`。
//!
//! # 为什么需要这个开关（上游缺陷的确切位置）
//!
//! 上游 `gpui-pre-macos` 0.3.8 在**每次建窗**时无条件给窗口 content view 装
//! `accesskit_macos::SubclassingAdapter`；它用 isa 交换把 view 的类换成动态子
//! 类，并在 `Drop` 时把类**还原**：
//!
//! - `gpui-pre-macos-0.3.8/src/window.rs:710`：窗口状态字段
//!   `accesskit_adapter: Option<accesskit_macos::SubclassingAdapter>`（持有者）。
//! - `gpui-pre-macos-0.3.8/src/window.rs:2365`：`SubclassingAdapter::for_window(…)`
//!   ——建窗时的**无条件安装点**（`a11y_install` 路径）。
//! - `accesskit_macos-0.26.3/src/subclass.rs:203`：`object_setClass(…)`
//!   ——换上 `AccessKitSubclassOf…` 动态子类（`new_internal`）。
//! - `accesskit_macos-0.26.3/src/subclass.rs:263`：`impl Drop` 里的
//!   `object_setClass(…)` ——把 view 换回原类，**缺陷触发点**。
//!
//! 触控栏机型上 AppKit 为支持 Touch Bar 已对 content view 做过 KVO isa 交换
//! （`_NSTouchBarFinder`），「还原成原类」把它改回去，于是关窗时 AppKit 自身
//! 的一致性校验失败，在 display-cycle flush 里抛未捕获 ObjC 异常 → `SIGILL`/
//! 进程崩（不是 Rust panic，捕获不到）。
//!
//! # 与 vendor 补丁的关系
//!
//! **主修复是 vendor 补丁**（`vendor/gpui-pre-macos/src/window.rs`，当前基线
//! 0.3.8；打补丁前的快照不再留档——重放指南见 `vendor/gpui-pre-macos/src/gpui_macos.rs`
//! 的 crate 文档）：补丁不再使用 `SubclassingAdapter`，改由
//! `GPUIView` 自己实现 `accessibilityChildren` / `accessibilityFocusedUIElement` /
//! `accessibilityHitTest:` 转发给普通 `accesskit_macos::Adapter`——**全程不改类**
//! ⇒ 不存在 isa 还原，正常路径不再踩这个坑。
//!
//! 本模块是**第二道保险**：机型带触控栏 **且** `HZIP_MACOS_A11Y=off` 时，GUI 走
//! `gpui::Application::new_inaccessible(platform)`（`gpui-pre-0.3.8/src/app.rs:195`，
//! 置 `accessibility_force_disabled = true`）启动，进程级关闭辅助功能——连
//! `Adapter` 都不安装，从源头规避；代价是本次会话没有辅助功能（屏幕阅读器不可
//! 用），且该开关**默认关闭**（变量未设或其它值 ⇒ 正常 a11y 路径）。
//!
//! 为什么用环境变量而不是配置项：现场机器已经「关窗即崩」，改配置文件得先能起
//! 来 GUI；`HZIP_MACOS_A11Y=off cmd` / launchd 环境 / 快捷指令都可在启动前设。

/// 逃生开关的环境变量名（值 `off`，大小写不敏感）。
pub const A11Y_ENV: &str = "HZIP_MACOS_A11Y";

/// 本机机型标识（macOS 的 `hw.model`，如 `MacBookPro15,1`）。
///
/// 非 macOS 恒 `None`（其它平台没有这一概念）；macOS 读取失败/为空也 `None`。
pub fn machine_model() -> Option<String> {
    hw_model()
}

/// 机型串 → 是否带触控栏（**纯函数**，无平台调用，任意平台可单测）。
///
/// 规则见模块文档的判定表；串首尾空白忽略，其余不宽容（多一个字符即不匹配，
/// 宁可漏报垃圾串也不猜）。非触控栏机型 / 空串 / 垃圾串 → `false`。
pub fn model_has_touch_bar(model: &str) -> bool {
    let model = model.trim();
    // Apple Silicon 起 Apple 换了机型标识族：**带触控栏的 13" MacBook Pro 有
    // 一个落在 `MacBookPro` 之外**——`Mac14,7`（13" M2, 2022；Apple 支持文档
    // 108052 明列该 Model Identifier，该机仍带 Touch Bar）。同族的其它标识
    // （`Mac14,2` M2 Air、`Mac14,5`/`14,9` M2 Pro/Max 14"·16"、`Mac14,13`/`14,14`
    // Mac Studio、`Mac15,x` M3 全系）都不带触控栏，无法按世代整体判定 ⇒ 只认这
    // 一个已知标识，不做 `Mac*` 前缀级判定（那会把 Air 与 14"/16" Pro 全判真）。
    if model == "Mac14,7" {
        return true;
    }
    let Some(rest) = model.strip_prefix("MacBookPro") else {
        // MacBookAir* / iMac* / 上述 Mac* 族 / 空串 / 垃圾串
        return false;
    };
    let Some((generation, minor)) = rest.split_once(',') else {
        return false;
    };
    // 解析失败（`MacBookPro15`、`MacBookPro15,x`、`MacBookPro15,1x`…）→ 否。
    let Ok(generation) = generation.parse::<u8>() else {
        return false;
    };
    let Ok(minor) = minor.parse::<u8>() else {
        return false;
    };
    match generation {
        // 13,x / 14,x：`,1` 是两代里仅有的「无触控栏」功能键机型（13,1 / 14,1），
        // 其余（13,2 / 13,3 / 14,2 / 14,3 及将来未知小号）均带触控栏。
        13..=14 => minor != 1,
        // 15,x（2018–2019）、16,x（16" 2019 / 13" 2020）、17,x（13" M1, 2020；
        // 该世代只有 17,1 一个小号）：全系标配触控栏。
        15..=17 => true,
        // 18,x（M1 Pro/Max 14"/16"）及之后的 Apple Silicon（标识已改走
        // `Mac14,x`/`Mac15,x` 族）与更早世代：均无触控栏。
        _ => false,
    }
}

/// 本机是否带触控栏；`model` 为 [`machine_model`] 的结果。
///
/// `None`（非 macOS / 读不到机型）⇒ `false`——**默认走正常辅助功能路径**，绝不
/// 因为「拿不到」而关掉辅助功能。
pub fn is_touch_bar_machine(model: Option<&str>) -> bool {
    model.is_some_and(model_has_touch_bar)
}

/// 纯函数：环境变量值 → 是否要求「进程级关闭辅助功能」。
///
/// 只认 `off`（大小写不敏感、两侧空白忽略）；`None`（未设置）、空串、其它任何
/// 值（含 `on` / `0` / `false`）一律假。
pub fn a11y_fallback_env_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// 读环境变量 [`A11Y_ENV`] 的当前取值并判定（非 UTF-8 / 未设置 ⇒ 假）。
pub fn a11y_fallback_requested() -> bool {
    a11y_fallback_env_value(std::env::var(A11Y_ENV).ok().as_deref())
}

/// 读 `hw.model`（macOS `sysctl(3)`）。
///
/// 走 `sysctlbyname` 而不是派生 `sysctl` 子进程：不依赖 `PATH`（Finder/launchd
/// 启动的 GUI 进程 `PATH` 常常只有 `/usr/bin:/bin:/usr/sbin:/sbin`，且不额外
/// fork 一个进程——本函数在启动早期调用，越省越好）。
///
/// 两趟式：先问长度再按长度取。第二趟多给 1 字节余量——先问后取之间值变长
/// （API 允许）时不至于截断成半截机型串；返回值超出缓冲则判失败而非采信。
#[cfg(target_os = "macos")]
fn hw_model() -> Option<String> {
    // `sysctlbyname` 要 NUL 结尾的 C 串；用静态字节串免堆分配（不做 CString）。
    const NAME: &[u8] = b"hw.model\0";

    // 第一趟：`oldp = NULL` 时 API 只回填 `oldlenp`。
    let mut len: libc::size_t = 0;
    // SAFETY：`NAME` 是静态 NUL 结尾字节串；`oldp` 为 NULL 是文档允许的「询问
    // 长度」入参；`newp = NULL, newlen = 0` 表示只读不写。
    let rc = unsafe {
        libc::sysctlbyname(
            NAME.as_ptr().cast(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }

    // 第二趟：按长度取值。
    let mut buf = vec![0u8; len + 1];
    let mut written = buf.len();
    // SAFETY：`buf` 由本函数独占、容量如实经 `written` 传入，API 至多在容量内写入。
    let rc = unsafe {
        libc::sysctlbyname(
            NAME.as_ptr().cast(),
            buf.as_mut_ptr().cast(),
            &mut written,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || written > buf.len() {
        return None;
    }
    buf.truncate(written);

    // `hw.model` 是 C 串：去掉结尾 NUL 再转 UTF-8（机型串本身就是 ASCII）。
    while buf.last() == Some(&0) {
        buf.pop();
    }
    let model = String::from_utf8(buf).ok()?;
    let model = model.trim();
    if model.is_empty() {
        None
    } else {
        Some(model.to_string())
    }
}

/// 非 macOS：没有 `hw.model` 这一概念，恒 `None`（调用方得到的安全值）。
#[cfg(not(target_os = "macos"))]
fn hw_model() -> Option<String> {
    None
}

#[cfg(test)]
#[path = "tests/machine.rs"]
mod tests;
