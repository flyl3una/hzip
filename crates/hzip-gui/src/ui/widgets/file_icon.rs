//! 文件类型图标（系统图标 + 回退；T-ICON）。
//!
//! # 职责
//!
//! 把「条目名 + 种类」变成图标元素：命中缓存 → 系统图标位图（`img`）；未命中 →
//! **本次先回退** `fallback`（现有 lucide 图标，逐像素等同 T-ICON 之前），同时把
//! 键登记给后台探测线程，探测到位后触发重绘原地替换。
//!
//! **渲染帧绝不调用平台 API**（`SHGetFileInfoW` 可能阻塞数百毫秒）；后台线程
//! 调 `hzip_platform::file_icon::lookup`，结果经主线程泵回投。
//!
//! # 数据流（三步，全部在主线程之外完成重活）
//!
//! 1. **渲染帧**（[`icon`]）：查缓存。`Ready` ⇒ 位图；`Wanted`（在飞）/
//!    `Missing`（负缓存）⇒ 回退；**未登记** ⇒ 登记 `Wanted` + 键入探测队列 +
//!    回退。全程只碰一把 `Mutex`，无平台调用、无分配（除首次登记的键）。
//! 2. **worker**（命名 `gui-file-icon` 的 1 个 `std::thread`，[`init`] 时启）：
//!    取键 → 调 `source` → 写缓存（`None` ⇒ `Missing`，不重复探测）→
//!    `tx.try_send(`[`VmSignal::IconCacheFilled`]`)`。
//! 3. **主线程泵**：`IconCacheFilled` 只重绘（**不**重导快照、**不**重建行），
//!    下一帧渲染帧即读到 `Ready` 并上屏。
//!
//! # 与两个先例的对照
//!
//! 7-Zip 用「扩展名 → 图标索引」有序表 + 二分查找缓存（源码注释：拆分档
//! `.001/.002` 逐次调用 `SHGetFileInfo` 「too slow」）；本模块按 `(kind, ext)`
//! 缓存（同一扩展名只探测一次，与文件个数无关）。
//!
//! # 缓存容量（[`CACHE_CAP`]）
//!
//! 键是 `(kind, ext)` 而非文件——一个目录 10k 个 `.txt` 只占 1 条。真实会话里
//! 不同扩展名（含无扩展名一档）远少于 512；512 条按每条 `Ready` ≈ 4 KiB 位图
//! 估算的上限约 2 MiB，兜住「异常档案里几千个不同扩展名」的极端情形；淘汰按
//! **插入序**（FIFO，`order` 队列）——图标本身无热度差异，LRU 只多一份簿记。
//!
//! # 接口（T-ICON 波2 冻结面）
//!
//! | 入口 | 语义 |
//! |---|---|
//! | [`icon`] | 图标元素（系统位图或回退） |
//! | [`fallback`] | 只画回退图标（没有可探测的名字时用；尺寸/光学下沉与 [`icon`] 同槽位） |
//! | [`type_name`] | 系统本地化类型名（缓存命中且语言一致 ⇒ `Some`），供「类型」列 |
//! | [`slot_px`] | 槽位目标边长（px） |
//! | [`kind_of`] / [`icon_key`] | 纯函数：种类判定 / 缓存键归一化 |
//! | [`init`] / [`set_enabled`] | 装配 / 设置项开关（关闭 = 清缓存 + 停探测） |

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, Pixels, RenderImage,
    SharedString, Styled, div, img, px,
};
use hzip_platform::file_icon::{IconKind, IconRequest, SystemIcon};

use crate::ui::constant::ICON_CELL_ICON;
use crate::ui::contracts::SignalTx;
use crate::ui::widgets::AppIcon;
use crate::viewmodel::VmSignal;

/// 渲染槽位（回退图标尺寸 + 位图绘制尺寸）。
///
/// **两分支强制同尺寸**（都走 [`slot_px`]）：否则位图替换回退时会有 1–2px 跳动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    /// 与文字**同行**的图标：明细表名称列、左栏树、紧凑列表（三处行盒同源 ⇒ 同一档）。
    Inline,
    /// 图标网格（图标在 96px 单元格里独立成块、名称在下——与文字无同行关系）。
    Grid,
}

/// 缓存键（= [`icon_key`] 的输出）。
type IconKey = (IconKind, Option<String>);

