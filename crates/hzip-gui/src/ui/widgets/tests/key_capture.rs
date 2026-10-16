//! `key_capture` 的测试（`#[cfg(test)]`；由 `key_capture.rs` 尾部的 `#[path]` 引入）。
//!
//! 两组验收：
//!
//! ① **拦截器行为**（T-KEYS-D §6.4；§9 改版没动这一段）
//!   —— [`recording_swallows_the_bound_action_and_delivers_the_keystroke`]（录制中
//!      按键不触发动作、且原样送达）与
//!      [`dropping_the_subscription_restores_action_dispatch`]（退订即恢复派发）；
//! ② **芯片显示与交互**（§9.2）
//!   —— 录制态/空闲态文案块、`pending` 预览块、未录制不残留旧预览、`unbound` 分支、
//!      真实点击上抛并把句柄送进 dispatch 树。
//!
//! # 为什么用真 keymap，而不是 `dispatch_action` 直投
//!
//! 本模块要证的核心命题是「拦截器在**动作分发之前**」。绕过按键派发（直接
//! `dispatch_action`）就永远查不出顺序错误——那种写法下，把 `stop_propagation`
//! 挪到 `on_key` 之后、甚至整行删掉，用例都还是绿的。故这里走完整链路：
//! `bind_keys` 绑一条真绑定 → `simulate_keystrokes` 走平台按键派发 → 拦截器
//! → （被吞 / 不吞）keymap 匹配 → 全局动作处理器。
//!
//! 探针动作 [`ProbeQuit`] **故意不复用** `ui::shell::actions::Quit`：本模块对业务
//! keymap 零知识（见模块文档），用例也不该把「能吃掉绑定」这件事绑在某个业务动作上。
//!
//! # 芯片用例为什么走 `debug_bounds` 而不是读样式
//!
//! gpui 不暴露渲染后的样式，`debug_bounds` 只认 `debug_selector` 注册的锚点。故
//! 「三态」在测试里的观测面就是**锚点 + 尺寸**：谁在（`kc-rec`/`kc-idle`/`kc-pending`/
//! `kc-unbound`）、谁多宽（文字真的画进去了 ⇒ 宽度 > 0）。芯片侧的锚点契约见
//! [`key_capture_chip`] 文档。

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::v_flex;
use gpui_kit::{FocusHandle, KeyBinding, Modifiers, Render, TestAppContext, VisualTestContext, px};

/// 芯片态构造（用例只关心差异项）。
fn chip(recording: bool, pending: Option<&str>, unbound: bool) -> KeyChipState {
    KeyChipState {
        recording,
        pending: pending.map(SharedString::from),
        unbound,
    }
}

/// 芯片/录制探针宿主：一台芯片（id = `probe`）。
///
/// 根节点发布 `key_context("Main")` 并自持焦点——与生产里 `ui::shell::render` 的
/// 壳体同构：加速键挂在 `Main` 上下文，动作从壳体向上派发。少了这层上下文，
/// `Some("Main")` 的绑定在「没有焦点」的测试窗口里根本匹配不到（派发路径只有根
/// 节点，上下文栈为空），用例会以「基线没派发动作」的形式假红。
///
/// 根句柄 [`Host::root_focus`] 另有用途：芯片句柄是否**真的**进了 dispatch 树，要靠
/// 「根是否在焦点路径上包含它」来验（见 [`click_anchor_fires_the_callback_and_registers_the_focus`]）。
struct Host {
    text: SharedString,
    state: KeyChipState,
    root_focus: FocusHandle,
    chip_focus: FocusHandle,
    clicks: Rc<Cell<usize>>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let clicks = self.clicks.clone();
        let focus = self.chip_focus.clone();
        v_flex()
            .key_context("Main")
            .track_focus(&self.root_focus)
            .child(key_capture_chip(
                "probe",
                self.text.clone(),
                self.state.clone(),
                &self.chip_focus,
                move |_, window, cx| {
                    clicks.set(clicks.get() + 1);
                    // 「聚焦」是点击回调自己要做的事（§9.2：芯片只登记句柄、不自己抢
                    // 焦点）——这里按生产面板的写法补上，顺带把 track_focus 弄成真被
                    // 断言到的东西。
                    window.focus(&focus, cx);
                },
            ))
    }
}

// 探针动作：绑在 `ctrl-q` 上（见文件头注释解释为什么不复用业务动作）。
gpui_kit::actions!(hzip_gui_key_capture_probe, [ProbeQuit]);

