//! 面包屑（目录导航栏：`档案名 / 层级路径`；段点击 → `on_crumb` 上行）。
//!
//! # 段渲染用 kit `Button`（`Text` 变体）而不是 `BreadcrumbItem`
//!
//! 用户口径 2026-09-24：「导航栏里某一层的名称过长时需要将名称中间替换为 `...`
//! 并且支持鼠标移动上去后浮现具体名称」。kit 的 `BreadcrumbItem` 只有
//! `new/disabled/on_click`（`Styled` 仅能改样式）——**没有 tooltip 槽位**，而
//! div 级 tooltip 的实现（`ManagedTooltipExt` / `Root::tooltip_overlay`）都是
//! kit 内部可见性（`pub(crate)`），外部接不上。故改用 `ButtonVariant::Text`
//! ——文档原文「no padding look like a normal text」——它同时提供 `.tooltip()`、
//! 点击回调与禁用态，观感与纯文本段一致，且 tooltip 机制是本应用已在用
//! （帮助图标/工具栏/页签关闭钮）的成熟路径。
//!
//! 段点击 → `WeakEntity<T: BrowseHandlerSource>` 就地调 `handlers.on_crumb`
//! （见 browse.rs 模块级「事件上行」）。
//!
//! # 名称过长：中段省略 + 悬停显全名
//!
//! 省略「中间」而不是尾部（用户明确要求）：层名/档案名的区分度常在两处——
//! 头部（`initramfs-tools…`）与**尾部**（`….cpio.gz`），只留头部会把
//! `huge-init.cpio.gz` 与 `huge-init.tar.gz` 显示成同一个。超过
//! [`CRUMB_MAX_CHARS`] 个**字符**时按 [`middle_ellipsis`] 折叠，全名经 tooltip
//! 浮现（未折叠的段不挂 tooltip——没有额外信息）。

use crate::ui::spacing::sp;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, Size, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, SharedString, WeakEntity, div, relative};

use crate::ui::contracts::{Crumb, ShellHandlers};
use crate::ui::views::browse::BrowseHandlerSource;

/// 单段最大显示字符数（超出走中段省略；全名在 tooltip 里）。
///
/// 24 个字符足够放下 `initramfs-tools_x86_64.cpio.gz` 这类常见层名，又不至于
/// 让一段吃掉整条导航栏；超长档案名（实测 70+ 字符）折叠后仍能认出来。
pub const CRUMB_MAX_CHARS: usize = 24;

/// 面包屑（档案根 + 逐段链；段点击 → handlers.on_crumb）。
///
/// 分隔符是**文本 `/`**（不是图标）：与段名同字号、同行高 ⇒ 共用基线、视觉居中
/// （2026-10-14 用户口径，缘由写在分隔符那一段的注释里）。
pub fn breadcrumb<T: BrowseHandlerSource + 'static>(
    cx: &App,
    crumbs: &[Crumb],
    shell: &WeakEntity<T>,
) -> AnyElement {
    let last = crumbs.len().saturating_sub(1);
    let mut row = h_flex()
        .debug_selector(|| "crumb-row".into())
        .gap(sp(1.5))
        .text_sm()
        .items_center();
    for (ix, crumb) in crumbs.iter().enumerate() {
        let seg = crumb.seg.clone();
        let level = crumb.level;
        let short = middle_ellipsis(&crumb.label, CRUMB_MAX_CHARS);
        let is_last = ix == last;
        let mut btn = Button::new(SharedString::from(format!("crumb-{ix}-{level}")))
            .text()
            .with_size(Size::Small)
            .label(SharedString::from(short));
        // 只有折叠过的段才挂 tooltip（全名 = 额外信息；未折叠时鼠标悬浮给同一句话
        // 只是噪音）
        if crumb.label.chars().count() > CRUMB_MAX_CHARS {
            btn = btn.tooltip(SharedString::from(crumb.label.clone()));
        }
        if is_last {
            // 当前位置：不再可点（点击 = 原地刷新）
            btn = btn.disabled(true);
        } else {
            let shell = shell.clone();
            btn = btn.on_click(move |_, _, cx| {
                let seg = seg.clone();
                let _ = shell.update(cx, |s, _shell_cx| {
                    forward_crumb::<T>(s, seg, level);
                });
            });
        }
        row = row.child(btn);
        if !is_last {
            // 分隔符：文本 `/`（用户口径 2026-10-14）。
            //
            // # 为什么不再用 kit `ChevronRight` 图标（原实现）
            //
            // 图标是 14px 方盒，`items_center` 只保证**盒**居中；chevron 的 SVG 墨迹
            // 在盒内偏上 ⇒ 「>` 看着比段名高一截」。换成文本 `/` 后与段名**同字号、
            // 同行高**（`text_sm` + `line_height(relative(1.25))`，即 kit `Button`
            // `Size::Small` 的那套——见 gpui-component `sizing.rs::button_text_size`
            // 与 `button.rs` 的 `.line_height(relative(1.25))`）⇒ 与段名共用同一基线，
            // 视觉上天然居中；改任一侧字号/行高都会立刻错开，故两者写在一起。
            row = row.child(
                div()
                    .debug_selector(move || format!("crumb-sep-{ix}"))
                    .text_sm()
                    .line_height(relative(1.25))
                    .text_color(cx.theme().muted_foreground)
                    .child("/"),
            );
        }
    }
    row.into_any_element()
}

/// 中段省略：`head…tail`（保头也保尾）。
///
/// 折叠条件与计数都按**字符**（`chars()`），故中文段名不会被切坏 UTF-8；
/// 总长（含 `…`）严格 ≤ `max`；短于等于 `max` 的串**原样返回**（不做无谓拷贝
/// 之外的任何改动）。
pub fn middle_ellipsis(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 2 {
        return s.to_string();
    }
    // 尾部占 1/3 **向上取整**：`max=24` 时尾部 = 8 字符，刚好容下 `.cpio.gz`
    // 这类扩展名（尾部区分度就靠它）；头部占其余。
    let tail = (max - 1).div_ceil(3);
    let head = max - 1 - tail;
    let mut out = String::with_capacity(s.len());
    out.extend(s.chars().take(head));
    out.push('…');
    if tail > 0 {
        out.extend(s.chars().skip(n - tail));
    }
    out
}

/// 就地调用 handlers.on_crumb（经 [`BrowseHandlerSource`] 取字段）。
fn forward_crumb<T: BrowseHandlerSource>(source: &T, seg: String, level: usize) {
    let handlers: &ShellHandlers = source.browse_handlers();
    (handlers.on_crumb)(Crumb {
        label: String::new(),
        seg,
        // 段点击必须带上**该段所属的层**：嵌套会话下面包屑横跨多个会话，
        // 光有 seg（"a/b"）不足以判断跳到哪一层（两层可能同名）。
        level,
    });
}

#[cfg(test)]
#[path = "tests/breadcrumb.rs"]
mod tests;
