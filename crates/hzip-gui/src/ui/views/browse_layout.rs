//! 浏览页双栏布局（T-G53 重写：gpui-kit `Tree` + `DataTable` 组件装配）。
//!
//! 结构：面包屑行 + 过滤行 → 双栏（左树 260px / 右表格 flex_1）。
//! 数据已由 [`super::browse::browse_root`] 灌入 [`BrowseHold`] 实体；
//! 本文件只做布局。树行/面包屑点击经 `shell`（WeakEntity）就地上行
//! handlers（见 browse.rs 模块级「事件上行」）；表格双击/过滤防抖订阅
//! 挂在 [`BrowseHold`]（browse.rs 一次性装配）。
//!
//! # 事件上行面（02 §11.1 语义；R-GUI4 #2 重定义树交互）
//!
//! - 树行**单击** → `on_tree_click`（进入目录；展开态**不变**）
//! - 树行**双击** → `on_tree_toggle`（仅展开/折叠被双击的目录）
//! - 压制 gpui-base `TreeState` 行包装层的内建单击 toggle（其
//!   `on_entry_click` 在 mousedown 翻转展开态——与上述语义冲突）：
//!   `ListItem::on_mouse_down` 先行 `stop_propagation` 阻断冒泡，选中态
//!   由本层 `set_selected_index` 补设；键盘展开（方向键/Enter）不受影响，
//!   其 TreeEvent 仍经 browse.rs 订阅回写 VM
//! - 面包屑段点击 → `on_crumb`
//! - 表格双击行 → 目录/容器 `on_tree_click`（enter）；文件 `on_row_activate`
//! - 列头点击 → 排序（升/降/默认三态循环；BrowseDelegate::perform_sort）

use crate::ui::constant::{FILTER_INPUT_W, ICON_CELL_ICON, ICON_CELL_W, TREE_W_MAX, TREE_W_MIN};
use crate::ui::spacing::{component_size, sp};
use gpui_kit::base::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::button::Button;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::table::DataTable;
use gpui_kit::component::tree::{Tree, TreeEntry, TreeState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Entity, IntoElement, MouseButton, SharedString, Styled,
    WeakEntity, Window, div, px,
};

use super::browse::{BrowseHandlerSource, BrowseHold};
use crate::ui::contracts::{AppStateUiSnapshot, Crumb};
use crate::ui::widgets::AppIcon;
use crate::ui::widgets::file_icon;

/// 分栏宽度写盘去抖代数（每拖动 tick +1；延迟任务只写「自己仍是最新」的那一次）。
static TREE_WIDTH_SAVE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
use crate::ui::widgets::breadcrumb::breadcrumb;

