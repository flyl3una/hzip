//! 按键录制 widget（T-KEYS-D；§9 交互改版）：设置页「快捷键」页签的**键位芯片** +
//! 按键拦截器。
//!
//! # 为什么单独成件
//!
//! 「改键必须以物理按键键入完成」这条需求落到实现上只有一个难点：**录制期间按下的
//! 键绝不能顺带触发它原本的动作**（录 `ctrl-q` 时若让绑定生效，程序当场退出）。
//! gpui 给出的唯一正解是 `App::intercept_keystrokes`（见 [`capture_keystrokes`]），
//! 而它挂的是 **App 级**拦截器——这类「全局副作用 + 显式生命周期」的件不该混进
//! 设置页渲染里（渲染函数每帧可能被调多次，退订时机很容易写错），故收口成本模块。
//!
//! # 本模块**不含任何 keymap 知识，也不持状态机**
//!
//! 这里不知道键位串、不知道动作清单、不做冲突判断、不读写配置——那些全在
//! `ui::shell::keymap`、设置页渲染与设置面板里。本模块只提供两件「哑」工具：
//!
//! - [`capture_keystrokes`]：把「下一次按键」原样交出来（并吃掉它）；
//! - [`key_capture_chip`]：一个显示态 + 点击上抛 + 可聚焦的芯片（纯视觉件）。
//!
//! # 新交互（§9.2）：录制由**面板**驱动，widget 只管「看」
//!
//! ```text
//! 点芯片        ──on_click──▶ 面板：先提交上一行 → window.focus(该行句柄) + capture_keystrokes
//! 录制中按键    ──拦截器──▶ 面板：写自己的 pending（芯片上的预览块随之刷新）
//! 焦点离开芯片  ──Context::on_blur(该行句柄)──▶ 面板：提交（冲突则只提示 / 否则写配置）
//! 录制中 Esc    ──拦截器──▶ 面板：丢弃 pending + 退订（不写配置）
//! ```
//!
//! 芯片**不碰** `AppData`、不碰 keymap、不持 `pending`：它是 `text`（调用方给的
//! 「此刻该显示的文案」）+ [`KeyChipState`]（三态配色）的纯函数。文案语义由调用方
//! 决定（空闲 = 键位显示串 / 录制中无 pending = `keymap.recording`「请按键」/
//! 未绑定 = `keymap.state.unbound`），**本模块不认识 i18n**——「请按键」到底是哪几个
//! 字不该由 widget 猜（语言一换就错）。
//!
//! 同理，`pending` 只是「录制中已按下的组合」的**镜像**：谁持有它（面板的
//! `pending: Option<String>`）、何时清（提交 / 取消 / 收口）全在面板，芯片只负责把它
//! 画出来（`kc-pending-*` 预览块）。
//!
//! # 焦点：芯片只**登记**句柄，不自己抢
//!
//! [`key_capture_chip`] 用 `track_focus(handle)` 把调用方给的句柄挂进焦点派发树，
//! **不在点击里调 `window.focus`**：「聚焦 + 进入录制态」是同一个动作的两半，拆到
//! widget 里写就会出现「聚焦了但没人挂拦截器」的中间态（§9.2 第一行把两半都交给
//! 调用方）。芯片因此也不监听 `on_blur`——失焦提交同样挂在面板侧（§9.3 的
//! `_key_blur`），那里才拿得到要写进配置的那一行。
//!
//! # 生命周期契约（本模块唯一的严重风险点）
//!
//! [`capture_keystrokes`] 返回的 [`Subscription`] **drop 即结束录制**：拦截器一旦
//! 挂上，所有按键都会被吞掉（不再有任何动作派发），所以调用方**必须**在
//! ①提交（失焦 / 点另一行芯片）、②Esc 取消、③切页签、④关面板、⑤离开设置主页签
//! 时把它 drop 掉，否则整个窗口的键盘会永久失灵。故：
//!
//! - 调用方持有一个 `Option<Subscription>`（或等价的「录制态 = Some」不变量），
//!   **不要**让它在局部作用域里提前 drop，也不要泄漏成 `mem::forget`；
//! - 「录制态」这个 UI 状态与「订阅还在」必须同生共死——一个在、另一个不在，
//!   症状分别是「键盘被吃掉但界面没显示录制」与「芯片亮着但按什么键都没反应」。