/// 平台取图函数（生产 = `hzip_platform::file_icon::lookup`；测试可注入计数 stub）。
pub(crate) type IconSource = Arc<dyn Fn(&IconRequest<'_>) -> Option<SystemIcon> + Send + Sync>;

/// 缓存容量上限（条）。依据见模块文档「缓存容量」。
const CACHE_CAP: usize = 512;

/// 「可见墨迹居中」的位移上限（占位图高的比例，±13%）。
///
/// 三重约束下的取值：
/// 1. 图标设计千差万别（图片/书本类内容本就偏下、徽标类偏上），无上限会把它们推歪；
/// 2. 要覆盖**文档类图标**最常见的「纸面顶部留白」——实测 4K/150% 上这类图标可见内容比
///    行内文字高 4~6 物理 px（≈2.7~4 逻辑 px），13% × 22px ≈ 2.9px 正好够；
/// 3. 位移后位图连同上下留白仍装得进行的内容盒（行框 28 − 槽位 22 ⇒ 单侧余量 3px，
///    ±2.9 不越界），否则会被单元格的 `overflow_hidden` 裁掉一条边。
const INK_DY_MAX: f32 = 0.13;

/// 槽位目标边长（px）。
///
/// # `Inline` = 文字的**字形盒**（`GLYPH_BOX_EM` × 基准字号）
///
/// 用户实报（2026-10-17，4K/Win11 明细列表）：「图标比较小，并且比字体高」——
/// 根因是这一档**写死 16px**：基准字号默认就是 16px，而行的文字行框是
/// `max(line_height×字号, 墨迹盒)` ≈ 1.74em ⇒ 图标只占行框 ~57%、更比字形墨迹
/// 小一圈；再叠加「按行框居中」（字面中心比行框中心低 [`SQUARE_OPTICAL_SHIFT`]）
/// 就成了「小且高」。资源管理器的比例可作对照：16px 图标配 ~12px 字
/// ⇒ **1.33em**；本档取 [`crate::ui::constant::GLYPH_BOX_EM`] = 1.35em（同一条
/// 「图标 = 字身框」的规则，且随 `[ui].font_size` 一起缩放——字号变大图标跟着大）。
///
/// # `Grid` = 单元格常量
///
/// [`ICON_CELL_ICON`]（`ui/constant.rs` 单一来源）：网格图标不随字号走（它与
/// 名称分两行、由单元格尺寸决定）。
///
/// 两分支（回退/位图）共用本函数 ⇒ 替换时无尺寸跳动。
pub(crate) fn slot_px(slot: Slot, cx: &App) -> Pixels {
    match slot {
        // `theme().font_size` = `[ui].font_size`（与 `window.rem_size()` 同源，
        // 见 `ui::theme` 的 font 槽覆写与 `theme_font_size_drives_window_rem`）
        Slot::Inline => {
            px((f32::from(cx.theme().font_size) * crate::ui::constant::GLYPH_BOX_EM).round())
        }
        Slot::Grid => ICON_CELL_ICON,
    }
}

/// 条目种类判定（纯函数）。
///
/// 顺序与 [`crate::ui::views::browse::row_type_icon`] 一致：软链接优先
/// （软链接目录也画链接），其次容器，其次目录，否则文件。
pub(crate) fn kind_of(is_dir: bool, is_container: bool, is_symlink: bool) -> IconKind {
    if is_symlink {
        IconKind::Symlink
    } else if is_container {
        IconKind::Container
    } else if is_dir {
        IconKind::Dir
    } else {
        IconKind::File
    }
}

/// 缓存键归一化（纯函数）：`(kind, 小写扩展名)`。
///
/// - 取**最后一段**扩展名（`a.TAR.GZ` → `gz`，与资源管理器/先例一致）；
/// - 无扩展名、尾点（`a.`）、隐藏文件（`.bashrc`）→ `None`（取系统默认文件图标）；
/// - `Container` 归一为 `File`：同一扩展名共用一次探测（图标本就相同；
///   `Container` 只改回退图标）。`Symlink` 保持独立（Windows 加箭头覆盖层）。
pub(crate) fn icon_key(name: &str, kind: IconKind) -> (IconKind, Option<String>) {
    let ext = name.rsplit_once('.').and_then(|(stem, ext)| {
        (!ext.is_empty() && !stem.is_empty()).then(|| ext.to_ascii_lowercase())
    });
    let kind = if kind == IconKind::Container {
        IconKind::File
    } else {
        kind
    };
    (kind, ext)
}

/// 图标元素：系统位图优先，未命中/开关关闭/平台不支持 → `fallback`。
///
/// `fallback` 由调用方按现有 [`crate::ui::views::browse::row_type_icon`] /
/// `tree_row_icon` 计算（**回退唯一来源**，本模块不复制那份映射）。
/// 回退元素自带 `muted_foreground` 着色（与 T-ICON 之前逐像素一致）；
/// 系统位图**不着色**（彩色图标即"与资源管理器一致"的意义所在）。
///
/// 本函数是**渲染帧**：只查缓存 + 首登入队，绝不调平台 API（见模块文档）。
pub(crate) fn icon(
    name: &str,
    kind: IconKind,
    slot: Slot,
    fallback: AppIcon,
    cx: &App,
) -> AnyElement {
    let key = icon_key(name, kind);
    // 未装配（无图形宿主的单测 / 直接模式）⇒ 纯回退：不建缓存、不起线程。
    let Some(cache) = cx.try_global::<FileIconCache>() else {
        return fallback_icon(fallback, slot, cx);
    };
    if !cache.inner.enabled.load(Ordering::Relaxed) {
        return fallback_icon(fallback, slot, cx);
    }
    let mut registered = false;
    let outcome = {
        let mut st = lock(&cache.inner.state);
        match st.entries.get(&key) {
            // 命中：只做一次 `Arc` 克隆（32px 源按 slot 缩放由渲染侧做）。
            Some(Entry::Ready { image, ink_dy, .. }) => Outcome::Ready {
                image: Arc::clone(image),
                ink_dy: *ink_dy,
            },
            // 在飞（`Wanted`）/ 负缓存（`Missing`）：回退且**不**重复入队。
            Some(_) => Outcome::Fallback,
            None => {
                register(&mut st, key);
                registered = true;
                Outcome::Fallback
            }
        }
    };
    if registered {
        // 唤醒 worker（`pending` 已在锁内写好；`Condvar` 自带内存序同步）。
        cache.inner.cv.notify_one();
    }
    match outcome {
        Outcome::Ready { image, ink_dy } => system_icon_element(image, slot, ink_dy, cx),
        Outcome::Fallback => fallback_icon(fallback, slot, cx),
    }
}

/// 系统本地化类型名（`Some` 仅在：开关开启 ∧ 缓存命中 ∧ 类型名可用 ∧ 语言一致）。
///
/// 「语言一致」= UI locale（[`crate::current_locale`]）等于系统语言的映射结果
/// （主标签 `en` → `en`，其余/探测不到 → `zh-CN`）——不一致时采用系统类型名会
/// 中英混排，此时由调用方回退 `row.type_text`。
///
/// 与 [`icon`] 同源读缓存但**不登记**：类型列总有对应图标在同帧渲染（登记由
/// `icon` 负责）；未命中期间返回 `None`，下一帧随位图一起到位。
pub(crate) fn type_name(name: &str, kind: IconKind, cx: &App) -> Option<SharedString> {
    let cache = cx.try_global::<FileIconCache>()?;
    if !cache.inner.enabled.load(Ordering::Relaxed) || !locale_matches_system() {
        return None;
    }
    let st = lock(&cache.inner.state);
    match st.entries.get(&icon_key(name, kind)) {
        Some(Entry::Ready {
            type_name: Some(t), ..
        }) => Some(SharedString::from(t.clone())),
        _ => None,
    }
}

/// 装配（进程启动调用一次，装配点由本模块负责）：建缓存 Global、启动后台探测线程。
///
/// `tx` = 主线程泵通道（shell 已有的 `signal_tx`；无图形宿主/测试可传 `None`，
/// 此时图标仍会进缓存，但需等下一次自然重绘才可见）。
///
/// 开关初值读 `[ui].file_type_icons`（大小写不敏感；未知/空值按 `system`）；
/// 此后由 [`set_enabled`] 跟随设置项。**幂等**：已有 Global 只刷新 `tx`。
pub(crate) fn init(cx: &mut App, tx: Option<Arc<SignalTx>>) {
    let enabled = configured_enabled();
    if let Some(inner) = install(cx, tx, Arc::new(hzip_platform::file_icon::lookup), enabled) {
        spawn_worker(inner);
    }
}

/// 设置项开关（`ui.file_type_icons` 变更时调用）：关闭 = 清缓存 + 停探测。
///
/// 重开**无需预热**：清空后下一次渲染会把出现过的键重新登记（首帧照旧先回退）。
pub(crate) fn set_enabled(cx: &mut App, enabled: bool) {
    let Some(cache) = cx.try_global::<FileIconCache>() else {
        return;
    };
    cache.inner.enabled.store(enabled, Ordering::Relaxed);
    {
        let mut st = lock(&cache.inner.state);
        st.entries.clear();
        st.order.clear();
        // 待处理键一并丢弃：在飞探测会因「状态不再是 Wanted」而被跳过。
        st.pending.clear();
    }
    // 唤醒 worker 重判开关（关闭态下它只是回到等待）。
    cache.inner.cv.notify_all();
}

// 测试钩子（`#[cfg(test)]` 子模块，实现外移到 `tests/test_hooks.rs`：业务文件里
// 不得内联测试模块，见布局检查器 L2）。
#[cfg(test)]
#[path = "tests/test_hooks.rs"]
pub(crate) mod test_hooks;

/// 回退图标元素（各槽位与 T-ICON 之前逐像素一致）。
///
/// 显式给尺寸（不依赖继承字号）：与位图分支共用 [`slot_px`] ⇒ 两分支同尺寸。
fn fallback_icon(fallback: AppIcon, slot: Slot, cx: &App) -> AnyElement {
    let icon = Icon::new(fallback).text_color(cx.theme().muted_foreground);
    let element = icon
        .with_size(Size::Size(slot_px(slot, cx)))
        .into_any_element();
    // 调试锚点（`cfg!(test)`；release 侧 `if` 常量折叠掉）：headless 烟测要分别
    // 定位「回退」与「位图」两个分支，才能断言「先回退后替换」的用户可见行为。
    if cfg!(test) {
        return nudge_optically(
            div()
                .flex_shrink_0()
                .debug_selector(|| "file-icon-fallback".into())
                .child(element)
                .into_any_element(),
            slot,
        );
    }
    nudge_optically(element, slot)
}

/// 只画回退图标（**不查系统图标**）：调用方没有可探测的名字时用（左栏树喂元数据
/// 滞后一帧 / 档案名未知）。尺寸与光学下沉与 [`icon`] 的同槽位完全一致。
pub(crate) fn fallback(fallback: AppIcon, slot: Slot, cx: &App) -> AnyElement {
    fallback_icon(fallback, slot, cx)
}

/// **行内图标的光学下沉**：图标框相对文字**字面**中心（而字形盒中心）下移
/// [`SQUARE_OPTICAL_SHIFT`]。
///
/// 手法与复选框同源但作用对象相反（复选框动文字、这里动图标）：给图标**加
/// `2 × 位移` 的上外边距**——外边距盒随行居中 ⇒ 图标本体恰好下移本值；图标两
/// 侧都是文字，动图标不动文字（文字行框/基线一律不碰）。
///
/// 外边距盒 = `slot + 2×位移`，必须仍装进行的内容盒（`Inline` 槽 = 字形盒
/// `1.35em` + 3px ≤ 行框 `max(line_height×字号, 墨迹盒)`；出厂配置下
/// 21.6+3=24.6 ≤ 27.8）⇒ 不会把图标挤出内容盒（`overflow_hidden` 裁边）。
/// `Grid` 槽不加（图标与名称分两行，无同行光学关系）。
fn nudge_optically(el: AnyElement, slot: Slot) -> AnyElement {
    if slot == Slot::Grid {
        return el;
    }
    div()
        // `flex_shrink_0`：包装层与图标同宽，不得参与收缩（否则它缩了、图标照原尺寸
        // 画出来 ⇒ 与右侧文字重叠；列宽不足时要收的是名称文本）
        .flex_shrink_0()
        .mt(px(crate::ui::constant::SQUARE_OPTICAL_SHIFT * 2.))
        .child(el)
        .into_any_element()
}

/// 系统位图元素：按槽位边长定尺寸（32px 源按 slot 缩放，T-ICON 决策 #2）。
///
/// 不着色——彩色图标即「与资源管理器一致」的意义所在。
///
/// **不参与 flex 收缩**（`flex_shrink_0`，用户口径 2026-10-10）：列宽不足时图标
/// 恒保尺寸，被挤掉/裁掉的必须是右侧的名称文本。kit 的 `Icon`（回退分支）自带
/// `flex_shrink_0`，位图分支必须显式声明——否则同一行里回退图标不缩、位图被挤，
/// 表现为「有图标时反而先消失」。
fn system_icon_element(image: Arc<RenderImage>, slot: Slot, ink_dy: f32, cx: &App) -> AnyElement {
    let size = slot_px(slot, cx);
    let image_box = img(image).w(size).h(size).flex_shrink_0();
    // 可见墨迹居中：给位图**加 2×位移的上/下内边距**——内边距盒随行居中 ⇒ 位图本体
    // 恰好下移/上移「位移」值（与 [`nudge_optically`] 同一手法，这里作用在位图自身）。
    // 用**布局**而非绘制偏移：① 测试能直接量到（`file-icon-system` 锚点跟着动）；
    // ② 位移被夹在槽位余量内（见 [`INK_DY_MAX`]），不会被单元格 `overflow_hidden` 裁边。
    let dy = size * ink_dy.clamp(-INK_DY_MAX, INK_DY_MAX);
    let inner: AnyElement = if cfg!(test) {
        image_box
            .debug_selector(|| "file-icon-system".into())
            .into_any_element()
    } else {
        image_box.into_any_element()
    };
    let element: AnyElement = if dy >= px(0.25) {
        div().pt(dy * 2.).child(inner).into_any_element()
    } else if dy <= px(-0.25) {
        div().pb(dy * -2.).child(inner).into_any_element()
    } else {
        inner
    };
    nudge_optically(element, slot)
}

/// UI locale 是否与系统语言一致（T-ICON 决策 #3）。
///
/// 映射规则：系统语言主标签 `en` → `en`，其余/探测不到 → `zh-CN`（产品默认
/// 文案语言）。系统语言进程内不变，故只探测一次。
fn locale_matches_system() -> bool {
    static MAPPED: std::sync::LazyLock<&'static str> = std::sync::LazyLock::new(|| {
        let is_english = hzip_platform::lang::system_language().is_some_and(|tag| {
            tag.split(['-', '_'])
                .next()
                .is_some_and(|main| main.eq_ignore_ascii_case("en"))
        });
        if is_english { "en" } else { "zh-CN" }
    });
    crate::current_locale().as_str() == *MAPPED
}

