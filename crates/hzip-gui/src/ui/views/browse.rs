//! 浏览页内容区（T-G20 → T-G53 组件化重写；06 §4.1）。
//!
//! 双栏布局（[`crate::ui::views::browse_layout`]）+ gpui-kit 组件渲染：
//! 面包屑 `Breadcrumb`（widgets/breadcrumb.rs）、左栏 `Tree`、右栏
//! `DataTable`（列头点击排序：升/降/默认三态循环）。
//! 数据面：`AppStateUiSnapshot`（nav_rows/dir_rows 快照）→ [`crate::ui::binding`]
//! 派生。快照语义 = 只读投影：排序/选择均为 UI 本地态。
//!
//! # 虚拟滚动（TC-GUI-007 闭环）
//!
//! 左右栏虚拟化由 gpui-kit 内建（`TreeState` / `TableState` 各自的虚拟行
//! 渲染），本文件不出现平铺列表原语（红线 grep 零命中）。
//!
//! # 实体稳态纪律
//!
//! `TreeState` / `TableState` / `InputState` 经 [`BrowseHold`]（Global）持有
//! Entity 句柄，渲染期复用——禁止每 render 重建（重建会丢选中/展开/输入态）。
//! Global 以 `TypeId` 键控：单实例语义（02 §11.2）下同进程一份浏览页态。
//!
//! # 事件上行（'static 约束解法）
//!
//! gpui 事件闭包要求 `'static`，而 `ShellHandlers` 字段为 `Box<dyn Fn>`
//! （不可 Clone、不可自 `&` 提权）。解法：事件闭包一律捕获
//! `WeakEntity<T: BrowseHandlerSource>`（`AppShell` 实现——孤儿 impl 在本文件，
//! 不动 shell/contracts.rs），触发时升级后就地调 `handlers` 对应字段——
//! T-G30 装配的真实闭包始终是热路径；默认 no-op（ShellHandlers::default）
//! 时输入态/选中态在 UI 本地保持（02 §11.1 快照投影语义）。
//!
//! # 交互语义（左树；R-GUI 走查 #2 统一「路径跳转事件」）
//!
//! - 单击目录 = 选中 + 进入（`on_tree_click` → 共用 enter_dir_handler：
//!   enter_dir + **expand_to 展开父链**——树跟进定位当前目录）
//! - 双击目录 = 翻转该目录展开态（browse_layout `click_count` 分流 →
//!   `on_tree_toggle` 回写 VM expanded 集；只动被双击的目录）
//! - 右栏表格双击目录 / 面包屑段 / `..` 行 = 同一跳转事件（enter_dir_handler）；
//!   树选中 + 滚动由 `browse_root` 按 current_dir 变化同步
//! - 行包装层 `TreeState::on_entry_click` 的内建单击 toggle 已被
//!   `ListItem::on_mouse_down` + `stop_propagation` 压制（详见
//!   browse_layout 模块文档）；键盘展开（方向键/Enter）的 `TreeEvent`
//!   仍经下方订阅回写 VM（保持键盘语义）
//! - VM expanded 集是唯一事实源；快照变化才 `set_items`（防丢内部态）。

use crate::ui::constant::{
    CELL_GUTTER_BASE, COL_ATTRS_W, COL_CRC_W, COL_ENCODING_W, COL_ENCRYPTION_W, COL_LEVEL_W,
    COL_MTIME_W, COL_NAME_W, COL_PACKED_W, COL_SIZE_W, COL_TYPE_W, LIST_ROW_BORDER_H,
};
use crate::ui::spacing::sp;
use gpui_kit::App;
use gpui_kit::Global;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{
    ColumnSort, TableDelegate, TableEvent, TableSelection, TableState,
};
use gpui_kit::component::tree::{TreeEvent, TreeState};
use gpui_kit::component::{ActiveTheme, Icon, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, Modifiers, MouseButton, MouseDownEvent, SharedString, Styled,
    Window, div,
};
use gpui_kit::{Subscription, WeakEntity};

use crate::ui::binding::{
    browse_row_from_tree_row, show_parent_row, sort_browse_rows, tree_items_from_nav,
};
use crate::ui::contracts::{
    AppStateUiSnapshot, BrowseColumnId, BrowseRow, Crumb, ShellHandlers, SortColumn, SortDir,
};
use crate::ui::widgets::icon::AppIcon;

/// `..` 行目标段推导（T-GUI T12 收口：原三处内联推导/死代码副本归一；
/// path 剥尾段——`docs/src/readme.txt` → `docs/src`，顶层 → ""）。
pub(crate) fn parent_seg_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// 浏览页稳态句柄（Global 单例）。
///
/// 生命周期 = 首次渲染创建至进程退出；数据每帧从快照重灌，句柄仅保交互态。
/// 事件处理不存句柄——订阅/点击闭包捕获 `WeakEntity<T>`，触发时就地调
/// `T::browse_handlers()`（见模块级「事件上行」）。
pub(crate) struct BrowseHold {
    /// 过滤输入框（T-MENU1 查找聚焦入口；防抖订阅 → FilterChanged 上行）
    pub filter_input: Entity<gpui_kit::component::input::InputState>,
    /// 左栏目录树交互态（选中/滚动；expanded 经快照同步）
    pub tree: Entity<TreeState>,
    /// 右栏表格交互态（选中/滚动）
    pub table: Entity<TableState<BrowseDelegate>>,
    /// 上次喂树的 nav 快照签名（变化才 set_items——防丢展开/滚动态）
    pub nav_sig: u64,
    /// 上次同步给左树的 current_dir（变化才改选中 + 滚动——不覆盖本地选中）
    pub last_dir: String,
    /// 上次灌入表格的 dir 快照签名（T-PERF2：变化才重灌委托行——与左树
    /// `nav_sig` 同款门控；旧实现每帧重灌 + notify，行集不变时是纯开销）
    pub dir_sig: u64,
    /// 上次灌入的 `..` 显隐输入（show_dotdot / 面包屑父段）
    pub last_show_dotdot: bool,
    pub last_show_parent: bool,
    /// 上次灌入的列配置（配置变化才重置 visible——实时拖动序不被覆盖）
    pub last_columns: crate::ui::contracts::ColumnsConfig,
    /// 分栏状态（左树宽度；gpui-kit Resizable 像素制，R-GUI2-04 §3.1）
    pub split: Entity<gpui_kit::base::ResizableState>,
    /// 目录树宽度记忆（px；初值 `[ui].tree_width`，拖动结束回写）。
    ///
    /// 面板从 Resizable 组里增删会让尺寸表按**索引**重分配——树重新显示
    /// 若不管，索引 0 会继承表格的全宽（用户实测「很长的目录树」）。故宽度
    /// 由本字段记忆，重新显示时按它插回面板（见 `restore_tree_panel_width`）。
    pub tree_width: f32,
    /// 上次快照的 `show_tree`（显隐切换沿检测；切换时修尺寸表）
    pub last_show_tree: bool,
    /// icon/list 模式选中集镜像（T-SEL1：点击集合运算的当前集——点击时经
    /// Global 读取（render 闭包捕获会滞后一帧，快速连点会算错）；快照回流
    /// 与点击本地回显双写，内容一致幂等）
    pub mode_selection: std::collections::HashSet<u64>,
    /// icon/list 模式隐式目录选中集镜像（T-SEL 走查 #1：目录行高亮/集合
    /// 运算第二通道——与 `mode_selection` 双通道）
    pub mode_selected_dirs: std::collections::HashSet<String>,
    /// icon/list 模式 Shift 区间锚点（T-SEL1：可见行索引——含 `..` 行占位
    /// 的 mode_rows 编址，与 detail 委托行编址同构；行集变化时失效清空）
    pub mode_anchor: Option<usize>,
    /// 右键菜单可用性（T-SEL3：icon/list 的 `ContextMenuExt` 闭包构建期
    /// 读此镜像——detail 模式由委托 `context_menu` 钩子现算，不经此字段）
    pub menu_enable: MenuEnable,
    /// 右键菜单/双击文件动作通道（T-SEL3：装配期从 shell Weak 构造；
    /// None = headless 测试装配——菜单动作 no-op）
    pub command: Option<BrowseCommandFn>,
    /// 树行元数据（path → is_dir；空目录图标判定源，§3.2/§3.3）
    pub tree_meta: crate::ui::binding::TreeMeta,
    /// 行内名称编辑的输入框实体（shell 建/销毁时经 [`set_name_input`] 写回；
    /// `None` = 未编辑）。渲染侧由委托镜像消费。
    pub name_input: Option<Entity<gpui_kit::component::input::InputState>>,
    /// 上次名称编辑签名（编辑态 + 输入框实体 + 可编辑位；变化才重灌表格委托）
    pub last_name_sig: NameEditSig,
    /// 订阅保活（drop 即退订——必须存实体上）
    _subscriptions: Vec<Subscription>,
}

/// 名称编辑签名（重灌门控用；`entity_id` 数值化——`Entity` 不实现 PartialEq）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NameEditSig {
    /// 编辑态（目标目录 + 源条目）
    pub edit: Option<(String, Option<String>)>,
    /// 输入框实体 id（None = 无）
    pub input: Option<usize>,
    /// 可编辑（caps.edit 且非嵌套）
    pub can_edit: bool,
}

/// Global 存储壳（Global 绑定在值类型上——包一层持 Entity 句柄）。
struct BrowseGlobal(Entity<BrowseHold>);

impl Global for BrowseGlobal {}

/// 过滤输入句柄（T-MENU1 菜单「查找文件」聚焦入口；浏览页未渲染 → None）。
pub(crate) fn filter_hold_in(cx: &App) -> Option<Entity<BrowseHold>> {
    cx.try_global::<BrowseGlobal>().map(|g| g.0.clone())
}

/// 浏览页右键菜单/双击文件动作（T-SEL3：闭包直调通道）。
///
/// 不走 `dispatch_action` 焦点链（T-PERF §8 X 失灵教训：弹层关闭后焦点可能
/// 残留在已移除元素上导致派发落空）——菜单项 on_click 闭包经
/// [`BrowseHold::command`] 直调 shell 的 [`BrowseHandlerSource::browse_command`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseCommand {
    /// 解压全部文件（`ToolbarAction::ExtractAll` 同路径）
    ExtractAll,
    /// 解压选定文件（`ToolbarAction::Extract` 同路径）
    ExtractSel,
    /// 复制选定条目到剪贴板（T-CP P0：路由到 shell `copy_selection`——内部
    /// 剪贴板 + 系统文件列表；路由臂由集成方在 `AppShell::browse_command`
    /// 落地）
    Copy,
    /// 粘贴剪贴板条目到当前目录（T-CP P0：路由到 shell `paste_files`——内部
    /// 剪贴板物化 + 系统剪贴板 `ExternalPaths`；路由臂由集成方落地）
    Paste,
    /// 打开（双击文件/菜单项：单选文件 → 物化后按扩展名分流指定程序/系统默认；
    /// 加密条目先过密码门——用户口径 2026-09-28）
    Open,
    /// 打开方式…（用户口径 2026-09-28：物化后由**系统询问对话框**选程序；
    /// 加密条目同「打开」先过密码门）
    OpenWith,
    /// 编辑（T-SEL5 挂账——一期占位禁用）
    Edit,
    /// 删除文件（`begin_delete_files` 同路径）
    Delete,
    /// 添加文件（`begin_add_files` 同路径）
    AddFiles,
    /// 全部选择（`main.select_all`）
    SelectAll,
    /// 新建文件夹（T-SEL6 挂账——一期占位禁用）
    NewFolder,
    /// 重命名（T-SEL6 挂账——一期占位禁用）
    Rename,
}

/// 动作通道闭包类型（T-SEL3：菜单项 on_click / 装配闭包共用签名——
/// clippy 复杂类型收编别名）
pub(crate) type BrowseCommandFn = std::rc::Rc<dyn Fn(BrowseCommand, &mut Window, &mut App)>;

