//! 数值步进控件（设置-常规 ▸ 界面字号）：`[−] 值[单位] [+]`。
//!
//! # 为什么自绘
//!
//! 设置里的数值行要的是**行内三元组**：减号、当前值、加号。kit 里没有对得上的
//! 件——`Slider` 是拖拽交互（无步长、无「值域内归一」语义，字号这种 10~24 的
//! 小整数域上拖拽精度也不合适），`Stepper` 是步骤指示器（不是输入控件）。
//! 故只做最小自绘：两个 kit 按钮（ghost + xsmall，与浏览列块的紧凑按钮同款）
//! 夹一个定宽文本。按钮本身复用 kit 的禁用/悬停/焦点语义，不自绘任何状态。
//!
//! # 行为契约（本控件唯一对外语义）
//!
//! - **值域闭区间** `[min, max]`：`value` 到 `min` ⇒ 减号 `disabled`；到 `max`
//!   ⇒ 加号 `disabled`。kit 按钮禁用时**摘掉 `on_click`**（gpui-base 只在
//!   非禁用时挂处理器），故回调里不必再判界，也不存在「禁用但仍收到回调」。
//! - **先归一后回调**：步长不整除值域时（如 `10..=24`、`step = 5`）越界是常态，
//!   `value ± step` 先夹进 `[min, max]` 再上行 —— 调用方拿到的永远是合法目标值，
//!   不需要在设置面板里再兜一次边界（两处各写一份边界语义必然漂移）。
//! - 回调第二参是 `&mut App`：设置面板在回调里写 store 后要 `refresh(cx)` 重读
//!   活读视图，故必须透传（这正是 gpui `on_click` 给的东西）。
//!
//! # 值文本的两条视觉约束（都为了「点得准」）
//!
//! 值文本 `min_w(sp(6.))`（= 1.5rem，随密度/字号缩放）且居中：位数变化
//! （`10px` → `24px`）不得让两侧按钮左右位移 —— 按钮一移位，第二次点击就会
//! 落到空白处（用户体感 = 「连点没反应」）。

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, ElementId, IntoElement};

use super::icon::AppIcon;
use crate::ui::constant::NUMBER_VALUE_MIN_STEPS;
use crate::ui::spacing::{component_size, sp};

/// 步进回调：新值 + `&mut App`（后者供设置面板在回调里 `refresh(cx)` 重读活读视图）。
///
/// 具名类型别名而非就地写 `Rc<dyn Fn(u32, &mut App)>`：同一签名在两个函数上出现，
/// 别名既过 `clippy::type_complexity`，也让「回调契约」只有一个可读的定义处。
pub type OnNumberChange = Rc<dyn Fn(u32, &mut App)>;

/// 数值步进控件：`[−] 值[单位] [+]`。
///
/// `id` 同时用作 gpui 元素身份与测试锚点（`num-minus-<id>` / `num-value-<id>` /
/// `num-plus-<id>`）；`suffix` 直接拼在值后（如 `"px"`）。
///
/// `min <= max` 是前置条件（值域来自设置注册表的常量）；`step = 0` 时两个按钮
/// 都回传原值（回调收到不变值，调用方幂等重读，无害）。
pub fn number_stepper(
    id: impl Into<ElementId>,
    value: u32,
    min: u32,
    max: u32,
    step: u32,
    suffix: &str,
    on_change: OnNumberChange,
) -> impl IntoElement {
    let key = id.into().to_string();
    // 归一：`clamp` 在 `min > max` 时会 panic，故此处依赖前置条件（见函数文档）——
    // 换成 `min().max()` 会触发 clippy::manual_clamp，而夹取语义本来就该用 clamp 表达。
    let target = |next: u32| next.clamp(min, max);

    let minus = on_change.clone();
    let plus = on_change;
    let value_anchor = format!("num-value-{key}");

    h_flex()
        .items_center()
        .gap(sp(1.))
        .child(step_button(
            &key,
            "minus",
            AppIcon::Minus,
            target(value.saturating_sub(step)),
            value <= min,
            minus,
        ))
        .child(
            h_flex()
                .justify_center()
                .min_w(sp(NUMBER_VALUE_MIN_STEPS))
                .text_sm()
                .child(format!("{value}{suffix}"))
                .when(cfg!(test), move |this| {
                    this.debug_selector(move || value_anchor)
                }),
        )
        .child(step_button(
            &key,
            "plus",
            AppIcon::Plus,
            target(value.saturating_add(step)),
            value >= max,
            plus,
        ))
}

/// 一个步进按钮（`side` = `minus` / `plus`，同时充当元素 id 与测试锚点后缀）。
///
/// `target` 由调用方**先归一**后传入（见 [`number_stepper`]）：边界语义只此一处，
/// 回调侧不再判界。
fn step_button(
    key: &str,
    side: &str,
    icon: AppIcon,
    target: u32,
    disabled: bool,
    on_change: OnNumberChange,
) -> Button {
    let anchor = format!("num-{side}-{key}");
    Button::new(anchor.clone())
        .with_size(component_size())
        .ghost()
        .xsmall()
        .icon(icon)
        .disabled(disabled)
        .when(cfg!(test), move |this| this.debug_selector(move || anchor))
        .on_click(move |_, _, cx| on_change(target, cx))
}

#[cfg(test)]
#[path = "tests/number.rs"]
mod tests;
