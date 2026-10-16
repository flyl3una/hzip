//! 设置注册表（T-SET §2）：统一设置流程的**单一事实来源**。
//!
//! # 流程（README §1.2 五步）
//!
//! ```text
//! 控件变更 → DialogEvent::SettingChanged { id, value }
//!   → AppData::apply_setting                          （app 层；无 Window）
//!       ├ normalize：越界下标/未知值/非白名单集合回落（store 内）
//!       ├ store    ：写 Config 节 + SettingsPrefs 镜像
//!       ├ persist  ：唯一一次 Config::save()（cfg!(test) 零落盘）
//!       └ dispatch ：Effect 数据半场（列脏旗）+ VmSignal UI 半场（主题等）
//!   → SettingsView::load/app 活读 → 面板逐帧回读 + 主窗口信号刷新
//! ```
//!
//! # 不变量
//!
//! 1. **唯一清单**：每项设置 = 一条 [`SettingDef`]。新增设置项 = 加一行 +
//!    两个函数（load/store），不改流程、不改面板（渲染器遍历本表）。
//! 2. **零插件落盘**：落盘/信号是框架尾步；`load`/`store` 不碰磁盘、不碰
//!    Window（[`Effect`] 只声明意图，由两侧半场消费）。
//! 3. **单一白名单**：label↔value 映射唯一来源 [`crate::app::labels`]；
//!    本表只登记 Select 的 label 序列（下标 = value 下标）。
//! 4. **无第二份快照**：面板与主窗口一律经 [`SettingsView`] 活读
//!    （AppData 镜像 + Config 两半），不存在打开期冻结副本。

use hzip_config::Config;

use crate::app::labels;
use crate::app::state::AppData;
use crate::ui::contracts::{CTX_COMPRESS_KEYS, CTX_EXTRACT_KEYS, ColumnsConfig, DialogEvent};
use crate::viewmodel::SettingId;
use crate::viewmodel::density::Density;

// —— 分类页 ——

/// 设置分类页（TabBar 选中态；下标 = [`SettingsTab::LABELS`]）。
///
/// 定义在 app 层（而非 `ui/views`）：注册表的 `page` 字段需要它，而依赖
/// 方向是 `ui → app`（07 §3 红线——app 不得反向 import ui）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    /// 常规
    #[default]
    General,
    /// 文件关联
    Assoc,
    /// 上下文菜单
    ContextMenu,
    /// 解压设置
    Extract,
    /// 压缩设置
    Compress,
    /// 查看器
    Viewer,
    /// 语言
    Language,
    /// 快捷键（T-KEYS：14 条动作的查看/改键/禁用/恢复默认）
    Keys,
}

impl SettingsTab {
    /// 全部页（顺序 = TabBar 下标）。
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::Assoc,
        Self::ContextMenu,
        Self::Extract,
        Self::Compress,
        Self::Viewer,
        Self::Language,
        Self::Keys,
    ];

    /// 页签文案 key（顺序 = [`Self::ALL`]）；渲染处经 `crate::tr` 查表。
    ///
    /// 存 **key** 而非文案：页签在 `TabBar` 里构建，文案在渲染时求值——
    /// 运行时切语言后下一帧即得新语言。
    pub const LABEL_KEYS: [&'static str; 8] = [
        "tab.general",
        "tab.assoc",
        "tab.context_menu",
        "tab.extract",
        "tab.compress",
        "tab.viewer",
        "tab.language",
        "tab.keys",
    ];

    /// TabBar 下标 → 分类（越界回落语言页末位守界——防守白名单漂移）。
    ///
    /// 守界项**钉死在 `Language`**（而不是「最后一个变体」）：越界是白名单漂移
    /// 信号，落到一个与下标无关的稳定页比落到新页更可预期（既有语义，勿改）。
    pub fn from_index(ix: usize) -> Self {
        Self::ALL.get(ix).copied().unwrap_or(Self::Language)
    }

    /// TabBar 下标。
    pub fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Assoc => 1,
            Self::ContextMenu => 2,
            Self::Extract => 3,
            Self::Compress => 4,
            Self::Viewer => 5,
            Self::Language => 6,
            Self::Keys => 7,
        }
    }

    /// 页签/分组标题（本地化）。
    pub fn label(self) -> String {
        crate::tr(Self::LABEL_KEYS[self.index()])
    }
}

// —— 取值 / 控件 / 效果 ——

/// 设置项取值（类型由该项 [`WidgetKind`] 决定）。
///
/// `Clone + PartialEq + Eq` 是硬要求：[`DialogEvent::SettingChanged`] 派生
/// 这三者；`ColumnsConfig` 同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    /// 开关
    Bool(bool),
    /// 白名单下标（Select）
    Index(usize),
    /// 自由文本（路径输入；空 = 系统默认）
    Text(String),
    /// 勾选集合（整组上行；`CheckGroup` = 白名单交集 / `ExtGroup` = 候选集）
    StrSet(Vec<String>),
    /// 浏览列顺序 + 显隐
    Columns(ColumnsConfig),
    /// 数值（步进控件上行；如界面字号 px）
    Number(u32),
    /// 快捷键绑定整节（T-KEYS：14 条动作一次上行——录制/禁用/恢复默认都写整份）
    ///
    /// 为什么整份而不是「单个动作一项」：配置侧就是 `[keys]` 一个表、keymap
    /// 装配要的也是整份（见 [`crate::ui::shell::keymap::install`]），逐字段上行
    /// 只会在两侧各加一层拼装。`Keys` 派生 `PartialEq + Eq`（T-KEYS §2 冻结），
    /// 故可进本枚举。
    ///
    /// **为什么 `Box`**：`Keys` 有 14 个 `String`（≈实参 336 B），内联进本枚
    /// 举会把 `SettingValue` 撑大 ⇒ `DialogEvent`/`ActionKind` 跟着变大
    /// （clippy `large_enum_variant`，且每次克隆都要真拷 14 个字符串）。
    /// 装箱后按值传递只剩一次引用计数，`as_keys()`/`keys_of()` 仍返回 `&Keys`。
    Keys(Box<hzip_config::Keys>),
}

impl SettingValue {
    /// 下标取值（非 `Index` → None；store 侧 `let ... else` 守界）。
    pub fn as_index(&self) -> Option<usize> {
        match self {
            Self::Index(ix) => Some(*ix),
            _ => None,
        }
    }

