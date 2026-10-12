//! 菜单条（T-GUI T21 → T-MENU1：六组完整菜单 + MenuContext 投影）。
//!
//! # 结构
//!
//! - 菜单注册：[`build_app_menus`] 产出 `OwnedMenu` 序列，由 `run_ui`
//!   （窗口打开前）写入 `GlobalState`，`AppMenuBar` 从那里装配弹层；
//! - [`MenuBar`]：`Entity<AppMenuBar>` 薄包装——[`MenuBar::sync`] 在
//!   「档案在场状态」「界面语言」「菜单数据版本 epoch」任一变化时重建
//!   app_menus 并 `reload()`。
//!
//! **为什么菜单要随语言重建**：`build_app_menus` 把触发器名与条目名**快照**进
//! `GlobalState`（`OwnedMenu` 持 `SharedString`），弹层由该快照装配。故语言一变
//! 快照即过期——触发器与弹出框会停留在旧语言（用户实测现象）。修复见
//! [`MenuBar::sync`]：渲染帧内检测 locale 变化并重建（与 has_archive 同路径）。
//!
//! **为什么菜单要随 epoch 重建**：MRU 列表、视图偏好勾选态（树型目录/工具栏/
//! 视图样式）都**快照**进菜单条目（勾选态/动态子菜单），数据变化时必须重装。
//! `ShellState.menu_epoch` 由这些数据的写入方递增，[`MenuBar::sync`] 消费。
//!
//! # 动作路由
//!
//! 菜单项是 `actions!(hzip_gui, …)` 的动作（shell 模块注册）；点击经
//! PopupMenu `dispatch_action` 沿焦点链上溯到 AppShell 的 `on_action` 链。
//! AppMenuBar `build_popup_menu` 已把焦点锚到 popup 自身（gpui 同款语义）。

use gpui_kit::{App, AppContext as _, Context, Entity, IntoElement, Render, Window};

use crate::ui::shell::{
    About, AddFiles, CheckUpdate, ClearMru, CopyFiles, DeleteFiles, Extract, FindFile, NewWindow,
    Open, OpenRecent, Paste, Quit, RefreshView, Reveal, SetToolbarStyle, SetViewStyle, Settings,
    ShowJobs, TestIntegrity, ToggleJobs, ToggleTree,
};

/// 最近文件条目展示名：文件名（长路径中段省略——设计 §2.1）。
///
/// 全路径放在 tooltip 不可行（kit OwnedMenuItem 无 tooltip 槽位），
/// 长路径以 `…\dir\a.zip` 形态显示，保留文件名与一级父目录。
pub(crate) fn recent_label(path: &str) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let name = parts.last().copied().unwrap_or(path);
    if parts.len() <= 1 {
        return name.to_string();
    }
    let parent = parts[parts.len() - 2];
    let formatted = format!("…{sep}{parent}{sep}{name}");
    if formatted.chars().count() > 48 {
        // 超长：仅文件名（前缀省略号）
        format!("…{sep}{name}")
    } else {
        formatted
    }
}

/// config 值 → ToolbarStyle（未知/旧值 large/small 回落 IconText——宽容
/// 手改配置；旧值 large 语义上本就是图标+文字，无感迁移）。
pub fn toolbar_style_of(v: &str) -> ToolbarStyle {
    match v {
        "icon_only" => ToolbarStyle::IconOnly,
        "hidden" => ToolbarStyle::Hidden,
        _ => ToolbarStyle::IconText,
    }
}

/// config 值 → ViewStyle（未知回落 Detail）。
pub fn view_style_of(v: &str) -> ViewStyle {
    match v {
        "icon" => ViewStyle::Icon,
        "list" => ViewStyle::List,
        _ => ViewStyle::Detail,
    }
}

/// 通知身份标记（T-MENU1 检查更新占位提示 id；同 id 通知不重复弹）。
#[derive(Debug, PartialEq, Eq)]
pub struct MenuEpochMarker;

use crate::ui::widgets::AppIcon;

/// 视图样式（T-MENU5 持久化偏好；icon/list 渲染归 T-MENU9）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewStyle {
    /// 详细（DataTable，默认）
    Detail,
    /// 图标网格（T-MENU9）
    Icon,
    /// 紧凑列表（T-MENU9）
    List,
}

impl ViewStyle {
    /// 全部取值（装配单选组顺序）。
    pub const ALL: [ViewStyle; 3] = [ViewStyle::Detail, ViewStyle::Icon, ViewStyle::List];

