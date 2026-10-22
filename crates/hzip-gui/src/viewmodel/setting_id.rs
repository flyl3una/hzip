//! 设置项标识（T-SET §2）：统一设置流程的注册表键 + 信号载荷。
//!
//! # 为什么定义在 viewmodel 层
//!
//! [`crate::viewmodel::VmSignal::SettingApplied`] 需要该类型，而依赖方向是
//! 单向 `ui/ → app/ → viewmodel/`（07 §3 红线）：较低层承载跨层共享的载荷
//! 类型，描述符表（page/widget/load/store/effect）在
//! [`crate::app::settings_registry`]，UI 只消费两者。
//!
//! 本类型**只承载身份**（无顺序语义）——REGISTRY 下标由
//! [`crate::app::settings_registry::index`] 穷举给出，避免同一顺序在两处
//! 各写一份（互锁单测把守）。

/// 设置项标识（REGISTRY 键）。
///
/// 变体 = 设置项全集；新增设置项时漏登记 REGISTRY/`index()` 会在编译期
/// （穷举 match）或单测（互锁）暴露。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingId {
    /// 主题（[ui].theme）
    Theme,
    /// 主题色（[ui].theme_color；hex `#RRGGBB`，空 = 默认——T-SEL 走查 #3）
    ThemeColor,
    /// 界面字体族（[ui].font_family；空 = 主题默认 `.SystemUIFont`）
    FontFamily,
    /// 界面字号 px（[ui].font_size；10~24，rem 基准）
    FontSize,
    /// 界面圆角弧度 px（[ui].corner_radius；0 = 直角，有效域 0~20）
    CornerRadius,
    /// 布局密度（[ui].density；compact/standard/loose）
    Density,
    /// 显示 `..` 跳上级（[ui].show_dotdot）
    ShowDotdot,
    /// 改动后立即写回（关 = 手动保存，见 `config.ui.write_back_immediate`）
    WriteBackImmediate,
    /// 自动更新开关（[ui].auto_update；检查逻辑挂账 T43）
    AutoUpdate,
    /// 文件关联格式勾选集合（[ui].assoc_formats）
    Assoc,
    /// 上下文解压菜单勾选 key（[ui].ctx_extract）
    CtxExtract,
    /// 上下文压缩菜单勾选 key（[ui].ctx_compress）
    CtxCompress,
    /// 智能解压（[extract].smart）
    ExtractSmart,
    /// 冲突策略（[extract].conflict）
    ExtractConflict,
    /// 尽力而为（[extract].best_effort）
    ExtractBestEffort,
    /// 文件名编码（[extract].codepage；0 = 自动/None）
    ExtractCodepage,
    /// 任务完成发送系统通知（[jobs].notify_on_finish）
    NotifyOnFinish,
    /// 进度窗口开关（[jobs].progress_window）
    ProgressWindow,
    /// 全部任务完成后退出（[jobs].exit_on_finish）
    ExitOnFinish,
    /// 默认压缩格式（[defaults].format）
    Format,
    /// 默认压缩级别（[defaults].level）
    Level,
    /// 保留元数据（[compress].keep_metadata）
    KeepMetadata,
    /// 完成后校验（[compress].verify_after）
    VerifyAfter,
    /// 目录行聚合大小（[ui].show_folder_size）
    FolderSize,
    /// 日志目录（[logging].dir；留空 = 平台默认）
    LogDir,
    /// 图像查看器路径（[ui].image_viewer；空 = 系统默认）
    ImageViewer,
    /// 编辑器路径（[ui].editor；空 = 系统默认）
    Editor,
    /// 浏览列显隐/顺序（[ui].columns）
    Columns,
    /// 界面语言（`[ui].lang`；重设 rust-i18n 全局 locale）
    Lang,
    /// 浏览页左树显隐（[ui].show_tree；T-MENU5 菜单/设置双入口）
    ShowTree,
    /// 任务中心页签显隐（[ui].show_jobs；菜单/设置双入口，默认关）
    ShowJobs,
    /// 工具栏三态（[ui].toolbar_style；T-MENU5）
    ToolbarStyle,
    /// 视图样式（[ui].view_style；T-MENU5/T-MENU9）
    ViewStyle,
    /// 文件类型图标来源（[ui].file_type_icons；system/builtin——T-ICON 决策 #1）
    FileTypeIcons,
    /// 快捷键绑定（`[keys]` 全节一次上行——T-KEYS）。
    ///
    /// 本项**一项顶一节**：界面上的 14 行动作共用这一个 id（不像其它项一项一
    /// 个字段），因为配置侧就是一个 `[keys]` 表、上行侧整份 `Keys` 一次写回
    /// （见 [`crate::app::settings_registry::SettingValue::Keys`]）。
    /// 生效半场 = `Effect::DataOnly` 声明 + UI 半场按**本 id** 重装 keymap
    /// （keymap 是 `App` 级全局态，快照不携带）。
    Keymap,
}

impl SettingId {
    /// gpui 元素 id（窗口内唯一，单测把守）。
    ///
    /// 与文案解耦（i18n 后仍稳定）；控件 id 用于 gpui 的跨帧状态复用，
    /// 重复会导致勾选态串台。
    pub fn key(self) -> &'static str {
        match self {
            Self::Theme => "set-theme",
            Self::ThemeColor => "set-theme-color",
            Self::FontFamily => "set-font-family",
            Self::FontSize => "set-font-size",
            Self::CornerRadius => "set-corner-radius",
            Self::Density => "set-density",
            Self::ShowDotdot => "set-show-dotdot",
            Self::WriteBackImmediate => "set-write-back-immediate",
            Self::AutoUpdate => "set-auto-update",
            Self::Assoc => "set-assoc",
            Self::CtxExtract => "set-ctx-extract",
            Self::CtxCompress => "set-ctx-compress",
            Self::ExtractSmart => "set-extract-smart",
            Self::ExtractConflict => "set-extract-conflict",
            Self::ExtractBestEffort => "set-extract-best-effort",
            Self::ExtractCodepage => "set-extract-codepage",
            Self::NotifyOnFinish => "set-notify-on-finish",
            Self::ProgressWindow => "set-progress-window",
            Self::ExitOnFinish => "set-exit-on-finish",
            Self::LogDir => "set-log-dir",
            Self::Format => "set-format",
            Self::Level => "set-level",
            Self::KeepMetadata => "set-keep-metadata",
            Self::VerifyAfter => "set-verify-after",
            Self::FolderSize => "set-folder-size",
            Self::ImageViewer => "set-image-viewer",
            Self::Editor => "set-editor",
            Self::Columns => "set-columns",
            Self::Lang => "set-lang",
            Self::ShowTree => "set-show-tree",
            Self::ShowJobs => "set-show-jobs",
            Self::ToolbarStyle => "set-toolbar-style",
            Self::ViewStyle => "set-view-style",
            Self::FileTypeIcons => "set-file-type-icons",
            Self::Keymap => "set-keymap",
        }
    }
}