/// 浏览页事件源（shell `AppShell` 实现——trait 与孤儿 impl 均在本文件）。
///
/// `'static` 上界：trait 方法签名含 `Context<Self>`（gpui 要求 `'static`）。
pub trait BrowseHandlerSource: 'static {
    /// 取 shell handlers 引用（事件闭包运行时就地调用）。
    fn browse_handlers(&self) -> &ShellHandlers;
    /// 右键菜单/双击文件动作路由（T-SEL3；AppShell 实现直调内部方法）。
    fn browse_command(&mut self, cmd: BrowseCommand, window: &mut Window, cx: &mut Context<Self>)
    where
        Self: Sized;
    /// 双击文件打开（T-SEL4：无窗口入口——预览/外部打开均不开对话框，
    /// 表格订阅（无 window）直调）。
    fn browse_open_selection(&mut self, cx: &mut Context<Self>)
    where
        Self: Sized;
    /// 双击**容器条目**（内层压缩包）→ 打开成下一层会话（R-GUI2-06）。
    fn browse_open_container(&mut self, entry_path: &str, cx: &mut Context<Self>)
    where
        Self: Sized;
    /// 嵌套会话档案根 `..` → 退出该层（回到外层档案）。
    fn browse_leave_container(&mut self, cx: &mut Context<Self>)
    where
        Self: Sized;
    /// 文件列表**空白区左键** → 取消选中（用户口径 2026-09-28：与资源管理器
    /// 一致——点空白处清空选中与焦点）。
    fn browse_clear_selection(&mut self, cx: &mut Context<Self>)
    where
        Self: Sized;
    /// 左栏树单击 → 跳到该节点（`id` = `"{level}:{path}"`）。
    ///
    /// 目标可能是**外层会话**的目录（先弹回那层再进），也可能是内层压缩包
    /// （开下一层会话）——两者都需要 `cx`（开窗/起后台任务），故不走
    /// `ShellHandlers` 槽（那些槽没有 cx）。
    fn browse_tree_click(&mut self, id: &str, cx: &mut Context<Self>)
    where
        Self: Sized;
}

impl BrowseHandlerSource for crate::ui::shell::AppShell {
    fn browse_handlers(&self) -> &ShellHandlers {
        &self.handlers
    }

    fn browse_command(&mut self, cmd: BrowseCommand, window: &mut Window, cx: &mut Context<Self>) {
        self.browse_command(cmd, window, cx);
    }

    fn browse_open_selection(&mut self, cx: &mut Context<Self>) {
        self.open_selection_entry(cx);
    }

    fn browse_open_container(&mut self, entry_path: &str, cx: &mut Context<Self>) {
        self.open_container(entry_path, cx);
    }

    fn browse_leave_container(&mut self, cx: &mut Context<Self>) {
        self.leave_container(cx);
    }

    fn browse_tree_click(&mut self, id: &str, cx: &mut Context<Self>) {
        self.tree_click(id, cx);
    }

    fn browse_clear_selection(&mut self, cx: &mut Context<Self>) {
        self.clear_selection_now();
        // **聚焦**也要清（用户口径 2026-09-28）：kit 的 `TableState::selected_row`
        // 是另一套「焦点行」状态，不清的话行高亮层继续按 `kit_focus` 画那一行
        // ——用户看到的是「点了空白，那一行还亮着」（选中集清空 ≠ 聚焦清空）。
        // 0.7.1 的 `clear_selection` 会把选择模式一并复位为 `Row` 且清空
        // `selected_row`（`table/state.rs:617-624`）⇒ `browse_root` 的
        // `selection()` 镜像取到 `None`，自绘高亮层随之消失。
        if let Some(hold) = crate::ui::views::browse::filter_hold_in(cx) {
            hold.update(cx, |hold, cx| {
                hold.table.update(cx, |table, cx| {
                    table.clear_selection(cx);
                });
            });
        }
    }
}

/// 浏览页根渲染（shell 内容插槽按 `MainTab::Browse` 分支调用）。
///
/// 未打开档案 → 引导空态；已打开 → 双栏布局（Tree/DataTable/面包屑/过滤行）。
pub fn browse_root<T: BrowseHandlerSource + 'static>(
    window: &mut Window,
    cx: &mut App,
    snap: &AppStateUiSnapshot,
    shell: WeakEntity<T>,
) -> AnyElement {
    if snap.archive_name.is_empty() {
        return empty_state(cx);
    }
    let hold = browse_hold(window, cx, shell.clone());

    // —— 数据重灌门控（左树 nav_sig 既有先例 + T-PERF2 表格 dir_sig）——
    let nav_sig = nav_snapshot_sig(&snap.nav_rows);
    let dir_sig = dir_snapshot_sig(&snap.dir_rows);
    let parent_row = show_parent_row(&snap.crumbs);
    let cur_dir_changed = hold.read(cx).last_dir != snap.current_dir;
    let nav_changed = hold.read(cx).nav_sig != nav_sig;
    // 行内名称编辑签名（编辑态 + 输入框实体 + 可编辑位）
    let name_sig = NameEditSig {
        edit: snap
            .name_edit
            .as_ref()
            .map(|e| (e.dir.clone(), e.from.clone())),
        input: hold
            .read(cx)
            .name_input
            .as_ref()
            .map(|e| e.entity_id().as_u64() as usize),
        can_edit: snap.caps_edit && !snap.nested,
    };
    // 表格委托输入（行内容 + `..` 显隐 + 列配置 + 名称编辑）任一变化才重灌
    let dir_inputs_changed = hold.read(cx).dir_sig != dir_sig
        || hold.read(cx).last_show_dotdot != snap.show_dotdot
        || hold.read(cx).last_show_parent != parent_row
        || hold.read(cx).last_columns != snap.columns
        || hold.read(cx).last_name_sig != name_sig
        || cur_dir_changed;
    // T-SEL1：selection 变化 → 委托/hold 选中集镜像同步（detail 高亮 +
    // icon/list 点击集合运算的数据源）。逐字段按引用比较（元组等值 == 字段等值，
    // Vec 的 PartialEq 逐元素比较且不分配）——集合重建仅变化时发生；
    // 行集不变的选择变化不再被 dir 门控吞掉。
    let sel_changed = {
        let d = hold.read(cx).table.read(cx).delegate();
        d.last_sel_sync.0 != snap.selection || d.last_sel_sync.1 != snap.selection_dirs
    };
    // kit 单选焦点行 → 委托镜像（每帧、表格渲染前）：行高亮层据此把 `..` 行
    // 与面包屑跳转后的聚焦行也画成自绘浅色（否则露 kit 的全量 accent）。
    //
    // **0.7.1 语义变化**：`TableState::selected_row()` 现在按 `SelectionMode`
    // 过滤（`gpui-component-0.7.1/src/table/state.rs:496-498`
    // `self.selected_row.filter(|_| self.selection_mode.is_row())`）。点列头
    // （表头 `on_click` → `on_col_head_click` → `set_selected_col`，
    // `table/state.rs:822-836`、`:550-558`）会把模式切成 `Column` ⇒ 该帧
    // `selected_row()` 返回 `None`；照 0.6 的写法直接取它，点列头排序后就会
    // 把刚聚焦的行从自绘高亮层里清掉（0.6 的 `selected_row()` 无此过滤，行为
    // 是保留旧行）。
    //
    // 我们的口径 = **保持 0.6 行为**：只有 kit 明确处于行选择（`selection()`
    // = `Row`）或无选择（`None`：清空选中 / 空白区左键）时才接受镜像值；
    // 列 / 单元格模式沿用上一帧值（行焦点语义与选择模式无关，用户口径：
    // 点列头排序不得丢焦点行高亮）。
    //
    // 为什么不改用 `set_selection(TableSelection::Row(ix))` 回写：那会重发
    // `TableEvent::SelectRow`（触发我们的行单击分发 / 选中集上行）、
    // `cx.stop_propagation()` 与滚动定位（`table/state.rs:501-524`），副作用
    // 远大于「保住焦点行高亮」这一件事。
    let (kit_focus, prev_focus) = {
        let table = hold.read(cx).table.read(cx);
        let prev = table.delegate().kit_focus;
        let focus = match table.selection() {
            TableSelection::Row(row_ix) => Some(row_ix),
            TableSelection::None => None,
            // 列头（`Column`）/ 单元格（`Cell`）模式：`selected_row()` 恒为
            // `None`，但用户并没有改焦点行——沿用上一帧镜像
            TableSelection::Column(_) | TableSelection::Cell(..) => prev,
        };
        (focus, prev)
    };
    if prev_focus != kit_focus {
        hold.update(cx, |hold, cx| {
            hold.table.update(cx, |table, _| {
                table.delegate_mut().kit_focus = kit_focus;
            });
        });
    }
    hold.update(cx, |hold, cx| {
        hold.nav_sig = nav_sig;
        if nav_changed {
            // tree_meta 与 set_items 同批替换（滞后一帧图标闪错，§6.2）——
            // meta 查不到的 id 回落文件图标。
            let (items, meta) = tree_items_from_nav(&snap.nav_rows);
            hold.tree_meta = meta;
            hold.tree.update(cx, |tree, cx| tree.set_items(items, cx));
        }
        // 左树跟进 current_dir（R-GUI 走查 #2：导航事件统一入口已 expand_to
        // 展开父链，此处同步选中行 + 滚动到位）。只在 current_dir 变化时刷新
        // ——不覆盖用户的本地选中；目标不可见（父链未展开）→ 不动选中。
        if hold.last_dir != snap.current_dir {
            hold.last_dir = snap.current_dir.clone();
            if let Some(ix) = snap
                .nav_rows
                .iter()
                .position(|r| r.level == 0 && r.path == snap.current_dir)
            {
                hold.tree.update(cx, |tree, cx| {
                    tree.set_selected_index(Some(ix), cx);
                    tree.scroll_to_item(ix, gpui_kit::ScrollStrategy::Top);
                });
            }
        }
        // —— 数据重灌：表格委托行 + `..` 显隐（委托实时查询渲染）——
        // T-PERF2：门控后才重灌（旧实现每帧重灌 + refresh + notify）
        if dir_inputs_changed {
            hold.dir_sig = dir_sig;
            hold.last_show_dotdot = snap.show_dotdot;
            hold.last_show_parent = parent_row;
            hold.last_columns.clone_from(&snap.columns);
            hold.last_name_sig = name_sig.clone();
            // T-SEL1：行序可能变（排序重放/过滤/导航）→ Shift 锚点失效
            hold.mode_anchor = None;
            hold.table.update(cx, |table, cx| {
                let delegate = table.delegate_mut();
                delegate.anchor = None;
                delegate.rows = snap.dir_rows.iter().map(browse_row_from_tree_row).collect();
                // 当前目录（`..` 目标段推导源；曾用 rows.first() 推导——空目录/
                // `..` 混入时取错目标，跳上级失效）
                delegate.current_dir = snap.current_dir.clone();
                // 排序态保持（R-GUI 走查：列头排序后每帧重灌不得丢序——
                // Default 态本就是原序等价排序，重复应用无害）
                if let Some((id, dir)) = delegate.sort {
                    sort_delegate_rows(&mut delegate.rows, id, dir);
                }
                // `..` 行显隐消费 ui.show_dotdot（R2-04 §3.5——非根 && 开关开）
                delegate.show_dotdot = snap.show_dotdot;
                delegate.show_parent = snap.show_dotdot && parent_row;
                delegate.nested = snap.nested;
                // 行内名称编辑（用户口径 2026-09-28）：编辑目标只对**当前目录**生效
                delegate.can_edit = name_sig.can_edit;
                delegate.name_draft = snap
                    .name_edit
                    .as_ref()
                    .is_some_and(|e| e.from.is_none() && e.dir == snap.current_dir);
                delegate.name_rename = snap.name_edit.as_ref().and_then(|e| {
                    (e.dir == snap.current_dir)
                        .then(|| e.from.clone())
                        .flatten()
                });
                delegate.name_input = hold.name_input.clone();
                // 列序从快照同步（**唯一来源**：设置页/拖列 → 落盘 → 泵快照 →
                // 此处；配置变化才重置 visible，拖动列序产生的实时序不被每帧覆盖）
                if delegate.columns != snap.columns {
                    delegate.columns.clone_from(&snap.columns);
                    delegate.visible = visible_columns_of(&delegate.columns);
                }
                table.refresh(cx);
                cx.notify();
            });
            cx.notify();
        }
        // —— 目录树显隐切换：修尺寸表（T-MENU5 双入口即时生效的补丁） ——
        // 面板从 Resizable 组里增删会让尺寸表按索引重分配；重新显示若不管，
        // 索引 0 会继承表格的全宽 →「很长的目录树」（用户实测）。故按记忆
        // 宽度把面板插回（`restore_tree_panel_width`）。
        if hold.last_show_tree != snap.show_tree {
            hold.last_show_tree = snap.show_tree;
            if snap.show_tree {
                let width = hold.tree_width;
                crate::ui::views::browse_layout::restore_tree_panel_width(&hold.split, width, cx);
            }
        }
        // —— T-SEL1：选中集镜像同步（detail 委托 selected_ids + icon/list
        // mode_selection/menu_enable；点击本地回显先落，快照回流后内容一致
        // ——幂等）——
        if sel_changed {
            let enable = menu_enable_of_rows(
                &snap.selection,
                &snap.selection_dirs,
                &snap.dir_rows,
                snap.caps_edit && !snap.nested,
                // 快照同步在每帧渲染前（browse_root 数据重灌段）——渲染帧
                // 红线：不查剪贴板，沿用上次镜像值（右键预处理会现算刷新）
                hold.menu_enable.paste_available,
            );
            hold.mode_selection = snap.selection.iter().copied().collect();
            hold.mode_selected_dirs = snap.selection_dirs.iter().cloned().collect();
            hold.menu_enable = enable;
            hold.table.update(cx, |table, cx| {
                let delegate = table.delegate_mut();
                delegate.last_sel_sync.0.clone_from(&snap.selection);
                delegate.last_sel_sync.1.clone_from(&snap.selection_dirs);
                delegate.selected_ids = snap.selection.iter().copied().collect();
                delegate.selected_dirs = snap.selection_dirs.iter().cloned().collect();
                cx.notify();
            });
        }
    });

    let crumbs: Vec<Crumb> = snap.crumbs.clone();
    crate::ui::views::browse_layout::browse_layout(window, cx, &hold, &crumbs, &shell, snap)
}