    /// i18n key（菜单项标签）。
    pub fn key(self) -> &'static str {
        match self {
            ViewStyle::Detail => "menu.view.style_detail",
            ViewStyle::Icon => "menu.view.style_icon",
            ViewStyle::List => "menu.view.style_list",
        }
    }

    /// config 落盘值（[ui].view_style）。
    pub fn value_str(self) -> &'static str {
        match self {
            ViewStyle::Detail => "detail",
            ViewStyle::Icon => "icon",
            ViewStyle::List => "list",
        }
    }
}

/// 工具栏三态（T-TVS2 内容语义：T-MENU5 原为尺寸语义 large/small——但
/// 渲染层从未有视觉差异（icon_size 死代码）；换为「图标+文字/仅图标/隐藏」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarStyle {
    /// 图标 + 文字（默认）
    IconText,
    /// 仅图标（tooltip 保留功能名提示）
    IconOnly,
    /// 整条收起
    Hidden,
}

impl ToolbarStyle {
    /// 全部取值（装配单选组顺序）。
    pub const ALL: [ToolbarStyle; 3] = [
        ToolbarStyle::IconText,
        ToolbarStyle::IconOnly,
        ToolbarStyle::Hidden,
    ];

    /// i18n key（菜单项标签）。
    pub fn key(self) -> &'static str {
        match self {
            ToolbarStyle::IconText => "menu.view.toolbar_icon_text",
            ToolbarStyle::IconOnly => "menu.view.toolbar_icon_only",
            ToolbarStyle::Hidden => "menu.view.toolbar_hidden",
        }
    }

    /// config 落盘值（[ui].toolbar_style）。
    pub fn value_str(self) -> &'static str {
        match self {
            ToolbarStyle::IconText => "icon_text",
            ToolbarStyle::IconOnly => "icon_only",
            ToolbarStyle::Hidden => "hidden",
        }
    }
}

/// 菜单条触发器（六组；顺序 = 渲染顺序，与设计 §2 一一对应：
/// 文件(F)/编辑(E)/工具(T)/选项(O)/视图(V)/帮助(H)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhichMenu {
    /// 文件(F)（B1 裁定默认：桌面惯例——「打开」留给组内首项）
    File,
    /// 编辑(E)
    Edit,
    /// 工具(T)
    Tools,
    /// 选项(O)
    Options,
    /// 视图(V)
    View,
    Help,
}

impl WhichMenu {
    /// 全部触发器（渲染顺序；设计 §1.1 六组新序）。
    pub const ALL: [WhichMenu; 6] = [
        WhichMenu::File,
        WhichMenu::Edit,
        WhichMenu::Tools,
        WhichMenu::Options,
        WhichMenu::View,
        WhichMenu::Help,
    ];

    /// 触发器展示名（经 i18n 查表）。
    pub fn label(self) -> String {
        match self {
            WhichMenu::File => crate::tr("menu.file.label"),
            WhichMenu::Edit => crate::tr("menu.edit.label"),
            WhichMenu::Tools => crate::tr("menu.tools.label"),
            WhichMenu::Options => crate::tr("menu.options.label"),
            WhichMenu::View => crate::tr("menu.view.label"),
            WhichMenu::Help => crate::tr("menu.help.label"),
        }
    }
}

/// 菜单项动作（`actions!` 集的纯数据投影——headless 测试面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Open,
    /// 打开最近文件（payload = MRU 下标；装配时解析为完整路径）
    OpenRecent(usize),
    /// 清空最近文件历史（T-MENU4）
    ClearMru,
    /// 设置工具栏三态（payload = ToolbarStyle::ALL 下标；T-MENU5）
    SetToolbarStyle(usize),
    /// 设置视图样式（payload = ViewStyle::ALL 下标；T-MENU5，icon/list T-MENU9 启用）
    SetViewStyle(usize),
    Extract,
    Reveal,
    NewWindow,
    Quit,
    AddFiles,
    DeleteFiles,
    CopyFiles,
    Paste,
    FindFile,
    TestIntegrity,
    Settings,
    ToggleTree,
    ShowJobs,
    ToggleJobs,
    RefreshView,
    About,
    CheckUpdate,
}

