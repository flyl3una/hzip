use super::*;
use std::cell::RefCell;

use gpui_kit::component::v_flex;
use gpui_kit::{Context, Render, TestAppContext, Window};

/// 探针值域（与生产 `hzip_config::FONT_SIZE_MIN/MAX` 同形，故意不复用常量：
/// 本用例只证控件自身语义，配置改值域不该让这里变红）。
const MIN: u32 = 10;
const MAX: u32 = 24;

/// 探针宿主：一台步进控件，回调把收到的值记进 `hits`。
struct Host {
    value: u32,
    step: u32,
    hits: Rc<RefCell<Vec<u32>>>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let hits = self.hits.clone();
        v_flex().w_full().child(number_stepper(
            "fs",
            self.value,
            MIN,
            MAX,
            self.step,
            "px",
            Rc::new(move |v: u32, _cx: &mut App| hits.borrow_mut().push(v)),
        ))
    }
}

/// 造一台控件 → 绘制一帧 → 真实点击 `anchor` → 返回**回调收到的值序列**。
///
/// 断言面恒为这个返回值（行为）；像素只用来定位命中点，不作断言。
fn click_anchor(cx: &mut TestAppContext, value: u32, step: u32, anchor: &'static str) -> Vec<u32> {
    let hits: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let host = hits.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view: gpui_kit::AnyView = cx
            .new(move |_| Host {
                value,
                step,
                hits: host,
            })
            .into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let bounds = vtcx
        .debug_bounds(anchor)
        .unwrap_or_else(|| panic!("锚点 {anchor} 未绘制（控件未渲染或锚点未挂）"));
    vtcx.simulate_click(bounds.center(), gpui_kit::Modifiers::default());
    hits.borrow().clone()
}

/// 中间值点加号 → 回调收到 `value + step`（步长真的参与运算，不是写死 +1）。
#[gpui_kit::test]
fn number_stepper_plus_adds_step_from_middle(cx: &mut TestAppContext) {
    assert_eq!(click_anchor(cx, 16, 4, "num-plus-fs"), vec![20]);
}

/// 中间值点减号 → 回调收到 `value - step`。
#[gpui_kit::test]
fn number_stepper_minus_subtracts_step_from_middle(cx: &mut TestAppContext) {
    assert_eq!(click_anchor(cx, 16, 4, "num-minus-fs"), vec![12]);
}

/// 值在上界：加号禁用（点击无回调）。同一次探针里减号仍可点 —— 这才排得掉
/// 「锚点没挂 / 点击没命中」造成的假绿。
#[gpui_kit::test]
fn number_stepper_plus_is_disabled_at_max(cx: &mut TestAppContext) {
    assert!(
        click_anchor(cx, MAX, 4, "num-plus-fs").is_empty(),
        "值已在上界，加号应禁用（点击不得回调）"
    );
    assert_eq!(
        click_anchor(cx, MAX, 4, "num-minus-fs"),
        vec![MAX - 4],
        "同一锚点体系下减号必须仍可点（否则上面那条是假绿）"
    );
}

/// 值在下界：减号禁用（点击无回调）；加号仍可点（同上，反假绿）。
#[gpui_kit::test]
fn number_stepper_minus_is_disabled_at_min(cx: &mut TestAppContext) {
    assert!(
        click_anchor(cx, MIN, 4, "num-minus-fs").is_empty(),
        "值已在下界，减号应禁用（点击不得回调）"
    );
    assert_eq!(
        click_anchor(cx, MIN, 4, "num-plus-fs"),
        vec![MIN + 4],
        "同一锚点体系下加号必须仍可点（否则上面那条是假绿）"
    );
}

/// 步长不整除值域：先归一后回调，不越界。
#[gpui_kit::test]
fn number_stepper_off_grid_step_clamps_into_range(cx: &mut TestAppContext) {
    assert_eq!(
        click_anchor(cx, MAX - 1, 5, "num-plus-fs"),
        vec![MAX],
        "`max - 1` 加 5 应夹到 max 再回调"
    );
    assert_eq!(
        click_anchor(cx, MIN + 2, 5, "num-minus-fs"),
        vec![MIN],
        "`min + 2` 减 5 应夹到 min 再回调（下界同规则）"
    );
}
