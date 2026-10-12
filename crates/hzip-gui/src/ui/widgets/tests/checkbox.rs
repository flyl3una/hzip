use super::*;
use gpui_kit::component::v_flex;
use gpui_kit::{IntoElement, Render, TestAppContext, Window, px};

/// 探针宿主：一个**矮标签**（8px）的复选框，行与标签都带锚点。
///
/// # 为什么用矮标签才能测出「同轴」
///
/// 方框（16px）在 kit 组件内部、不注册 `debug_selector`，测试拿不到它的边界
/// （`debug_bounds` 只认自己注册的锚点）。但行向对齐 `align_items` 决定矮子
/// 元素落点：标签矮于方框时，`items_start` ⇒ 标签贴顶（中心在行中心**上方**），
/// `items_center` ⇒ 标签居中（中心 = 行中心）。故「标签中心 vs 行中心」就是
/// 方框与文字是否同轴的证据。
struct Tiny {
    /// `true` = 本模块的构造（`items_center`）；`false` = 组件原生（写死 `items_start`）
    ours: bool,
}

impl Render for Tiny {
    fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        let text = div()
            .line_height(relative(checkbox_line_height()))
            .h(px(8.))
            .debug_selector(|| "tiny-lbl".into())
            .child("gz");
        let cb = if self.ours {
            Checkbox::new("tiny").items_center().child(text)
        } else {
            Checkbox::new("tiny").child(text)
        };
        v_flex()
            .w_full()
            .child(div().debug_selector(|| "tiny-row".into()).child(cb))
    }
}

/// 量一帧：返回（标签 y、标签高、行 y、行高）。
fn measure(cx: &mut TestAppContext, ours: bool) -> (f32, f32, f32, f32) {
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view: gpui_kit::AnyView = cx.new(|_| Tiny { ours }).into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let lbl = vtcx.debug_bounds("tiny-lbl").expect("标签已绘制");
    let row = vtcx.debug_bounds("tiny-row").expect("行已绘制");
    (
        f32::from(lbl.origin.y),
        f32::from(lbl.size.height),
        f32::from(row.origin.y),
        f32::from(row.size.height),
    )
}

/// 方框中心落在**字面中心**（拉丁小写 = x-height 带中心），且中英标签同规则。
///
/// 期望值由**实时字体度量**算出（`ascent`/`descent`/`x_height` 取自 `TextSystem`）：
/// 行框 `L = 字号 × CHECKBOX_LINE_HEIGHT`；字身框 `F = ascent + |descent|`；
/// 基线 `= (L − F)/2 + ascent`；字面中心 `= 基线 − x_height/2`。
/// 不引用实现里的 `CHECKBOX_OPTICAL_SHIFT` —— 把该常量改错/把内边距删掉，本用例都会失败。
///
/// 方框边界不可直接测（kit 组件内部无锚点），但「方框在行内垂直居中」由
/// [`indicator_centers_with_label`] 独立证明，故这里用**行中心**代表方框中心。
#[gpui_kit::test]
fn box_centers_on_text_ink(cx: &mut TestAppContext) {
    struct Host {
        label: SharedString,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            v_flex().w_full().child(
                div()
                    .debug_selector(|| "cb-row".into())
                    .child(checkbox("cb", self.label.clone())),
            )
        }
    }

    // 断言一个标签：返回（方框中心 − 标签文本中心）
    fn probe(cx: &mut TestAppContext, label: &'static str) -> (f32, f32, f32, f32) {
        let (_root, vtcx) = cx.add_window_view(move |window, cx| {
            gpui_kit::component::init(cx);
            crate::ui::theme::init(cx);
            let view: gpui_kit::AnyView = cx
                .new(|_| Host {
                    label: label.into(),
                })
                .into();
            gpui_kit::component::Root::new(view, window, cx)
        });
        vtcx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let key = format!("cbl-{label}");
        let lbl = vtcx
            .debug_bounds(Box::leak(key.into_boxed_str()))
            .expect("标签锚点已绘制");
        let row = vtcx.debug_bounds("cb-row").expect("行已绘制");
        let (ascent, descent, x_height, size) = vtcx.update(|window, cx| {
            let ts = cx.text_system();
            let id = ts.resolve_font(&window.text_style().font());
            let size = window.rem_size(); // 复选框默认 `Size::Medium` ⇒ 标签 1rem
            (
                f32::from(ts.ascent(id, size)),
                f32::from(ts.descent(id, size).abs()),
                f32::from(ts.x_height(id, size)),
                f32::from(size),
            )
        });
        let line_box = size * checkbox_line_height();
        let baseline = (line_box - (ascent + descent)) / 2. + ascent;
        let ink_center = baseline - x_height / 2.;
        let box_center = f32::from(row.origin.y) + f32::from(row.size.height) / 2.;
        (
            box_center - (f32::from(lbl.origin.y) + f32::from(lbl.size.height) / 2.),
            f32::from(lbl.size.height),
            ink_center - line_box / 2., // 期望的下沉量
            size,                       // 实测 rem（断言用，不写死 16）
        )
    }

    for label in ["gz", "rar", "解压到此处", "自动解压"] {
        let (d, lbl_h, want, rem) = probe(cx, label);
        assert!(
            (d - want).abs() <= 0.75,
            "{label:?}: 方框中心比文字中心低 {d:+.2}px，字面中心应为 {want:+.2}px\
             （标签行框 {lbl_h:.2}）—— 方框看起来偏高/偏低"
        );
        // 行框 = **实测 rem** × 行高系数：写死 16 会让用例随机器配置
        // （`[ui].font_size`）飘——用户把字号改成 14 时它就会红。
        let want_h = rem * checkbox_line_height();
        assert!(
            (lbl_h - want_h).abs() <= 0.5,
            "{label:?}: 内边距不得改变标签行框高度（应 {want_h:.2}，实得 {lbl_h:.2}，rem {rem:.1}）"
        );
    }
}

/// 本模块的 `items_center` 必须真的生效（覆盖组件的 `items_start`）。
///
/// 守的缺陷（用户实测）：「文件关联页勾选框比文字高一些、文字低一些」——
/// 方框 16px 与标签行框 24px 顶边对齐 ⇒ 方框中心比文字中心高 4px。
#[gpui_kit::test]
fn indicator_centers_with_label(cx: &mut TestAppContext) {
    // 对照：组件原生构造 —— 矮标签贴顶（中心在行中心上方）
    let (n_lbl_y, n_lbl_h, n_row_y, _) = measure(cx, false);
    let native_above = (n_row_y + 16. / 2.) - (n_lbl_y + n_lbl_h / 2.);
    assert!(
        native_above > 1.,
        "对照构造本应贴顶（组件写死 items_start）：标签中心竟高于行中心 {native_above:.2}px？"
    );

    // 本模块构造 —— 标签在行内垂直居中 ⇒ 方框与文字同轴
    let (lbl_y, lbl_h, row_y, row_h) = measure(cx, true);
    let d = (lbl_y + lbl_h / 2.) - (row_y + row_h / 2.);
    assert!(
        d.abs() <= 0.5,
        "标签未在行内居中（偏 {d:+.2}px）：`items_center` 未覆盖组件的 `items_start`\
         ——方框会比文字高（行高 {row_h:.2}、标签高 {lbl_h:.2}）"
    );
}