/// 浏览页双栏布局（crumbs 已拷贝；树/面包屑事件经 `shell` 就地上行）。
pub(crate) fn browse_layout<T: BrowseHandlerSource + 'static>(
    window: &mut Window,
    cx: &mut App,
    hold: &gpui_kit::Entity<BrowseHold>,
    crumbs: &[Crumb],
    shell: &WeakEntity<T>,
    snap: &AppStateUiSnapshot,
) -> AnyElement {
    let border = cx.theme().border;
    let crumbs: Vec<Crumb> = crumbs.to_vec();
    let shell = shell.clone();

    // 过滤输入（T-MENU1：菜单「查找文件」/Ctrl+F 聚焦目标；空输入 = 无过滤）。
    // 2026-09-15 用户要求：不再单独占一行，内嵌**目录导航栏（面包屑）右侧**
    // ——面包屑 flex_1 吃满剩余宽度，过滤框固定宽收在行尾。
    let filter_input = hold.read(cx).filter_input.clone();
    let filter_box = div()
        .flex_shrink_0()
        .w(FILTER_INPUT_W)
        .child(gpui_kit::component::input::Input::new(&filter_input).with_size(component_size()));

    let header = h_flex()
        .flex_shrink_0()
        .items_center()
        .gap(sp(2.))
        .px(sp(2.))
        .py(sp(1.))
        .border_b_1()
        .border_color(border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_x_hidden()
                .child(breadcrumb(cx, &crumbs, &shell)),
        )
        .children((snap.write_back_manual || snap.pending_count > 0).then(|| {
            // 手动写回（设置-通用「改动后立即写回」= 关）：有挂起才出现。
            // 点它把全部挂起改动**一次性**合并写回——组合链/大档案只重压一次。
            let cmd = hold.read(cx).command.clone();
            let n = snap.pending_count;
            Button::new("browse-save-pending")
                .with_size(component_size())
                .label(crate::tr_with(
                    "browse.pending.save",
                    &[("n", &n.to_string())],
                ))
                .on_click(move |_, window, cx| {
                    if let Some(c) = cmd.as_ref() {
                        c(
                            crate::ui::views::browse::BrowseCommand::SavePending,
                            window,
                            cx,
                        );
                    }
                })
        }))
        .child(filter_box);

    // 树行点击 → on_tree_click（进入目录）。闭包从 Global 读稳态无需持数据；
    // shell Weak 每次 render 克隆进 render_item（'static 闭包捕获 Weak）。
    // tree_meta 每帧快照进闭包（空目录图标判定源——禁用 entry.is_folder()，
    // 其 = !children.is_empty()，空目录误判文件，R-GUI2-04 §3.3）。
    // tree 实体克隆进闭包：压制内建 toggle 后由本层补设选中态（R-GUI4 #2）。
    let click_shell = shell.clone();
    let tree_meta = hold.read(cx).tree_meta.clone();
    let tree_entity = hold.read(cx).tree.clone();
    // 档案根节点的系统图标要按**档案名**（带扩展名）取——名字来自快照
    // （`archive_name` 空 = 未打开，此时直接回退）。
    let archive_name = snap.archive_name.clone();
    let nav_tree = Tree::new(
        &hold.read(cx).tree,
        move |ix, entry, selected, window, cx| {
            render_tree_item(
                &click_shell,
                &tree_entity,
                ix,
                entry,
                selected,
                &tree_meta,
                &archive_name,
                window,
                cx,
            )
        },
    );

    // 右栏渲染模式（T-MENU5 偏好 view_style_ix：detail / icon / list；
    // T-MENU9 落地 icon/list 两模式）。
    // - detail：DataTable（唯一消费列设置的模式）
    // - icon/list：网格/紧凑列表——**忽略列设置**（设计 §2.5），交互语义
    //   （单击选、双击进入/激活、`..` 行为）与 DataTable 对齐
    let view_style = crate::ui::bar::menu_bar::ViewStyle::ALL
        .get(snap.view_style_ix)
        .copied()
        .unwrap_or(crate::ui::bar::menu_bar::ViewStyle::Detail);
    // DataTable 根 div 是 size_full——但作为面板子项默认不伸展，
    // wrapper 需 flex_1 占满右栏剩余高度（高度塌缩则行不可见）。
    use gpui_kit::component::menu::ContextMenuExt as _;
    let entry_table = v_flex()
        .flex_1()
        .h_full()
        .min_h_0()
        .bg(cx.theme().background)
        .child(match view_style {
            crate::ui::bar::menu_bar::ViewStyle::Detail => div()
                .id("table-wrap")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                // 空白区左键 → 取消选中（用户口径 2026-09-28：与资源管理器一致）。
                // 行内点击由行自己的 `on_mouse_down` 先跑并置 `last_hit_row`（事件
                // 由内向外冒泡），down 阶段这里只把它**转存**到 `press_hit_row`。
                //
                // 为什么判空放到 **up** 阶段：列头点击是在 mouse-up 才由 kit 的
                // `on_click`（`on_col_head_click` → `set_selected_col`）把选择模式
                // 切成 `Column`/`Cell`；同在 up 阶段、子元素先冒泡 ⇒ 容器读模式时
                // 已是最终值。若沿用 down 阶段判空，列头点击会被当成「点空白」而
                // 连带清掉焦点行（用户口径 2026-10-12：点列头排序不得丢焦点行高亮）。
                .on_mouse_down(MouseButton::Left, {
                    let hold_down = hold.clone();
                    move |_: &gpui_kit::MouseDownEvent, _window, cx| {
                        hold_down.update(cx, |v, cx| {
                            v.table.update(cx, |t, _| {
                                let d = t.delegate_mut();
                                d.press_hit_row = std::mem::take(&mut d.last_hit_row);
                            })
                        });
                    }
                })
                .on_mouse_up(MouseButton::Left, {
                    let shell_up = shell.clone();
                    let hold_up = hold.clone();
                    move |_: &gpui_kit::MouseUpEvent, _window, cx| {
                        let hit_row = hold_up.update(cx, |v, cx| {
                            v.table.update(cx, |t, _| {
                                std::mem::take(&mut t.delegate_mut().press_hit_row)
                            })
                        });
                        if hit_row {
                            return;
                        }
                        // 非 `Row`/`None`（即选中列/单元格）⇒ 这一下是列头点击，
                        // 不是点空白，别清选中、更别清焦点行。
                        let blank_like = hold_up.update(cx, |v, cx| {
                            matches!(
                                v.table.read(cx).selection(),
                                gpui_kit::component::table::TableSelection::None
                                    | gpui_kit::component::table::TableSelection::Row(_)
                            )
                        });
                        if blank_like && let Some(shell) = shell_up.upgrade() {
                            shell.update(cx, |s, cx| s.browse_clear_selection(cx));
                        }
                    }
                })
                // 走查 #12：关掉 DataTable 自带整圈边框（默认 bordered=true）
                // ——与内容区外框重合成双层线；行分隔线（table_row_border）保留
                .child(
                    DataTable::new(&hold.read(cx).table)
                        .with_size(component_size())
                        .bordered(false),
                )
                // 负责（容器读到命中即返回空菜单，kit 不渲染空菜单）
                //
                // 右键按下时先清掉 kit 的陈旧「右键行」（右键行 → 再右键空白时，
                // kit 表格级菜单会拿旧行号再弹一次行菜单）
                .on_mouse_down(MouseButton::Right, |_, _, cx| {
                    if !super::browse::peek_right_hit_row(cx) {
                        super::browse::clear_kit_right_row(cx);
                    }
                })
                .context_menu(|menu, _window, cx| {
                    if super::browse::take_right_hit_row(cx) {
                        return menu;
                    }
                    let Some((command, can_edit, paste_available)) =
                        super::browse::blank_menu_from(cx)
                    else {
                        return menu;
                    };
                    super::browse::build_browse_menu(
                        menu,
                        command,
                        super::browse::blank_menu_enable(can_edit, paste_available),
                    )
                })
                .into_any_element(),
            crate::ui::bar::menu_bar::ViewStyle::Icon => icon_grid(cx, snap, &shell),
            crate::ui::bar::menu_bar::ViewStyle::List => compact_list(window, cx, snap, &shell),
        });

    // 左树/右列表 gpui-kit Resizable 分栏（R-GUI2-04 §3.1；像素制——
    // 窗口缩放不重排宽度）。拖动结束（Resized 回调一次）写 config
    // ui.tree_width 持久化（同步小文件写盘，主线程可接受——toml <2KB）。
    // T-MENU5：show_tree=false → 树面板整体隐藏（设置/菜单双入口即时生效）。
    // 初始宽度取记忆值（`hold.tree_width` ← 配置）：面板从组里增删会按索引
    // 重分配尺寸表，重新显示靠 browse_root 的切换沿 + 本值插回面板，
    // 不能靠 `.size()` 硬编码 260。
    let tree_width = hold.read(cx).tree_width;
    let tree_panel = resizable_panel()
        .size(px(tree_width))
        .size_range(TREE_W_MIN..TREE_W_MAX)
        .flex_none()
        .child(
            v_flex()
                .size_full()
                .border_r_1()
                .border_color(border)
                .bg(cx.theme().background)
                .child(nav_tree),
        );
    let hold_for_resize = hold.clone();
    let body = h_resizable("browse-split")
        .with_state(&hold.read(cx).split)
        .on_resize({
            move |state: &Entity<ResizableState>, _window, cx: &mut App| {
                let Some(w) = state.read(cx).sizes().first().copied() else {
                    return;
                };
                let px_w = u32::from(w);
                // 记忆值同步（重新显示插回面板用的就是它）
                hold_for_resize.update(cx, |hold, _| hold.tree_width = w.as_f32());
                // 写盘**去抖**（用户口径 2026-09-24：UI 线程不做可避免的 IO）：
                // `on_resize` 每次拖动 tick 都触发，旧实现每帧一次
                // `Config::load + save`（几十次/秒的 2 KB 同步写）。现在内存值照旧
                // 立刻同步（重新显示插回用它），落盘交给**最后一次 tick 的延迟任务**
                // ——400 ms 内没有更新的 tick 才写：拖动中不写盘，松手后的最终宽度不丢。
                let epoch =
                    TREE_WIDTH_SAVE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(400))
                        .await;
                    if TREE_WIDTH_SAVE_EPOCH.load(std::sync::atomic::Ordering::Acquire) != epoch {
                        return; // 还有更新的 tick：由那一次负责落盘
                    }
                    cx.update(|_| {
                        let mut cfg = hzip_config::Config::load();
                        if cfg.ui.tree_width != px_w {
                            cfg.ui.tree_width = px_w;
                            // 异步落盘（与设置写盘同口径：后台合并写，UI 帧内零 IO）
                            cfg.save_async();
                            tracing::debug!("树宽已提交持久化: {px_w}px");
                        }
                    });
                })
                .detach();
            }
        })
        .when(snap.show_tree, |body| body.child(tree_panel))
        .child(resizable_panel().child(entry_table));

    v_flex()
        .size_full()
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        .child(header)
        .child(body)
        .into_any_element()
}