impl MenuAction {
    /// 投影回真实 Action（菜单项 `dispatch_action` 派发）。
    pub fn boxed(self) -> Box<dyn gpui_kit::Action> {
        match self {
            MenuAction::Open => Box::new(Open),
            MenuAction::Extract => Box::new(Extract),
            // OpenRecent 由 build_app_menus 装配 payload 实例——本函数不达
            MenuAction::OpenRecent(i) => {
                let _ = i;
                Box::new(OpenRecent {
                    path: String::new(),
                })
            }
            MenuAction::Reveal => Box::new(Reveal),
            MenuAction::NewWindow => Box::new(NewWindow),
            MenuAction::Quit => Box::new(Quit),
            MenuAction::AddFiles => Box::new(AddFiles),
            MenuAction::DeleteFiles => Box::new(DeleteFiles),
            MenuAction::CopyFiles => Box::new(CopyFiles),
            MenuAction::Paste => Box::new(Paste),
            MenuAction::FindFile => Box::new(FindFile),
            MenuAction::TestIntegrity => Box::new(TestIntegrity),
            MenuAction::Settings => Box::new(Settings),
            MenuAction::ToggleTree => Box::new(ToggleTree),
            MenuAction::ShowJobs => Box::new(ShowJobs),
            MenuAction::ToggleJobs => Box::new(ToggleJobs),
            MenuAction::RefreshView => Box::new(RefreshView),
            MenuAction::About => Box::new(About),
            MenuAction::ClearMru => Box::new(ClearMru),
            MenuAction::CheckUpdate => Box::new(CheckUpdate),
            MenuAction::SetToolbarStyle(ix) => {
                // Set* 装配点解析 payload（build_app_menus）——占位
                let _ = ix;
                Box::new(SetToolbarStyle {
                    value: String::new(),
                })
            }
            MenuAction::SetViewStyle(ix) => {
                let _ = ix;
                Box::new(SetViewStyle {
                    value: String::new(),
                })
            }
        }
    }
}

/// 弹出项纯数据描述（清单断言面；渲染时映射为菜单条目）。
///
/// 不派生 `Copy`：`label` 是 i18n 查表结果（`String`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// 分隔线
    Separator,
    /// 子菜单（T-MENU2 Spike 结论：`MenuItem::Submenu(OwnedMenu)` 原生支持）
    Submenu {
        /// 展示名
        label: String,
        /// 子菜单条目（可含 Separator；空 = 渲染禁用占位）
        entries: Vec<MenuEntry>,
        /// 禁用态
        disabled: bool,
    },
    /// 动作项
    Item {
        /// 展示名（i18n 查表结果）
        label: String,
        /// 图标（None = 不带）
        icon: Option<AppIcon>,
        /// 动作
        action: MenuAction,
        /// 禁用态
        disabled: bool,
        /// 勾选态（视图组开关/单选项）
        checked: bool,
    },
}

impl MenuEntry {
    /// 动作项构造（checked 默认 false——直连项不携带勾选态）。
    fn item(label: String, icon: Option<AppIcon>, action: MenuAction, disabled: bool) -> MenuEntry {
        MenuEntry::Item {
            label,
            icon,
            action,
            disabled,
            checked: false,
        }
    }
}

/// 菜单投影输入（设计 §3 状态机；禁用/勾选矩阵的唯一事实源）。
///
/// T-MENU1 落地骨架：`caps_edit` 恒 true（T-MENU6 填实 registry 投影），
/// `mru`/`show_tree`/`toolbar_style`/`view_style` 恒默认值（T-MENU4/5 填实）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuContext {
    /// 已打开档案（编辑组/解压/查找/测试完整性/在文件管理器显示 前置）
    pub has_archive: bool,
    /// 右栏有选中（删除/复制前置）
    pub has_selection: bool,
    /// 档案格式可编辑（registry caps.edit + 非组合；T-MENU6 填实，恒 true）
    pub caps_edit: bool,
    /// 最近文件（LRU 序；T-MENU4 填实，恒空）
    pub mru: Vec<String>,
    /// 浏览页左树显隐（T-MENU5 填实）
    pub show_tree: bool,
    /// 任务中心页签显隐（ui.show_jobs；勾选态 = 界面显示任务中心）
    pub show_jobs: bool,
    /// 工具栏三态（T-MENU5 填实，恒 Large）
    pub toolbar_style: ToolbarStyle,
    /// 视图样式（T-MENU5 填实，恒 Detail）
    pub view_style: ViewStyle,
    /// 剪贴板有待粘贴文件（T-MENU8：粘贴项禁用输入）
    pub has_clipboard: bool,
}