/// 空态：未打开档案的引导文案（06 §4.1 空态约定）。
fn empty_state(cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(sp(2.))
        .bg(cx.theme().background)
        .text_color(muted)
        .child(Icon::new(AppIcon::Archive).text_color(muted))
        .child(
            div()
                .text_sm()
                .child(SharedString::from(crate::tr("browse.empty_title"))),
        )
        .child(
            div()
                .text_xs()
                .child(SharedString::from(crate::tr("browse.empty_hint"))),
        )
        .into_any_element()
}

/// 行签名混入：FNV-1a 64 的 8 字节批量变体——输入按 8 字节一组 + u64 乘法混入，
/// 尾部逐字节收尾，并额外混入字段长度（否则「尾块字节相同、长度不同」的两字段
/// 会碰撞）。与逐字节 FNV-1a 同语义：同序同值 → 同签，任一字段/次序/长度变 → 异签。
/// 10k 行 × 全列字段的逐字节循环是每帧 ~0.6MB 迭代，批量为 ~1/8 次乘法。
///
/// **块内字节序敏感**：`u64::from_le_bytes` 把块内字节映射到不同位段，交换块内
/// 两字节即得不同 u64（不存在「同块内换序撞签」）。
///
/// **已知取舍**：这是 64 位非密码学摘要，理论上仍有 ~2⁻⁶⁴/对 的碰撞（后果 =
/// 该帧不重灌表格，**不丢数据**，下一次真实变化即自愈）。想要更强（仍非零碰撞）
/// 可换 `std::hash::SipHasher13`；当前取的是「每帧零分配 + 一次乘法/8 字节」。
fn mix_sig_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    let (chunks, tail) = bytes.split_at(bytes.len() - bytes.len() % 8);
    for c in chunks.chunks_exact(8) {
        // chunks_exact(8) 保证 c.len() == 8，索引不会越界。
        hash ^= u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for b in tail {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= bytes.len() as u64;
    hash.wrapping_mul(0x100000001b3)
}

/// nav 快照签名（FNV-1a 64 批量混入；字段 = 懒展开可视序列全量）。
fn nav_snapshot_sig(rows: &[crate::viewmodel::rows::TreeRow]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for r in rows {
        hash = mix_sig_bytes(hash, r.path.as_bytes());
        hash = mix_sig_bytes(hash, r.name.as_bytes());
        hash = mix_sig_bytes(hash, r.depth.to_le_bytes().as_slice());
        // 层号必须进签名：嵌套展示下不同层可能有同名同深度的节点
        hash = mix_sig_bytes(hash, r.level.to_le_bytes().as_slice());
        hash = mix_sig_bytes(
            hash,
            &[
                u8::from(r.is_dir),
                u8::from(r.expanded),
                u8::from(r.has_children),
                u8::from(r.is_container),
            ],
        );
    }
    hash
}

/// dir 快照签名（FNV-1a 64 批量混入；字段 = DataTable 委托行展示输入全量——含
/// size/mtime/encrypted 及目录聚合大小回填，`show_folder_size` 开关切换后
/// 签名必变）。T-PERF2 表格重灌门控用，与 `nav_snapshot_sig` 同形态。
fn dir_snapshot_sig(rows: &[crate::viewmodel::rows::TreeRow]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for r in rows {
        hash = mix_sig_bytes(hash, r.path.as_bytes());
        hash = mix_sig_bytes(hash, r.name.as_bytes());
        hash = mix_sig_bytes(hash, r.depth.to_le_bytes().as_slice());
        hash = mix_sig_bytes(hash, r.size_raw.to_le_bytes().as_slice());
        hash = mix_sig_bytes(hash, r.size_packed.to_le_bytes().as_slice());
        hash = mix_sig_bytes(
            hash,
            &[
                u8::from(r.is_dir),
                u8::from(r.is_container),
                u8::from(r.encrypted),
                u8::from(r.is_symlink),
            ],
        );
        hash = mix_sig_bytes(
            hash,
            r.entry_id
                .map(|i| (i as u64).to_le_bytes())
                .unwrap_or(u64::MAX.to_le_bytes())
                .as_slice(),
        );
        if let Some(m) = &r.mtime {
            hash = mix_sig_bytes(hash, m.as_bytes());
        }
    }
    hash
}

/// 取稳态句柄（不存在则创建；创建需 Window——InputState/TableState 构造）。
///
/// 订阅（树 toggle、表格双击/选择）在此一次性挂接。
fn browse_hold<T: BrowseHandlerSource + 'static>(
    window: &mut Window,
    cx: &mut App,
    shell: WeakEntity<T>,
) -> Entity<BrowseHold> {
    if cx.has_global::<BrowseGlobal>() {
        return cx.global::<BrowseGlobal>().0.clone();
    }
    // —— 实体创建（需要 Window 的 TableState/InputState 在此构造） ——
    let tree = cx.new(|cx| TreeState::new(cx));
    let table = cx.new(|cx| TableState::new(BrowseDelegate::default(), window, cx));
    let split = cx.new(|_| gpui_kit::base::ResizableState::default());
    let filter_input = cx.new(|cx| {
        gpui_kit::component::input::InputState::new(window, cx)
            .placeholder(crate::tr("browse.filter_placeholder"))
    });
    // T-SEL3 动作通道：菜单项/双击文件闭包直调 shell（不走 dispatch_action
    // 焦点链——T-PERF §8 弹层焦点残留导致派发落空的教训）
    let cmd_shell = shell.clone();
    let command: BrowseCommandFn = std::rc::Rc::new(move |cmd, window, cx| {
        let _ = cmd_shell.update(cx, |s, cx| s.browse_command(cmd, window, cx));
    });
    let hold = cx.new(|_| BrowseHold {
        filter_input,
        tree,
        table,
        nav_sig: 0,
        last_dir: String::new(),
        dir_sig: 0,
        last_show_dotdot: false,
        last_show_parent: false,
        last_columns: crate::ui::contracts::ColumnsConfig::default(),
        split,
        // 首挂恢复用户上次宽度（`[ui].tree_width` 落盘值；此前只写不读，
        // 重启/重开窗恒回 260）
        tree_width: hzip_config::Config::load().ui.tree_width as f32,
        last_show_tree: true,
        mode_selection: std::collections::HashSet::new(),
        mode_selected_dirs: std::collections::HashSet::new(),
        mode_anchor: None,
        menu_enable: MenuEnable::default(),
        command: Some(command),
        tree_meta: std::collections::HashMap::new(),
        name_input: None,
        last_name_sig: NameEditSig::default(),
        _subscriptions: Vec::new(),
    });

    // —— 订阅装配（一次性；Subscription 存 hold 保活） ——
    hold.update(cx, |hold, cx| {
        // 过滤输入 → 防抖 250ms → VM set_filter（T-MENU1：名称子串过滤；
        // Change 事件即上行——InputState 按 key debounce 由本层计时器合并，
        // 简化实现：直接每次 Change 上行（VM set_filter 对缓存 O(n) 过滤，
        // 无 IO；防抖收益边际）
        let filter_shell = shell.clone();
        hold._subscriptions.push(cx.subscribe(
            &hold.filter_input,
            move |_hold: &mut BrowseHold,
                  input,
                  event: &gpui_kit::component::input::InputEvent,
                  cx: &mut Context<BrowseHold>| {
                if !matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    return;
                }
                let text = input.read(cx).value().to_string();
                let _ = filter_shell.update(cx, |s, _| {
                    (s.browse_handlers().on_filter_changed)(text);
                });
            },
        ));

        // 树展开/收起 → on_tree_toggle（懒展开回写 VM expanded 集）
        let tree_shell = shell.clone();
        hold._subscriptions.push(cx.subscribe(
            &hold.tree,
            move |_hold: &mut BrowseHold,
                  _tree,
                  event: &TreeEvent,
                  cx: &mut Context<BrowseHold>| {
                let path: String = match event {
                    TreeEvent::Expanded(p) | TreeEvent::Collapsed(p) => p.to_string(),
                };
                // VM toggle_expand 是翻转语义：Expanded/Collapsed 各发一次翻转
                // 即等价（重复翻转被 VM 集合吸收）。TODO(T-G30)：如需幂等
                // 语义，VM 侧改 set_expanded(path, expanded: bool)。
                let _ = tree_shell.update(cx, |s, _| {
                    (s.browse_handlers().on_tree_toggle)(path);
                });
            },
        ));

        // 表格双击/选择 → 行激活/`..` 跳转/选择上行；列宽/列序拖动 → 回写
        //（R-GUI4 #1）
        let shell = shell.clone();
        hold._subscriptions.push(cx.subscribe(
            &hold.table,
            move |_hold: &mut BrowseHold,
                  table,
                  event: &TableEvent,
                  cx: &mut Context<BrowseHold>| {
                match event {
                    // 拖动列宽 → 委托 widths 回存（会话保持；列宽拖动即时生效，
                    // col_groups 重建时 column() 以拖动值回填）
                    TableEvent::ColumnWidthsChanged(ws) => {
                        let visible = table.read(cx).delegate().visible.clone();
                        table.update(cx, |t, _| {
                            t.delegate_mut().widths =
                                visible.into_iter().zip(ws.iter().copied()).collect();
                        });
                        return;
                    }
                    // 拖动列序 → 委托已重排（move_column 钩子）；把新序回写
                    // ColumnsConfig 经 SettingChanged 事件持久化（设置页/表格
                    // 拖动同一事实源）；泵快照回带新序后由 browse_root 落到
                    // delegate.columns（唯一来源）。
                    TableEvent::MoveColumn(..) => {
                        let (visible, hidden, old_order) = {
                            let d = table.read(cx).delegate();
                            (
                                d.visible.clone(),
                                d.columns.hidden.clone(),
                                d.columns.order.clone(),
                            )
                        };
                        let mut order = visible;
                        for col in old_order {
                            if !order.contains(&col) {
                                order.push(col);
                            }
                        }
                        let cfg = crate::ui::contracts::ColumnsConfig { order, hidden };
                        let _ = shell.update(cx, |s, _| {
                            (s.browse_handlers().on_dialog_event)(
                                crate::ui::contracts::DialogEvent::SettingChanged {
                                    id: crate::viewmodel::SettingId::Columns,
                                    value: crate::app::settings_registry::SettingValue::Columns(
                                        cfg,
                                    ),
                                },
                            );
                        });
                        return;
                    }
                    _ => {}
                }
                // T-SEL3：行右键 → 上行右键预处理结果（render_tr 挂的
                // on_mouse_down(Right) 已同步改写委托选中集——未选中行右键
                // = 集合替换为该行；已选中 = 保持多选。此处 uplink 与菜单
                // 构建（bubble 同步执行）的先后无关：预处理先行完成，
                // uplink 幂等）。`RightClickedRow(None)` = kit 左键
                // set_selected_row 的伴发噪声 / 空白区右键——忽略。
                if let TableEvent::RightClickedRow(Some(_)) = event {
                    let (ids, dirs) = {
                        let d = table.read(cx).delegate();
                        (
                            d.selected_ids.iter().copied().collect(),
                            d.selected_dirs.iter().cloned().collect(),
                        )
                    };
                    let _ = shell.update(cx, |s, _| {
                        (s.browse_handlers().on_selection_changed)(
                            crate::ui::contracts::SelectionUpdate { ids, dirs },
                        );
                    });
                    return;
                }
                // 行左键：仅 `SelectRow`/`DoubleClickedRow` 参与分发
                // （`SelectRow` 在单击与双击时**都**会发出——table.rs
                // `on_row_left_click` 每次点击都 `set_selected_row`）
                let (row_ix, double_clicked) = match event {
                    TableEvent::DoubleClickedRow(ix) => (*ix, true),
                    TableEvent::SelectRow(ix) => (*ix, false),
                    _ => return,
                };
                // 决策 + 选择集合运算在委托只读借用内算完（owned 数据出借用域）
                let (action, sel_ctx) = {
                    let d = table.read(cx).delegate();
                    let action = row_action(
                        double_clicked,
                        data_row(d, row_ix),
                        &d.current_dir,
                        d.nested,
                    );
                    // T-SEL1 单击选择 = 修饰键三分支（Ctrl 增删单条 / Shift
                    // 锚点区间整组替换 / 普通单击整组替换）。双击的第二次
                    // SelectRow（mouse_down 捕获的 click_count≥2）跳过集合
                    // 运算——防 Ctrl+双击把已选行 toggle 掉（双击语义是
                    // 打开，不是改选）。
                    let sel_ctx = match &action {
                        Some(RowAction::Select(clicked))
                            if !double_clicked && d.last_click_count < 2 =>
                        {
                            let visible: Vec<RowSel> = d
                                .rows
                                .iter()
                                .map(|r| match r.entry_id {
                                    Some(id) => RowSel::Entry(id),
                                    None if r.is_dir => RowSel::Dir(r.path.clone()),
                                    None => RowSel::None,
                                })
                                .collect();
                            let (ids, dirs, anchor) = click_selection(
                                &d.selected_ids,
                                &d.selected_dirs,
                                clicked,
                                d.last_modifiers.control,
                                d.last_modifiers.shift,
                                &visible,
                                d.anchor,
                            );
                            Some((ids, dirs, anchor))
                        }
                        _ => None,
                    };
                    (action, sel_ctx)
                };
                // T-SEL1：本地回显（修饰键/计数消费 + 锚点 + 委托选中集）后
                // 整组上行（VM `select_ids` 整组替换 → 快照回流幂等）
                if let Some((ids, dirs, anchor)) = sel_ctx {
                    table.update(cx, |t, _| {
                        let d = t.delegate_mut();
                        d.last_modifiers = Modifiers::default();
                        d.last_click_count = 0;
                        d.anchor = anchor;
                        d.selected_ids = ids.iter().copied().collect();
                        d.selected_dirs = dirs.iter().cloned().collect();
                    });
                    let _ = shell.update(cx, |s, _| {
                        (s.browse_handlers().on_selection_changed)(
                            crate::ui::contracts::SelectionUpdate { ids, dirs },
                        );
                    });
                    return;
                }
                let Some(action) = action else {
                    return;
                };
                let _ = shell.update(cx, |s, cx| match action {
                    // `..` 双击 → 跳上一级（经面包屑段通道，与面包屑点击同语义）
                    RowAction::Up(seg) => {
                        (s.browse_handlers().on_crumb)(Crumb {
                            label: String::new(),
                            seg,
                            level: 0, // `..` 行永远作用于**当前会话**
                        });
                    }
                    RowAction::Enter(path) => (s.browse_handlers().on_tree_click)(path),
                    // 容器条目：以它为下一层会话打开（解出内层档案 → browse，
                    // 长活走统一入口：后台 + 按需「正在打开」窗）
                    RowAction::EnterContainer(path) => s.browse_open_container(&path, cx),
                    // 嵌套会话档案根的 `..`：退出该层
                    RowAction::LeaveContainer => s.browse_leave_container(cx),
                    // T-SEL4：双击文件 → 打开（用户口径 2026-09-28：内置查看器停用，
                    // 一律物化 + 指定程序/系统默认；加密条目先过密码门）。
                    // 旧 on_row_activate「定位所在目录 + 单选」语义被 Explorer 式
                    // 打开语义取代（detail 行集 = 当前目录条目，定位是原地绕路）；
                    // 槽保留（其他消费面/装配兼容）。
                    RowAction::Activate(_) => s.browse_open_selection(cx),
                    // 双击第二次 SelectRow（集合运算跳过）→ 旧单击语义兜底
                    RowAction::Select(sel) => {
                        let up = match sel {
                            RowSel::Entry(id) => crate::ui::contracts::SelectionUpdate {
                                ids: vec![id],
                                dirs: Vec::new(),
                            },
                            RowSel::Dir(p) => crate::ui::contracts::SelectionUpdate {
                                ids: Vec::new(),
                                dirs: vec![p],
                            },
                            RowSel::None => Default::default(),
                        };
                        (s.browse_handlers().on_selection_changed)(up);
                    }
                });
            },
        ));
    });

    cx.set_global(BrowseGlobal(hold.clone()));
    hold
}