/// 目录树重新显示时按记忆宽度把面板插回 Resizable 组。
///
/// 面板从组里增删会让尺寸表**按索引**重分配：`show_tree=false` 时只剩表格，
/// 它占据索引 0 并拿到全宽；重新显示时索引 0 若沿用这份尺寸，树就会继承
/// 表格的全宽（用户实测「很长的目录树」）。故显隐切换沿由 `browse_root` 检出，
/// 在此按 `[ui].tree_width` 记忆值插回索引 0（表格余量按比例回吐）。
///
/// 已是双面板（尺寸表未截断）→ 不动：避免重复插入造成面板数错位。
pub(crate) fn restore_tree_panel_width(
    split: &Entity<gpui_kit::base::ResizableState>,
    width: f32,
    cx: &mut App,
) {
    split.update(cx, |split, cx| {
        if split.sizes().len() == 1 {
            split.insert_panel(Some(px(width)), Some(0), cx);
        }
    });
}

/// 树行图标（纯函数；测试锚点）。
///
/// **压缩包（档案根 / 内层容器）一律 `FolderZip`**（用户实测反馈）：它们
/// 「双击能进去」，用文件图标会让人以为只是预览；容器也不用 `Folder`——它是文件
/// 不是目录（层级/选中语义按文件走），文件夹图标会暗示能当目录操作。
fn tree_row_icon(
    is_root_archive: bool,
    meta: Option<&crate::ui::binding::TreeRowMeta>,
    expanded: bool,
) -> AppIcon {
    let is_dir = meta.map(|m| m.is_dir).unwrap_or(false);
    let is_container = meta.map(|m| m.is_container).unwrap_or(false);
    if is_root_archive || is_container {
        AppIcon::FolderZip
    } else if !is_dir {
        AppIcon::File
    } else if expanded {
        AppIcon::FolderOpen
    } else {
        AppIcon::Folder
    }
}

/// 左栏树的回退图标（**没有可探测的名字**时用：喂树元数据滞后一帧 / 档案名未知）。
///
/// 改走 widget 的同槽位回退（`file_icon::fallback`）——尺寸与光学下沉和系统位图
/// 分支完全一致（此前本地写死 `Size::Small`，与位图分支的 `Slot` 尺寸各算各的）。
fn fallback_small_icon(icon: AppIcon, cx: &App) -> AnyElement {
    crate::ui::widgets::file_icon::fallback(icon, crate::ui::widgets::file_icon::Slot::Inline, cx)
}