/// 一个探针场景：窗口 + 各观测点。
struct Scene {
    /// 录制态订阅槽：`Some` = 拦截器已挂（= 正在录制），`None` = 未录制。
    ///
    /// 与生产调用方同款持有方式（`RefCell` 只是为了让用例能在窗口外主动 drop，
    /// 生产里一个 `Option<Subscription>` 字段即可）——**订阅必须被持有到录制结束**。
    subscription: RefCell<Option<Subscription>>,
    /// 拦截器收到的按键（顺序 = 按下顺序）。
    hits: Rc<RefCell<Vec<Keystroke>>>,
    /// [`ProbeQuit`] 被派发的次数。
    quits: Rc<Cell<usize>>,
    /// 芯片点击回调次数。
    clicks: Rc<Cell<usize>>,
    /// 宿主的根句柄（焦点树的祖先）。
    root_focus: FocusHandle,
    /// 芯片句柄（`track_focus` 交给芯片的那个）。
    chip_focus: FocusHandle,
}

/// 造场景：建宿主实体 → 开窗 → 初始化 kit 组件/主题 → 绘制一帧 → 聚焦壳体。
///
/// 宿主实体先于窗口创建，只为让用例拿到它的 [`FocusHandle`]（`Root` 包一层后
/// `add_window_view` 返回的是 `Entity<Root>`，读不到探针宿主的字段）。
///
/// 绘制必须发生在聚焦之前：焦点句柄要先进**已渲染帧**的派发树（`track_focus`
/// 在 prepaint 注册），`Window::focus` 才能把按键派发指向壳体节点。
///
/// `text` 收 `SharedString` 而不是 `&'static str`：返回的 `&mut VisualTestContext`
/// 要沿用 `cx` 的生命周期，签名里再出现一个输入生命周期就无法省略（E0106）。
fn scene(
    cx: &mut TestAppContext,
    text: SharedString,
    state: KeyChipState,
) -> (&mut VisualTestContext, Scene) {
    let clicks: Rc<Cell<usize>> = Rc::default();
    let host_clicks = clicks.clone();
    let host = cx.update(move |cx| {
        cx.new(move |cx| Host {
            text,
            state,
            root_focus: cx.focus_handle(),
            chip_focus: cx.focus_handle(),
            clicks: host_clicks,
        })
    });
    let (root_focus, chip_focus) = cx.update(|cx| {
        let host = host.read(cx);
        (host.root_focus.clone(), host.chip_focus.clone())
    });
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view: gpui_kit::AnyView = host.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    vtcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.focus(&root_focus, cx);
        window.activate_window();
    });
    (
        vtcx,
        Scene {
            subscription: RefCell::new(None),
            hits: Rc::default(),
            quits: Rc::default(),
            clicks,
            root_focus,
            chip_focus,
        },
    )
}

/// 把 `ctrl-q` 绑到探针动作，并挂一个全局处理器记数。
fn bind_probe_quit(vtcx: &mut VisualTestContext, quits: Rc<Cell<usize>>) {
    vtcx.update(|_window, cx| {
        cx.bind_keys([KeyBinding::new("ctrl-q", ProbeQuit, Some("Main"))]);
        cx.on_action(move |_: &ProbeQuit, _: &mut App| quits.set(quits.get() + 1));
    });
}

/// 进入录制：挂上拦截器，并把订阅交给调用方（`Scene`）持有。
///
/// 这就是生产调用方的用法——**绝不能**把它留在局部作用域里（那等于当场退订，
/// 录制永远不会生效）。
fn start_recording(scene: &Scene, vtcx: &mut VisualTestContext) {
    let hits = scene.hits.clone();
    vtcx.update(|_window, cx| {
        assert!(
            scene.subscription.borrow().is_none(),
            "同一时刻只应有一份录制订阅（App 级拦截器互不排斥，挂两份会都收到按键）"
        );
        let subscription = capture_keystrokes(cx, move |keystroke, _window, _cx| {
            hits.borrow_mut().push(keystroke.clone());
        });
        *scene.subscription.borrow_mut() = Some(subscription);
    });
}