// —— 右栏 TableDelegate（T-G53；R-GUI4 #1 列宽/列序拖动）——

/// 右栏表格委托：行 = [`BrowseRow`] 视图 + `..` 占位首行。
///
/// **列控制（R-GUI4 #1）**：`resizable(true).movable(true)`——列宽拖动经
/// `TableEvent::ColumnWidthsChanged` 回存 `widths`（会话内保持）；列序拖动经
/// 委托钩子 [`move_column`](Self::move_column) 重排 `visible`，并经订阅回写
/// `ColumnsConfig`（持久化到 hzip-config `ui.columns`——设置页与表格拖动
/// 同一事实源）。列显隐仍由设置页（`ui.columns.hidden`）控制。
pub(crate) struct BrowseDelegate {
    /// 当前目录行（不含 `..` 占位行）
    pub rows: Vec<BrowseRow>,
    /// 当前目录路径（`..` 目标段推导源；每帧从快照同步）
    pub current_dir: String,
    /// `..` 占位行开关（ui.show_dotdot；R2-04 §3.5）
    pub show_dotdot: bool,
    /// `..` 占位行实际显隐（show_dotdot && 非档案根）
    pub show_parent: bool,
    /// 是否嵌套会话（快照 `nested`；决定档案根 `..` 是「退出该层」还是 no-op）
    pub nested: bool,
    /// 浏览列配置（**每帧从快照同步**——`AppStateUiSnapshot.columns`；
    /// 变化时才重置 `visible`，default = 全序）
    pub columns: crate::ui::contracts::ColumnsConfig,
    /// 可见列的**实时顺序**（拖动列头即重排；与 `columns.visible()` 同源，
    /// 由 `columns` 变化时重置）
    pub visible: Vec<BrowseColumnId>,
    /// 拖动后的列宽（col → px；会话内保持——col_groups 重建时回填）
    pub widths: Vec<(BrowseColumnId, gpui_kit::Pixels)>,
    /// 列头排序态（列 → 方向；`column()` 回填指示器、重灌后重排——
    /// UI 本地态，不进 VM）
    pub sort: Option<(BrowseColumnId, ColumnSort)>,
    /// 最近一次行左键按下时的修饰键（T-SEL1：`render_tr` 挂的
    /// `on_mouse_down(Left)` 捕获——kit 的 `SelectRow` 不带修饰键；
    /// 订阅消费后重置，防键盘导航（方向键无 mouse_down）读到陈旧值）
    pub last_modifiers: Modifiers,
    /// 最近一次行左键按下的 click_count（T-SEL1：双击的第二次按下 ≥2——
    /// SelectRow 集合运算跳过，防 Ctrl+双击 toggle 掉已选行）
    pub last_click_count: usize,
    /// 最近一次左键按下是否落在**行**上（用户口径 2026-09-28：空白区左键要
    /// 取消选中——行处理器先跑并置位，表格容器处理器读后清零；读到时为 false
    /// 即「这一下点在空白处」）。
    pub last_hit_row: bool,
    /// 左键**按下**那一瞬是否落在行上（容器 down 处理器把 `last_hit_row` 转存
    /// 到这里，up 处理器消费）。为什么要跨阶段转存：列头点击是在 **mouse-up**
    /// 才由 kit 的 `on_click` 把选择模式切成 `Column`/`Cell`，所以「算不算点空白」
    /// 必须等 up 阶段才判得准（用户口径 2026-10-12：点列头排序不得丢焦点行高亮）。
    pub press_hit_row: bool,
    /// Shift 区间锚点（T-SEL1：可见行索引，`rows` 编址不含 `..` 行；
    /// 行集重灌/列头排序时清空——旧行序的区间没有意义）
    pub anchor: Option<usize>,
    /// 选中集镜像（T-SEL1：detail 模式行高亮数据源 + Ctrl/Shift 集合运算
    /// 的当前集。快照回流与点击本地回显双写——内容一致幂等）
    pub selected_ids: std::collections::HashSet<u64>,
    /// 隐式目录选中集镜像（T-SEL 走查 #1：目录行高亮第二数据源 + 集合
    /// 运算当前集——与 `selected_ids` 双通道）
    pub selected_dirs: std::collections::HashSet<String>,
    /// 上次快照同步的双通道 selection（T-SEL1：等值比较零分配，变化才
    /// 重建镜像）
    pub last_sel_sync: (Vec<u64>, Vec<String>),
    /// 当前档案可编辑（`caps.edit` 且非嵌套会话；快照投影——菜单「重命名/
    /// 新建文件夹」可用性来源）。
    pub can_edit: bool,
    /// 本次右键是否落在**行**上（行内处理器置位，容器菜单消费清零）。
    ///
    /// 用户口径 2026-09-29：空白区右键要弹「空白菜单」（含新建文件夹），行上
    /// 右键弹「条目菜单」。事件由内向外冒泡 ⇒ 行处理器先跑；容器菜单读到 false
    /// 才是真的点在空白处（同 `last_hit_row` 的左键口径）。
    pub right_hit_row: bool,
    /// 正在**改名**的目标条目名（末段；`None` = 无改名编辑）。名称列据此渲染
    /// 输入框（用户口径 2026-09-28）。
    pub name_rename: Option<String>,
    /// 是否处于「新建文件夹」**草稿行**态（名称列渲染输入框 + 表格末尾多一行）。
    pub name_draft: bool,
    /// 行内编辑的输入框实体（shell 持有；渲染侧只读引用）。
    pub name_input: Option<Entity<gpui_kit::component::input::InputState>>,
    /// kit 单选焦点行的**每帧镜像**（由 `browse_root` 在表格渲染前按
    /// `TableState::selection()` 灌入：`Row(ix)` → `Some(ix)`、`None` → `None`、
    /// 列头 / 单元格模式 → 沿用上一帧值；0.7.1 起 `selected_row()` 会按
    /// `SelectionMode` 过滤，点列头排序会把它变成 `None`——取舍见 `browse_root`
    /// 里那段注释）。行高亮层据此覆盖三类「应用选中集表达不了」的行：
    /// `..` 占位行（`RowSel::None`）、面包屑跳转后 index 悬空的聚焦行、以及 kit
    /// 自己设过的行——否则这些行会露出 kit 的**全量 accent**（0.7.1
    /// `table/state.rs:2012` 的 `is_selected = selected_row() == Some(row_ix)`
    /// → `:2242-2250` 的 `list.active_highlight=false` 分支取 `tokens.accent`），
    /// 与自绘的浅色行高亮不一致。
    pub kit_focus: Option<usize>,
}

impl Default for BrowseDelegate {
    fn default() -> Self {
        let columns = crate::ui::contracts::ColumnsConfig::default();
        let visible = visible_columns_of(&columns);
        Self {
            rows: Vec::new(),
            current_dir: String::new(),
            show_dotdot: false,
            show_parent: false,
            nested: false,
            can_edit: false,
            right_hit_row: false,
            name_rename: None,
            name_draft: false,
            name_input: None,
            kit_focus: None,
            columns,
            visible,
            widths: Vec::new(),
            sort: None,
            last_modifiers: Modifiers::default(),
            last_click_count: 0,
            last_hit_row: false,
            press_hit_row: false,
            anchor: None,
            selected_ids: std::collections::HashSet::new(),
            selected_dirs: std::collections::HashSet::new(),
            last_sel_sync: (Vec::new(), Vec::new()),
        }
    }
}