/// 启动开关初值：`[ui].file_type_icons`（大小写不敏感；未知/空值按 `system`）。
fn configured_enabled() -> bool {
    !hzip_config::Config::load()
        .ui
        .file_type_icons
        .eq_ignore_ascii_case("builtin")
}

/// 安装缓存 Global；已有则只刷新回投通道（幂等）并返回 `None`。
///
/// 返回 `Some(inner)` = 本次新建（调用方负责 [`spawn_worker`]，避免重复起线程）。
fn install(
    cx: &mut App,
    tx: Option<Arc<SignalTx>>,
    source: IconSource,
    enabled: bool,
) -> Option<Arc<CacheInner>> {
    let existing = cx
        .try_global::<FileIconCache>()
        .map(|cache| Arc::clone(&cache.inner));
    if let Some(inner) = existing {
        *lock(&inner.tx) = tx;
        return None;
    }
    let inner = Arc::new(CacheInner {
        enabled: AtomicBool::new(enabled),
        source,
        tx: Mutex::new(tx),
        state: Mutex::new(CacheState::default()),
        cv: Condvar::new(),
    });
    cx.set_global(FileIconCache {
        inner: Arc::clone(&inner),
    });
    Some(inner)
}

/// 登记一个新键：置 `Wanted`、记插入序（FIFO 淘汰依据）、入探测队列。
fn register(st: &mut CacheState, key: IconKey) {
    if !st.entries.contains_key(&key) {
        st.entries.insert(key.clone(), Entry::Wanted);
        st.order.push_back(key.clone());
    }
    // 同键只入队一次（`Wanted` 态重复渲染不会再加）。
    if !st.pending.contains(&key) {
        st.pending.push_back(key);
    }
    while st.order.len() > CACHE_CAP {
        if let Some(old) = st.order.pop_front() {
            st.entries.remove(&old);
        }
    }
}