/// 验收①：录制中按 `ctrl-q` 不触发绑定动作，且回调原样收到该按键。
#[gpui_kit::test]
fn recording_swallows_the_bound_action_and_delivers_the_keystroke(cx: &mut TestAppContext) {
    let (vtcx, scene) = scene(cx, "Ctrl+A".into(), chip(false, None, false));
    bind_probe_quit(vtcx, scene.quits.clone());

    // 基线（反假绿）：还没开始录制时，`ctrl-q` 必须真的派发动作。否则下面的
    // 「录制中没派发」可能只是绑定/处理器压根没生效。
    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(scene.quits.get(), 1, "未录制时 `ctrl-q` 应派发 ProbeQuit");

    start_recording(&scene, vtcx);
    vtcx.simulate_keystrokes("ctrl-q");

    assert_eq!(
        scene.quits.get(),
        1,
        "录制中不得派发任何动作（录 `ctrl-q` 时不能顺手退出程序）"
    );
    let hits = scene.hits.borrow();
    assert_eq!(hits.len(), 1, "拦截器应恰好收到一次按键");
    assert_eq!(hits[0].key, "q", "按键名须原样送达");
    assert!(
        hits[0].modifiers.control,
        "ctrl 修饰键须原样送达（改键要按物理键位回写）"
    );
}

/// 验收①（后半）：`Subscription` drop（= 结束录制）之后，同一按键恢复原有派发。
#[gpui_kit::test]
fn dropping_the_subscription_restores_action_dispatch(cx: &mut TestAppContext) {
    let (vtcx, scene) = scene(cx, "Ctrl+A".into(), chip(false, None, false));
    bind_probe_quit(vtcx, scene.quits.clone());
    start_recording(&scene, vtcx);

    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(scene.quits.get(), 0, "录制中不得派发动作");
    assert_eq!(scene.hits.borrow().len(), 1, "录制中应收到按键");

    // 退订 = 结束录制（本模块的生命周期契约：drop 即生效）。
    let dropped = scene.subscription.borrow_mut().take();
    assert!(dropped.is_some(), "录制态应有订阅被持有");
    drop(dropped);

    vtcx.simulate_keystrokes("ctrl-q");
    assert_eq!(scene.quits.get(), 1, "退订后按键必须恢复原有动作派发");
    assert_eq!(
        scene.hits.borrow().len(),
        1,
        "退订后回调不得再收到按键（订阅真的被摘掉了）"
    );
}

/// 验收②（录制态）：点击锚点在、录制态文案块在、空闲态文案块不在。
#[gpui_kit::test]
fn recording_state_shows_the_rec_state_block(cx: &mut TestAppContext) {
    let (vtcx, _scene) = scene(cx, "请按键".into(), chip(true, None, false));

    assert!(
        vtcx.debug_bounds("kc-probe").is_some(),
        "芯片锚点 `kc-probe` 缺失（`cfg!(test)` 下芯片本体须带它）"
    );
    let state_block = vtcx
        .debug_bounds("kc-rec-probe")
        .expect("`recording = true` 未渲染成录制态（recording 参数被忽略了？）");
    assert!(
        state_block.size.width > px(0.),
        "录制态文案块宽度为 0（文案没画进去？）"
    );
    assert!(
        vtcx.debug_bounds("kc-idle-probe").is_none(),
        "`recording = true` 不得出现空闲态锚点"
    );
    assert!(
        vtcx.debug_bounds("kc-pending-probe").is_none(),
        "还没按下任何键，不该有预览块"
    );
}

/// 验收②（空闲态）：与上一条互为反向断言——两条同时绿，才排得掉「恒真锚点」的假象。
#[gpui_kit::test]
fn idle_state_shows_the_idle_state_block(cx: &mut TestAppContext) {
    let (vtcx, _scene) = scene(cx, "Ctrl+A".into(), chip(false, None, false));

    assert!(
        vtcx.debug_bounds("kc-probe").is_some(),
        "芯片锚点 `kc-probe` 缺失"
    );
    let state_block = vtcx
        .debug_bounds("kc-idle-probe")
        .expect("`recording = false` 未渲染成空闲态");
    assert!(
        state_block.size.width > px(0.),
        "空闲态文案块宽度为 0（键位串没画进去？）"
    );
    assert!(
        vtcx.debug_bounds("kc-rec-probe").is_none(),
        "`recording = false` 不得出现录制态锚点"
    );
}