impl MenuContext {
    /// 无档案默认投影（启动/空态）。
    pub fn closed() -> Self {
        Self {
            has_archive: false,
            has_selection: false,
            caps_edit: false,
            mru: Vec::new(),
            show_tree: true,
            show_jobs: false,
            toolbar_style: ToolbarStyle::IconText,
            view_style: ViewStyle::Detail,
            has_clipboard: false,
        }
    }

    /// 打开档案投影（无选中；caps_edit 等由任务卡 4/5/6 填实——本任务恒默认）。
    pub fn opened() -> Self {
        Self {
            has_archive: true,
            // T-MENU6 填实 registry 投影；opened() 语义 = 可编辑档案在场
            caps_edit: true,
            ..Self::closed()
        }
    }
}

/// MRU → 菜单项（「文件 ▸ 最近的文件」与工具栏「打开▾」**共用**——同一份
/// 内容与动作，避免两处菜单长得不一样）。
pub(crate) fn mru_items(mru: &[String]) -> Vec<MenuEntry> {
    // T-MENU4：「最近的文件」子菜单（LRU 序；label = 文件名）。
    // 空 → 禁用占位「（无最近文件）」；尾部「清空所有历史记录」。
    if mru.is_empty() {
        return vec![MenuEntry::Item {
            label: crate::tr("menu.file.recent_empty"),
            icon: None,
            action: MenuAction::OpenRecent(usize::MAX),
            disabled: true,
            checked: false,
        }];
    }
    let mut v: Vec<MenuEntry> = mru
        .iter()
        .enumerate()
        .map(|(i, p)| MenuEntry::Item {
            label: recent_label(p),
            icon: Some(AppIcon::FolderZip),
            action: MenuAction::OpenRecent(i),
            disabled: false,
            checked: false,
        })
        .collect();
    v.push(MenuEntry::Separator);
    v.push(MenuEntry::item(
        crate::tr("menu.file.recent_clear"),
        None,
        MenuAction::ClearMru,
        false,
    ));
    v
}