/// 树行渲染（TreeState render_item 回调）。
///
/// 图标判定用 `tree_meta`（喂树同批替换的元数据：路径/是否目录/是否容器）而非
/// `entry.is_folder()`（= !children.is_empty()——空目录误判文件）。图标选择见
/// [`tree_row_icon`]（压缩包用 `FolderZip`；id 带层号前缀，故档案根按
/// meta.path 是否为空判定）。
/// 标签 `flex_1().min_w_0().truncate()` 单行省略（裸 SharedString 无宽度
/// 约束是折两行根因）。
///
/// 交互（R-GUI4 #2）：
/// - mousedown `stop_propagation`——阻断行包装层 `TreeState::on_entry_click`
///   的内建 toggle（单击翻转展开态），选中态由本层补设；
/// - 单击（click_count=1）→ `browse_tree_click`（进入；目标可能是外层会话的
///   节点或一个内层压缩包，都要 cx 才能路由）；
/// - 双击（click_count≥2）→ `on_tree_toggle`（仅翻转该目录展开态）。
#[allow(clippy::too_many_arguments)] // 树渲染闭包的固有参数面 + meta 快照，收口成结构体只会把可读性换掉
fn render_tree_item<T: BrowseHandlerSource + 'static>(
    shell: &WeakEntity<T>,
    tree: &Entity<TreeState>,
    ix: usize,
    entry: &TreeEntry,
    selected: bool,
    tree_meta: &crate::ui::binding::TreeMeta,
    archive_name: &str,
    window: &Window,
    cx: &mut App,
) -> ListItem {
    let item = entry.item();
    let meta = tree_meta.get(&item.id);
    // 档案根虚拟节点（path = ""）——id 带层号前缀，故按 meta 里的 path 判定
    let is_root_archive = meta.map(|m| m.path.is_empty()).unwrap_or(false);
    let fallback = tree_row_icon(is_root_archive, meta, entry.is_expanded());
    // 系统图标：档案根按快照档案名（带扩展名）、其余按 `meta.path` 最后一段
    // （容器/目录/文件各按自身 kind）。`meta` 缺失（喂树元数据滞后一帧）与
    // 档案名未知时直接回退——绝不拿错名字去问 shell。
    let icon: AnyElement = match meta {
        Some(m) if m.path.is_empty() => {
            if archive_name.is_empty() {
                fallback_small_icon(fallback, cx)
            } else {
                file_icon::icon(
                    archive_name,
                    file_icon::kind_of(false, true, false),
                    file_icon::Slot::Inline,
                    fallback,
                    cx,
                )
            }
        }
        Some(m) => {
            let name = m.path.rsplit('/').next().unwrap_or(m.path.as_str());
            file_icon::icon(
                name,
                file_icon::kind_of(m.is_dir, m.is_container, false),
                file_icon::Slot::Inline,
                fallback,
                cx,
            )
        }
        None => fallback_small_icon(fallback, cx),
    };
    let path = item.id.to_string();
    let shell = shell.clone();
    // 悬浮提示的独立副本（`shell` 随后被 `on_click` 移走）
    let tip_hover_shell = shell.clone();
    let tip_move_shell = shell.clone();
    let tree = tree.clone();
    // 选中底色与文件列表**同一来源**（`selection_row_bg`：背景 × 30% 主题色，
    // 比主题色浅、深色模式自适应）。kit 在 `list.active_highlight=false` 下对
    // 选中行画的是**全量 accent**（0.7.1 `list_item.rs:255-262` 的 else 分支），且其
    // `list.active.background` 槽被 schema 的 alpha clamp 到 ≤0.2 表达不了这个
    // 不透明混色——故自绘绝对定位子层盖掉（与 browse.rs `render_tr` 同法）。
    let selection_bg = crate::ui::theme::selection_row_bg(cx);
    // 行内边距覆盖库默认（ListItem 把应用样式 refine 在写死的 py_1/px_3 之后
    // ⇒ 这里的 py/px 胜出）：
    // - 上下 **恒为 0**（用户口径：文字/图标的上下 padding 一律不要，行间隔
    //   交给「每行的间隔」= 内容子元素的外边距，见下）；
    // - 左右 = 0：缩进由下方 pl(4·depth+3) 精确给出，库默认 px_3 的 12px 与之
    //   **相加**，正是「文字/图标前多一截」的来源；
    // - 行框（`row_line_height(window)`）= 字体**自然字形盒**：默认行高是
    //   φ(1.618em)，多出的 ~0.3em 正是被当成「文字/图标上下 padding」的留白；
    //   但**不能**用小固定值（1.2em）——那会小于字形盒而裁掉降部（实测字形盒
    //   ≈1.3em）。
    let m = crate::ui::spacing::metrics();
    ListItem::new(("nav-item", ix))
        .py(gpui_kit::px(0.))
        .px(gpui_kit::px(0.))
        .line_height(row_line_height(window))
        .selected(selected)
        .when(selected, |item| {
            item.child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(selection_bg)
                    .debug_selector(|| "tree-selected-layer".into()),
            )
        })
        // 压制内建 toggle：本层先收 mousedown，阻止冒泡到行包装层的
        // on_entry_click（select + toggle_expand）；选中态在此补设。
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            tree.update(cx, |t, cx| t.set_selected_index(Some(ix), cx));
        })
        .on_click(move |event: &ClickEvent, _, cx| {
            // click_count=1 单击进入；≥2 双击仅翻转该目录展开态
            // （VM expanded 是唯一事实源，经快照 set_items 同步回树）
            let path = path.clone();
            let dbl = event.click_count() >= 2;
            let _ = shell.update(cx, |s, cx| {
                if dbl {
                    // 展开态翻转不需要 cx（纯数据面），仍走既有 handler 槽
                    (s.browse_handlers().on_tree_toggle)(path);
                } else {
                    // 单击进入：目标可能是**外层会话**的节点，或一个内层压缩包
                    // （要开新会话）——路由需要 cx，走 trait 方法。
                    s.browse_tree_click(&path, cx);
                }
            });
        })
        .child(
            // 图标 + 名称同行（R-GUI3 #2）：单行 flex，overflow_hidden +
            // nowrap 防折行；名称 min_w_0 + truncate 长名省略。
            h_flex()
                .items_center()
                .gap(sp(2.))
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                // 缩进与行间隔都挂在**行框之外**的外边距上（上下 = 行间隔、
                // 左 = 层级缩进；标准档 = 原 16/12px：sp(4.) = 16px、sp(3.) = 12px）。
                // 关键：行内 padding 必须四边全 0——高亮子层是 `absolute inset_0`，
                // 命中的是**内容盒**；只要左内边距非 0，缩进那截就盖不到，会露出
                // kit 的全量 accent（用户实测的「文字/图标是一色、左侧行 padding
                // 是另一色」）。挂外边距后：子层 = 整行，与文件列表选中态同形；
                // 「贴多紧」只由 `row_gap()` 一个旋钮控制。
                .my(m.row_gap())
                .ml(sp(4. * entry.depth() as f32 + 3.))
                .debug_selector(move || format!("tree-row-{ix}"))
                .child(div().flex_shrink_0().child(icon))
                .child({
                    let tip_text: SharedString = tree_tooltip_text(
                        meta.map(|m| m.path.as_str()),
                        item.label.as_ref(),
                        archive_name,
                    )
                    .into();
                    let hover_shell = tip_hover_shell;
                    let move_shell = tip_move_shell;
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .id(("tree-tip", ix))
                        .on_hover(move |hovered: &bool, _w, cx: &mut gpui_kit::App| {
                            let t = hovered.then(|| tip_text.clone());
                            let _ =
                                hover_shell.update(cx, |s, cx| s.browse_set_hover_tip(t, None, cx));
                        })
                        .on_mouse_move(
                            move |ev: &gpui_kit::MouseMoveEvent, _w, cx: &mut gpui_kit::App| {
                                let x: f32 = ev.position.x.into();
                                let y: f32 = ev.position.y.into();
                                let pos = (x as i32, y as i32);
                                let _ = move_shell.update(cx, |s, cx| {
                                    s.browse_set_hover_tip(None, Some(pos), cx)
                                });
                            },
                        )
                        .child(SharedString::from(item.label.to_string()))
                }),
        )
}

