//! 表单/输入通道构造：Select/Input/NumberInput/Slider 的窗口亲和实体预构建。
//!
//! 从 `dialogs/mod.rs` 迁出（T-STD：`mod.rs` 只做声明与再导出）；内容逐字未改。
//! 由 T-G30 在 `open_dialog` builder 内调用，产物存入 props 供渲染函数消费。

use crate::ui::contracts::DialogEvent;
use gpui_kit::component::IndexPath;
use gpui_kit::component::input::{InputState, MaskPattern};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, Window};

// SelectState 组装：字符串白名单 → SearchableVec delegate；选中确认 →
// `on(DialogEvent)` 上抛（T-G11 §3.2 事件映射）。
//
// 需要 `&mut Window`（SelectState 窗口亲和）——由 T-G30 在 `open_dialog`
// builder 内调用，产物存入 props 供渲染函数消费。
#[allow(dead_code)] // T-G30 接线消费（预构建 Select 实体放入 props）
pub(crate) fn select_string_state(
    items: Vec<&'static str>,
    selected_ix: usize,
    window: &mut Window,
    cx: &mut App,
    tx: &smol::channel::Sender<DialogEvent>,
    on_pick: impl Fn(usize) -> DialogEvent + 'static,
) -> Entity<SelectState<SearchableVec<String>>> {
    // 候选项 = label key 的**本地化文案**（`opt.format.7z` → `*.7z`）。
    //
    // 直接塞 label key 的话下拉里就是一串 `opt.format.zip`（用户看到的是 i18n
    // 内部键，不是格式名）。下标映射仍按同一份序列（翻译不改变顺序，也不去重），
    // 故 Confirm 回查下标可靠。
    let labels: Vec<String> = items.iter().map(|k| crate::tr(k)).collect();
    let vec: SearchableVec<String> = SearchableVec::new(labels.clone());
    let state = cx.new(|cx| SelectState::new(vec, Some(IndexPath::new(selected_ix)), window, cx));
    let tx = tx.clone();
    cx.subscribe(
        &state,
        move |_sel: Entity<SelectState<SearchableVec<String>>>,
              ev: &SelectEvent<SearchableVec<String>>,
              _cx: &mut App| {
            if let SelectEvent::Confirm(Some(value)) = ev
                && let Some(ix) = labels.iter().position(|s| s == value.as_str())
            {
                // ⚠ 事件**必须送出**：旧实现只 `on_pick(ix);` 丢掉了返回值——
                // 下拉「选了等于没选」（格式/方式/分卷/线程… 全停在初始值，
                // 于是不管选什么格式，输出永远是初始的那个 zip）。
                let _ = tx.try_send(on_pick(ix));
            }
        },
    )
    .detach();
    state
}

// InputState 组装（T-G56：输出/排除/密码真输入的预构建通道）。
//
// 需要 `&mut Window`（InputState 窗口亲和）——由 T-G30 在 `open_dialog`
// builder 内调用，产物存入 props。
#[allow(dead_code)] // T-G30 接线消费（预构建 Input 实体放入 props）
pub(crate) fn input_state(
    initial: &str,
    masked: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let state = cx.new(|cx| InputState::new(window, cx).masked(masked));
    if !initial.is_empty() {
        state.update(cx, |s, cx| s.set_value(initial, window, cx));
    }
    state
}

// NumberInput 配套 InputState（分卷字节；`MaskPattern::Number` 数字校验，
// "0" = 不分卷）。
#[allow(dead_code)] // T-G30 接线消费
pub(crate) fn number_input_state(
    initial: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let state = cx.new(|cx| InputState::new(window, cx).mask_pattern(MaskPattern::number(None)));
    if !initial.is_empty() {
        state.update(cx, |s, cx| s.set_value(initial, window, cx));
    }
    state
}

/// 压缩级别滑块状态（09 §6.1）：**6 档离散**。
///
/// `SliderState` 自带 `step`，故离散档位是原生能力（`min(0).max(5).step(1.)`），
/// 不需要自绘吸附逻辑。档位数取 [`crate::app::labels::LEVEL`] 的长度——
/// 白名单加档位时滑块自动跟随，不会与下拉/配置值域脱节。
pub(crate) fn level_slider_state(
    level_ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> Entity<gpui_kit::component::slider::SliderState> {
    let _ = window; // SliderState 无窗口亲和（对比 InputState/SelectState）
    let last = (crate::app::labels::LEVEL.len() - 1) as f32;
    let ix = (level_ix as f32).clamp(0., last);
    cx.new(|_| {
        gpui_kit::component::slider::SliderState::new()
            .min(0.)
            .max(last)
            .step(1.)
            .default_value(ix)
    })
}