/// 菜单清单（设计 §2 完整菜单树；T-MENU1 六组重排 + P0 十项直连）。
pub fn menu_entries(which: WhichMenu, ctx: &MenuContext) -> Vec<MenuEntry> {
    match which {
        WhichMenu::File => {
            let recent = MenuEntry::Submenu {
                label: crate::tr("menu.file.recent"),
                entries: mru_items(&ctx.mru),
                disabled: false,
            };
            vec![
                MenuEntry::item(
                    crate::tr("menu.file.open"),
                    Some(AppIcon::Folder),
                    MenuAction::Open,
                    false,
                ),
                recent,
                MenuEntry::item(
                    crate::tr("menu.file.reveal"),
                    Some(AppIcon::ExternalLink),
                    MenuAction::Reveal,
                    !ctx.has_archive,
                ),
                MenuEntry::item(
                    crate::tr("menu.file.extract"),
                    Some(AppIcon::PackageMinus),
                    MenuAction::Extract,
                    !ctx.has_archive,
                ),
                MenuEntry::Separator,
                MenuEntry::item(
                    crate::tr("menu.file.new_window"),
                    None,
                    MenuAction::NewWindow,
                    false,
                ),
                MenuEntry::item(
                    crate::tr("menu.file.quit"),
                    Some(AppIcon::X),
                    MenuAction::Quit,
                    false,
                ),
            ]
        }
        WhichMenu::Edit => {
            // 编辑四件套（T-MENU6/7/8 接线；caps_edit 本任务恒 true——
            // 无档案即整组 disabled；T-MENU6 填实格式投影）
            let group_disabled = !ctx.has_archive || !ctx.caps_edit;
            vec![
                MenuEntry::item(
                    crate::tr("menu.edit.add_files"),
                    Some(AppIcon::Plus),
                    MenuAction::AddFiles,
                    group_disabled,
                ),
                MenuEntry::item(
                    crate::tr("menu.edit.delete_files"),
                    Some(AppIcon::Minus),
                    MenuAction::DeleteFiles,
                    group_disabled || !ctx.has_selection,
                ),
                MenuEntry::item(
                    crate::tr("menu.edit.copy"),
                    None,
                    MenuAction::CopyFiles,
                    group_disabled || !ctx.has_selection,
                ),
                MenuEntry::item(
                    crate::tr("menu.edit.paste"),
                    None,
                    MenuAction::Paste,
                    group_disabled || !ctx.has_clipboard,
                ),
            ]
        }
        WhichMenu::Tools => vec![
            MenuEntry::item(
                crate::tr("menu.tools.find_file"),
                Some(AppIcon::Search),
                MenuAction::FindFile,
                !ctx.has_archive,
            ),
            MenuEntry::item(
                crate::tr("menu.tools.test_integrity"),
                Some(AppIcon::ShieldCheck),
                MenuAction::TestIntegrity,
                !ctx.has_archive,
            ),
        ],
        WhichMenu::Options => vec![MenuEntry::item(
            crate::tr("menu.options.settings"),
            Some(AppIcon::Settings),
            MenuAction::Settings,
            false,
        )],
        WhichMenu::View => vec![
            MenuEntry::Item {
                label: crate::tr("menu.view.tree"),
                icon: None,
                action: MenuAction::ToggleTree,
                disabled: false,
                checked: ctx.show_tree,
            },
            // 工具栏 ▸ 三选一（单选组；checked 随值——T-MENU5）
            MenuEntry::Submenu {
                label: crate::tr("menu.view.toolbar"),
                entries: ToolbarStyle::ALL
                    .iter()
                    .enumerate()
                    .map(|(ix, style)| MenuEntry::Item {
                        label: crate::tr(style.key()),
                        icon: None,
                        action: MenuAction::SetToolbarStyle(ix),
                        disabled: false,
                        checked: *style == ctx.toolbar_style,
                    })
                    .collect(),
                disabled: false,
            },
            // 视图样式 ▸ 三选一（T-MENU9 起 icon/list 均可选）
            MenuEntry::Submenu {
                label: crate::tr("menu.view.style"),
                entries: ViewStyle::ALL
                    .iter()
                    .enumerate()
                    .map(|(ix, style)| MenuEntry::Item {
                        label: crate::tr(style.key()),
                        icon: None,
                        action: MenuAction::SetViewStyle(ix),
                        disabled: false,
                        checked: *style == ctx.view_style,
                    })
                    .collect(),
                disabled: false,
            },
            MenuEntry::Separator,
            // 任务中心：**显隐开关**（与「树型目录」同款语义——勾选 = 界面
            // 显示任务中心；默认关 = 只有浏览页、连页签栏都不显示）。
            // 切页签由页签栏/ctrl-j 承担，本项不再兼职。
            MenuEntry::Item {
                label: crate::tr("menu.view.jobs"),
                icon: Some(AppIcon::Inbox),
                action: MenuAction::ToggleJobs,
                disabled: false,
                checked: ctx.show_jobs,
            },
            MenuEntry::item(
                crate::tr("menu.view.refresh"),
                Some(AppIcon::Refresh),
                MenuAction::RefreshView,
                false,
            ),
        ],
        // T-MENU1 恢复 Help 组：About 对话框（T34 裁撤后重新实现）+ 检查更新占位
        WhichMenu::Help => vec![
            MenuEntry::item(
                crate::tr("menu.help.about"),
                Some(AppIcon::Info),
                MenuAction::About,
                false,
            ),
            MenuEntry::item(
                crate::tr("menu.help.check_update"),
                None,
                MenuAction::CheckUpdate,
                false,
            ),
        ],
    }
}