/// 验收③：录制中已按键 → 预览块在（且真的画出了文案），文案块仍是 `kc-rec`。
#[gpui_kit::test]
fn pending_preview_is_rendered_while_recording(cx: &mut TestAppContext) {
    let (vtcx, _scene) = scene(cx, "请按键".into(), chip(true, Some("ctrl-a"), false));

    let preview = vtcx
        .debug_bounds("kc-pending-probe")
        .expect("录制中已按键却缺预览块 `kc-pending-probe`");
    assert!(
        preview.size.width > px(0.) && preview.size.height > px(0.),
        "预览块是空盒（pending 文案没画进去？）"
    );
    assert!(
        vtcx.debug_bounds("kc-rec-probe").is_some(),
        "预览不该顶掉「正在录制」这一事实（`kc-rec-*` 恒表示录制态）"
    );
    assert!(
        vtcx.debug_bounds("kc-idle-probe").is_none(),
        "录制中不得出现空闲态锚点"
    );
}

/// 验收③（反面）：录制结束后的**旧 pending**（面板还没清）不得残留在芯片上。
///
/// 面板契约是「提交/取消即清 pending」（§9.3），但在它清干净之前的一帧里，芯片
/// 显示什么不该由那个脏值决定——显示的永远是调用方给的文案。
#[gpui_kit::test]
fn pending_is_ignored_when_not_recording(cx: &mut TestAppContext) {
    let (vtcx, _scene) = scene(cx, "Ctrl+A".into(), chip(false, Some("ctrl-b"), false));

    assert!(
        vtcx.debug_bounds("kc-idle-probe").is_some(),
        "未录制时应显示空闲态文案块"
    );
    assert!(
        vtcx.debug_bounds("kc-pending-probe").is_none(),
        "未录制时不得显示预览块（旧 pending 会盖住真实键位）"
    );
}

/// 验收③（未绑定分支）：`unbound` 与 `false` 渲染出差异。
///
/// 差异的观测面是覆盖层锚点（见 [`key_capture_chip`] 文档：`unbound` 只换文字色，
/// gpui 不暴露样式，故用「不画像素的标记层」把它变成可断言的**存在性**）。
#[gpui_kit::test]
fn unbound_state_is_visible_in_the_rendered_tree(cx: &mut TestAppContext) {
    let (vtcx, _scene) = scene(cx, "未绑定".into(), chip(false, None, true));

    assert!(
        vtcx.debug_bounds("kc-unbound-probe").is_some(),
        "`unbound = true` 未走弱化色分支（标记层缺失）"
    );
    assert!(
        vtcx.debug_bounds("kc-idle-probe").is_some(),
        "未绑定的行仍要显示文案块（显示的是「未绑定」三字）"
    );
}

/// 验收③（未绑定分支的反面）：正常绑定**不**带那个标记层。
#[gpui_kit::test]
fn bound_state_has_no_unbound_layer(cx: &mut TestAppContext) {
    let (vtcx, _scene) = scene(cx, "Ctrl+A".into(), chip(false, None, false));

    assert!(
        vtcx.debug_bounds("kc-unbound-probe").is_none(),
        "有绑定的行不该带未绑定标记层（恒真锚点会让上一条用例失去意义）"
    );
}

/// 验收④：真实点击命中 `kc-<id>` → 回调上抛；且点击后聚焦的句柄**真的进了
/// dispatch 树**（面板的失焦提交完全依赖这一点，见 `KeysPanelRedesign` 的提醒）。
#[gpui_kit::test]
fn click_anchor_fires_the_callback_and_registers_the_focus(cx: &mut TestAppContext) {
    let (vtcx, scene) = scene(cx, "Ctrl+A".into(), chip(false, None, false));

    let bounds = vtcx.debug_bounds("kc-probe").expect("芯片锚点缺失");
    vtcx.simulate_click(bounds.center(), Modifiers::default());

    assert_eq!(scene.clicks.get(), 1, "点击回调应被上抛一次");
    vtcx.update(|window, cx| {
        assert!(
            scene.chip_focus.is_focused(window),
            "点击回调里的 `window.focus` 没生效"
        );
        // 强断言：`is_focused` 只读 `window.focus`，句柄没进 dispatch 树它也会真。
        // `contains_focused` 要沿焦点树走——芯片节点不在树里（漏了 `track_focus`）
        // 时恒为 false，故它才是「失焦监听能不能触发」的守卫。
        assert!(
            scene.root_focus.contains_focused(window, cx),
            "芯片句柄没进 dispatch 树（芯片上漏了 `track_focus`）⇒ 面板的 on_blur 永不触发"
        );
    });
}