/// 树行悬浮提示文本：档案根（path 空）→ 档案名；其余 → **档案内完整路径**
/// （`meta.path`，含分区/目录前缀，比单段标签更有用）；元数据缺失（喂树元数据
/// 滞后一帧）→ 行标签本身。用户口径 2026-10-19：长名被截断时悬浮要看全。
pub(crate) fn tree_tooltip_text(path: Option<&str>, label: &str, archive_name: &str) -> String {
    match path {
        Some("") if !archive_name.is_empty() => archive_name.to_string(),
        Some(p) if !p.is_empty() => p.to_string(),
        _ => label.to_string(),
    }
}

/// 行框高 = 字体的**自然字形盒**（`ascent − descent`；实测 Segoe UI @16px =
/// 16.4 − (−4.4) = 20.8px ≈ 1.3em）。
///
/// 为什么不能写成固定 em 系数：`line_height` 一旦小于字形盒，gpui 的基线
/// 会被顶出框外，降部（g/y/p 的弯钩）随即被 `overflow_hidden` 裁掉——用户实测
/// 「g 看起来像 a」。行框 = 「文字必须完整」与「上下 padding 恒 0」的交点。
///
/// 取值来自 [`crate::ui::spacing::text_line_floor`]（配置 `[ui].line_height` 夹墨迹盒下限 ×
/// 字号，**夹到字形盒下限**）：出厂配置（1.2）比字形盒（≈1.3em）更紧 ⇒ 生效的
/// 就是字形盒本身（= 改造前的行为，目录树观感不变）；把它调大才会放松。
///
/// 字号取 `rem_size()`：行内文字走 `text_base`（1rem，`ListItem` 内建），
/// 字体族/字重取窗口当前文本样式（组件库 `Root` 每帧把 `theme.font_family`
/// 刷到根元素 ⇒ 此处与渲染一致）。
fn row_line_height(window: &Window) -> gpui_kit::Pixels {
    crate::ui::spacing::text_line_floor(window, window.rem_size())
}

// —— T-MENU9：图标网格 / 紧凑列表（右栏非 detail 模式） ——
//
// 两个模式的**交互语义与 DataTable 对齐**（设计 §2.5 / T-MENU9 验收）：
// - 单击：选中该行（`on_selection_changed`，`..` 行 → 清选中）
// - 双击：`..` → 跳上级（on_crumb）；目录/容器 → 进入（on_tree_click）；
//   文件 → 激活（on_row_activate）
// - `..` 行显隐随 `snap.show_dotdot`；空态与 detail 一致（无占位行时留白）
//
// **列设置仅 detail 消费**：两模式只渲染「名称 + 尺寸」（icon 模式再加大图标），
// 不读 `snap.columns`（设计 §2.5 明确）。

/// 右栏行清单（`..` 行 + 条目行；与 detail 的 `data_row` 同构）。
///
/// 返回 `(是否父行, 行数据)`——父行数据为 `None`。
fn mode_rows(snap: &AppStateUiSnapshot) -> Vec<(bool, Option<crate::ui::contracts::BrowseRow>)> {
    let mut out: Vec<(bool, Option<crate::ui::contracts::BrowseRow>)> = Vec::new();
    let show_parent = snap.show_dotdot && crate::ui::binding::show_parent_row(&snap.crumbs);
    if show_parent {
        out.push((true, None));
    }
    // T-PERF4：`&Arc<Vec>` 不实现 IntoIterator——显式 .iter()
    for row in snap.dir_rows.iter() {
        out.push((
            false,
            Some(crate::ui::binding::browse_row_from_tree_row(row)),
        ));
    }
    out
}

/// 行内名称输入框元素（浏览页持输入框实体 → Some）。
///
/// icon/list 与明细表**共用同一个输入框实体**（shell 建、`browse::set_name_input`
/// 写回 hold）；用户口径 2026-09-28：重命名/新建文件夹时名称就地编辑。
fn name_input_state(cx: &App) -> Option<Entity<gpui_kit::component::input::InputState>> {
    crate::ui::views::browse::filter_hold_in(cx)?
        .read(cx)
        .name_input
        .clone()
}

/// 输入框元素（`AnyElement` 不可克隆——每处消费现场现建）。
fn name_input_element(input: &Entity<gpui_kit::component::input::InputState>) -> AnyElement {
    use gpui_kit::component::Sizable as _;
    gpui_kit::component::input::Input::new(input)
        .with_size(gpui_kit::component::Size::Small)
        .into_any_element()
}

/// 当前行是否处于「改名编辑中」（`name_edit.from == 本行名` 且目录一致）。
fn is_renaming(snap: &AppStateUiSnapshot, name: &str) -> bool {
    snap.name_edit
        .as_ref()
        .is_some_and(|e| e.dir == snap.current_dir && e.from.as_deref() == Some(name))
}

/// 是否处于「新建文件夹」草稿态（当前目录）。
fn is_new_folder_draft(snap: &AppStateUiSnapshot) -> bool {
    snap.name_edit
        .as_ref()
        .is_some_and(|e| e.from.is_none() && e.dir == snap.current_dir)
}