/// `OwnedMenu` 装配（T-GUI T21：写入 `GlobalState` 前的产物；窗口打开前调用）。
pub fn build_app_menus(ctx: &MenuContext) -> Vec<gpui_kit::OwnedMenu> {
    fn build_items(entries: Vec<MenuEntry>, ctx_mru: &[String]) -> Vec<gpui_kit::MenuItem> {
        entries
            .into_iter()
            .map(|entry| match entry {
                MenuEntry::Separator => gpui_kit::MenuItem::separator(),
                MenuEntry::Item {
                    label,
                    action,
                    disabled,
                    checked,
                    ..
                } => {
                    // payload 动作在装配点解析（OpenRecent=路径 / Set* =落盘值）
                    let action: Box<dyn gpui_kit::Action> = match action {
                        MenuAction::OpenRecent(i) => Box::new(OpenRecent {
                            path: ctx_mru.get(i).cloned().unwrap_or_default(),
                        }),
                        MenuAction::SetToolbarStyle(ix) => Box::new(SetToolbarStyle {
                            value: ToolbarStyle::ALL
                                .get(ix)
                                .map(|s| s.value_str().to_string())
                                .unwrap_or_else(|| "icon_text".into()),
                        }),
                        MenuAction::SetViewStyle(ix) => Box::new(SetViewStyle {
                            value: ViewStyle::ALL
                                .get(ix)
                                .map(|s| s.value_str().to_string())
                                .unwrap_or_else(|| "detail".into()),
                        }),
                        other => other.boxed(),
                    };
                    gpui_kit::MenuItem::Action {
                        name: label.into(),
                        action,
                        os_action: None,
                        checked,
                        disabled,
                    }
                }
                MenuEntry::Submenu {
                    label,
                    entries,
                    disabled,
                } => gpui_kit::MenuItem::Submenu(
                    gpui_kit::Menu::new(label)
                        .disabled(disabled)
                        .items(build_items(entries, ctx_mru)),
                ),
            })
            .collect()
    }

    WhichMenu::ALL
        .iter()
        .filter_map(|which| {
            let items = build_items(menu_entries(*which, ctx), &ctx.mru);
            // 空条目组不注册（防悬空触发器）
            if items.is_empty() {
                return None;
            }
            Some(gpui_kit::Menu::new(which.label()).items(items).owned())
        })
        .collect()
}

/// 菜单条（AppShell 持有；包装 gpui-kit `AppMenuBar`）。
pub struct MenuBar {
    /// 内建菜单条（Esc/左右方向键/hover 互斥/焦点锚全内建）
    inner: Entity<gpui_kit::component::menu::AppMenuBar>,
    /// 最近一次装配的投影（has_archive/勾选态/动态子菜单 数据源）
    ctx: MenuContext,
    /// app_menus 最近一次装配时的 locale。
    ///
    /// 菜单名/条目名是**快照**（见模块文档），语言变了必须重建；本字段是变化
    /// 检测的唯一依据（构造时即取当前 locale，与启动期 `register_menus` 同语言）。
    built_locale: String,
    /// app_menus 最近一次装配时的 menu_epoch（ShellState 持有；变化即重建）。
    built_epoch: u64,
}

impl MenuBar {
    /// 创建菜单条实体（AppShell::with_data 一次；须先经 [`register_menus`]
    /// 写入 GlobalState——`AppMenuBar::reload` 从那里装配）。
    pub fn new(cx: &mut App) -> Entity<Self> {
        let inner = gpui_kit::component::menu::AppMenuBar::new(cx);
        cx.new(|_| Self {
            inner,
            ctx: MenuContext::closed(),
            built_locale: crate::current_locale(),
            built_epoch: 0,
        })
    }

    /// 同步菜单快照（每帧由 `AppShell::render` 调用）。
    ///
    /// 三个触发条件，任一命中即重建 app_menus + `reload()`：
    ///
    /// 1. **投影变化**（has_archive/has_selection/勾选态/MRU——T-MENU1 扩展
    ///    自原 has_archive 单条件）；
    /// 2. **界面语言**变化——快照文案过期；
    /// 3. **menu_epoch** 变化——MRU/视图偏好的显式版本号（ShellState 递增）。
    ///
    /// 无变化时零成本：locale 比较是免分配的 `&str` 比较，不进 `reload`。
    pub fn sync(&mut self, ctx: &MenuContext, epoch: u64, cx: &mut Context<Self>) {
        let locale_stale = !crate::locale_is(&self.built_locale);
        if *ctx != self.ctx || epoch != self.built_epoch || locale_stale {
            self.ctx = ctx.clone();
            self.built_epoch = epoch;
            register_menus(ctx, cx);
            self.inner.update(cx, |bar, cx| bar.reload(cx));
            self.built_locale = crate::current_locale();
            cx.notify();
        }
    }
}

/// 菜单清单注册（T-GUI T21：窗口打开前调用一次；`run_ui` 与
/// [`MenuBar::sync`] 变化路径共用）。
pub fn register_menus(ctx: &MenuContext, cx: &mut App) {
    use gpui_kit::base::GlobalState;
    GlobalState::global_mut(cx).set_app_menus(build_app_menus(ctx));
}

impl Render for MenuBar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.inner.clone()
    }
}

#[cfg(test)]
#[path = "tests/menu_bar.rs"]
mod tests;