/// 多选集合运算（T-SEL1 纯函数——三分支语义单测断言面；T-SEL 走查 #1
/// 双通道：实体 id 集 + 隐式目录路径集）。
///
/// * `current_ids/current_dirs`：当前选中双集；`clicked`：点击行载荷
///   （`RowSel::None` = `..` 行 → 双集清空）；`ctrl`/`shift`：修饰键。
/// * `visible`：可见行载荷序（委托 `rows` 编址，不含 `..` 行）；
///   `anchor_ix`：现锚点（同编址；`None`/越界 = 无效）。
///
/// 返回 `(新 id 集, 新目录集, 新锚点)`。语义对齐资源管理器：
/// 普通单击 = 整组替换 + 锚点移至该行；Ctrl = 单条增删 + 锚点移至该行；
/// Shift = 锚点到该行的可见行序区间整组替换（锚点不变；区间内实体行进
/// id 集、隐式目录行进目录集；无效锚点退化为普通替换）；Ctrl+Shift 一期
/// 不做（按 Shift 处理）。VM `select_rows` 是整组替换语义——返回值直接
/// 整组上行。
fn click_selection(
    current_ids: &std::collections::HashSet<u64>,
    current_dirs: &std::collections::HashSet<String>,
    clicked: &RowSel,
    ctrl: bool,
    shift: bool,
    visible: &[RowSel],
    anchor_ix: Option<usize>,
) -> (Vec<u64>, Vec<String>, Option<usize>) {
    let valid_anchor = anchor_ix.filter(|a| *a < visible.len());
    match clicked {
        RowSel::None => (Vec::new(), Vec::new(), None),
        RowSel::Entry(id) => {
            let Some(ix) = visible.iter().position(|v| *v == RowSel::Entry(*id)) else {
                // 防御：点击行不在可见集（行集与快照竞态）→ 普通替换
                return (vec![*id], Vec::new(), None);
            };
            if ctrl {
                let mut next = current_ids.clone();
                if !next.remove(id) {
                    next.insert(*id);
                }
                let mut out: Vec<u64> = next.into_iter().collect();
                out.sort_unstable();
                (out, Vec::new(), Some(ix))
            } else if shift {
                shift_range(valid_anchor, ix, visible)
            } else {
                (vec![*id], Vec::new(), Some(ix))
            }
        }
        RowSel::Dir(path) => {
            let Some(ix) = visible
                .iter()
                .position(|v| matches!(v, RowSel::Dir(p) if p == path))
            else {
                return (Vec::new(), vec![path.clone()], None);
            };
            if ctrl {
                let mut next = current_dirs.clone();
                if !next.remove(path) {
                    next.insert(path.clone());
                }
                let mut out: Vec<String> = next.into_iter().collect();
                out.sort();
                (Vec::new(), out, Some(ix))
            } else if shift {
                shift_range(valid_anchor, ix, visible)
            } else {
                (Vec::new(), vec![path.clone()], Some(ix))
            }
        }
    }
}

/// Shift 区间收集（`click_selection` 的公共尾步：锚点到点击行的可见行
/// 序区间整组替换——区间内实体行进 id 集、隐式目录行进目录集、`..`/占位
/// 跳过；锚点不变，无效锚点退化为普通替换）。区间语义是**整组替换**——
/// 不依赖当前集。
fn shift_range(
    anchor: Option<usize>,
    ix: usize,
    visible: &[RowSel],
) -> (Vec<u64>, Vec<String>, Option<usize>) {
    let a = anchor.unwrap_or(ix);
    let (lo, hi) = (a.min(ix), a.max(ix));
    let mut ids = std::collections::BTreeSet::new();
    let mut dirs = std::collections::BTreeSet::new();
    for row in &visible[lo..=hi] {
        match row {
            RowSel::Entry(id) => {
                ids.insert(*id);
            }
            RowSel::Dir(p) => {
                dirs.insert(p.clone());
            }
            RowSel::None => {}
        }
    }
    (
        ids.into_iter().collect(),
        dirs.into_iter().collect(),
        anchor.or(Some(ix)),
    )
}

// —— T-SEL3：右键菜单装配（三模式共用件） ——

/// 菜单项可用性输入（T-SEL3）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MenuEnable {
    /// 有选中条目（解压选定/删除可用）
    pub has_selection: bool,
    /// 恰好单选一个文件（打开/编辑可用）
    pub single_file: bool,
    /// 恰好单选一个**可改名条目**（文件或目录；容器层/`..` 行不算）
    pub single_entry: bool,
    /// 可编辑（`caps.edit` 且非嵌套会话；新建文件夹/重命名可用）
    pub can_edit: bool,
    /// 右键落点是**列表空白区**（用户口径 2026-09-29）。
    ///
    /// 空白区：只给「作用于当前目录/整档」的项（解压全部/添加文件/全部选择/
    /// 新建文件夹）；文件/文件夹行：给条目相关项（打开/编辑/删除/重命名…），
    /// **不给新建文件夹**。
    pub blank: bool,
    /// 剪贴板有可粘贴内容（T-CP P0：内部剪贴板非空，或系统剪贴板含外部
    /// 文件列表——右键菜单构建属交互时机，允许查剪贴板；渲染帧仍禁止）
    pub paste_available: bool,
}

/// 粘贴可用性判定（T-CP P0；右键菜单构建/后台线程属交互时机，允许查剪贴板
/// ——红线只禁渲染帧查询）。内部剪贴板非空，或系统剪贴板含外部文件列表
/// （gpui Windows 侧 `ExternalPaths`——资源管理器「复制」来源）。
pub(crate) fn paste_available_now(cx: &App) -> bool {
    if !hzip_platform::clipboard::internal_is_empty() {
        return true;
    }
    cx.read_from_clipboard().is_some_and(|item| {
        item.entries()
            .iter()
            .any(|e| matches!(e, gpui_kit::ClipboardEntry::ExternalPaths(_)))
    })
}

/// 可用性判定（快照行集口径：`TreeRow`——icon/list 装配与 sel 同步用；
/// T-SEL 走查 #1：双通道合计——隐式目录选择计入选中数但恒非单文件）。
/// `paste_available` 由调用方现算（快照回流在泵线程临界区外，查剪贴板
/// 时机由调用点决定——保持本函数纯函数属性可测）。
fn menu_enable_of_rows(
    selection: &[u64],
    dir_selection: &[String],
    rows: &[crate::viewmodel::rows::TreeRow],
    can_edit: bool,
    paste_available: bool,
) -> MenuEnable {
    let mut single_file = false;
    let mut single_entry = false;
    if selection.len() == 1
        && dir_selection.is_empty()
        && let Some(r) = rows
            .iter()
            .find(|r| r.entry_id == Some(selection[0] as usize))
    {
        single_file = !r.is_dir && !r.is_container;
        // 容器层（内层压缩包合成行）不是真条目，改名无意义
        single_entry = !r.is_container;
    }
    // 隐式目录（无目录条目，按路径选中）：也可改名（子树前缀改名）
    if selection.is_empty() && dir_selection.len() == 1 {
        single_entry = true;
    }
    MenuEnable {
        has_selection: !selection.is_empty() || !dir_selection.is_empty(),
        single_file,
        single_entry,
        can_edit,
        blank: false,
        paste_available,
    }
}

/// 可用性判定（委托行集口径：`BrowseRow`——detail 的 `context_menu` 钩子用；
/// 双通道合计同上）。`paste_available` 由调用方现算（同 `menu_enable_of_rows`
/// ——保持纯函数属性可测）。
fn menu_enable_of_delegate(
    selected: &std::collections::HashSet<u64>,
    selected_dirs: &std::collections::HashSet<String>,
    rows: &[BrowseRow],
    can_edit: bool,
    paste_available: bool,
) -> MenuEnable {
    let mut single_file = false;
    let mut single_entry = false;
    if selected.len() == 1
        && selected_dirs.is_empty()
        && let Some(id) = selected.iter().next()
        && let Some(r) = rows.iter().find(|r| r.entry_id == Some(*id))
    {
        single_file = !r.is_dir && !r.is_container;
        single_entry = !r.is_container;
    }
    if selected.is_empty() && selected_dirs.len() == 1 {
        single_entry = true;
    }
    MenuEnable {
        has_selection: !selected.is_empty() || !selected_dirs.is_empty(),
        single_file,
        single_entry,
        can_edit,
        blank: false,
        paste_available,
    }
}

/// 空白区菜单可用性（只有目录/整档级项；`blank = true`）。
pub(crate) fn blank_menu_enable(can_edit: bool, paste_available: bool) -> MenuEnable {
    MenuEnable {
        blank: true,
        can_edit,
        paste_available,
        ..Default::default()
    }
}

/// 标记「本次右键落在行上」（行内右键处理器调用；与 [`take_right_hit_row`] 配对）。
pub(crate) fn note_right_hit_row(cx: &mut App) {
    if let Some(g) = cx.try_global::<BrowseGlobal>() {
        let hold = g.0.clone();
        hold.update(cx, |h, cx| {
            h.table.update(cx, |t, _| {
                t.delegate_mut().right_hit_row = true;
            });
        });
    }
}

/// 读右键命中标记（**不消费**；容器在右键按下时判落点用）。
pub(crate) fn peek_right_hit_row(cx: &App) -> bool {
    crate::ui::views::browse::filter_hold_in(cx)
        .map(|h| h.read(cx).table.read(cx).delegate().right_hit_row)
        .unwrap_or(false)
}

/// 清掉 kit 的**陈旧**「右键行」（空白区右键前调用）。
///
/// kit 的 `right_clicked_row` 只在行右键/左键选择时更新——右键行之后再右键空白，
/// 它仍是旧行号，kit 的**表格级**右键菜单会据此再弹一次「行菜单」（指向错的行）。
/// 空白区右键前清掉它，后续空白右键不再冒出陈旧行菜单。
pub(crate) fn clear_kit_right_row(cx: &mut App) {
    if let Some(g) = cx.try_global::<BrowseGlobal>() {
        let hold = g.0.clone();
        hold.update(cx, |h, cx| {
            h.table.update(cx, |t, cx| {
                if t.right_clicked_row().is_some() {
                    t.set_right_clicked_row(None, cx);
                }
            });
        });
    }
}

/// 右键落点是否为行（消费并清零；容器空白菜单用）。
///
/// 行内 `on_mouse_down(Right)` 先于容器菜单触发（事件由内向外），故读到 `false`
/// 即「点在空白处」——与左键 `last_hit_row` 同一口径。
pub(crate) fn take_right_hit_row(cx: &mut App) -> bool {
    let Some(g) = cx.try_global::<BrowseGlobal>() else {
        return false;
    };
    let hold = g.0.clone();
    hold.update(cx, |h, cx| {
        h.table.update(cx, |t, _| {
            let d = t.delegate_mut();
            let hit = d.right_hit_row;
            d.right_hit_row = false;
            hit
        })
    })
}

/// 容器**空白区**右键菜单（三视图共用；用户口径 2026-09-29）。
///
/// `None` = 本次右键落在行上（行自己的菜单负责），容器不出菜单。
pub(crate) fn blank_menu_from(cx: &App) -> Option<(BrowseCommandFn, bool, bool)> {
    let command = command_channel(cx)?;
    let can_edit = crate::ui::views::browse::filter_hold_in(cx)
        .map(|h| h.read(cx).table.read(cx).delegate().can_edit)
        .unwrap_or(false);
    let paste_available = paste_available_now(cx);
    Some((command, can_edit, paste_available))
}

/// 行内编辑输入框实体写回（shell 建/销毁时调用）+ 触发重绘。
///
/// 浏览页未挂（未打开档案/无 hold）→ no-op（headless 测试语义）。
pub(crate) fn set_name_input(
    cx: &mut App,
    input: Option<Entity<gpui_kit::component::input::InputState>>,
) {
    let Some(g) = cx.try_global::<BrowseGlobal>() else {
        return;
    };
    let hold = g.0.clone();
    hold.update(cx, |hold, cx| {
        hold.name_input = input;
        cx.notify();
    });
}

/// 菜单项（纯数据；[`menu_items`] 产出，[`build_browse_menu`] 消费——这样
/// 「哪个场景出现哪些项」可以单测断言，不受渲染细节影响）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowseMenuItem {
    /// 解压全部
    ExtractAll,
    /// 解压选定
    ExtractSel,
    /// 分隔线
    Sep,
    /// 复制选定条目到剪贴板（T-CP P0）
    Copy,
    /// 粘贴剪贴板条目到当前目录（T-CP P0）
    Paste,
    /// 打开
    Open,
    /// 打开方式…
    OpenWith,
    /// 编辑（= 打开该文件）
    Edit,
    /// 删除文件
    Delete,
    /// 添加文件
    AddFiles,
    /// 全部选择
    SelectAll,
    /// 新建文件夹
    NewFolder,
    /// 重命名
    Rename,
}