/// 处理一个待探测键：调平台取图并写缓存；返回是否真的探测了。
///
/// 只处理仍处于 `Wanted` 的键——被 [`set_enabled`] 清空、被容量淘汰、或已被
/// 其它路径填过的键在此跳过（队列与缓存状态可能因这些操作而错位）。
fn probe_one(inner: &CacheInner, key: &IconKey) -> bool {
    if !inner.enabled.load(Ordering::Relaxed) {
        return false;
    }
    {
        let st = lock(&inner.state);
        if !matches!(st.entries.get(key), Some(Entry::Wanted)) {
            return false;
        }
    }
    let req = IconRequest {
        ext: key.1.as_deref(),
        kind: key.0,
    };
    let entry = match (inner.source)(&req) {
        Some(icon) => system_icon_entry(icon).unwrap_or(Entry::Missing),
        None => Entry::Missing,
    };
    {
        let mut st = lock(&inner.state);
        // 期间被关闭（清空已发生）或键已不在 `Wanted` ⇒ 不回填（防复活）。
        if !inner.enabled.load(Ordering::Relaxed)
            || !matches!(st.entries.get(key), Some(Entry::Wanted))
        {
            return false;
        }
        st.entries.insert(key.clone(), entry);
    }
    true
}

/// **可见墨迹**的纵向偏移比例（正 = 墨迹偏上、位图需下移；已夹到 ±[`INK_DY_MAX`]）。
///
/// # 为什么需要（用户实报 2026-10-17，4K/Win11 明细列表）
///
/// 「图标没有垂直居中（有点靠上了）」。Windows 的文档类图标是**一张近白纸面 + 彩色/
/// 深色内容**：纸面在浅色行背景上几乎不可见，眼睛只看见内容 ⇒ 图标看着又小又靠上
/// （实测这类图标可见内容比行内文字高 4~6 物理 px，而文件夹/压缩包等满幅图标是对齐的）。
/// 这里按**看得见的部分**算墨迹中心，渲染侧据此把位图整体下移，使可见内容落在行内
/// 文字的光学中心上。
///
/// 判定：`alpha > 8` 且 **不是**「亮度 ≥ 225 且饱和度 ≤ 16」（近白且低饱和 = 纸面，
/// 在浅色行背景上看不见）。深色主题下纸面本身是深色（亮度低）⇒ 全幅计入 ⇒ 偏移 ≈ 0
/// （不会把深色图标推歪）——即该修正自带主题自适应。
fn visible_ink_dy_ratio(icon: &SystemIcon) -> f32 {
    let (w, h) = (icon.width, icon.height);
    if w == 0 || h == 0 {
        return 0.;
    }
    let (mut top, mut bottom) = (u32::MAX, 0u32);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let Some(p) = icon.premultiplied_bgra.get(i..i + 4) else {
                return 0.;
            };
            let (b, g, r, a) = (p[0], p[1], p[2], p[3]);
            if a <= 8 {
                continue;
            }
            let max = r.max(g).max(b) as u32;
            let min = r.min(g).min(b) as u32;
            let lum = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
            if lum >= 225 && max - min <= 16 {
                continue; // 纸面：浅色行背景上看不见
            }
            top = top.min(y);
            bottom = bottom.max(y);
        }
    }
    if top == u32::MAX {
        return 0.;
    }
    let ink_center = (top + bottom) as f32 / 2.;
    let ratio = (h as f32 / 2. - ink_center) / h as f32;
    ratio.clamp(-INK_DY_MAX, INK_DY_MAX)
}

