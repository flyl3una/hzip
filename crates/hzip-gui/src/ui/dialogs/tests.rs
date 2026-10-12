//! dialogs 目录测试（T-G56 验收 + T-G11 用例迁移；T-GUI T10 随拆分迁入）。
//!
//! T-STD 2026-10-08：本文件此前是**孤儿**——`dialogs/mod.rs` 从未声明它，于是
//! 自某次重构起就再没被编译过（与新结构脱节：`ExtractDialogProps` 已更名
//! `ExtractPanelProps`、`input_state` 等迁至 `fields`、内嵌的 `mod tests` 与外层
//! 文件重名）。现由 `dialogs/mod.rs` 的 `#[cfg(test)] mod tests;` 声明接活：
//! 摊平内嵌模块、import 路径对齐现状；失效断言（已删类型的 Clone 断言）随类型
//! 更名同步修正。

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{Entity, Window};

use super::{DialogEvent, ExtractPanelProps, PasswordDialogProps, input_state, number_input_state};
use crate::ui::spacing::component_size;

// read_only_field 合法保留断言（grep 契约：自绘共用件清零）。
// 注意：断言串须拆分拼接，避免 include_str! 把本测试自身也算命中。
#[test]
fn shared_helpers_renamed_contract() {
    // T-STD：`read_only_field` 等共用件随拆分迁至 `shared.rs` / `fields.rs`——
    // 契约扫描面同步扩到全目录的自绘件来源（否则断言会因漏扫而假红）。
    let source = concat!(
        include_str!("mod.rs"),
        include_str!("shared.rs"),
        include_str!("fields.rs"),
        include_str!("compress.rs"),
        include_str!("extract.rs"),
        include_str!("password.rs"),
        include_str!("conflict.rs"),
    );
    let form_row_fn = concat!("fn ", "form_row");
    let text_display_fn = concat!("fn ", "text_display");
    assert!(
        !source.contains(form_row_fn),
        "form_row 自绘件必须清零（T-G56 #7）"
    );
    assert!(
        !source.contains(text_display_fn),
        "text_display 假输入必须清零（T-G56 #8）"
    );
    assert!(
        source.contains("fn read_only_field"),
        "read_only_field 合法保留（压缩对象摘要行）"
    );
}

// 面板 props 可 Clone（T-G30 快照通道要求）。
#[test]
fn dialog_props_are_clonable() {
    fn assert_clone<T: Clone>() {}
    assert_clone::<ExtractPanelProps>();
    assert_clone::<PasswordDialogProps>();
}

// 输入态构造通道（input_state/number_input_state 预构建件；窗口亲和——
// headless 窗口内构造，断言初值落位 + 密文标志不影响值读写）。
// 测试宿主：Entity 不可直接作根视图，包一层 Render 宿主；经
// `add_window_view` 拿 VisualTestContext（update 给 &mut Window+&mut App）。
struct InputHost {
    state: Entity<InputState>,
}

impl Render for InputHost {
    fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        Input::new(&self.state).with_size(component_size())
    }
}

fn input_host(
    cx: &mut gpui_kit::TestAppContext,
    initial: &str,
    masked: bool,
) -> (Entity<InputHost>, gpui_kit::VisualTestContext) {
    let initial = initial.to_owned();
    let (host, cx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        InputHost {
            state: input_state(&initial, masked, window, cx),
        }
    });
    (host, cx.clone())
}

#[gpui_kit::test]
fn input_state_channel_builds_and_round_trips(cx: &mut gpui_kit::TestAppContext) {
    let (host, vtcx) = input_host(cx, "F:/out/demo", false);
    let state = vtcx.read_entity(&host, |h, _| h.state.clone());
    assert_eq!(
        vtcx.read_entity(&state, |s, _| s.value().to_string()),
        "F:/out/demo"
    );

    let (masked_host, mut vtcx) = input_host(cx, "", true);
    let masked = vtcx.read_entity(&masked_host, |h, _| h.state.clone());
    vtcx.update(|window, cx| {
        masked.update(cx, |s, cx| s.set_value("pw", window, cx));
    });
    assert_eq!(
        vtcx.read_entity(&masked, |s, _| s.value().to_string()),
        "pw"
    );
}

// Input 值 → DialogEvent 构造（CompressSplit/CompressExcludes 从 InputState
// 值构造——T-G56 §3.4 契约零变化）。
#[gpui_kit::test]
fn input_value_maps_to_dialog_events(cx: &mut gpui_kit::TestAppContext) {
    let (host, mut vtcx) = input_host(cx, "1048576", false);
    let state = vtcx.read_entity(&host, |h, _| h.state.clone());
    let mut events: Vec<DialogEvent> = Vec::new();
    vtcx.update(|window, cx| {
        events.push(DialogEvent::CompressSplit(
            state.read(cx).value().to_string(),
        ));
        state.update(cx, |s, cx| s.set_value("*", window, cx));
        events.push(DialogEvent::CompressExcludes(
            state.read(cx).value().to_string(),
        ));
    });
    assert_eq!(
        events,
        vec![
            DialogEvent::CompressSplit("1048576".into()),
            DialogEvent::CompressExcludes("*".into()),
        ]
    );
}

// 密码提交即清空（T-G11 用例迁移；02 §1.2——组件不持有密码副本）：
// 提交路径 = 读值 → 上抛 PasswordSubmit → set_value("") 清零内部态。
#[gpui_kit::test]
fn password_submit_clears_internal_state(cx: &mut gpui_kit::TestAppContext) {
    let (host, mut vtcx) = input_host(cx, "pw1", true);
    let state = vtcx.read_entity(&host, |h, _| h.state.clone());
    assert_eq!(
        vtcx.read_entity(&state, |s, _| s.value().to_string()),
        "pw1"
    );

    let events: std::rc::Rc<std::cell::RefCell<Vec<DialogEvent>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let probe = events.clone();
    vtcx.update(|window, cx| {
        let text = state.read(cx).value().to_string();
        probe.borrow_mut().push(DialogEvent::PasswordSubmit(text));
        // 提交即清空（02 §1.2）
        state.update(cx, |s, cx| s.set_value("", window, cx));
    });
    assert_eq!(
        vtcx.read_entity(&state, |s, _| s.value().to_string()),
        "",
        "提交后内部态必须清空"
    );
    assert_eq!(
        &*events.borrow(),
        &[DialogEvent::PasswordSubmit("pw1".into())],
        "PasswordSubmit 事件值 = 提交前输入"
    );
}

// 分卷输入态数字校验通道（MaskPattern::number 装配不 panic；步进/钳制
// 语义归组件库测试）。
#[gpui_kit::test]
fn number_input_state_builds(cx: &mut gpui_kit::TestAppContext) {
    let (host, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        InputHost {
            state: number_input_state("0", window, cx),
        }
    });
    let state = vtcx.read_entity(&host, |h, _| h.state.clone());
    assert_eq!(vtcx.read_entity(&state, |s, _| s.value().to_string()), "0");
}
