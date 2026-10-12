//! 组件封装：修正 gpui-component 复选框的**行高与纵向对齐写死**缺陷。
//!
//! # 问题
//!
//! `gpui_component::checkbox::Checkbox` 在 `render` 里写死两件事：
//!
//! 1. `.line_height(relative(1.))`——**行框高度 = 字号**。而字体的
//!    `ascent + |descent|` 通常是 1.15~1.45 em（实测本机 14px 字号：
//!    ascent 14.35 + descent 3.85 = 18.20px）。行框只有 14px 时，字形下降部
//!    （`g`/`y`/`p`/`j`/`q`）超出元素边界，被内容遮罩裁掉：**`gz` 的 `g` 看起来像 `a`**
//!    （用户实测报告）。同一缺陷在 `radio`/`button`/`tab`/`tag` 等组件里同样存在，
//!    但凡标签为纯中文时下降部不显形，故长期未被发现；文件关联页的扩展名标签
//!    （`tgz`/`gz`/`tbz2`…）是第一个大面积暴露它的位置。
//! 2. `.items_start()`——**方框与标签顶边对齐**。方框（`Size::Medium` ⇒
//!    `size_4()` = 16px）与标签行框（16px 字号 × 行高 1.5 = 24px）顶边相齐
//!    ⇒ 方框中心比文字中心**高 4px**（用户实测：「勾选框要高一些，文字要低一些」，
//!    在文件关联页成片出现）。
//!
//! # 对策
//!
//! 组件实现了 `Styled`，且其 `render` 末尾的 `refine_style(&self.style)` 在
//! 上述写死项**之后**应用，故我们传入的样式会覆盖它们（但无法用 `None`
//! 「清除」，`StyleRefinement` 的合并不支持取消）：
//!
//! - 行高：**不能**只改根节点——组件把标签包在 `overflow_hidden` 的容器里，
//!   且标签 div 自己写死 `.line_height(relative(1.))`，外层 `refine_style`
//!   覆盖不到它（实测改外层行高后标签框仍 16px）。故标签经 **children 槽**
//!   自绘，用显式行高 [`CHECKBOX_LINE_HEIGHT`]。
//! - 对齐：根节点传 `.items_center()`（覆盖组件的 `items_start`）⇒ 方框与
//!   自绘标签块垂直居中。
//! - 字号：0.7.1 起组件只把 `Size → 字号` 挂在根节点、且 `Medium ⇒ text_sm`
//!   （行号见 [`checkbox`] 实现处），故标签块自己钉映射（`Small` → `text_sm`，
//!   其余 → `text_base`）——自绘标签的**行框按自身字号算**，字号不钉住行高就跟着漂。
//!
//! `ui/widgets` 下**所有**复选框都必须走 [`checkbox`]，不要直接 `Checkbox::new`——
//! 否则该处又会退回 1.0 行高 + 顶边对齐（`crates/hzip-gui/tests/` 无 grep 类守卫，靠评审）。

use crate::ui::constant::CHECKBOX_OPTICAL_SHIFT;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::prelude::*;
use gpui_kit::{ElementId, SharedString, div, px, relative};

/// 复选框行高倍率（**已夹下限**）：取配置 `[ui].line_height`，下限 = 字形盒常量
/// [`crate::ui::spacing::GLYPH_BOX_EM`]（覆盖主流 UI 字体的 `ascent + |descent|`）。
///
/// 历史：曾写死 1.5 —— 那是「行框 ≥ 字形盒」的粗保险，代价是**密集勾选列表
/// 每行上下各多出约 2px 死白**（用户实测：紧凑档勾选行看着仍很空）。改为跟配置
/// 走的字形盒下限后，出厂配置下 = 1.35（16px ⇒ 21.6px 行框，比 24 紧 2.4px）。
///
/// 回归锚点：`ui::views::settings_layout::tests::assoc_labels_fit_font_metrics`
/// 用**真实字体度量**断言标签行框 ≥ ascent+|descent|——换字体后若仍被裁，该用例会失败。
pub fn checkbox_line_height() -> f32 {
    crate::ui::spacing::line_height_em()
}