use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Size, StyleSized as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClickEvent, ElementId, FocusHandle, Hsla, Keystroke, RenderOnce, Role, SharedString,
    Subscription, ViewElement, Window, div, relative,
};

use crate::ui::spacing::component_size;

/// 挂上按键拦截器（录制态）。
///
/// gpui 的 `intercept_keystrokes` 回调在**所有** keymap 动作分发之前触发
///（`gpui-pre-0.3.8/src/window.rs` 的 `dispatch_key_event`：拦截器跑完若
/// `propagate_event == false` 就直接返回，连绑定匹配都不做），故回调里：
///
/// 1. **先** `cx.stop_propagation()` —— 这一步是「录制」与「按下即执行」的
///    分水岭；漏掉它，录 `ctrl-q` 就是退出程序，录 `ctrl-w` 就是关窗口。
/// 2. 再把 `event.keystroke`（克隆后的按键，含修饰键与 `key_char`）交给 `on_key`。
///
/// 顺序不可颠倒：`on_key` 里可能 panic/提前返回（如调用方想在非法键位上放弃），
/// 先把「这一下按键归我」落实，才谈得上后续处理。
///
/// 返回的 [`Subscription`] **drop 即结束录制**（拦截器从 App 的订阅集里摘除，
/// 此后的按键恢复正常动作派发）——生命周期契约见模块文档。
///
/// 同一时刻只应存在一个录制态：拦截器是 App 级的，挂两个会**都**收到按键
///（且都吃掉它），调用方按「单行录制」的状态机使用即可。
pub fn capture_keystrokes(
    cx: &mut App,
    mut on_key: impl FnMut(&Keystroke, &mut Window, &mut App) + 'static,
) -> Subscription {
    cx.intercept_keystrokes(move |event, window, cx| {
        // 先吞掉再上报（理由见函数文档）。
        cx.stop_propagation();
        on_key(&event.keystroke, window, cx);
    })
}

/// 键位芯片的显示态（**纯数据**；widget 不解释它的语义，只按它配色/分块）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyChipState {
    /// 这一行是否正在录制（= 拦截器已挂）。决定描边/底色是否切到 primary。
    pub recording: bool,
    /// 录制中已按下的组合（**面板 pending 的镜像**）；`None` = 还没按下任何键。
    ///
    /// 只在 [`KeyChipState::recording`] 为真时有意义：未录制时本模块**忽略**它
    ///（面板契约保证录制结束即清 pending，见 §9.3）——否则一行刚提交完的旧预览会
    /// 残留在芯片上。
    pub pending: Option<SharedString>,
    /// 该动作当前没有绑定（三态里的「已删除」）。只弱化**文字**色。
    pub unbound: bool,
}