/// 场景 → 菜单项序列（用户口径 2026-09-29：菜单内容随右键目标变）。
///
/// - **空白区**：解压全部 · 添加文件 · 全部选择 · ─ · 新建文件夹
///   （**没有**打开/打开方式/编辑/删除/重命名/解压选定——这些都要具体条目）；
/// - **文件/文件夹行**：解压全部 · 解压选定 · ─ · 打开 · 打开方式 · 编辑 · ─ ·
///   删除 · 全部选择 · ─ · 重命名（**没有**新建文件夹，也没有添加文件）。
///
/// 项内的可用性（打开仅单选**文件**、重命名仅单选一个条目等）由
/// [`item_disabled`] 判定 → 同一条目在文件/文件夹/多选下呈现不同灰态。
pub(crate) fn menu_items(enable: &MenuEnable) -> Vec<BrowseMenuItem> {
    use BrowseMenuItem::*;
    if enable.blank {
        // 空白区：粘贴（作用于当前目录）+ 目录/整档级操作
        return vec![ExtractAll, Paste, AddFiles, SelectAll, Sep, NewFolder];
    }
    vec![
        ExtractAll, ExtractSel, Sep, Copy, Sep, Open, OpenWith, Edit, Sep, Delete, SelectAll, Sep,
        Rename,
    ]
}

/// 单项灰态判定（纯函数；`menu_items` 的场景过滤之外的细粒度可用性）。
pub(crate) fn item_disabled(item: BrowseMenuItem, enable: &MenuEnable) -> bool {
    use BrowseMenuItem::*;
    match item {
        ExtractAll | SelectAll => false,
        ExtractSel | Delete => !enable.has_selection,
        Copy => !enable.has_selection,
        Paste => !enable.paste_available,
        Open | OpenWith | Edit => !enable.single_file,
        AddFiles => !enable.can_edit,
        NewFolder => !enable.can_edit,
        Rename => !enable.can_edit || !enable.single_entry,
        Sep => false,
    }
}

/// 菜单项 i18n key（与 [`BrowseMenuItem`] 一一对应）。
fn item_key(item: BrowseMenuItem) -> &'static str {
    use BrowseMenuItem::*;
    match item {
        ExtractAll => "ctx.extract_all",
        ExtractSel => "ctx.extract_sel",
        Sep => "",
        Copy => "ctx.copy",
        Paste => "ctx.paste",
        Open => "ctx.open",
        OpenWith => "ctx.open_with",
        Edit => "ctx.edit",
        Delete => "ctx.delete",
        AddFiles => "ctx.add",
        SelectAll => "ctx.select_all",
        NewFolder => "ctx.new_folder",
        Rename => "ctx.rename",
    }
}

/// 菜单项 → 命令（分隔线无命令）。
fn item_command(item: BrowseMenuItem) -> Option<BrowseCommand> {
    use BrowseMenuItem::*;
    Some(match item {
        ExtractAll => BrowseCommand::ExtractAll,
        ExtractSel => BrowseCommand::ExtractSel,
        Sep => return None,
        Copy => BrowseCommand::Copy,
        Paste => BrowseCommand::Paste,
        Open => BrowseCommand::Open,
        OpenWith => BrowseCommand::OpenWith,
        Edit => BrowseCommand::Edit,
        Delete => BrowseCommand::Delete,
        AddFiles => BrowseCommand::AddFiles,
        SelectAll => BrowseCommand::SelectAll,
        NewFolder => BrowseCommand::NewFolder,
        Rename => BrowseCommand::Rename,
    })
}

/// 单个命令菜单项（闭包直调通道——不走 `dispatch_action` 焦点链）。
fn cmd_item(
    command: &BrowseCommandFn,
    cmd: BrowseCommand,
    key: &str,
    disabled: bool,
) -> PopupMenuItem {
    let c = command.clone();
    PopupMenuItem::new(crate::tr(key))
        .disabled(disabled)
        .on_click(move |_, window, cx| c(cmd, window, cx))
}

/// 右键菜单装配（T-SEL3；detail 委托 `context_menu` 钩子与 icon/list 的
/// `ContextMenuExt` 闭包共用）。
///
/// 菜单内容由 [`menu_items`] 按**右键目标**给出（用户口径 2026-09-29）：
/// 空白区 = 目录/整档级操作（含新建文件夹），文件/文件夹行 = 条目级操作
/// （不含新建文件夹）。项内灰态由 [`item_disabled`] 判定。
pub(crate) fn build_browse_menu(
    menu: PopupMenu,
    command: BrowseCommandFn,
    enable: MenuEnable,
) -> PopupMenu {
    let mut menu = menu;
    for item in menu_items(&enable) {
        if item == BrowseMenuItem::Sep {
            menu = menu.item(PopupMenuItem::separator());
            continue;
        }
        let Some(cmd) = item_command(item) else {
            continue;
        };
        menu = menu.item(cmd_item(
            &command,
            cmd,
            item_key(item),
            item_disabled(item, &enable),
        ));
    }
    menu
}

/// 动作通道读取（icon/list 菜单闭包构建期；Global 未装配 = headless → None）。
pub(crate) fn command_channel(cx: &App) -> Option<BrowseCommandFn> {
    cx.try_global::<BrowseGlobal>()
        .and_then(|g| g.0.read(cx).command.clone())
}

/// 修饰键合并（T-SEL 走查 #1 兜底：`MouseDownEvent.modifiers` 源自
/// GetKeyState 的线程队列语义，焦点/队列竞态下可能取空；`GetAsyncKeyState`
/// 物理硬件态 OR 合并——按住的键必定可见）。
pub(crate) fn merge_modifiers(m: Modifiers) -> Modifiers {
    // **测试下不查硬件态**：兜底层读的是 `GetAsyncKeyState`（本机**物理**修饰键），
    // 而测试里的鼠标事件是合成的 ⇒ 合成「普通单击」会被宿主机当前按着的
    // Shift/Ctrl 变成区间/增选（实测症状：`ctrl_click_multi_selects_through_real_mouse`
    // 偶发 `普通单击 = 单选基线`，实得 `[0,1]` = 锚点到点击行的区间选择；本机跑长任务
    // 时（终端/自动化注入按键）尤其容易命中）。这与「合成的按键事件不该受物理键盘
    // 影响」是同一条测试隔离纪律——生产语义（焦点切换竞态下兜底）不受影响。
    if cfg!(test) {
        return m;
    }
    let (ctrl, shift, alt) = hzip_platform::kb::async_key_modifiers();
    Modifiers {
        control: m.control || ctrl,
        shift: m.shift || shift,
        alt: m.alt || alt,
        ..m
    }
}

/// icon/list 当前菜单可用性（右键预处理已同步镜像——构建期读即最新）。
pub(crate) fn menu_enable_now(cx: &App) -> MenuEnable {
    cx.try_global::<BrowseGlobal>()
        .map(|g| g.0.read(cx).menu_enable)
        .unwrap_or_default()
}

/// icon/list 模式单击选择（T-SEL1：与 detail 同源的 `click_selection` 三分支
/// 并 hold 镜像回显后整组上行；T-SEL 走查 #1 双通道）。`visible` = mode_rows
/// 编址（`..` 行 = `RowSel::None`）。
pub(crate) fn mode_click_selection<T: BrowseHandlerSource + 'static>(
    shell: &WeakEntity<T>,
    clicked: RowSel,
    modifiers: Modifiers,
    visible: &[RowSel],
    cx: &mut App,
) {
    let Some(hold) = cx.try_global::<BrowseGlobal>().map(|g| g.0.clone()) else {
        return;
    };
    let (current, current_dirs, anchor) = {
        let h = hold.read(cx);
        (
            h.mode_selection.clone(),
            h.mode_selected_dirs.clone(),
            h.mode_anchor,
        )
    };
    let (ids, dirs, new_anchor) = click_selection(
        &current,
        &current_dirs,
        &clicked,
        modifiers.control,
        modifiers.shift,
        visible,
        anchor,
    );
    hold.update(cx, |h, _| {
        h.mode_selection = ids.iter().copied().collect();
        h.mode_selected_dirs = dirs.iter().cloned().collect();
        h.mode_anchor = new_anchor;
    });
    let _ = shell.update(cx, |s, _| {
        (s.browse_handlers().on_selection_changed)(crate::ui::contracts::SelectionUpdate {
            ids,
            dirs,
        });
    });
}

/// icon/list 模式右键预处理（T-SEL3：资源管理器语义——未选中行右键 =
/// 集合替换为该行；已选中行保持多选。本地回显 + 整组上行；菜单构建在
/// 同一右键事件内随后发生，读到的是预处理后的镜像。T-SEL 走查 #1：
/// 双通道载荷——隐式目录行按路径预选）。
pub(crate) fn mode_right_click_preselect<T: BrowseHandlerSource + 'static>(
    shell: &WeakEntity<T>,
    clicked: RowSel,
    is_dirlike: bool,
    cx: &mut App,
) {
    let (id, dir) = match clicked {
        RowSel::Entry(id) => (Some(id), None),
        RowSel::Dir(p) => (None, Some(p)),
        RowSel::None => return,
    };
    let Some(hold) = cx.try_global::<BrowseGlobal>().map(|g| g.0.clone()) else {
        return;
    };
    let already = match (id, &dir) {
        (Some(id), _) => hold.read(cx).mode_selection.contains(&id),
        (None, Some(p)) => hold.read(cx).mode_selected_dirs.contains(p),
        _ => false,
    };
    if already {
        return;
    }
    let up = crate::ui::contracts::SelectionUpdate {
        ids: id.into_iter().collect(),
        dirs: dir.into_iter().collect(),
    };
    // 右键预处理属交互时机——现算粘贴可用性（update 闭包借用 cx 前取出；
    // 菜单构建闭包读 hold 镜像时即此最新值；T-CP P0）
    let paste_available = paste_available_now(cx);
    hold.update(cx, |h, _| {
        h.mode_selection.clear();
        h.mode_selected_dirs.clear();
        if let Some(id) = up.ids.first() {
            h.mode_selection.insert(*id);
        }
        if let Some(p) = up.dirs.first() {
            h.mode_selected_dirs.insert(p.clone());
        }
        h.mode_anchor = None;
        h.menu_enable = MenuEnable {
            has_selection: true,
            single_file: !is_dirlike,
            single_entry: true,
            can_edit: h.menu_enable.can_edit,
            blank: false,
            paste_available,
        };
    });
    let _ = shell.update(cx, |s, _| {
        (s.browse_handlers().on_selection_changed)(up);
    });
}

/// 按排序态重排行（`perform_sort` 与重灌共用纯函数）。
///
/// `Default`（第三击取消）→ 恢复 VM 原序的等价排序：目录恒前 + 名升序
/// （`dir_children` 双 pass BTreeMap 序）。
fn sort_delegate_rows(rows: &mut [BrowseRow], id: BrowseColumnId, sort: ColumnSort) {
    let col = id.sort_column();
    match sort {
        ColumnSort::Ascending => sort_browse_rows(rows, col, SortDir::Asc),
        ColumnSort::Descending => sort_browse_rows(rows, col, SortDir::Desc),
        ColumnSort::Default => sort_browse_rows(rows, SortColumn::Name, SortDir::Asc),
    }
}

/// 行模型统一下标：0 = `..`（show_parent 时），其余 = rows 偏移。
fn data_row(delegate: &BrowseDelegate, row_ix: usize) -> Option<DataRowRef<'_>> {
    let base = delegate.rows.len() + usize::from(delegate.show_parent);
    if delegate.name_draft && row_ix == base {
        return Some(DataRowRef::Draft);
    }
    if delegate.show_parent {
        match row_ix {
            0 => Some(DataRowRef::Parent),
            ix => delegate.rows.get(ix - 1).map(DataRowRef::Entry),
        }
    } else {
        delegate.rows.get(row_ix).map(DataRowRef::Entry)
    }
}

/// 行引用（渲染/激活分发用）。
enum DataRowRef<'a> {
    /// `..` 返回上级
    Parent,
    /// 实体行
    Entry(&'a BrowseRow),
    /// 「新建文件夹」草稿行（名称列 = 行内输入框；用户口径 2026-09-28）。
    ///
    /// 不是数据行（不进选中集/不响应点击）——同 `..` 行的地位，只是多挂了
    /// 一个输入框。
    Draft,
}

/// 行选择载荷（T-SEL 走查 #1：`..` 行与隐式目录行的 `entry_id` 均为
/// None——隐式目录（tar/无目录条目档案）按**档案内路径**选择，与实体行
/// 的 entry id 双通道互补）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowSel {
    /// 实体行（entry_id 在场）
    Entry(u64),
    /// 隐式目录行（按档案内路径选）
    Dir(String),
    /// `..` 行 / 无 entry 行 → 清空选择
    None,
}