/// 造一个行高正常、方框与文字**光学同轴**的复选框（替代直接 `Checkbox::new` + `.label(...)`）。
///
/// # 纵向光学对齐
///
/// 方框对齐**字身框中心**（`items_center`），眼睛看的却是**字面**中心 —— 本机字体的
/// 字面中心比字身框中心低 [`CHECKBOX_OPTICAL_SHIFT`]（推导见该常量的文档），于是「文字看起来
/// 偏下、方框偏高」（用户实测：文件关联页拉丁扩展名、上下文菜单页中文勾选项都如此）。
///
/// 对策：标签块加**下内边距 `2 × CHECKBOX_OPTICAL_SHIFT`** —— 行高由标签块决定、方框随行
/// 垂直居中 ⇒ 方框恰好相对文字下移 `CHECKBOX_OPTICAL_SHIFT`；标签行框本身高度不变。
///
/// 该 nudge 对同页所有复选框是**同一个常量**（不按标签逐个算）——同排复选框的方框
/// 必须落在同一条水平线上，逐个对齐会把一排方框做成锯齿。
pub fn checkbox(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Checkbox {
    let label = label.into();
    let a11y = label.clone();
    // 测试锚点（`cfg!(test)`）：标签**文本**边界——「方框与文字同轴」回归用。
    // 锚点只包文本本身（不含下内边距），故其高度恒为行框高。
    let anchor = format!("cbl-{a11y}");
    // 字号按控件尺寸档映射（0.6 观感）：`Small`（紧凑）→ `text_sm`，其余 → `text_base`。
    //
    // 为什么必须自己补：gpui-kit 0.7.1 的 `Checkbox::render` 只把 `Size → 字号`
    // 挂在**根节点**上（`gpui-component-0.7.1/src/checkbox.rs:275-277` 的
    // `input_text_size`），其中的 `Size::Medium ⇒ text_sm`
    // （`gpui-component-0.7.1/src/sizing.rs:245-251`）——而本仓 0.6 期的断言是按
    // 「Medium ⇒ 标签 1rem」写的（`tests/checkbox.rs` 的 `want_h = rem × 行高`），
    // 故 0.7.1 下标准档标签会掉一档（1rem → 0.875rem）。本组件自绘标签、行高又按
    // **自身字号**算（`line_height(relative(...))`），字号一掉行框就矮约 2.7px，
    // 「方框与文字同轴」的锚点（`tests/checkbox.rs::box_centers_on_text_ink`）会红。
    // 故在这一层显式钉回映射。
    //
    // 挂标签块（inner）而非根节点：根节点的字号会被组件自己的 `input_text_size`
    // 按 `Size` 重算，方框尺寸走 `rems(...)`（与字号无关），只钉标签块最不易漂。
    let inner = div()
        .map(|d| match crate::ui::spacing::component_size() {
            gpui_kit::component::Size::Small => d.text_sm(),
            _ => d.text_base(),
        })
        .line_height(relative(checkbox_line_height()))
        .when(cfg!(test), |t| t.debug_selector(move || anchor.clone()))
        .child(label);
    let text = div().pb(px(CHECKBOX_OPTICAL_SHIFT * 2.)).child(inner);
    // 固有尺寸随布局密度（紧凑 → `Size::Small`：方框与内边距一起收）——
    // 与全 UI 其它控件同一口径（组件库按 `Size` 算的那层，`sp()` 管不到）
    Checkbox::new(id)
        .with_size(crate::ui::spacing::component_size())
        .items_center()
        .accessibility_label(a11y)
        .child(text)
}

#[cfg(test)]
#[path = "tests/checkbox.rs"]
mod tests;