/// 行点击分发（icon/list 共用；语义与 `browse.rs::row_action` 一致）。
///
/// - T-SEL1 单击：修饰键三分支选择（Ctrl 增删 / Shift 区间 / 普通替换——
///   与 detail 同源的 `click_selection` 纯函数，经 `mode_click_selection`
///   走 hold 镜像回显 + 整组上行）；
/// - 双击：`..` 跳上级（on_crumb）；目录/容器进入（on_tree_click）；
///   文件 → **打开**（T-SEL4：动作通道 `BrowseCommand::Open`；用户口径
///   2026-09-28：物化后按扩展名分流指定程序/系统默认程序，加密条目先过密码门；
///   通道未装配的 headless 场景回退旧 on_row_activate）。
#[allow(clippy::too_many_arguments)]
fn dispatch_row_click<T: BrowseHandlerSource + 'static>(
    shell: &WeakEntity<T>,
    event: &ClickEvent,
    is_parent: bool,
    row: Option<&crate::ui::contracts::BrowseRow>,
    current_dir: &str,
    visible: &[super::browse::RowSel],
    window: &mut Window,
    cx: &mut App,
) {
    if event.click_count() < 2 {
        // T-SEL 走查 #1：双通道载荷（隐式目录行 = entry_id None → 按路径选）
        let clicked = match (is_parent, row) {
            (false, Some(r)) => match r.entry_id {
                Some(id) => super::browse::RowSel::Entry(id),
                None if r.is_dir => super::browse::RowSel::Dir(r.path.clone()),
                None => super::browse::RowSel::None,
            },
            _ => super::browse::RowSel::None,
        };
        super::browse::mode_click_selection(
            shell,
            clicked,
            super::browse::merge_modifiers(event.modifiers()),
            visible,
            cx,
        );
        return;
    }
    let (enter_path, activate_path, up_seg) = match (is_parent, row) {
        (true, _) => (
            None,
            None,
            Some(crate::ui::views::browse::parent_seg_of(current_dir)),
        ),
        (false, Some(r)) => {
            let is_dirlike = r.is_dir || r.is_container;
            (
                is_dirlike.then(|| r.path.clone()),
                (!is_dirlike).then(|| r.path.clone()),
                None,
            )
        }
        (false, None) => (None, None, None),
    };
    if let Some(seg) = up_seg {
        let _ = shell.update(cx, |s, _| {
            (s.browse_handlers().on_crumb)(Crumb {
                label: String::new(),
                seg,
                level: 0,
            });
        });
    } else if let Some(p) = enter_path {
        let _ = shell.update(cx, |s, _| (s.browse_handlers().on_tree_click)(p));
    } else if let Some(p) = activate_path {
        match super::browse::command_channel(cx) {
            Some(command) => command(super::browse::BrowseCommand::Open, window, cx),
            None => {
                let _ = shell.update(cx, |s, _| (s.browse_handlers().on_row_activate)(p));
            }
        }
    }
}

/// 行右键交互装配（T-SEL3；icon/list 两模式共用）：
/// `on_mouse_down(Right)` 预处理（未选中行 → 集合替换为该行——先于
/// ContextMenu 包装层的菜单构建）+ `.context_menu` 弹九项菜单（可用性读
/// hold 镜像 = 预处理后现态；动作闭包直调）。
fn row_right_click<T: BrowseHandlerSource + 'static, E>(
    element: E,
    shell: &WeakEntity<T>,
    clicked: super::browse::RowSel,
    is_dirlike: bool,
) -> gpui_kit::component::menu::ContextMenu<E>
where
    E: gpui_kit::InteractiveElement + gpui_kit::ParentElement + gpui_kit::Styled,
{
    use gpui_kit::component::menu::ContextMenuExt as _;
    let pre_shell = shell.clone();
    element
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            // 右键落在行上：容器空白菜单据此抑制自己（用户口径 2026-09-29）
            super::browse::note_right_hit_row(cx);
            super::browse::mode_right_click_preselect(&pre_shell, clicked.clone(), is_dirlike, cx);
        })
        .context_menu(move |menu, _window, cx| {
            let Some(command) = super::browse::command_channel(cx) else {
                return menu;
            };
            let enable = super::browse::menu_enable_now(cx);
            super::browse::build_browse_menu(menu, command, enable)
        })
}

/// 容器空白区右键菜单（三视图共用；用户口径 2026-09-29）。
///
/// 行上右键 → 行自己的菜单负责，这里返回**空菜单**（kit 不渲染空菜单）；
/// 空白处右键 → 目录/整档级菜单（含新建文件夹）。
fn blank_context_menu<E>(element: E) -> gpui_kit::component::menu::ContextMenu<E>
where
    E: gpui_kit::InteractiveElement + gpui_kit::ParentElement + gpui_kit::Styled + 'static,
{
    use gpui_kit::component::menu::ContextMenuExt as _;
    element.context_menu(|menu, _window, cx| {
        if super::browse::take_right_hit_row(cx) {
            return menu; // 行上：让行菜单说话
        }
        let Some((command, can_edit, paste_available)) = super::browse::blank_menu_from(cx) else {
            return menu;
        };
        super::browse::build_browse_menu(
            menu,
            command,
            super::browse::blank_menu_enable(can_edit, paste_available),
        )
    })
}