/// 行左键动作（订阅侧按此分发；纯函数——单测直接断言分发决策）。
///
/// **单击/双击语义分离**（T-SET 修复轮 bug 修复）：单击只改选中，
/// 双击才「激活」——与实体目录行一致（`..` 行此前单击即跳转，
/// 且双击连跳两级直达根）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowAction {
    /// 双击 `..` 行 → 跳上一级（目标 = `current_dir` 剥尾段）
    Up(String),
    /// 双击目录/容器 → 进入
    Enter(String),
    /// 双击文件 → 激活（预览/外部打开由 shell 决定）
    Activate(String),
    /// 双击**容器条目**（内层压缩包）→ 以它为下一层会话打开
    /// （R-GUI2-06：压缩包当目录进；条目路径 = 档案内路径）
    EnterContainer(String),
    /// 嵌套会话档案根的 `..` → 退出该层（回到外层档案）
    LeaveContainer,
    /// 单击行 → 上报选择载荷（`RowSel::None` = `..`/无 entry，清选中）
    Select(RowSel),
}

/// 行图标（列表名称列 / 图标网格 / 紧凑列表共用）。
///
/// **容器（内层压缩包）用 `FolderZip`，不用文件图标**（用户实测反馈）：它是
/// 「双击能进去的包」，画成文件图标会误导成「只能预览」；同时也不该用文件夹
/// 图标——它是文件不是目录（层级/选中语义都按文件走）。
pub(crate) fn row_type_icon(row: &BrowseRow) -> AppIcon {
    if row.is_symlink {
        AppIcon::Link
    } else if row.is_container {
        AppIcon::FolderZip
    } else if row.is_dir {
        AppIcon::Folder
    } else {
        AppIcon::File
    }
}

/// 行图标元素（系统图标优先，未命中/开关关闭/平台不支持 → [`row_type_icon`] 回退）。
///
/// 回退映射的**唯一来源仍是 [`row_type_icon`]**（本函数只把行字段归一成
/// `file_icon` 的 `(name, kind)`，不复制那份映射）；回退元素的着色由 widget
/// 内部负责，调用处不再自着色。
pub(crate) fn row_file_icon(
    row: &BrowseRow,
    slot: crate::ui::widgets::file_icon::Slot,
    cx: &App,
) -> AnyElement {
    use crate::ui::widgets::file_icon;
    let kind = file_icon::kind_of(row.is_dir, row.is_container, row.is_symlink);
    file_icon::icon(&row.name, kind, slot, row_type_icon(row), cx)
}

/// 「类型」列文案优先级（纯函数）：系统类型名（开关开 ∧ 缓存命中 ∧ 语言一致）
/// 优先，否则用行的自有本地化类型文案。
pub(crate) fn type_cell_text(system: Option<SharedString>, fallback: &str) -> SharedString {
    system.unwrap_or_else(|| SharedString::from(fallback.to_owned()))
}

/// 动作决策（`double_clicked` = 事件为 `DoubleClickedRow`）。
///
/// `..` 行的上行目标由 [`parent_seg_of`] 从 `current_dir` 现算——**只剥一层**
/// （`docs/src` → `docs`；顶层 → `""` = 根）。
fn row_action(
    double_clicked: bool,
    resolved: Option<DataRowRef<'_>>,
    current_dir: &str,
    nested: bool,
) -> Option<RowAction> {
    match (double_clicked, resolved) {
        // 草稿行（新建文件夹）：不响应双击/单击（输入框自己消费点击）
        (_, Some(DataRowRef::Draft)) => None,
        // `..`：嵌套会话的档案根再往上 = 退出该层（回到外层档案）；
        // 顶层会话的档案根 = 无处可去（沿用旧语义：回到根，实际 no-op）
        (true, Some(DataRowRef::Parent)) if nested && current_dir.is_empty() => {
            Some(RowAction::LeaveContainer)
        }
        (true, Some(DataRowRef::Parent)) => Some(RowAction::Up(parent_seg_of(current_dir))),
        // 容器条目（内层压缩包）与目录一样「双击进入」——但进的是**新会话**。
        //
        // **顺序要紧：目录优先**。组合档内层容器层（`init.cpio`）同时满足两个标记：
        // `is_dir = true`（树行展开/右栏进层，导航语义）与 `is_container = true`
        // （图标/类型列按容器显示）。若按容器路由，会拿 `init.cpio` 这个名字去找
        // **条目**——那条目根本不存在（合成节点）⇒ 双击毫无反应（用户实测缺陷
        // 2026-09-24：左栏树里双击能进、右栏列表里双击没反应）。
        (true, Some(DataRowRef::Entry(row))) => Some(if row.is_dir {
            RowAction::Enter(row.path.clone())
        } else if row.is_container {
            RowAction::EnterContainer(row.path.clone())
        } else {
            RowAction::Activate(row.path.clone())
        }),
        // 单击：仅选中（`..` 行 → 清选中；隐式目录行 → 按路径选）
        (false, Some(DataRowRef::Parent)) => Some(RowAction::Select(RowSel::None)),
        (false, Some(DataRowRef::Entry(row))) => Some(RowAction::Select(match row.entry_id {
            Some(id) => RowSel::Entry(id),
            None if row.is_dir => RowSel::Dir(row.path.clone()),
            None => RowSel::None,
        })),
        (_, None) => None,
    }
}

/// 委托行数（`..` + 实体行；TableDelegate::rows_count 的纯数据面）。
/// 行内名称输入框元素（委托未持输入框实体 → None）。
///
/// 尺寸取 `Small`：行盒高度只有 ~24px，默认尺寸会撑破行（表格单元格不做
/// 裁剪，撑破表现为相邻行错位）。
fn name_input_element(delegate: &BrowseDelegate) -> Option<gpui_kit::AnyElement> {
    let input = delegate.name_input.as_ref()?;
    use gpui_kit::component::Sizable as _;
    Some(
        gpui_kit::component::input::Input::new(input)
            .with_size(gpui_kit::component::Size::Small)
            .into_any_element(),
    )
}

fn browse_rows_count(delegate: &BrowseDelegate) -> usize {
    delegate.rows.len() + usize::from(delegate.show_parent) + usize::from(delegate.name_draft)
}

/// 可见列序（委托 columns 消费面；纯函数——单测断言列序消费）。
fn visible_columns_of(config: &crate::ui::contracts::ColumnsConfig) -> Vec<BrowseColumnId> {
    config.visible()
}