    /// 开关取值。
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// 文本取值。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// 勾选集合取值。
    pub fn as_str_set(&self) -> Option<&[String]> {
        match self {
            Self::StrSet(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// 列设置取值。
    pub fn as_columns(&self) -> Option<&ColumnsConfig> {
        match self {
            Self::Columns(c) => Some(c),
            _ => None,
        }
    }

    /// 数值取值（步进控件）。
    pub fn as_number(&self) -> Option<u32> {
        match self {
            Self::Number(v) => Some(*v),
            _ => None,
        }
    }

    /// 快捷键整节取值（非 `Keys` → None；store 侧 `let ... else` 守界）。
    pub fn as_keys(&self) -> Option<&hzip_config::Keys> {
        match self {
            Self::Keys(k) => Some(k.as_ref()),
            _ => None,
        }
    }
}

/// 控件种类（渲染器按此产出 gpui 元素；`Select` 的 label 序列 = 白名单）。
#[derive(Debug, Clone, Copy)]
pub enum WidgetKind {
    /// 开关行（文案 = `SettingDef.label`；帮助经行尾 ？ 悬浮）
    Switch,
    /// 下拉选择
    Select {
        /// 白名单 label **key** 序列（下标 = value 下标；展示文案渲染时查表）
        label_keys: &'static [&'static str],
    },
    /// 白名单勾选组（整组上行）
    CheckGroup {
        /// (key, label) 白名单
        keys: &'static [(&'static str, &'static str)],
    },
    /// 扩展名勾选组（候选集由打开路径现算）
    ExtGroup,
    /// 路径文本输入 + 浏览按钮（空 = 系统默认）
    Text,
    /// 主题色选择（gpui-kit ColorPicker；值 = hex 文本，空 = 默认）
    /// ——T-SEL 走查 #3
    Color,
    /// 数值步进（[−] 值 [+]）：边界/步长由注册表给出（界面字号等）
    Number {
        /// 下界（含）
        min: u32,
        /// 上界（含）
        max: u32,
        /// 单次点击步长
        step: u32,
    },
    /// 系统字体族下拉：候选项**运行期**取自系统字体列表（`TextSystem::all_font_names`），
    /// 故不是 `Select`（后者的候选是编译期白名单）+ 首项「系统默认」（值 = 空串）
    FontSelect,
    /// 列顺序/显隐（[`crate::ui::views::settings_render::columns_section`]）
    ColumnsBlock,
    /// 快捷键块：14 行动作（查看/录制/禁用/恢复默认；T-KEYS
    /// [`crate::ui::views::settings_render::keys_section`]）。
    ///
    /// 与 [`Self::ColumnsBlock`] 同形：**一项一页块**，行集合来自
    /// [`crate::ui::shell::keymap::ACTIONS`]（不是本枚举携带，避免 app 层
    /// 依赖 ui 层的动作清单）。
    KeysBlock,
}

impl WidgetKind {
    /// 取值类型是否匹配（防跨类型串用：不匹配即忽略，不 panic）。
    ///
    /// `Select` 除 `Index` 外还接受 `Text`（config 落盘值字符串）——菜单
    /// 入口上行值字符串而非下标（白名单重排不错位），各 Select 项的
    /// `store` 以 `index_of_value_ci` 归一（双归一：面板 Index / 菜单 Text）。
    /// 不认识 Text 值的 Select 项在其 store 内 `_ => return` 静默忽略。
    pub fn accepts(self, value: &SettingValue) -> bool {
        matches!(
            (self, value),
            (Self::Switch, SettingValue::Bool(_))
                | (
                    Self::Select { .. },
                    SettingValue::Index(_) | SettingValue::Text(_)
                )
                | (Self::CheckGroup { .. }, SettingValue::StrSet(_))
                | (Self::ExtGroup, SettingValue::StrSet(_))
                | (Self::Text, SettingValue::Text(_))
                | (Self::Color, SettingValue::Text(_))
                | (Self::Number { .. }, SettingValue::Number(_))
                | (Self::FontSelect, SettingValue::Text(_))
                | (Self::ColumnsBlock, SettingValue::Columns(_))
                | (Self::KeysBlock, SettingValue::Keys(_))
        )
    }
}

/// 生效半场声明（框架持久化之后的分发）。
///
/// **判定标准**：非 `DataOnly` ⇔ 若数据面之外还需动作。数据面动作在
/// `AppData::dispatch_effect` 内；UI 半场动作由主线程泵按
/// `VmSignal::SettingApplied(id)` 分派（需 `&mut App`，app 层不可达）。
/// 故本枚举是**声明面**：泵按 id 分支，本枚举供读者/测试判断该项是否只靠
/// 快照刷新即可生效。
///
/// **不走本枚举的项**（如列设置）：值随 [`crate::ui::contracts::AppStateUiSnapshot`]
/// 进视图，改完即随下一次快照刷新生效——声明 `DataOnly` 即准确。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// 仅数据面：主窗口随 `SettingApplied` 信号刷新快照（无额外副作用）
    DataOnly,
    /// 主题：UI 半场必须调 `theme::apply_theme_replay`（全局 Theme，快照不携带）
    Theme,
    /// 语言：UI 半场必须重设 locale（rust-i18n 全局，快照不携带）
    Locale,
    /// 延后项（默认格式/级别）：只更新压缩对话框模板；不逐帧刷新（守 T-SET1 §1）
    Deferred,
    /// 文件关联：注册表写在主控后台线程（勾选集合数据面已落盘）
    BackgroundAssoc,
    /// 布局密度：间距刻度是**进程级**原子量（布局构建期没有 `App`），
    /// 泵须写全局并请求重绘
    Density,
    /// 文件类型图标来源：`file_icon` 缓存/worker 是跨渲染帧的进程级态，
    /// 泵须按 `SettingId::FileTypeIcons` 调
    /// [`crate::ui::widgets::file_icon::set_enabled`]（关闭 ⇒ 清缓存 + 停探测）
    FileIconSource,
}

/// 提交模式（与 [`Effect`] 正交：`Effect` 管「生效半场」，`Commit` 管「何时落盘」）。
///
/// 默认 [`Commit::Immediate`] 是既有语义：控件一变就落盘并分派 Effect。
/// [`Commit::Manual`] 用于**写操作系统**的项（右键菜单）——勾选只改内存草稿，
/// 由页面「应用」按钮显式提交，避免每点一次勾选就动注册表/文件系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    /// 控件变更即落盘 + 分派 Effect（除右键菜单外的全部设置项）
    Immediate,
    /// 草稿态：变更只改内存草稿；由页面动作显式提交（`AppData::commit_ctx_draft`）
    Manual,
}

/// 动作按钮类别（无状态；`panel` 侧 `Action` 控件的点击语义）。
#[derive(Debug, Clone)]
pub enum ActionKind {
    /// 直接上行既有事件（应用/解除关联）
    Emit(DialogEvent),
    /// 置空该项文本值（清空路径 → 系统默认）
    Clear(SettingId),
    /// 弹文件选择器 → 选中后写入该项文本值（取消 = 无动作）
    FilePicker {
        /// 目标设置项（须为 `Text` 控件项）
        target: SettingId,
    },
    /// 打开（只读展示：日志目录 / 运行时配置文件）——用于「我配了却找不到」的场景。
    Open(OpenTarget),
}

/// [`ActionKind::Open`] 的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTarget {
    /// 当前进程**实际**在写的日志目录（`hzip_log::effective_log_dir`）
    LogDir,
    /// 运行时配置文件 `{config_dir}/config.toml`（改日志目录等要改的那份）
    ConfigFile,
}

/// 动作按钮描述（**非设置项**：不进 REGISTRY，不参与重置/持久化）。
#[derive(Debug)]
pub struct ActionDef {
    /// gpui 元素 id（须窗口内唯一；**不本地化**——元素身份须跨语言稳定）
    pub key: &'static str,
    /// 所属分类页
    pub page: SettingsTab,
    /// 按钮文案 **i18n key**（渲染处 `crate::tr` 查表；zh-CN 值 = 原中文）
    pub label: &'static str,
    /// 动作语义
    pub kind: ActionKind,
}

/// 全量动作按钮（渲染器按 `page` 过滤；顺序 = 页内顺序）。
pub const ACTIONS: &[ActionDef] = &[
    ActionDef {
        key: "set-log-dir-open",
        page: SettingsTab::General,
        label: "action.open_log_dir",
        kind: ActionKind::Open(OpenTarget::LogDir),
    },
    ActionDef {
        key: "set-log-dir-clear",
        page: SettingsTab::General,
        label: "action.reset_default",
        kind: ActionKind::Clear(SettingId::LogDir),
    },
    ActionDef {
        key: "set-config-file-open",
        page: SettingsTab::General,
        label: "action.open_config",
        kind: ActionKind::Open(OpenTarget::ConfigFile),
    },
    ActionDef {
        key: "set-check-update",
        page: SettingsTab::General,
        label: "action.check_update",
        kind: ActionKind::Emit(DialogEvent::SettingsCheckUpdate),
    },
    // 「恢复默认配置」：原设置 Dialog 的底部按钮（2026-09-23 设置改页签后，
    // 弹层 footer 随之下线，功能挪到常规页底部——用户口径「对齐原来 Dialog 上的
    // 恢复默认按钮」）。事件走 AppData 五步流程（注册表驱动全量重置 + 落盘 +
    // 草稿清空），成功提示见 `settings_render::action_toast`。
    ActionDef {
        key: "set-reset-defaults",
        page: SettingsTab::General,
        label: "setting.panel.reset",
        kind: ActionKind::Emit(DialogEvent::SettingsReset),
    },
    ActionDef {
        key: "set-assoc-apply",
        page: SettingsTab::Assoc,
        label: "action.assoc_apply",
        kind: ActionKind::Emit(DialogEvent::SettingsApplyAssoc),
    },
    ActionDef {
        key: "set-assoc-remove",
        page: SettingsTab::Assoc,
        label: "action.assoc_remove",
        kind: ActionKind::Emit(DialogEvent::SettingsRemoveAssoc),
    },
    ActionDef {
        key: "set-ctx-apply",
        page: SettingsTab::ContextMenu,
        label: "action.ctx_apply",
        kind: ActionKind::Emit(DialogEvent::SettingsContextMenuApply),
    },
    ActionDef {
        key: "set-ctx-clear",
        page: SettingsTab::ContextMenu,
        label: "action.ctx_clear",
        kind: ActionKind::Emit(DialogEvent::SettingsContextMenuClear),
    },
    ActionDef {
        key: "set-image-viewer-browse",
        page: SettingsTab::Viewer,
        label: "action.browse",
        kind: ActionKind::FilePicker {
            target: SettingId::ImageViewer,
        },
    },
    ActionDef {
        key: "set-image-viewer-clear",
        page: SettingsTab::Viewer,
        label: "action.reset_default",
        kind: ActionKind::Clear(SettingId::ImageViewer),
    },
    ActionDef {
        key: "set-editor-browse",
        page: SettingsTab::Viewer,
        label: "action.browse",
        kind: ActionKind::FilePicker {
            target: SettingId::Editor,
        },
    },
    ActionDef {
        key: "set-editor-clear",
        page: SettingsTab::Viewer,
        label: "action.reset_default",
        kind: ActionKind::Clear(SettingId::Editor),
    },
];