/// 图标网格模式（大图标 + 文件名；网格平铺）。
fn icon_grid<T: BrowseHandlerSource + 'static>(
    cx: &mut App,
    snap: &AppStateUiSnapshot,
    shell: &WeakEntity<T>,
) -> AnyElement {
    use gpui_kit::component::scroll::ScrollableElement as _;
    let muted = cx.theme().muted_foreground;
    let fg = cx.theme().foreground;
    let current_dir = snap.current_dir.clone();
    let rows = mode_rows(snap);
    // 选中集（快照唯一来源——与 DataTable 的计数/解压选中同源；
    // T-SEL 走查 #1 双通道：隐式目录按路径）
    let selected: std::collections::HashSet<u64> = snap.selection.iter().copied().collect();
    let selected_dirs: std::collections::HashSet<String> =
        snap.selection_dirs.iter().cloned().collect();

    // flex_wrap 流式换行：cell 定宽（96px + gap）随窗宽自然增减列数
    // （T-TVS3——旧实现固定 6 列手工分行，不随窗宽自适应）
    // T-SEL1：可见行载荷序（mode_rows 编址，`..` = RowSel::None）——Shift
    // 区间/锚点编址源；T-SEL 走查 #1 双通道（隐式目录行按路径）。Rc 共享
    // 进每行闭包（免每行克隆 O(n²)）
    let visible: std::rc::Rc<Vec<super::browse::RowSel>> = std::rc::Rc::new(
        rows.iter()
            .map(|(_, r)| match r.as_ref() {
                Some(r) => match r.entry_id {
                    Some(id) => super::browse::RowSel::Entry(id),
                    None if r.is_dir => super::browse::RowSel::Dir(r.path.clone()),
                    None => super::browse::RowSel::None,
                },
                None => super::browse::RowSel::None,
            })
            .collect(),
    );
    let name_input = name_input_state(cx);
    let draft = is_new_folder_draft(snap);
    let mut grid = h_flex().w_full().flex_wrap().gap(sp(2.)).p(sp(2.));
    for (ix, (is_parent, row)) in rows.iter().enumerate() {
        let (icon, label, _entry_id): (AnyElement, String, Option<u64>) = match (is_parent, row) {
            (true, _) => (
                // `..` 行用自有箭头图标（无系统图标语义）
                Icon::new(AppIcon::ArrowUp)
                    .with_size(Size::Size(ICON_CELL_ICON))
                    .text_color(muted)
                    .into_any_element(),
                "..".to_string(),
                None,
            ),
            (false, Some(r)) => (
                crate::ui::views::browse::row_file_icon(
                    r,
                    crate::ui::widgets::file_icon::Slot::Grid,
                    cx,
                ),
                r.name.clone(),
                r.entry_id,
            ),
            (false, None) => continue,
        };
        let cell_shell = shell.clone();
        let row_owned = row.clone();
        let cur = current_dir.clone();
        let is_p = *is_parent;
        let visible_r = visible.clone();
        let is_dirlike = row.as_ref().is_some_and(|r| r.is_dir || r.is_container);
        // 选中高亮：secondary 纯色背景（无边框——T-SEL 走查 #2）。双通道：
        // 实体行按 id、隐式目录行按路径；`..` 行恒不高亮
        let row_sel = match row.as_ref() {
            Some(r) => match r.entry_id {
                Some(id) => selected.contains(&id),
                None => r.is_dir && selected_dirs.contains(&r.path),
            },
            None => false,
        };
        let is_selected = row_sel;
        let clicked_sel = match row.as_ref() {
            Some(r) => match r.entry_id {
                Some(id) => super::browse::RowSel::Entry(id),
                None if r.is_dir => super::browse::RowSel::Dir(r.path.clone()),
                None => super::browse::RowSel::None,
            },
            None => super::browse::RowSel::None,
        };
        grid = grid.child(row_right_click(
            div()
                .id(SharedString::from(format!("icon-cell-{ix}")))
                .w(ICON_CELL_W)
                .flex_shrink_0()
                .flex()
                .flex_col()
                .items_center()
                .gap(sp(1.))
                .p(sp(1.))
                .rounded_sm()
                .when(is_selected, |c| {
                    c.bg(crate::ui::theme::selection_row_bg(cx))
                })
                .hover(|s| s.bg(cx.theme().secondary))
                .child(
                    // 32px（Size 档位到 Large=24px 封顶——像素档升一档到桌面
                    // 图标视图的经典尺寸；T-TVS 走查反馈）
                    icon,
                )
                .child(
                    // 改名中的格子：名称换成行内输入框（用户口径 2026-09-28）
                    match (is_renaming(snap, &label), name_input.as_ref()) {
                        (true, Some(input)) => div()
                            .w_full()
                            .child(name_input_element(input))
                            .into_any_element(),
                        _ => {
                            let mut d = div()
                                .w_full()
                                .text_xs()
                                .text_center()
                                .text_color(fg)
                                .truncate()
                                .id(("grid-tip", ix));
                            let hs = shell.clone();
                            let ms = shell.clone();
                            let tip_text = label.clone();
                            d = d
                                .on_hover(move |hovered: &bool, _w, cx: &mut gpui_kit::App| {
                                    let t = hovered
                                        .then(|| gpui_kit::SharedString::from(tip_text.clone()));
                                    let _ = hs.update(cx, |s, cx| {
                                        s.browse_set_hover_tip(t, None, cx)
                                    });
                                })
                                .on_mouse_move(
                                    move |ev: &gpui_kit::MouseMoveEvent, _w, cx: &mut gpui_kit::App| {
                                        let x: f32 = ev.position.x.into();
                                        let y: f32 = ev.position.y.into();
                                        let pos = (x as i32, y as i32);
                                        let _ = ms.update(cx, |s, cx| {
                                            s.browse_set_hover_tip(None, Some(pos), cx)
                                        });
                                    },
                                );
                            d.child(SharedString::from(label)).into_any_element()
                        }
                    },
                )
                .on_click(move |event: &ClickEvent, window, cx| {
                    dispatch_row_click(
                        &cell_shell,
                        event,
                        is_p,
                        row_owned.as_ref(),
                        &cur,
                        &visible_r,
                        window,
                        cx,
                    );
                }),
            shell,
            clicked_sel,
            is_dirlike,
        ));
    }
    // 「新建文件夹」草稿格（用户口径 2026-09-28）：无点击语义，只有输入框
    if draft && let Some(input) = name_input.as_ref() {
        grid = grid.child(
            div()
                .id("icon-new-folder-draft")
                .w(ICON_CELL_W)
                .flex_shrink_0()
                .flex()
                .flex_col()
                .items_center()
                .gap(sp(1.))
                .p(sp(1.))
                .rounded_sm()
                .bg(crate::ui::theme::selection_row_bg(cx))
                .child(
                    Icon::new(AppIcon::Folder)
                        .with_size(Size::Size(ICON_CELL_ICON))
                        .text_color(muted),
                )
                .child(div().w_full().child(name_input_element(input))),
        );
    }
    if rows.is_empty() && !draft {
        grid = grid.child(
            div()
                .w_full()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(crate::tr("browse.empty_dir"))),
        );
    }

    blank_context_menu(
        v_flex()
            .id("icon-grid-wrap")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(grid),
    )
    .into_any_element()
}