impl TableDelegate for BrowseDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.visible.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        browse_rows_count(self)
    }

    /// 列定义（R-GUI4 #1：可拖宽 + 可拖序；宽 = 拖动值回填，缺省白名单宽）。
    fn column(&self, col_ix: usize, _cx: &App) -> gpui_kit::component::table::Column {
        let id = self.visible[col_ix];
        let default_w = match id {
            BrowseColumnId::Name => COL_NAME_W,
            BrowseColumnId::Type => COL_TYPE_W,
            BrowseColumnId::Size => COL_SIZE_W,
            BrowseColumnId::Packed => COL_PACKED_W,
            BrowseColumnId::Encoding => COL_ENCODING_W,
            BrowseColumnId::Mtime => COL_MTIME_W,
            BrowseColumnId::Level => COL_LEVEL_W,
            BrowseColumnId::Encryption => COL_ENCRYPTION_W,
            BrowseColumnId::Crc => COL_CRC_W,
            BrowseColumnId::Attrs => COL_ATTRS_W,
        };
        let w = self
            .widths
            .iter()
            .find(|(c, _)| *c == id)
            .map(|(_, w)| *w)
            .unwrap_or(default_w);
        // 排序指示器（列头箭头）：排序列给方向、其余列 Default（可排）——
        // table.refresh 重建 col_groups 后指示器不丢
        let sort = match self.sort {
            Some((c, dir)) if c == id => dir,
            _ => ColumnSort::Default,
        };
        // 单元格内边距：上下 = row_gap（紧凑 0 / 标准 1 / 宽松 2px；覆盖 kit
        // 的 4/3px——用户要求行内不再有默认竖向 padding）。左右 = 8px × 密度
        // 系数（6 / 8 / 11.2）：列对齐依赖横向留白，且紧凑档不应比标准档更宽
        // （kit 的 `Size::Small` 原本给 6px）。
        use gpui_kit::Edges;
        let gutter = CELL_GUTTER_BASE * crate::ui::spacing::density_scale();
        gpui_kit::component::table::Column::new(format!("{id:?}"), id.label())
            .width(w)
            .resizable(true)
            .movable(true)
            .sort(sort)
            .paddings(Edges {
                top: crate::ui::spacing::row_gap(),
                bottom: crate::ui::spacing::row_gap(),
                left: gutter,
                right: gutter,
            })
    }

    /// 列头点击排序（kit 循环 Default→Descending→Ascending→Default 后回调）：
    /// 记录排序态 + 重排行。列宽/列序拖动与点击由 kit 内建区分。
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        let id = self.visible[col_ix];
        self.sort = Some((id, sort));
        sort_delegate_rows(&mut self.rows, id, sort);
        // T-SEL1：行序已变——Shift 锚点指向旧行序，失效清空
        self.anchor = None;
    }

    /// 行容器（T-SEL1 覆写）：
    ///
    /// 1. **多选高亮**：`selected_ids` 命中行画 secondary 背景（icon/list
    ///    同语义色）。kit 的行焦点镜像 `kit_focus`（0.7.1 起由
    ///    `TableState::selection()` 的 `Row` 档灌入；焦点行：`..` 行、面包屑
    ///    跳转后悬空的行、kit 自设行）**并入同一高亮层**——统一
    ///    `selection_row_bg` 浅色，不再让 kit 的全量 accent 露出来（用户口径：
    ///    所有选中/聚焦行视觉一致）。
    /// 2. **修饰键捕获**：先挂 `on_mouse_down(Left)` 存
    ///    `MouseDownEvent.modifiers/click_count`（gpui 监听器是 Vec 叠加
    ///    不替换——kit 后链的 `on_click`（up 阶段）→ `SelectRow` 订阅消费
    ///    读到的是同一次按下的捕获值）。
    /// 3. **右键预处理**：`on_mouse_down(Right)` 先于 kit 行级右键监听与
    ///    bubble 到 inner_table 的菜单构建（同一事件内同步顺序）——未选中
    ///    行右键 = 选中集替换为该行（资源管理器语义，本地回显；uplink 由
    ///    订阅 `RightClickedRow(Some)` 分支完成，时序无关）。
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        // 「新建文件夹」草稿行：数据行之外的一行，只挂名称输入框——不参与
        // 选中/右键/双击（用户口径 2026-09-28：名称处于编辑态即可）。
        if matches!(data_row(self, row_ix), Some(DataRowRef::Draft)) {
            return div()
                .id(("row-draft", row_ix))
                .h(list_row_height(_window))
                .line_height(list_text_line_height(_window))
                .debug_selector(|| "list-row-draft".to_string())
                // 点草稿行（输入框之外的空隙）不得被空白区处理器当成「空白点击」
                // 而清空选中——与实体行的 `last_hit_row` 同语义。
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                });
        }
        let table = cx.entity();
        let table_r = table.clone();
        // 双通道高亮判定：实体行按 id、隐式目录行按路径（T-SEL 走查 #1）；
        // 再并上 kit 的单选焦点行（`kit_focus`：`..` 行、面包屑跳转后悬空的
        // 聚焦行、kit 自设行）——统一走自绘浅色，不留全量 accent
        let selected = self.kit_focus == Some(row_ix)
            || data_row(self, row_ix).is_some_and(|r| match r {
                DataRowRef::Entry(e) => match e.entry_id {
                    Some(id) => self.selected_ids.contains(&id),
                    None if e.is_dir => self.selected_dirs.contains(&e.path),
                    _ => false,
                },
                DataRowRef::Parent | DataRowRef::Draft => false,
            });
        // `..` 行的可见行索引换算（rows 编址，Shift 锚点同编址）
        let data_ix = if self.show_parent {
            row_ix.checked_sub(1)
        } else {
            Some(row_ix)
        };
        let clicked = data_row(self, row_ix).map(|r| match r {
            DataRowRef::Entry(e) => match e.entry_id {
                Some(id) => RowSel::Entry(id),
                None if e.is_dir => RowSel::Dir(e.path.clone()),
                None => RowSel::None,
            },
            DataRowRef::Parent | DataRowRef::Draft => RowSel::None,
        });
        let bg = crate::ui::theme::selection_row_bg(cx);
        div()
            .id(("row", row_ix))
            // 边界锚点（回归用：按行距断言密度对行高的作用面）
            .debug_selector(move || format!("list-row-{row_ix}"))
            .h(list_row_height(_window))
            // 文字行框与行盒同源（同一个 px 值：见 `list_text_line_height`）
            .line_height(list_text_line_height(_window))
            .when(selected, |d| {
                // 选中覆盖子层（T-SEL 走查 #4：kit 的单选 accent 背景画在
                // 行 div 背景槽——若同槽覆盖会胜过自绘色。改为绝对定位
                // 子层画在行背景之上、单元格（kit 后追加的 child）之下：
                // 最近点击行与其余选中行视觉统一）
                d.relative().child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(bg)
                        .debug_selector(move || format!("list-selected-layer-{row_ix}")),
                )
            })
            .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, cx| {
                table.update(cx, |t, _| {
                    let d = t.delegate_mut();
                    d.last_modifiers = merge_modifiers(ev.modifiers);
                    d.last_click_count = ev.click_count;
                    // 行上按下（容器处理器据此区分「点空白」）
                    d.last_hit_row = true;
                });
            })
            .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                // 右键预处理：未选中行 → 集合替换为该行 + 锚点移至该行；
                // 已选中 → 保持多选（菜单作用于整集）。`..` 行不动（菜单
                // 行相关项由 enable 判定禁用）。T-SEL 走查 #1：双通道——
                // 隐式目录行按路径预选
                let Some(sel) = clicked.clone() else {
                    return;
                };
                let (id, dir) = match &sel {
                    RowSel::Entry(id) => (Some(*id), None),
                    RowSel::Dir(p) => (None, Some(p.clone())),
                    RowSel::None => return,
                };
                table_r.update(cx, |t, cx| {
                    let d = t.delegate_mut();
                    // 右键落在行上（容器空白菜单据此抑制自己）
                    d.right_hit_row = true;
                    let already = match (id, &dir) {
                        (Some(id), _) => d.selected_ids.contains(&id),
                        (None, Some(p)) => d.selected_dirs.contains(p),
                        _ => false,
                    };
                    if !already {
                        d.selected_ids.clear();
                        d.selected_dirs.clear();
                        if let Some(id) = id {
                            d.selected_ids.insert(id);
                        }
                        if let Some(p) = dir {
                            d.selected_dirs.insert(p);
                        }
                        d.anchor = data_ix;
                        cx.notify();
                    }
                });
            })
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl gpui_kit::IntoElement {
        let col = self.visible[col_ix];
        let muted = cx.theme().muted_foreground;
        let warn = cx.theme().warning;
        // 单元格内容统一壳（用户口径：密度只调**行内上下 padding**，不动行高与对齐）：
        // - `h_full()` 填满单元格**内容盒**（= 行盒 − 上下列内边距）⇒ 行盒多出来的
        //   高度全部表现为上下 padding，文字在其中**垂直居中**；此前内容按块流顶在
        //   内容盒上沿，多出来的留白全落在一侧，才有「`_` 太贴行底」的观感。
        // - `text_base()` 把字号钉在**基准字号**：kit 的 `Size` 档会给 `Size::Small`
        //   （紧凑档）挂 `text_sm`，字号随密度变 ⇒ 墨迹/行框跟着变，正是要避免的。
        //   行框 [`list_text_line_height`] 同样按基准字号算，两侧同源。
        let shell = |d: gpui_kit::Div| -> gpui_kit::Div {
            d.h_full()
                .flex()
                .items_center()
                .text_base()
                // 单元格不参与 flex 最小宽度（`min_w_0`）+ 右侧溢出直接裁掉
                // （用户口径 2026-10-10）：列宽不足时内容不得挤到邻列去。
                .min_w_0()
                .overflow_hidden()
                .when(cfg!(test), |d| {
                    d.debug_selector(move || format!("td-cell-{row_ix}-{col_ix}"))
                })
        };
        match data_row(self, row_ix) {
            Some(DataRowRef::Parent) => shell(div()).text_color(muted).child(SharedString::from(
                if col == BrowseColumnId::Name {
                    ".."
                } else {
                    ""
                },
            )),
            // 「新建文件夹」草稿行：名称列 = 行内输入框，其余列空（用户口径 2026-09-28）
            Some(DataRowRef::Draft) => {
                if col == BrowseColumnId::Name {
                    match name_input_element(self) {
                        Some(input) => shell(h_flex()).gap(sp(1.)).child(input),
                        None => shell(div()),
                    }
                } else {
                    shell(div())
                }
            }
            Some(DataRowRef::Entry(row)) => match col {
                BrowseColumnId::Name => {
                    // 名称列类型图标（R-GUI3 #5）：回退映射见 [`row_type_icon`]
                    // ——容器（内层压缩包）用 FolderZip，别画成文件/文件夹；
                    // 系统图标命中时由 `file_icon` 换彩色位图（T-ICON）。
                    // 改名中的行：名称换成行内输入框（图标保留——用户口径 2026-09-28）
                    if self.name_rename.as_deref() == Some(row.name.as_str())
                        && let Some(input) = name_input_element(self)
                    {
                        return shell(h_flex())
                            .gap(sp(1.))
                            .child(row_file_icon(
                                row,
                                crate::ui::widgets::file_icon::Slot::Inline,
                                cx,
                            ))
                            .child(input);
                    }
                    let badges = h_flex().gap(sp(1.)).children(
                        [
                            row.encrypted.then_some(AppIcon::Lock),
                            row.suspect.then_some(AppIcon::Warn),
                        ]
                        .into_iter()
                        .flatten()
                        .map(|icon| {
                            Icon::new(icon).text_color(if icon == AppIcon::Warn {
                                warn
                            } else {
                                muted
                            })
                        }),
                    );
                    // 名称（T-ICON 挤压修复，2026-10-10）：`flex_1 + min_w_0 + truncate`
                    // ⇒ 列宽不足时**名称**收缩并省略，图标（`flex_shrink_0`）恒保尺寸。
                    shell(h_flex().min_w_0())
                        .gap(sp(1.))
                        .child(row_file_icon(
                            row,
                            crate::ui::widgets::file_icon::Slot::Inline,
                            cx,
                        ))
                        .child(badges)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(row.name.clone())),
                        )
                }
                BrowseColumnId::Size => shell(div())
                    .text_color(muted)
                    .child(SharedString::from(row.size_text.clone())),
                BrowseColumnId::Type => {
                    // 类型列（T-ICON 决策 #3）：系统类型名可用时优先，
                    // 否则回退行的自有本地化文案（优先级见 [`type_cell_text`]）。
                    let kind = crate::ui::widgets::file_icon::kind_of(
                        row.is_dir,
                        row.is_container,
                        row.is_symlink,
                    );
                    let text = type_cell_text(
                        crate::ui::widgets::file_icon::type_name(&row.name, kind, cx),
                        &row.type_text,
                    );
                    shell(div()).text_color(muted).child(text)
                }
                BrowseColumnId::Packed => shell(div())
                    .text_color(muted)
                    .child(SharedString::from(row.packed_text.clone())),
                BrowseColumnId::Encoding => shell(div())
                    .text_color(if row.suspect { warn } else { muted })
                    .child(SharedString::from(row.encoding_text.clone())),
                BrowseColumnId::Mtime => shell(div())
                    .text_color(muted)
                    .child(SharedString::from(row.mtime_text.clone())),
                BrowseColumnId::Level => shell(div())
                    .text_color(muted)
                    .child(SharedString::from(row.level_text.clone())),
                BrowseColumnId::Encryption => shell(div())
                    .text_color(if row.encrypted { warn } else { muted })
                    .child(SharedString::from(row.encryption_text.clone())),
                BrowseColumnId::Crc => shell(div())
                    .text_color(muted)
                    .child(SharedString::from(row.crc_text.clone())),
                BrowseColumnId::Attrs => shell(div())
                    .text_color(muted)
                    .child(SharedString::from(row.attrs_text.clone())),
            },
            None => div(),
        }
    }

    /// 列头拖动换序（R-GUI4 #1）：重排实时可见列序。订阅侧把新序回写
    /// `ColumnsConfig`（持久化）——见 browse_hold 表格订阅 `MoveColumn` 分支。
    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        reorder_visible(&mut self.visible, col_ix, to_ix);
    }

    /// 行右键菜单（T-SEL3；kit 原生钩子——行级右键 mouse_down（含 render_tr
    /// 的预处理）→ bubble 到 inner_table 的 `.context_menu` 包装 → 本钩子）。
    /// 可用性从**委托现态**算（render_tr 的 on_mouse_down(Right) 已同步完成
    /// 右键预处理——selected_ids 即最新集合）；动作通道经 Global 取。
    fn context_menu(
        &mut self,
        _row_ix: usize,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(command) = command_channel(cx) else {
            return menu;
        };
        let enable = menu_enable_of_delegate(
            &self.selected_ids,
            &self.selected_dirs,
            &self.rows,
            self.can_edit,
            // 交互时机现算（委托钩子构建期——非渲染帧）
            paste_available_now(cx),
        );
        build_browse_menu(menu, command, enable)
    }
}

/// 可见列序重排（move_column 纯数据面；越界夹紧 + 同位 no-op）。
fn reorder_visible(visible: &mut Vec<BrowseColumnId>, col_ix: usize, to_ix: usize) {
    if col_ix == to_ix || col_ix >= visible.len() {
        return;
    }
    let to_ix = to_ix.min(visible.len() - 1);
    let col = visible[col_ix];
    visible.remove(col_ix);
    visible.insert(to_ix, col);
}

/// 文件列表行高 = 本档单元格文本行高 + 上下各 `row_gap()`。
///
/// 用户逐档口径：紧凑 0 / 标准 1 / 宽松 2px（`row_gap()`）。kit 先按
/// `Size` 档给行高（`table_row_height`：Small 30 / Medium 32px），再
/// `refine_style` 委托返回的样式 ⇒ `render_tr` 里的 `h()` 胜出。
///
/// **字号折算**：kit 的单元格字号随 `Size` 档走（`Size::Small` → `text_sm`
/// = 0.875rem，见 `table_cell_size`），而行高必须跟**本档实际字号**齐平——
/// 否则紧凑档会平白多出约 3px（0.125rem × 行高系数）的空白，与「紧凑下
/// 上下不要 padding」相悖。
///
/// 行框取自 [`crate::ui::spacing::line_height_px`]（配置 `[ui].line_height` ×
/// 本档字号，夹字形盒下限）——不再直接吃 gpui 的默认行高 φ(1.618em)：
/// 那是「每行文字上下各留 ~0.3em 空白」的排版默认值，与「行距只手感值
/// `row_gap`」的口径相抵（用户实测：列表行比目录树松一截）。
fn list_row_height(window: &Window) -> gpui_kit::Pixels {
    // `+1`：kit 给行挂 `border_b_1`（`need_render_border` 分支），而 gpui 是
    // **border-box**——边框算在行高里 ⇒ 不减这 1px，单元格内容盒就比文字行框矮
    // 1px（文字上下各被裁 0.5px）。补上后内容盒 == 行框，密度只作用在 `row_gap`。
    list_text_line_height(window) + crate::ui::spacing::metrics().row_gap() * 2. + LIST_ROW_BORDER_H
}

/// 列表单元格文字的行高（px）——**与密度无关**，只随基准字号与 `[ui].line_height`。
///
/// 用户逐档实测的根因就在这里：行框一旦跟着密度走（紧凑档经 `Size::Small` ⇒
/// 单元格字号 0.875rem ⇒ 行框 21px），紧凑档的 `_`/降部与行框边缘只剩不到 1px，
/// 观感就是「`_` 直接显示不出来」；标准档（24px）则「`_` 过于贴近行底部」。
///
/// 现行口径（用户要求）：**紧凑/标准/宽松只调行内上下 padding，不动行高与对齐**
/// ——故行框按**基准字号**（`window.rem_size()`）算，`Size` 档与密度都不参与，
/// 三档同一个值（本机 16px 基准下 = 26px：墨迹盒 23.8 + 上下各 2px 余量）。
///
/// 与 [`list_row_height`] **必须同源**：行盒压到 gpui 默认行高 φ 以下、而单元格
/// 文字仍按 φ 排行框时，文字墨迹会溢出内容盒被裁（用户实测：行高改小后「文字
/// 底下丢了一部分」）。`render_tr` 把同一个 **px** 值挂到行上（不是 `relative(em)`：
/// 相对值要经文本系统再乘一次字号，取整差一点就是「文字底部被裁」），单元格文字
/// 继承它 ⇒ 行框恰好装得下字形。
fn list_text_line_height(window: &Window) -> gpui_kit::Pixels {
    crate::ui::spacing::text_line_floor(window, window.rem_size())
}

#[cfg(test)]
#[path = "tests/browse.rs"]
mod tests;
