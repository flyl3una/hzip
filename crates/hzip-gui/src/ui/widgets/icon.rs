//! `AppIcon` 收口枚举（T-G50 §3.2）——全 UI 图标引用唯一入口。
//!
//! lucide 全集按需引入：首批 = hzip 已用 + R2 可预见（T-G51~G54 实施中需
//! 新图标时，先跑 `build/icons/fetch-lucide-icons.sh` 追加下载（清单 append-only），
//! 再在此登记变体）。
//!
//! `IconNamed` 落地后经 `impl Into<Icon>` 直接可传 `Button::icon` /
//! `Tab::icon` / `PopupMenuItem::icon`（与 `IconName` 同一消费面）。

use gpui_kit::SharedString;
use gpui_kit::component::IconNamed;

/// 自有 lucide 图标（kebab-case 文件名 = `assets/icons/<name>.svg`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppIcon {
    /// 目录（闭合）
    Folder,
    /// 目录（展开）
    FolderOpen,
    /// 目录（闭合；树节点折叠态）
    FolderClosed,
    /// 文件
    File,
    /// 文本文件
    FileText,
    /// 解压输出（file-output）
    Extract,
    /// 档案（根节点/工具栏）
    Archive,
    /// 压缩（package-plus）
    Compress,
    /// 解压到指定目录（package-minus）
    PackageMinus,
    /// 预览
    Eye,
    /// 隐藏（eye-off；列设置显隐切换）
    EyeOff,
    /// 帮助（circle-help；设置行 ？ 悬浮帮助——走查 #8）
    CircleHelp,
    /// 刷新
    Refresh,
    /// 完整性校验
    ShieldCheck,
    /// 设置
    Settings,
    /// 置顶/上传
    ArrowUp,
    /// 下移
    ArrowDown,
    /// 树展开
    ChevronRight,
    /// 树折叠
    ChevronDown,
    /// 过滤/搜索
    Search,
    /// 关闭
    X,
    /// 加密徽标
    Lock,
    /// 可疑编码徽标（triangle-alert）
    Warn,
    /// 严重错误（octagon-alert；标准消息弹窗的「错误」级别）
    OctagonAlert,
    /// 磁盘
    HardDrive,
    /// 列表
    List,
    /// 表格
    Table,
    /// 列（列设置；备用语义位——工具栏「查看」已改用 layout-list）
    Columns,
    /// 查看（layout-list；工具栏「查看」下拉，用户口径 2026-09-29）
    LayoutList,
    /// 语言
    Languages,
    /// 深色主题
    Moon,
    /// 浅色主题
    Sun,
    /// 跟随系统
    Monitor,
    /// 双击动作
    MousePointerClick,
    /// 外部打开
    ExternalLink,
    /// 完成/确认
    Check,
    /// 减号
    Minus,
    /// 重试
    RotateCw,
    /// 任务/全部解压
    Inbox,
    /// 新建
    Plus,
    /// 替换（解压）
    Replace,
    /// 信息
    Info,
    /// 成功
    CircleCheck,
    /// 加载中
    LoaderCircle,
    /// 软链接标记（link；R-GUI1-02 §2.4）
    Link,
    /// 压缩档案根节点（line-md folder-zip 静态终态；R-GUI4 #1）
    FolderZip,
}

impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        // 静态字面量路径 + `SharedString::new_static`：零分配。`Icon::new` 在
        // 每帧渲染时构造，逐帧 `format!("icons/{name}.svg")` 会把图标引用放大成
        // 「每图标每帧一次堆分配」；`SharedString` 无 `From<&'static str>` 静态
        // 分支，`new_static` 是唯一免堆路径。
        let path = match self {
            Self::Folder => "icons/folder.svg",
            Self::FolderOpen => "icons/folder-open.svg",
            Self::FolderClosed => "icons/folder-closed.svg",
            Self::File => "icons/file.svg",
            Self::FileText => "icons/file-text.svg",
            Self::Extract => "icons/file-output.svg",
            Self::Archive => "icons/archive.svg",
            Self::Compress => "icons/package-plus.svg",
            Self::PackageMinus => "icons/package-minus.svg",
            Self::Eye => "icons/eye.svg",
            Self::EyeOff => "icons/eye-off.svg",
            Self::CircleHelp => "icons/circle-help.svg",
            Self::Refresh => "icons/rotate-cw.svg",
            Self::ShieldCheck => "icons/shield-check.svg",
            Self::Settings => "icons/settings.svg",
            Self::ArrowUp => "icons/arrow-up.svg",
            Self::ArrowDown => "icons/arrow-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::Search => "icons/search.svg",
            Self::X => "icons/x.svg",
            Self::Lock => "icons/lock.svg",
            Self::Warn => "icons/triangle-alert.svg",
            Self::OctagonAlert => "icons/octagon-alert.svg",
            Self::HardDrive => "icons/hard-drive.svg",
            Self::List => "icons/list.svg",
            Self::Table => "icons/table.svg",
            Self::Columns => "icons/columns.svg",
            Self::LayoutList => "icons/layout-list.svg",
            Self::Languages => "icons/languages.svg",
            Self::Moon => "icons/moon.svg",
            Self::Sun => "icons/sun.svg",
            Self::Monitor => "icons/monitor.svg",
            Self::MousePointerClick => "icons/mouse-pointer-click.svg",
            Self::ExternalLink => "icons/external-link.svg",
            Self::Link => "icons/link.svg",
            Self::Check => "icons/check.svg",
            Self::Minus => "icons/minus.svg",
            Self::RotateCw => "icons/refresh-cw.svg",
            Self::Inbox => "icons/inbox.svg",
            Self::Plus => "icons/plus.svg",
            Self::Replace => "icons/replace.svg",
            Self::Info => "icons/info.svg",
            Self::CircleCheck => "icons/circle-check.svg",
            Self::LoaderCircle => "icons/loader-circle.svg",
            Self::FolderZip => "icons/folder-zip.svg",
        };
        SharedString::new_static(path)
    }
}

#[cfg(test)]
#[path = "tests/icon.rs"]
mod tests;