// —— 描述符 ——

/// 设置项描述符（REGISTRY 行）。
///
/// `load`/`store`/`default` 是函数指针而非闭包：无可变状态、可 const 化、
/// 无生命周期参数（README §2 选型）。
pub struct SettingDef {
    /// 项标识
    pub id: SettingId,
    /// 所属分类页
    pub page: SettingsTab,
    /// 行文案 **i18n key**（本地化展示文案由渲染处 `crate::tr` 求值）
    pub label: &'static str,
    /// 帮助文案 **i18n key**（行尾 ？ 悬浮显示；None = 该项无需帮助——
    /// 走查 #8：显而易见的项不加 ？，降低设置页信息密度）
    pub help: Option<&'static str>,
    /// 控件种类
    pub widget: WidgetKind,
    /// 出厂默认（T-SET8 重置用）
    pub default: fn() -> SettingValue,
    /// 活读：当前值（AppData 镜像优先，Config 兜底）
    pub load: fn(&AppData, &Config) -> SettingValue,
    /// 落库：normalize + 写 Config 节 + 写 SettingsPrefs 镜像
    pub store: fn(&mut Config, &mut AppData, &SettingValue),
    /// 生效半场声明
    pub effect: Effect,
    /// 提交模式（何时落盘；见 [`Commit`]）
    pub commit: Commit,
}

/// Select 白名单 label **key** 序列（下标 = `labels::*` value 下标）。
const THEME_LABEL_KEYS: [&str; 3] = labels::label_keys(labels::THEME);
const LANG_LABEL_KEYS: [&str; 3] = labels::label_keys(labels::LANG);
/// 格式下拉只露**单格式**（组合格式界面屏蔽，见 `labels::UI_FORMAT`）。
const FORMAT_LABEL_KEYS: [&str; 7] = labels::label_keys(labels::UI_FORMAT);
const LEVEL_LABEL_KEYS: [&str; 6] = labels::label_keys(labels::LEVEL);
const CONFLICT_LABEL_KEYS: [&str; 5] = labels::label_keys(labels::CONFLICT);
const CODEPAGE_LABEL_KEYS: [&str; 7] = labels::label_keys(labels::CODEPAGE);
const TOOLBAR_STYLE_LABEL_KEYS: [&str; 3] = labels::label_keys(labels::TOOLBAR_STYLE);
const VIEW_STYLE_LABEL_KEYS: [&str; 3] = labels::label_keys(labels::VIEW_STYLE);
const DENSITY_LABEL_KEYS: [&str; 3] = labels::label_keys(labels::DENSITY);
const FILE_ICONS_LABEL_KEYS: [&str; 2] = labels::label_keys(labels::FILE_ICONS);