/// 紧凑列表模式（双列行：名称 + 尺寸）。
fn compact_list<T: BrowseHandlerSource + 'static>(
    window: &Window,
    cx: &mut App,
    snap: &AppStateUiSnapshot,
    shell: &WeakEntity<T>,
) -> AnyElement {
    use gpui_kit::component::scroll::ScrollableElement as _;
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let current_dir = snap.current_dir.clone();
    let rows = mode_rows(snap);
    // 选中集（与 icon_grid 同源——快照唯一来源；双通道含隐式目录路径）
    let selected: std::collections::HashSet<u64> = snap.selection.iter().copied().collect();
    let selected_dirs: std::collections::HashSet<String> =
        snap.selection_dirs.iter().cloned().collect();

    let mut list = v_flex().w_full();
    // T-SEL1：可见行载荷序（同 icon_grid——Rc 共享免 O(n²) 克隆；双通道）
    let visible: std::rc::Rc<Vec<super::browse::RowSel>> = std::rc::Rc::new(
        rows.iter()
            .map(|(_, r)| match r.as_ref() {
                Some(r) => match r.entry_id {
                    Some(id) => super::browse::RowSel::Entry(id),
                    None if r.is_dir => super::browse::RowSel::Dir(r.path.clone()),
                    None => super::browse::RowSel::None,
                },
                None => super::browse::RowSel::None,
            })
            .collect(),
    );
    let name_input = name_input_state(cx);
    let draft = is_new_folder_draft(snap);
    for (ix, (is_parent, row)) in rows.iter().enumerate() {
        let (icon, label, size_text, _entry_id): (AnyElement, String, String, Option<u64>) =
            match (is_parent, row) {
                (true, _) => (
                    // `..` 行用自有箭头图标（无系统图标语义）——尺寸与同排文件图标
                    // 同槽位（否则箭头与下面的行图标一大一小）
                    Icon::new(AppIcon::ArrowUp)
                        .with_size(Size::Size(file_icon::slot_px(file_icon::Slot::Inline, cx)))
                        .text_color(muted)
                        .into_any_element(),
                    "..".to_string(),
                    String::new(),
                    None,
                ),
                (false, Some(r)) => (
                    crate::ui::views::browse::row_file_icon(
                        r,
                        crate::ui::widgets::file_icon::Slot::Inline,
                        cx,
                    ),
                    r.name.clone(),
                    r.size_text.clone(),
                    r.entry_id,
                ),
                (false, None) => continue,
            };
        // 选中高亮：secondary 纯色背景（无边框——T-SEL 走查 #2）。双通道：
        // 实体行按 id、隐式目录行按路径；`..` 行恒不高亮
        let is_selected = match row.as_ref() {
            Some(r) => match r.entry_id {
                Some(id) => selected.contains(&id),
                None => r.is_dir && selected_dirs.contains(&r.path),
            },
            None => false,
        };
        let clicked_sel = match row.as_ref() {
            Some(r) => match r.entry_id {
                Some(id) => super::browse::RowSel::Entry(id),
                None if r.is_dir => super::browse::RowSel::Dir(r.path.clone()),
                None => super::browse::RowSel::None,
            },
            None => super::browse::RowSel::None,
        };
        let cell_shell = shell.clone();
        let row_owned = row.clone();
        let cur = current_dir.clone();
        let is_p = *is_parent;
        let visible_r = visible.clone();
        let is_dirlike = row.as_ref().is_some_and(|r| r.is_dir || r.is_container);
        list = list.child(row_right_click(
            h_flex()
                .id(SharedString::from(format!("list-row-{ix}")))
                .w_full()
                .items_center()
                .gap(sp(2.))
                .px(sp(2.))
                // 行上下与目录树/明细表同口径：紧凑 0 / 标准 1 / 宽松 2px
                .py(crate::ui::spacing::row_gap())
                // 文字行框同样跟 `[ui].line_height`（三个视图一套节奏；行高由
                // 内容撑开，不像明细表那样需要与行盒同步——但不同步就会「视图
                // 之间疏密不一」）
                // 与明细列表同一口径：行框 = 墨迹盒下限（px，不是 relative(em)）
                .line_height(crate::ui::spacing::text_line_floor(
                    window,
                    window.rem_size() * 0.875,
                ))
                .border_b_1()
                .border_color(border)
                .when(is_selected, |c| {
                    c.bg(crate::ui::theme::selection_row_bg(cx))
                })
                .hover(|s| s.bg(cx.theme().secondary))
                .child(icon)
                .child(
                    // 改名中的行：名称换成行内输入框（用户口径 2026-09-28）
                    match (is_renaming(snap, &label), name_input.as_ref()) {
                        (true, Some(input)) => div()
                            .flex_1()
                            .min_w_0()
                            .child(name_input_element(input))
                            .into_any_element(),
                        _ => {
                            let mut d = div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .truncate()
                                .id(("list-tip", ix));
                            let hs = shell.clone();
                            let ms = shell.clone();
                            let tip_text = label.clone();
                            d = d
                                .on_hover(move |hovered: &bool, _w, cx: &mut gpui_kit::App| {
                                    let t = hovered
                                        .then(|| gpui_kit::SharedString::from(tip_text.clone()));
                                    let _ = hs.update(cx, |s, cx| {
                                        s.browse_set_hover_tip(t, None, cx)
                                    });
                                })
                                .on_mouse_move(
                                    move |ev: &gpui_kit::MouseMoveEvent, _w, cx: &mut gpui_kit::App| {
                                        let x: f32 = ev.position.x.into();
                                        let y: f32 = ev.position.y.into();
                                        let pos = (x as i32, y as i32);
                                        let _ = ms.update(cx, |s, cx| {
                                            s.browse_set_hover_tip(None, Some(pos), cx)
                                        });
                                    },
                                );
                            d.child(SharedString::from(label)).into_any_element()
                        }
                    },
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(size_text)),
                )
                .on_click(move |event: &ClickEvent, window, cx| {
                    dispatch_row_click(
                        &cell_shell,
                        event,
                        is_p,
                        row_owned.as_ref(),
                        &cur,
                        &visible_r,
                        window,
                        cx,
                    );
                }),
            shell,
            clicked_sel,
            is_dirlike,
        ));
    }
    // 「新建文件夹」草稿行（用户口径 2026-09-28）：无点击语义，只有输入框
    if draft && let Some(input) = name_input.as_ref() {
        list = list.child(
            h_flex()
                .id("list-new-folder-draft")
                .w_full()
                .items_center()
                .gap(sp(2.))
                .px(sp(2.))
                .py(crate::ui::spacing::row_gap())
                .border_b_1()
                .border_color(border)
                .bg(crate::ui::theme::selection_row_bg(cx))
                .child(
                    Icon::new(AppIcon::Folder)
                        .with_size(Size::Size(file_icon::slot_px(file_icon::Slot::Inline, cx)))
                        .text_color(muted),
                )
                .child(div().flex_1().min_w_0().child(name_input_element(input)))
                .child(div().flex_shrink_0().text_xs().text_color(muted).child("")),
        );
    }
    if rows.is_empty() && !draft {
        list = list.child(
            div()
                .p(sp(2.))
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(crate::tr("browse.empty_dir"))),
        );
    }

    blank_context_menu(
        v_flex()
            .id("compact-list-wrap")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(list),
    )
    .into_any_element()
}

#[cfg(test)]
#[path = "tests/browse_layout.rs"]
mod tests;