/// 键位芯片：点它 = 进入录制；本模块只做「显示 + 点击上抛 + 可聚焦」。
///
/// `id` 同时用作 gpui 元素身份与测试锚点（`cfg!(test)` 下）：
///
/// | 锚点 | 挂在谁身上 | 何时在 |
/// |---|---|---|
/// | `kc-<id>` | 芯片本体（**点击命中点**） | 恒在 |
/// | `kc-rec-<id>` / `kc-idle-<id>` | 文案块（二者互斥） | 恒在（按录制态二选一） |
/// | `kc-pending-<id>` | 预览块（嵌在文案块内） | 录制中且 `pending` 有值 |
/// | `kc-unbound-<id>` | 未绑定态覆盖层（不画像素） | `unbound`（仅测试态） |
///
/// 文案块与预览块是**嵌套**而非互斥的：`kc-rec-*` 恒表示「这行正在录制」，
/// `kc-pending-*` 恒表示「已抓到按键」，两条断言互不干扰。
///
/// `kc-<id>` 的名字**必须稳定**：壳层的跨层集成用例点它来起录制。
///
/// `text` = 调用方给的「此刻该显示的文案」；录制中若已有 `pending`，芯片显示
/// `pending` 而**不是** `text`（实时预览，§9.2）。
///
/// # 视觉三态
///
/// | 态 | 底色 | 描边 | 文字 |
/// |---|---|---|---|
/// | 未录制 | `tokens.button` | `theme.input` | `button_foreground` |
/// | 录制中 | `tokens.button_primary` | `theme.primary` | `button_primary_foreground` |
/// | `unbound` | 同「未录制」 | 同「未录制」 | `muted_foreground` |
///
/// 前两态**逐色同** kit `ButtonVariant::Primary` / 默认按钮（`gpui-component-0.7.1`
/// `ButtonVariant::{bg,border,text}_color`）：改版前那个录制按钮就用这两个变体，故
/// 换芯片不引入新配色，深浅主题下都跟着主题走。
///
/// `unbound` 只弱化文字：绑定没了是**内容**的事，把边框/底色也变灰会让行里多出一个
/// 与相邻芯片不同「料」的方块。三态共用同一套 1px 描边与几何，故切换态不跳行。
///
/// 悬停态**不在契约内**（§9.2 只定义这三态），故只给指针形状：可点性由它和页底
/// 说明行（`setting.keymap.help`）承担。
///
/// # 为什么返回的不是一个 `div()`
///
/// 冻结签名（§9.3）里没有 `cx`，而三态配色全要主题色。gpui 里「构建期拿 cx」的
/// 官方正解是**组件**：`RenderOnce::render(self, window, cx)` 在元素构建时被调用，
/// `cx.theme()` 就在手边——kit 自己的 `Button`/`Checkbox` 也是这么写的
///（`#[derive(IntoElement)]` + `RenderOnce`）。这里手写 `IntoElement`
///（`ViewElement::new`）而不用那口 derive：它展开出的路径写死 `gpui::…`，而本
/// workspace 里 gpui 只以 `gpui_kit` 之名依赖（`hzip-gui/Cargo.toml` 没有 `gpui` 别名）。
pub fn key_capture_chip(
    id: impl Into<ElementId>,
    text: SharedString,
    state: KeyChipState,
    handle: &FocusHandle,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    KeyChip {
        id: id.into(),
        text,
        state,
        handle: handle.clone(),
        on_click: Rc::new(on_click),
    }
}

/// 芯片点击回调类型（`Rc<dyn Fn>` 直接写在字段上会触发
/// `clippy::type_complexity`；与 `settings_render::StartRecording` 同一理由）。
pub type ChipClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// 芯片本体（[`key_capture_chip`] 的真正元素类型）。
///
/// 字段全是「构建期定死、渲染期只读」的入参：芯片没有任何可变状态——这正是它不该
/// 持状态机的原因（录制态、pending、冲突都在面板里）。
struct KeyChip {
    id: ElementId,
    text: SharedString,
    state: KeyChipState,
    handle: FocusHandle,
    on_click: ChipClick,
}

impl IntoElement for KeyChip {
    type Element = ViewElement<KeyChip>;

    fn into_element(self) -> Self::Element {
        // 无实体（entity）的「一次性组件」：渲染期才拿得到 `cx`（主题色），见函数文档。
        ViewElement::new(self)
    }
}