/// 全量设置项（顺序 = 面板渲染顺序 = [`index`] 下标；按页分组）。
pub const REGISTRY: &[SettingDef] = &[
    // —— 常规 ——
    SettingDef {
        id: SettingId::Theme,
        page: SettingsTab::General,
        label: "setting.theme.label",
        help: None,
        widget: WidgetKind::Select {
            label_keys: &THEME_LABEL_KEYS,
        },
        default: default_index_zero,
        load: load_theme,
        store: store_theme,
        effect: Effect::Theme,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ThemeColor,
        page: SettingsTab::General,
        label: "setting.theme_color.label",
        help: Some("setting.theme_color.help"),
        widget: WidgetKind::Color,
        default: default_text_empty,
        load: load_theme_color,
        store: store_theme_color,
        // 与 Theme 同一 UI 半场：泵重放全局 Theme（含主题色覆盖注入）
        effect: Effect::Theme,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::FontFamily,
        page: SettingsTab::General,
        label: "setting.font_family.label",
        help: Some("setting.font_family.help"),
        widget: WidgetKind::FontSelect,
        default: default_text_empty,
        load: load_font_family,
        store: store_font_family,
        // 与 Theme 同一 UI 半场：字体族经全局 Theme 的 `typography.sans` 下发
        // （gpui-component 的 `Root` 把它挂到整棵树 ⇒ 一处改全界面）
        effect: Effect::Theme,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::FontSize,
        page: SettingsTab::General,
        label: "setting.font_size.label",
        help: Some("setting.font_size.help"),
        widget: WidgetKind::Number {
            min: hzip_config::FONT_SIZE_MIN,
            max: hzip_config::FONT_SIZE_MAX,
            step: 1,
        },
        default: default_font_size,
        load: load_font_size,
        store: store_font_size,
        // 与 Theme 同一 UI 半场：字号经全局 Theme 的 `font.size` 下发——
        // 每个窗口的 `Root` 渲染帧把它设为该窗口的 rem 基准（root.rs）
        effect: Effect::Theme,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::CornerRadius,
        page: SettingsTab::General,
        label: "setting.corner_radius.label",
        help: Some("setting.corner_radius.help"),
        widget: WidgetKind::Number {
            min: hzip_config::CORNER_RADIUS_MIN,
            max: hzip_config::CORNER_RADIUS_MAX,
            step: 1,
        },
        default: default_corner_radius,
        load: load_corner_radius,
        store: store_corner_radius,
        // 与主题/字号同一 UI 半场：圆角装在全局 `Theme` 的 `radius` 上（kit 的
        // 菜单/按钮/输入框/弹层容器都取它）⇒ 必须重放主题 + 重绘。
        effect: Effect::Theme,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::Density,
        page: SettingsTab::General,
        label: "setting.density.label",
        help: Some("setting.density.help"),
        widget: WidgetKind::Select {
            label_keys: &DENSITY_LABEL_KEYS,
        },
        default: default_density,
        load: load_density,
        store: store_density,
        // 间距刻度是进程级原子量（布局期无 `App`）——须 UI 半场写全局
        effect: Effect::Density,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ShowDotdot,
        page: SettingsTab::General,
        label: "setting.show_dotdot.label",
        help: Some("setting.show_dotdot.help"),
        widget: WidgetKind::Switch,
        default: default_bool_true,
        load: load_show_dotdot,
        store: store_show_dotdot,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::AutoUpdate,
        page: SettingsTab::General,
        label: "setting.auto_update.label",
        help: Some("setting.auto_update.help"),
        widget: WidgetKind::Switch,
        default: default_bool_false,
        load: load_auto_update,
        store: store_auto_update,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::LogDir,
        page: SettingsTab::General,
        label: "setting.log_dir.label",
        help: Some("setting.log_dir.help"),
        widget: WidgetKind::Text,
        default: default_text_empty,
        load: load_log_dir,
        store: store_log_dir,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    // —— 文件关联 ——
    SettingDef {
        id: SettingId::Assoc,
        page: SettingsTab::Assoc,
        label: "setting.assoc.label",
        help: Some("setting.assoc.help"),
        widget: WidgetKind::ExtGroup,
        default: default_set_empty,
        load: load_assoc,
        store: store_assoc,
        effect: Effect::BackgroundAssoc,
        commit: Commit::Immediate,
    },
    // —— 上下文菜单 ——
    SettingDef {
        id: SettingId::CtxExtract,
        page: SettingsTab::ContextMenu,
        label: "setting.ctx_extract.label",
        help: Some("setting.ctx_extract.help"),
        widget: WidgetKind::CheckGroup {
            keys: &CTX_EXTRACT_KEYS,
        },
        default: default_set_empty,
        load: load_ctx_extract,
        store: store_ctx_extract,
        effect: Effect::BackgroundAssoc,
        // 手动提交：勾选只改草稿；点「应用」才落盘 + 写操作系统
        commit: Commit::Manual,
    },
    SettingDef {
        id: SettingId::CtxCompress,
        page: SettingsTab::ContextMenu,
        label: "setting.ctx_compress.label",
        help: Some("setting.ctx_compress.help"),
        widget: WidgetKind::CheckGroup {
            keys: &CTX_COMPRESS_KEYS,
        },
        default: default_set_empty,
        load: load_ctx_compress,
        store: store_ctx_compress,
        effect: Effect::BackgroundAssoc,
        // 手动提交（同 CtxExtract）
        commit: Commit::Manual,
    },
    // —— 解压设置 ——
    SettingDef {
        id: SettingId::ExtractSmart,
        page: SettingsTab::Extract,
        label: "setting.extract_smart.label",
        help: Some("setting.extract_smart.help"),
        widget: WidgetKind::Switch,
        default: default_bool_false,
        load: load_extract_smart,
        store: store_extract_smart,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ExtractConflict,
        page: SettingsTab::Extract,
        label: "setting.extract_conflict.label",
        help: Some("setting.extract_conflict.help"),
        widget: WidgetKind::Select {
            label_keys: &CONFLICT_LABEL_KEYS,
        },
        default: default_index_zero,
        load: load_extract_conflict,
        store: store_extract_conflict,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ExtractBestEffort,
        page: SettingsTab::Extract,
        label: "setting.extract_best_effort.label",
        help: Some("setting.extract_best_effort.help"),
        widget: WidgetKind::Switch,
        default: default_bool_false,
        load: load_extract_best_effort,
        store: store_extract_best_effort,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ExtractCodepage,
        page: SettingsTab::Extract,
        label: "setting.extract_codepage.label",
        help: Some("setting.extract_codepage.help"),
        widget: WidgetKind::Select {
            label_keys: &CODEPAGE_LABEL_KEYS,
        },
        default: default_index_zero,
        load: load_extract_codepage,
        store: store_extract_codepage,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::NotifyOnFinish,
        page: SettingsTab::Extract,
        label: "setting.notify_on_finish.label",
        help: None,
        widget: WidgetKind::Switch,
        default: default_bool_false,
        load: load_notify_on_finish,
        store: store_notify_on_finish,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ProgressWindow,
        page: SettingsTab::Extract,
        label: "setting.progress_window.label",
        help: None,
        widget: WidgetKind::Switch,
        default: default_bool_true,
        load: load_progress_window,
        store: store_progress_window,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ExitOnFinish,
        page: SettingsTab::Extract,
        label: "setting.exit_on_finish.label",
        help: None,
        widget: WidgetKind::Switch,
        default: default_bool_false,
        load: load_exit_on_finish,
        store: store_exit_on_finish,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    // —— 压缩设置 ——
    SettingDef {
        id: SettingId::Format,
        page: SettingsTab::Compress,
        label: "setting.format.label",
        help: None,
        widget: WidgetKind::Select {
            label_keys: &FORMAT_LABEL_KEYS,
        },
        default: default_index_zero,
        load: load_format,
        store: store_format,
        effect: Effect::Deferred,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::Level,
        page: SettingsTab::Compress,
        label: "setting.level.label",
        help: None,
        widget: WidgetKind::Select {
            label_keys: &LEVEL_LABEL_KEYS,
        },
        default: default_index_three,
        load: load_level,
        store: store_level,
        effect: Effect::Deferred,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::KeepMetadata,
        page: SettingsTab::Compress,
        label: "setting.keep_metadata.label",
        help: Some("setting.keep_metadata.help"),
        widget: WidgetKind::Switch,
        default: default_bool_true,
        load: load_keep_metadata,
        store: store_keep_metadata,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::VerifyAfter,
        page: SettingsTab::Compress,
        label: "setting.verify_after.label",
        help: None,
        widget: WidgetKind::Switch,
        default: default_bool_true,
        load: load_verify_after,
        store: store_verify_after,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    // —— 查看器 ——
    SettingDef {
        id: SettingId::FolderSize,
        page: SettingsTab::Viewer,
        label: "setting.folder_size.label",
        help: Some("setting.folder_size.help"),
        widget: WidgetKind::Switch,
        default: default_bool_false,
        load: load_folder_size,
        store: store_folder_size,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ImageViewer,
        page: SettingsTab::Viewer,
        label: "setting.image_viewer.label",
        help: Some("setting.image_viewer.help"),
        widget: WidgetKind::Text,
        default: default_text_empty,
        load: load_image_viewer,
        store: store_image_viewer,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::Editor,
        page: SettingsTab::Viewer,
        label: "setting.editor.label",
        help: Some("setting.editor.help"),
        widget: WidgetKind::Text,
        default: default_text_empty,
        load: load_editor,
        store: store_editor,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::Columns,
        page: SettingsTab::Viewer,
        label: "setting.columns.label",
        help: Some("setting.columns.help"),
        widget: WidgetKind::ColumnsBlock,
        default: default_columns,
        load: load_columns,
        store: store_columns,
        // 值随快照进浏览页（AppStateUiSnapshot.columns）——改完即随下一次
        // 快照刷新生效，无需额外 UI 半场动作
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    // —— 视图偏好（T-MENU5：菜单与设置面板双入口）——
    SettingDef {
        id: SettingId::ShowTree,
        page: SettingsTab::Viewer,
        label: "setting.show_tree.label",
        help: None,
        widget: WidgetKind::Switch,
        default: default_bool_true,
        load: load_show_tree,
        store: store_show_tree,
        // 值随快照进浏览页（AppStateUiSnapshot.show_tree）——DataOnly
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ShowJobs,
        page: SettingsTab::Viewer,
        label: "setting.show_jobs.label",
        help: Some("setting.show_jobs.help"),
        widget: WidgetKind::Switch,
        default: default_bool_false,
        load: load_show_jobs,
        store: store_show_jobs,
        // 值随快照进主窗口（AppStateUiSnapshot.show_jobs → 页签栏/内容分支）——DataOnly
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ToolbarStyle,
        page: SettingsTab::Viewer,
        label: "setting.toolbar_style.label",
        help: None,
        widget: WidgetKind::Select {
            label_keys: &TOOLBAR_STYLE_LABEL_KEYS,
        },
        default: default_index_zero,
        load: load_toolbar_style,
        store: store_toolbar_style,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::ViewStyle,
        page: SettingsTab::Viewer,
        label: "setting.view_style.label",
        help: None,
        widget: WidgetKind::Select {
            label_keys: &VIEW_STYLE_LABEL_KEYS,
        },
        default: default_index_zero,
        load: load_view_style,
        store: store_view_style,
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
    SettingDef {
        id: SettingId::FileTypeIcons,
        page: SettingsTab::Viewer,
        label: "setting.file_icons.label",
        help: Some("setting.file_icons.help"),
        widget: WidgetKind::Select {
            label_keys: &FILE_ICONS_LABEL_KEYS,
        },
        default: default_index_zero,
        load: load_file_type_icons,
        store: store_file_type_icons,
        // `file_icon` 的缓存/worker 是进程级态（元素构建期没有 `App`）——
        // 泵须按 id 调 `file_icon::set_enabled`，`DataOnly` 不够
        effect: Effect::FileIconSource,
        commit: Commit::Immediate,
    },
    // —— 语言 ——
    SettingDef {
        id: SettingId::Lang,
        page: SettingsTab::Language,
        label: "setting.lang.label",
        help: Some("setting.lang.help"),
        widget: WidgetKind::Select {
            label_keys: &LANG_LABEL_KEYS,
        },
        default: default_index_zero,
        load: load_lang,
        store: store_lang,
        // rust-i18n locale 是全局态（快照不携带）→ UI 半场需重设
        effect: Effect::Locale,
        commit: Commit::Immediate,
    },
    // —— 快捷键（T-KEYS）——
    SettingDef {
        id: SettingId::Keymap,
        page: SettingsTab::Keys,
        label: "setting.keymap.label",
        help: Some("setting.keymap.help"),
        widget: WidgetKind::KeysBlock,
        default: default_keys,
        load: load_keys,
        store: store_keys,
        // 数据面 = 写 `[keys]` + 镜像；**生效半场**是 `App` 级 keymap（全局态，
        // 快照不携带）⇒ 泵按 `SettingId::Keymap` 取 `settings.keys` 镜像后调
        // `keymap::install`（同 `theme_replay_of` 口径：取镜像不读盘，避免与
        // 异步落盘赛跑）。
        // 声明 `DataOnly` 是准确的：本枚举只声明「除数据面外**本层**还要做什么」，
        // 重装绑定属 UI 半场（与列设置/开关同款口径）。**不新增 Effect 变体**。
        effect: Effect::DataOnly,
        commit: Commit::Immediate,
    },
];

/// 设置项 → REGISTRY 下标（穷举匹配：新增变体漏登记 = 编译错误）。
pub const fn index(id: SettingId) -> usize {
    match id {
        SettingId::Theme => 0,
        SettingId::ThemeColor => 1,
        SettingId::FontFamily => 2,
        SettingId::FontSize => 3,
        SettingId::CornerRadius => 4,
        SettingId::Density => 5,
        SettingId::ShowDotdot => 6,
        SettingId::AutoUpdate => 7,
        SettingId::LogDir => 8,
        SettingId::Assoc => 9,
        SettingId::CtxExtract => 10,
        SettingId::CtxCompress => 11,
        SettingId::ExtractSmart => 12,
        SettingId::ExtractConflict => 13,
        SettingId::ExtractBestEffort => 14,
        SettingId::ExtractCodepage => 15,
        SettingId::NotifyOnFinish => 16,
        SettingId::ProgressWindow => 17,
        SettingId::ExitOnFinish => 18,
        SettingId::Format => 19,
        SettingId::Level => 20,
        SettingId::KeepMetadata => 21,
        SettingId::VerifyAfter => 22,
        SettingId::FolderSize => 23,
        SettingId::ImageViewer => 24,
        SettingId::Editor => 25,
        SettingId::Columns => 26,
        SettingId::ShowTree => 27,
        SettingId::ShowJobs => 28,
        SettingId::ToolbarStyle => 29,
        SettingId::ViewStyle => 30,
        SettingId::FileTypeIcons => 31,
        SettingId::Lang => 32,
        SettingId::Keymap => 33,
    }
}

/// 描述符查询（下标由 [`index`] 给出；REGISTRY 顺序互锁由单测把守）。
pub fn def(id: SettingId) -> &'static SettingDef {
    &REGISTRY[index(id)]
}

// —— 出厂默认（重置单源；与 hzip-config B.1 默认互锁单测） ——

fn default_index_zero() -> SettingValue {
    SettingValue::Index(0)
}
fn default_index_three() -> SettingValue {
    SettingValue::Index(3)
}
fn default_bool_true() -> SettingValue {
    SettingValue::Bool(true)
}
fn default_bool_false() -> SettingValue {
    SettingValue::Bool(false)
}
fn default_text_empty() -> SettingValue {
    SettingValue::Text(String::new())
}
fn default_set_empty() -> SettingValue {
    SettingValue::StrSet(Vec::new())
}
fn default_columns() -> SettingValue {
    SettingValue::Columns(ColumnsConfig::default())
}
fn default_font_size() -> SettingValue {
    SettingValue::Number(hzip_config::FONT_SIZE_DEFAULT)
}
fn default_corner_radius() -> SettingValue {
    SettingValue::Number(hzip_config::CORNER_RADIUS_DEFAULT)
}
fn default_density() -> SettingValue {
    SettingValue::Index(Density::Standard.index())
}
/// 快捷键出厂默认 = **全空串**（`Keys::default()`）。
///
/// 「空串 = 用内置默认键位」是配置层契约（T-KEYS §2 三态）：默认态不在配置里
/// 写死那 14 个键位串，而是让 `keymap::ACTIONS` 的 `default_spec` 当唯一事实源
/// ⇒ 改默认键位只需改一处，且「恢复默认」等于写空配置（没有第二份键位表）。
fn default_keys() -> SettingValue {
    SettingValue::Keys(Box::default())
}

// —— 活读视图 ——

/// 设置活读视图（每项一值；顺序 = REGISTRY）。
///
/// 面板逐帧回读 / 主窗口快照刷新共用；`assoc_candidates` 每次现算不入值。
#[derive(Debug, Clone)]
pub struct SettingsView {
    values: Vec<SettingValue>,
    assoc_candidates: Vec<String>,
    /// 右键菜单在操作系统上的当前安装状态（设置页状态行）。
    ///
    /// 由调用方**在 AppData 锁外**查询后传入（平台 `backend().status()` 可能
    /// 走注册表/文件系统，macOS 上还会起子进程——不放进锁内、不放进渲染帧）。
    ctx_status: Option<crate::app::state::CtxMenuStatus>,
    /// 是否存在未提交的右键菜单草稿（面板据此显示「有未应用的更改」）。
    ctx_dirty: bool,
    /// 已勾选格式的**双击实况**（扩展名 → 系统当前实际用哪个 ProgId 打开）。
    ///
    /// 与 `ctx_status` 同为**锁外查询后传入**：每项一次注册表读，不进渲染帧。
    /// 状态行据此把「双击到底归谁」如实告诉用户（`FileExts` 的 UserChoice 是
    /// 哈希保护的，我们写了注册表也可能不生效——不能靠「我们写过」自证成功）。
    assoc_states: Vec<(String, hzip_platform::assoc::ExtAssocState)>,
    /// 关联系统侧最后一次失败的原因（None = 无失败）。
    assoc_error: Option<String>,
}

impl SettingsView {
    /// 右键菜单安装状态（`None` = 未查询）。
    pub fn ctx_status(&self) -> Option<&crate::app::state::CtxMenuStatus> {
        self.ctx_status.as_ref()
    }

    /// 是否有未应用的右键菜单改动。
    pub fn has_pending_ctx_changes(&self) -> bool {
        self.ctx_dirty
    }

    /// 已勾选格式的双击实况（关联页状态行）。
    pub fn assoc_states(&self) -> &[(String, hzip_platform::assoc::ExtAssocState)] {
        &self.assoc_states
    }

    /// 关联系统侧最后一次失败的原因。
    pub fn assoc_error(&self) -> Option<&str> {
        self.assoc_error.as_deref()
    }
}

impl SettingsView {
    /// 活读装配（AppData 镜像 + Config 两半；候选集由调用方现算）。
    pub fn load(
        app: &AppData,
        cfg: &Config,
        assoc_candidates: Vec<String>,
        ctx_status: Option<crate::app::state::CtxMenuStatus>,
        assoc_states: Vec<(String, hzip_platform::assoc::ExtAssocState)>,
    ) -> Self {
        Self {
            values: REGISTRY.iter().map(|d| (d.load)(app, cfg)).collect(),
            assoc_candidates,
            ctx_status,
            ctx_dirty: app.ctx_draft.is_some(),
            assoc_states,
            assoc_error: app.assoc_error.clone(),
        }
    }

    /// 全默认视图（锁中毒/无 AppData 时的守界回落）。
    pub fn defaults(assoc_candidates: Vec<String>) -> Self {
        Self {
            values: REGISTRY.iter().map(|d| (d.default)()).collect(),
            assoc_candidates,
            // 守界路径不查平台（锁中毒时更不该再去动注册表）
            ctx_status: None,
            ctx_dirty: false,
            assoc_states: Vec::new(),
            assoc_error: None,
        }
    }

    /// 单项值（缺项 → None）。
    pub fn value(&self, id: SettingId) -> Option<&SettingValue> {
        self.values.get(index(id))
    }

    /// Switch 项当前值（缺项 → false）。
    pub fn bool_of(&self, id: SettingId) -> bool {
        matches!(self.value(id), Some(SettingValue::Bool(true)))
    }

    /// Select 项当前下标（缺项 → 0）。
    pub fn index_of(&self, id: SettingId) -> usize {
        match self.value(id) {
            Some(SettingValue::Index(ix)) => *ix,
            _ => 0,
        }
    }

    /// Text 项当前值（缺项 → 空 = 系统默认）。
    pub fn text_of(&self, id: SettingId) -> &str {
        match self.value(id) {
            Some(SettingValue::Text(s)) => s.as_str(),
            _ => "",
        }
    }

    /// 数值项当前值（缺项 → 下界兜底 = 0；步进控件另有 min/max 归一）。
    pub fn number_of(&self, id: SettingId) -> u32 {
        match self.value(id) {
            Some(SettingValue::Number(v)) => *v,
            _ => 0,
        }
    }

    /// 勾选集合项当前值（缺项 → 空集）。
    pub fn set_of(&self, id: SettingId) -> &[String] {
        match self.value(id) {
            Some(SettingValue::StrSet(v)) => v.as_slice(),
            _ => &[],
        }
    }

    /// 列设置项当前值（缺项 → None）。
    pub fn columns_of(&self, id: SettingId) -> Option<&ColumnsConfig> {
        match self.value(id) {
            Some(SettingValue::Columns(c)) => Some(c),
            _ => None,
        }
    }

    /// 快捷键项当前值（缺项 → None）。设置页「快捷键」页逐行取值用。
    pub fn keys_of(&self, id: SettingId) -> Option<&hzip_config::Keys> {
        match self.value(id) {
            Some(SettingValue::Keys(k)) => Some(k.as_ref()),
            _ => None,
        }
    }

    /// 文件关联候选扩展名（Engine 能力派生；每次打开现算）。
    pub fn assoc_candidates(&self) -> &[String] {
        &self.assoc_candidates
    }
}

/// 恢复默认（注册表驱动全量重置）：逐项 `store(default)`。
///
/// 只写本表登记的项——`[security]`/`[logging]`/`extract.delete_after`/
/// `compress.solid_block_mib` 等非设置项保持原值（R2-03 §恢复默认语义）。
pub fn apply_defaults(cfg: &mut Config, app: &mut AppData) {
    for def in REGISTRY {
        (def.store)(cfg, app, &(def.default)());
    }
}

// —— 归一化纯函数（UI 与 store 共用） ——

/// 下标归一化：越界回落 0（沿用 `labels::index_of_value` 向后兼容语义）。
fn norm_ix(ix: usize, len: usize) -> usize {
    if ix < len { ix } else { 0 }
}

/// 值 → 下标（ASCII 大小写不敏感；未知回落 0——宽容旧配置/手改配置）。
///
/// 与 [`crate::app::state::AppData::load_settings`] 共用（读方向同策略）。
pub(crate) fn index_of_value_ci(table: &[labels::Pair], value: &str) -> usize {
    table
        .iter()
        .position(|(_, v)| v.eq_ignore_ascii_case(value))
        .unwrap_or(0)
}

/// 白名单序交集：非白名单 key 丢弃，输出按白名单顺序（含 key 去重）。
pub fn whitelist_order(keys: &[(&'static str, &'static str)], checked: &[String]) -> Vec<String> {
    keys.iter()
        .map(|(k, _)| *k)
        .filter(|k| checked.iter().any(|s| s == k))
        .map(|k| k.to_string())
        .collect()
}

/// 白名单勾选切换纯函数：**整组上行**（全量勾选集，白名单顺序）。
pub fn toggle_set(
    keys: &[(&'static str, &'static str)],
    checked: &[String],
    key: &str,
    new_checked: bool,
) -> Vec<String> {
    let mut next: Vec<String> = checked.to_vec();
    if new_checked {
        if !next.iter().any(|s| s == key) {
            next.push(key.to_string());
        }
    } else {
        next.retain(|s| s != key);
    }
    whitelist_order(keys, &next)
}

/// 关联扩展名切换纯函数（整组上行）。
pub fn assoc_toggle(checked: &[String], ext: &str, new_checked: bool) -> Vec<String> {
    let mut next: Vec<String> = checked.to_vec();
    if new_checked {
        if !next.iter().any(|s| s == ext) {
            next.push(ext.to_string());
        }
    } else {
        next.retain(|s| s != ext);
    }
    next
}

/// 关联扩展名归一化：去空 + 去重（保序；不做大小写/前缀改写）。
fn assoc_normalize(set: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in set {
        if !s.is_empty() && !out.iter().any(|e| e == s) {
            out.push(s.clone());
        }
    }
    out
}

/// ColumnsConfig → hzip-config POD（T-G54 双向转换半场）。
pub fn columns_to_pod(config: &ColumnsConfig) -> hzip_config::Columns {
    hzip_config::Columns {
        order: config.order.iter().map(|c| column_key(*c)).collect(),
        hidden: config.hidden.iter().map(|c| column_key(*c)).collect(),
    }
}

/// hzip-config POD → ColumnsConfig（未知名忽略——向后兼容）。
pub fn columns_from_pod(pod: &hzip_config::Columns) -> ColumnsConfig {
    // 键→枚举反查单一来源 = [`column_by_key`]（T-OPT 项12：此处原有一份逐字重复
    // 的 10 臂内联 match——增/改列 key 时两处必须同改，漏一处即静默丢列）
    let mut order: Vec<crate::ui::contracts::BrowseColumnId> =
        pod.order.iter().filter_map(|n| column_by_key(n)).collect();
    let mut missing: Vec<crate::ui::contracts::BrowseColumnId> = Vec::new();
    // 补全缺失列（旧配置/手工编辑防漂移）：保持 ALL 白名单完整
    for col in crate::ui::contracts::BrowseColumnId::ALL {
        if !order.contains(&col) {
            order.push(col);
            missing.push(col);
        }
    }
    let mut hidden: Vec<crate::ui::contracts::BrowseColumnId> = pod
        .hidden
        .iter()
        .filter_map(|n| column_by_key(n))
        .filter(|c| *c != crate::ui::contracts::BrowseColumnId::Name) // 名称列强制显示
        .collect();
    // 老配置升级：本次新增的列（此前版本不存在）默认隐藏——与新出厂默认一致
    //（用户口径「这几个列数据默认不显示」不能被旧配置文件绕过）。其余缺失列
    // 维持旧行为（补序不隐藏）；Name 永不入 hidden。
    for col in missing {
        if crate::ui::contracts::BrowseColumnId::DEFAULT_HIDDEN.contains(&col)
            && !hidden.contains(&col)
        {
            hidden.push(col);
        }
    }
    ColumnsConfig { order, hidden }
}

/// 列枚举 → 配置键名（单一事实来源在 [`BrowseColumnId::key`]）。
///
/// 契约键（写 `[ui.columns].order/hidden`、`columns_from_pod` 回读）**永不本地化**——
/// 委托给 `key()` 而非 `label()`（后者是给人看的、随语言变化）。
fn column_key(col: crate::ui::contracts::BrowseColumnId) -> String {
    col.key().to_string()
}

/// 契约键 → 列枚举（[`crate::ui::contracts::BrowseColumnId::key`] 的反查）。
///
/// 枚举只有「枚举→键」的 `key()`，没有反查，故在此按 [`BrowseColumnId::ALL`]
/// 线性查（10 项，调用发生在一次点击/一次勾选，不入热路径）。未知键回 `None`，
/// 由调用方决定宽容策略——本文件对未知键一律原样返回。
fn column_by_key(key: &str) -> Option<crate::ui::contracts::BrowseColumnId> {
    crate::ui::contracts::BrowseColumnId::ALL
        .iter()
        .copied()
        .find(|c| c.key() == key)
}

/// 切换一列的显隐（工具栏「查看」下拉与设置页列段**同一口径**）。
///
/// - `key` 为 [`crate::ui::contracts::BrowseColumnId::key`]（契约键，永不本地化）；
///   未知 key → 原样返回；
/// - **名称列不可隐藏**（[`ColumnsConfig::visible`] 本就强制保留 Name，这里前置
///   拒绝，避免配置里留下一个「隐藏但不生效」的假状态）；
/// - 幂等：对同一 key 连续调用两次回到原配置；
/// - 不动 `order`（列序归拖拽/设置页顺序段）。
pub fn toggle_column(config: &ColumnsConfig, key: &str) -> ColumnsConfig {
    use crate::ui::contracts::BrowseColumnId;
    // 未知键 / 名称列：原样返回（克隆入参，调用方拿到的仍是独立值）
    match column_by_key(key) {
        Some(col) if col != BrowseColumnId::Name => {
            let mut next = config.clone();
            if next.hidden.contains(&col) {
                next.hidden.retain(|c| *c != col);
            } else {
                next.hidden.push(col);
            }
            next
        }
        _ => config.clone(),
    }
}

// —— load：活读各项当前值 ——

/// ui.* 项读 AppData 镜像（= 面板/主窗口当前态，store 同步维护）；
/// `[extract]`/`[compress]`/`[jobs]` 项读 Config（不入 SettingsPrefs）。
fn load_theme(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Index(app.settings.theme_ix)
}
fn load_lang(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Index(app.settings.lang_ix)
}
fn load_theme_color(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Text(app.settings.theme_color.clone())
}
fn load_font_family(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Text(app.settings.font_family.clone())
}
fn load_font_size(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Number(app.settings.font_size)
}
fn load_corner_radius(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Number(app.settings.corner_radius)
}
fn load_density(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Index(app.settings.density_ix)
}

fn load_show_dotdot(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Bool(app.settings.show_dotdot)
}

fn load_show_tree(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Bool(app.settings.show_tree)
}
fn load_show_jobs(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Bool(app.settings.show_jobs)
}
fn load_toolbar_style(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Index(app.settings.toolbar_style_ix)
}
fn load_view_style(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Index(app.settings.view_style_ix)
}
fn load_file_type_icons(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Index(app.settings.file_type_icons_ix)
}
fn load_auto_update(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Bool(app.settings.auto_update)
}
fn load_assoc(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::StrSet(app.settings.assoc.clone())
}
/// 草稿优先投影：有未提交改动时显示草稿，否则显示「已应用」值。
///
/// 这是**活读**而非冻结快照——草稿存在 AppData 里，每次面板 reload 都重新求值，
/// 故不存在「面板显示陈旧值」（T-SET3 不变量）。
fn ctx_set(
    app: &AppData,
    draft: impl Fn(&crate::app::state::CtxMenuDraft) -> &Vec<String>,
    applied: &[String],
) -> SettingValue {
    match app.ctx_draft.as_ref() {
        Some(d) => SettingValue::StrSet(draft(d).clone()),
        None => SettingValue::StrSet(applied.to_vec()),
    }
}

fn load_ctx_extract(app: &AppData, _cfg: &Config) -> SettingValue {
    ctx_set(app, |d| &d.extract, &app.settings.ctx_extract)
}
fn load_ctx_compress(app: &AppData, _cfg: &Config) -> SettingValue {
    ctx_set(app, |d| &d.compress, &app.settings.ctx_compress)
}
fn load_format(app: &AppData, cfg: &Config) -> SettingValue {
    let _ = cfg;
    // `format_ix` 是 UI_FORMAT 下标域（组合格式界面屏蔽；`load_settings`
    // 已保证非法/组合配置值回落 0=zip）。
    SettingValue::Index(app.settings.format_ix)
}
fn load_level(app: &AppData, cfg: &Config) -> SettingValue {
    let _ = cfg;
    SettingValue::Index(app.settings.level_ix)
}
fn load_columns(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Columns(app.settings.columns.clone())
}
/// 快捷键活读：读 **AppData 镜像**而不是 `cfg.keys`（与其余 ui.* 项同口径）。
///
/// 镜像就是「本进程当前生效的那一份」：`store` 已就地更新它，而落盘是异步的
/// （`persist` → `save_async`）——面板每帧回读镜像，于是「刚录完的键位」与
/// 「界面显示」永远同拍，不必与后台写线程赛跑。
fn load_keys(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Keys(Box::new(app.settings.keys.clone()))
}
fn load_folder_size(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Bool(app.settings.show_folder_size)
}
fn load_log_dir(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Text(cfg.logging.dir.clone())
}
fn load_image_viewer(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Text(app.settings.image_viewer.clone())
}
fn load_editor(app: &AppData, _cfg: &Config) -> SettingValue {
    SettingValue::Text(app.settings.editor.clone())
}
fn load_extract_smart(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Bool(cfg.extract.smart)
}
fn load_extract_conflict(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Index(index_of_value_ci(&labels::CONFLICT, &cfg.extract.conflict))
}
fn load_extract_best_effort(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Bool(cfg.extract.best_effort)
}
fn load_extract_codepage(_app: &AppData, cfg: &Config) -> SettingValue {
    let ix = cfg
        .extract
        .codepage
        .as_deref()
        .map(|s| index_of_value_ci(&labels::CODEPAGE, s))
        .unwrap_or(0);
    SettingValue::Index(ix)
}
fn load_notify_on_finish(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Bool(cfg.jobs.notify_on_finish)
}
fn load_progress_window(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Bool(cfg.jobs.progress_window)
}
fn load_exit_on_finish(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Bool(cfg.jobs.exit_on_finish)
}
fn load_keep_metadata(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Bool(cfg.compress.keep_metadata)
}
fn load_verify_after(_app: &AppData, cfg: &Config) -> SettingValue {
    SettingValue::Bool(cfg.compress.verify_after)
}

fn store_theme(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(ix) = v.as_index() else { return };
    let ix = norm_ix(ix, labels::THEME.len());
    let value = labels::value_of(&labels::THEME, ix).to_string();
    cfg.ui.theme = value;
    app.settings.theme_ix = ix;
}

fn store_toolbar_style(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    // 菜单入口上行 Text（config 值），设置面板 Select 上行 Index——双归一
    let ix = match v {
        SettingValue::Index(ix) => norm_ix(*ix, labels::TOOLBAR_STYLE.len()),
        SettingValue::Text(s) => {
            crate::app::settings_registry::index_of_value_ci(&labels::TOOLBAR_STYLE, s)
        }
        _ => return,
    };
    let value = labels::value_of(&labels::TOOLBAR_STYLE, ix).to_string();
    cfg.ui.toolbar_style = value;
    app.settings.toolbar_style_ix = ix;
}

fn store_view_style(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let ix = match v {
        SettingValue::Index(ix) => norm_ix(*ix, labels::VIEW_STYLE.len()),
        SettingValue::Text(s) => {
            crate::app::settings_registry::index_of_value_ci(&labels::VIEW_STYLE, s)
        }
        _ => return,
    };
    let value = labels::value_of(&labels::VIEW_STYLE, ix).to_string();
    cfg.ui.view_style = value;
    app.settings.view_style_ix = ix;
}

/// 文件类型图标来源：白名单归一（大小写不敏感；未知/空 ⇒ 下标 0 = `system`）。
///
/// 面板下拉上行 `Index`、菜单入口上行 `Text`（config 落盘值）——与
/// [`store_toolbar_style`] 同款双归一。
fn store_file_type_icons(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let ix = match v {
        SettingValue::Index(ix) => norm_ix(*ix, labels::FILE_ICONS.len()),
        SettingValue::Text(s) => {
            crate::app::settings_registry::index_of_value_ci(&labels::FILE_ICONS, s)
        }
        _ => return,
    };
    let value = labels::value_of(&labels::FILE_ICONS, ix).to_string();
    cfg.ui.file_type_icons = value;
    app.settings.file_type_icons_ix = ix;
}

fn store_lang(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(ix) = v.as_index() else { return };
    let ix = norm_ix(ix, labels::LANG.len());
    let value = labels::value_of(&labels::LANG, ix).to_string();
    cfg.ui.lang = value;
    app.settings.lang_ix = ix;
}

fn store_show_dotdot(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.ui.show_dotdot = b;
    app.settings.show_dotdot = b;
}

fn store_auto_update(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.ui.auto_update = b;
    app.settings.auto_update = b;
}

fn store_show_tree(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.ui.show_tree = b;
    app.settings.show_tree = b;
}

fn store_show_jobs(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.ui.show_jobs = b;
    app.settings.show_jobs = b;
}

fn store_assoc(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(set) = v.as_str_set() else { return };
    let set = assoc_normalize(set);
    cfg.ui.assoc_formats = set.clone();
    app.settings.assoc = set;
}

fn store_ctx_extract(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(set) = v.as_str_set() else { return };
    let set = whitelist_order(&CTX_EXTRACT_KEYS, set);
    cfg.ui.ctx_extract = set.clone();
    app.settings.ctx_extract = set;
}

fn store_ctx_compress(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(set) = v.as_str_set() else { return };
    let set = whitelist_order(&CTX_COMPRESS_KEYS, set);
    cfg.ui.ctx_compress = set.clone();
    app.settings.ctx_compress = set;
}

fn store_extract_smart(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.extract.smart = b;
}

fn store_extract_conflict(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(ix) = v.as_index() else { return };
    let ix = norm_ix(ix, labels::CONFLICT.len());
    cfg.extract.conflict = labels::value_of(&labels::CONFLICT, ix).to_string();
}

fn store_extract_best_effort(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.extract.best_effort = b;
}

fn store_extract_codepage(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(ix) = v.as_index() else { return };
    let ix = norm_ix(ix, labels::CODEPAGE.len());
    cfg.extract.codepage = if ix == 0 {
        None
    } else {
        Some(labels::value_of(&labels::CODEPAGE, ix).to_string())
    };
}

fn store_notify_on_finish(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.jobs.notify_on_finish = b;
}

fn store_progress_window(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.jobs.progress_window = b;
}

fn store_exit_on_finish(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.jobs.exit_on_finish = b;
}

/// 默认压缩格式（单一事实来源 `[defaults].format`；`[ui].default_format` 是
/// hzip-config 文档约定的 GUI 镜像，同写保持两键一致）。
fn store_format(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(ix) = v.as_index() else { return };
    let ix = norm_ix(ix, labels::UI_FORMAT.len());
    let value = labels::value_of(&labels::UI_FORMAT, ix).to_string();
    cfg.defaults.format = value.clone();
    cfg.ui.default_format = value;
    app.settings.format_ix = ix;
}

/// 默认压缩级别（同上：`[defaults].level` 为准，`[ui].default_level` 镜像）。
fn store_level(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(ix) = v.as_index() else { return };
    let ix = norm_ix(ix, labels::LEVEL.len());
    let value = labels::value_of(&labels::LEVEL, ix).to_string();
    cfg.defaults.level = value.clone();
    cfg.ui.default_level = value;
    app.settings.level_ix = ix;
}

fn store_keep_metadata(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.compress.keep_metadata = b;
}

fn store_verify_after(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.compress.verify_after = b;
}

fn store_folder_size(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(b) = v.as_bool() else { return };
    cfg.ui.show_folder_size = b;
    app.settings.show_folder_size = b;
}

/// 主题色落库（T-SEL 走查 #3）：hex 归一（trim；非 `#RRGGBB` 可解析值
/// 静默丢弃——ColorPicker 上行总是合法 hex 或空串）。
/// 界面字体族：空串 = 主题默认（渲染侧回落 `.SystemUIFont`）。
///
/// 非白名单校验（系统字体列表随机器变化，无法编译期固定）——手改配置写了
/// 不存在的字体族时，gpui 的字体解析回落默认族，不会 panic。
fn store_font_family(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(name) = v.as_text() else {
        return;
    };
    let name = name.trim().to_string();
    cfg.ui.font_family = name.clone();
    app.settings.font_family = name;
}

/// 界面字号 px：控件已按 min/max 归一，此处再兜一次（跨版本/手改上行不越界）。
fn store_font_size(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(px) = v.as_number() else {
        return;
    };
    let px = if (hzip_config::FONT_SIZE_MIN..=hzip_config::FONT_SIZE_MAX).contains(&px) {
        px
    } else {
        hzip_config::FONT_SIZE_DEFAULT
    };
    cfg.ui.font_size = px;
    app.settings.font_size = px;
}

/// 界面圆角 px：控件已按 min/max 归一，此处再兜一次（跨版本/手改上行不越界）。
fn store_corner_radius(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(px) = v.as_number() else {
        return;
    };
    let px = if (hzip_config::CORNER_RADIUS_MIN..=hzip_config::CORNER_RADIUS_MAX).contains(&px) {
        px
    } else {
        hzip_config::CORNER_RADIUS_DEFAULT
    };
    cfg.ui.corner_radius = px;
    app.settings.corner_radius = px;
}

/// 布局密度：只认 `Index`（面板下拉）；`Text`（菜单入口上行）在本项无来源，
/// 按注册表约定静默忽略。
fn store_density(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(ix) = v.as_index() else {
        return;
    };
    let density = Density::from_index(ix);
    cfg.ui.density = density.config_value().to_string();
    app.settings.density_ix = density.index();
}

fn store_theme_color(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(text) = v.as_text() else { return };
    let hex = text.trim().to_string();
    if !hex.is_empty() && crate::ui::theme::theme_color_family(&hex).is_none() {
        return;
    }
    cfg.ui.theme_color = hex.clone();
    app.settings.theme_color = hex;
}
fn store_log_dir(cfg: &mut Config, _app: &mut AppData, v: &SettingValue) {
    let Some(text) = v.as_text() else { return };
    // 只写配置：生效目录在**启动时**落定（hzip-log），改后需重启；
    // 视图经 `load` 活读 config.toml，故不需要 AppData 镜像字段。
    cfg.logging.dir = text.to_string();
}
fn store_image_viewer(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(text) = v.as_text() else { return };
    cfg.ui.image_viewer = text.to_string();
    app.settings.image_viewer = text.to_string();
}

fn store_editor(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(text) = v.as_text() else { return };
    cfg.ui.editor = text.to_string();
    app.settings.editor = text.to_string();
}

fn store_columns(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(cols) = v.as_columns() else { return };
    cfg.ui.columns = columns_to_pod(cols);
    app.settings.columns = cols.clone();
}

/// 快捷键整节落库（T-KEYS）：**双写** `[keys]` 与 AppData 镜像。
///
/// 本层不做键位校验/归一（`None`/空串/非法串全部原样存下）：
/// - 三态语义（`""` 默认 / `"none"` 禁用 / 其它 = 键位串）的判读在
///   [`crate::ui::shell::keymap::binding_of`]，非法串由装配时回落默认 + `warn!`
///   ——配置层不悄悄改写用户写下的值（手改错的值要留在盘上，用户才看得出错在
///   哪；若这里悄悄改成默认，用户会以为自己写的东西被程序吃了）；
/// - 录制路径上行前已由 UI 半场做过 `Keystroke::parse` 校验 + 冲突检测
///   （`spec_from_keystroke` / `conflict_with`），故此处只做搬运。
fn store_keys(cfg: &mut Config, app: &mut AppData, v: &SettingValue) {
    let Some(keys) = v.as_keys() else { return };
    cfg.keys = keys.clone();
    app.settings.keys = keys.clone();
}

#[cfg(test)]
#[path = "tests/settings_registry.rs"]
mod tests;