/// `SystemIcon` → 缓存条目；位图自相矛盾（尺寸/长度不符）⇒ `None`（当缺失）。
///
/// 调用方（本模块的探测路径）**先校验后转换**：`bgra_pa_to_render_image` 对长度
/// 不符是 panic 语义（与兄弟函数 `rgba_to_render_image` 同款），不得让平台侧的
/// 脏数据把探测线程打挂。
fn system_icon_entry(icon: SystemIcon) -> Option<Entry> {
    let (w, h) = (icon.width, icon.height);
    let expected = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if w == 0 || h == 0 || expected != icon.premultiplied_bgra.len() {
        tracing::warn!(
            width = w,
            height = h,
            bytes = icon.premultiplied_bgra.len(),
            "系统图标位图尺寸与字节数不符，按「拿不到」处理"
        );
        return None;
    }
    let ink_dy = visible_ink_dy_ratio(&icon);
    Some(Entry::Ready {
        image: crate::ui::views::preview::bgra_pa_to_render_image(w, h, icon.premultiplied_bgra),
        type_name: icon.type_name,
        ink_dy,
    })
}

/// 启后台探测线程（1 个，命名 `gui-file-icon`；进程生命周期内常驻）。
///
/// 线程只持 `Arc<CacheInner>`（不与 `App` 交互）；结果经 `tx` 回投主线程泵。
fn spawn_worker(inner: Arc<CacheInner>) {
    let spawned = std::thread::Builder::new()
        .name("gui-file-icon".into())
        .spawn(move || worker_loop(inner));
    if let Err(e) = spawned {
        // 起不来线程 ⇒ 图标恒回退（功能降级，不 panic）。
        tracing::warn!("文件图标探测线程启动失败: {e}（系统图标全部走回退）");
    }
}