impl RenderOnce for KeyChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let KeyChip {
            id,
            text,
            state,
            handle,
            on_click,
        } = self;

        // 锚点串先算好再 `move` 进闭包：`debug_selector` 收的是 `FnOnce() -> String`，
        // 借用 `id` 会要求它活得比闭包长（`id` 随后被移进 `.id(...)`）。
        let click_anchor = format!("kc-{id}");
        let state_anchor = format!("kc-{}-{id}", if state.recording { "rec" } else { "idle" });
        let preview_anchor = format!("kc-pending-{id}");
        let unbound_anchor = format!("kc-unbound-{id}");

        let theme = cx.theme();
        let (bg, border, fg): (Hsla, Hsla, Hsla) = if state.recording {
            (
                theme.tokens.button_primary.into(),
                theme.primary,
                theme.button_primary_foreground,
            )
        } else {
            (
                theme.tokens.button.into(),
                theme.input,
                if state.unbound {
                    theme.muted_foreground
                } else {
                    theme.button_foreground
                },
            )
        };
        let radius = theme.radius;

        // 录制中已按键 → 显示预览；否则显示调用方给的文案（含「请按键」）。
        // 未录制时的 `pending` 一律忽略（理由见 `KeyChipState::pending` 文档）。
        let preview = state.recording.then_some(state.pending).flatten();
        // 无障碍名 = 屏幕上真正显示的那串字（自建文案块后组件推不出名字）。
        let shown = preview.clone().unwrap_or_else(|| text.clone());

        let mut body = div()
            .min_w_0()
            .whitespace_nowrap()
            .text_ellipsis()
            .when(cfg!(test), |d| {
                d.debug_selector(move || state_anchor.clone())
            });
        match preview {
            Some(pending) => {
                body = body.child(
                    div()
                        // 与文案块同一套文字规则：预览是**同一个槽位**的替换内容，
                        // 长组合键不该因为换了个块就开始折行。
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .when(cfg!(test), |d| {
                            d.debug_selector(move || preview_anchor.clone())
                        })
                        .child(pending),
                );
            }
            None => body = body.child(shown.clone()),
        }

        let size = component_size();
        let chip = div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(radius)
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            // kit `Button` 同一行高（`button.rs`：`.line_height(relative(1.25))`），
            // 否则芯片文字与同一行的 ✕ 按钮文字不共基线。
            .line_height(relative(1.25))
            .cursor_pointer()
            .role(Role::Button)
            .aria_label(shown)
            .track_focus(&handle)
            .on_click(move |event, window, cx| on_click(event, window, cx))
            .when(cfg!(test), |d| {
                d.debug_selector(move || click_anchor.clone())
            })
            .child(body);
        // 几何与字号按布局密度（`component_size()`）：与工具栏按钮/页签同源。
        let chip = chip_metrics(size, chip).button_text_size(size);

        // 未绑定态的**测试观测点**：gpui 不暴露渲染后的样式（`debug_bounds` 只认
        // 锚点），而 `unbound` 的全部差别就是文字色换 muted。故额外挂一个不画任何
        // 像素的覆盖层（无底色/无边框/无处理器）带自己的锚点：它在 = 走的是弱化色
        // 分支。手法同 `ui::views::browse` 的 `list-selected-layer`（那里也是为了让
        // 「底色分支」可断言）。
        chip.when(state.unbound && cfg!(test), |d| {
            d.relative().child(
                div()
                    .absolute()
                    .inset_0()
                    .debug_selector(move || unbound_anchor.clone()),
            )
        })
    }
}

/// 芯片几何 = **工具按钮几何**（由 `component_size()` 决定）。
///
/// 逐条抄 `gpui-component-0.7.1/src/button/button.rs` 普通按钮（有文字）那一支：
/// XSmall `h_5/px_1` · Small `h_6/px_2` · Medium `h_8/px_2p5` · Large `h_8/px_3`
///（`Size::Size(_)` 档 `component_size()` 不会返回，落 Medium 口径）。与页签同一张
/// 表（`ui::bar::tab_bar::tab_metrics`）⇒ 芯片与工具栏按钮/页签逐像素等高。
fn chip_metrics<T: Styled>(size: Size, chip: T) -> T {
    match size {
        Size::XSmall => chip.h_5().px_1(),
        Size::Small => chip.h_6().px_2(),
        Size::Large => chip.h_8().px_3(),
        _ => chip.h_8().px_2p5(),
    }
}

#[cfg(test)]
#[path = "tests/key_capture.rs"]
mod tests;