/// worker 主循环：等待待处理键 → [`probe_one`] → 回投一个重绘信号。
fn worker_loop(inner: Arc<CacheInner>) {
    loop {
        let key = {
            let mut st = lock(&inner.state);
            while st.pending.is_empty() {
                st = inner.cv.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            st.pending.pop_front()
        };
        let Some(key) = key else { continue };
        if !probe_one(&inner, &key) {
            continue;
        }
        let tx = lock(&inner.tx).clone();
        if let Some(tx) = tx {
            // 通道满/接收端已丢（宿主退出中）：静默丢弃——图标仍已进缓存，
            // 下一次自然重绘同样可见。
            let _ = tx.try_send(VmSignal::IconCacheFilled);
        }
    }
}

/// 锁取用（中毒 → 取内层数据继续：本模块无「锁下必须成立」的不变量）。
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 缓存条目三态。
enum Entry {
    /// 已登记、等待后台探测（`Wanted` 态只入队一次）。
    Wanted,
    /// 探测成功：位图 + 系统本地化类型名 + 可见墨迹的纵向偏移比例。
    Ready {
        image: Arc<RenderImage>,
        type_name: Option<String>,
        /// 可见墨迹中心相对位图中心的偏移**比例**（正 = 墨迹偏上 ⇒ 位图整体下移）。
        /// 见 [`visible_ink_dy_ratio`]。
        ink_dy: f32,
    },
    /// 负缓存：平台拿不到（失败/本机该类型只有通用图标）——不再重复探测。
    Missing,
}

/// 缓存 + 待探测队列（一把 `Mutex` 内一致）。
#[derive(Default)]
struct CacheState {
    entries: HashMap<IconKey, Entry>,
    /// 插入序（FIFO 淘汰依据，见 [`CACHE_CAP`]）。
    order: VecDeque<IconKey>,
    /// 待探测键。
    pending: VecDeque<IconKey>,
}

/// 渲染态/worker 线程共持的缓存内核。
struct CacheInner {
    /// 开关（`ui.file_type_icons`）：关闭 ⇒ 渲染帧直接回退、worker 空转。
    enabled: AtomicBool,
    /// 平台取图。
    source: IconSource,
    /// 主线程泵通道（worker 回投 [`VmSignal::IconCacheFilled`]）。
    tx: Mutex<Option<Arc<SignalTx>>>,
    state: Mutex<CacheState>,
    /// worker 等待「有待处理键」（`set_enabled` 亦用它唤醒重判）。
    cv: Condvar,
}

/// 缓存 Global（进程内一份；`App` 持有）。
pub(crate) struct FileIconCache {
    inner: Arc<CacheInner>,
}

impl gpui_kit::Global for FileIconCache {}

/// 渲染帧的查表结果（锁外构造元素）。
enum Outcome {
    /// 命中：位图已就绪（附可见墨迹偏移比例，见 [`visible_ink_dy_ratio`]）。
    Ready {
        image: Arc<RenderImage>,
        ink_dy: f32,
    },
    /// 回退（在飞 / 负缓存 / 新登记）。
    Fallback,
}

#[cfg(test)]
#[path = "tests/file_icon.rs"]
mod tests;
