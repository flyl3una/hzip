//! 右键菜单的**数据面**：规格（`MenuSpec`）、状态（`MenuStatus`）、错误与后端 trait。
//!
//! 平台分发与公共查表在 `backend.rs`；模块路径（`crate::context_menu::*`）由 `mod.rs` 的再导出保持不变。

use std::fmt;
use std::path::PathBuf;

/// 一个 verb 的静态定义（[`VERBS`] 表元素）。
#[derive(Debug, Clone, Copy)]
pub struct EntrySpec {
    /// 命令参数模板（不含 exe；占位符见模块文档）。
    pub argv: &'static [&'static str],
    /// 适用范围（后端据此决定挂载位置 / MIME / 作用域）。
    pub scopes: &'static [MenuScope],
    /// Windows 是否跳过（平台专用开关；**当前无条目使用**——保留是为了
    /// 「某动词在 Windows 上不该挂载」这类将来需求有明确表达位置，
    /// 而不是散落 `cfg`）。
    pub win_skip: bool,
}

/// 菜单项适用范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MenuScope {
    /// 已关联扩展名的档案文件（解压类）。
    ArchiveFile,
    /// 任意文件或文件夹（压缩类）。
    Any,
    /// 文件夹空白处（无选中项时压缩当前目录内容）。
    DirectoryBackground,
}

impl MenuScope {
    /// 落盘/传输用的稳定标识（Windows 的 `shell-menu.json` 与 Linux 产物的键）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ArchiveFile => "archive_file",
            Self::Any => "any",
            Self::DirectoryBackground => "dir_background",
        }
    }

    /// [`Self::as_str`] 的逆（未知值 → `None`）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "archive_file" => Some(Self::ArchiveFile),
            "any" => Some(Self::Any),
            "dir_background" => Some(Self::DirectoryBackground),
            _ => None,
        }
    }
}

/// 调用方给出的菜单项（顺序 = 菜单展示顺序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// verb key（`CTX_EXTRACT_KEYS` / `CTX_COMPRESS_KEYS` 的落盘 key，永不翻译）。
    pub key: &'static str,
    /// 已本地化文案（调用方经 `crate::tr` 求值后传入）。
    pub label: String,
}

/// 安装规格（调用方填 [`MenuSpec::exe_path`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuSpec {
    /// 本程序可执行文件绝对路径。
    pub exe_path: PathBuf,
    /// 参与右键的解压项。
    pub extract: Vec<MenuEntry>,
    /// 参与右键的压缩项。
    pub compress: Vec<MenuEntry>,
    /// **本程序能解压的**档案扩展名（不带点），如 `["zip","7z","tar","gz"]`。
    ///
    /// 用途：**Linux/macOS 后端**按它过滤解压类条目；Windows 的 DLL 自 2026-10-26
    /// 起**不再用它判定可见性**（改由「任意文件都显示，打不开就报错」——扩展名
    /// 不是证据，改过后缀的压缩包也要有入口），仍照常写入 `shell-menu.json`
    /// 以保持三平台配置一致。
    ///
    /// **语义红线**：这是**能力集**（引擎注册的读处理器扩展名），不是用户在
    /// 「文件关联」页勾选的处理器注册范围。两者错配过一次：`assoc_formats = ["7z"]`
    /// 的机器上右键 `.zip` 解压项全部消失（用户没勾关联 ≠ 不能解压）。
    pub archive_exts: Vec<String>,
    /// Shell 扩展 DLL 路径覆盖（`None` = 主程序同目录默认查找；
    /// `Some` = 注册用该路径——DLL 与主程序分目录部署时由 GUI 层从
    /// `[ui].shell_ext_dll` 传入。仅 Windows 经典注册消费；稀疏包
    /// 仍取主程序同目录）。
    pub shell_dll: Option<std::path::PathBuf>,
}

impl MenuSpec {
    /// 全部条目（解压在前，压缩在后；顺序即菜单顺序）。
    pub fn entries(&self) -> impl Iterator<Item = &MenuEntry> {
        self.extract.iter().chain(self.compress.iter())
    }

    /// 条目总数。
    pub fn len(&self) -> usize {
        self.extract.len() + self.compress.len()
    }

    /// 无任何条目。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 逐条目取静态定义（[`super::backend::entry_for`] 命中失败即忽略——白名单已由调用方收敛）。
    pub fn resolved(&self) -> impl Iterator<Item = (&MenuEntry, EntrySpec)> {
        self.entries()
            .filter_map(|e| entry_for(e.key).map(|s| (e, s)))
    }
}

/// 按 key 取 verb 定义（白名单外 → `None`）。
pub fn entry_for(key: &str) -> Option<EntrySpec> {
    VERBS.iter().find(|(k, _)| *k == key).map(|(_, spec)| *spec)
}

/// 安装形态（[`MenuStatus::Installed`] 携带）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// Windows 11 现代菜单顶层（稀疏包 + `IExplorerCommand`）。
    ModernTopLevel,
    /// 传统菜单（Win10/Server 经典注册；Windows 11 的「显示更多选项」）。
    LegacyMenu,
    /// macOS Finder Sync 扩展（顶层项，限授权目录）。
    FinderSync,
    /// macOS 服务菜单（右键底部「服务」子菜单）。
    Services,
    /// Linux 桌面扩展 / 脚本 / servicemenu。
    Desktop,
}

/// 当前安装状态（设置页状态行数据源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuStatus {
    /// 未安装。
    NotInstalled,
    /// 已安装。
    Installed {
        /// 生效形态。
        mode: InstallMode,
        /// 条目数。
        items: usize,
    },
    /// 已安装但需重启文件管理器/资源管理器才可见。
    NeedsRestart,
    /// 已安装但能力受限（如 Win11 包注册失败回落经典、macOS 无 appex 回落 Services）。
    Degraded {
        /// 面向用户的原因（已本地化，可直接展示）。
        reason: String,
    },
    /// 当前环境不支持（Server Core / 未知桌面 / 非目标文件管理器）。
    Unsupported {
        /// 面向用户的原因（已本地化，可直接展示）。
        reason: String,
    },
}

/// 安装结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// 实际生效形态。
    pub mode: InstallMode,
    /// 实际写入条目数。
    pub items: usize,
    /// 是否需要重启文件管理器才可见。
    pub restart_required: bool,
}

/// 错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuErrorKind {
    /// 当前平台/桌面不支持。
    Unsupported,
    /// 文件系统/注册表 I/O 失败。
    Io,
    /// 规格非法（空集、未知 key、路径不可执行等）。
    Spec,
    /// 外部工具失败（PowerShell / pluginkit / makeappx 等）。
    Platform,
}

/// 平台后端错误（多细节聚合——幂等清理时逐项记录，不首个失败即中断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuError {
    /// 类别。
    pub kind: MenuErrorKind,
    /// 逐条失败细节。
    pub details: Vec<String>,
}

impl MenuError {
    /// 单条错误。
    pub fn new(kind: MenuErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            details: vec![detail.into()],
        }
    }

    /// 追加一条细节。
    pub fn push(&mut self, detail: impl Into<String>) {
        self.details.push(detail.into());
    }

    /// 是否为「不支持」类别（调用方据此降级而非报错）。
    pub fn is_unsupported(&self) -> bool {
        self.kind == MenuErrorKind::Unsupported
    }
}

impl fmt::Display for MenuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.details.join("; "))
    }
}

impl std::error::Error for MenuError {}

/// 右键菜单后端（每平台一个静态实例，见 [`super::backend::backend`]）。
///
/// **幂等**：`install` 可重复调用（先清后写），`uninstall` 可重复调用
/// （无残迹时不报错）。二者都必须是**阻塞、可离线**的短调用——调用方在
/// 后台线程执行。
pub trait ContextMenu: Send + Sync {
    /// 安装/覆盖写入（按 `spec` 全量重建）。
    fn install(&self, spec: &MenuSpec) -> Result<InstallReport, MenuError>;

    /// 清除本程序写入的一切右键菜单痕迹（不动文件关联）。
    fn uninstall(&self) -> Result<(), MenuError>;

    /// 回读当前状态（设置页状态行）。
    fn status(&self) -> MenuStatus;

    /// 只更新**条目集**（勾选项），不动任何操作系统注册。
    ///
    /// # 为什么需要一条独立路径
    ///
    /// 条目集存放在 `shell-menu.json`（DLL 每次构建菜单现读），而 verb 名、
    /// CLSID、稀疏包、`ExplorerCommandHandler` **都不随勾选变化**。让「勾一下
    /// 选项」去跑完整 [`ContextMenu::install`]（注册表 + PowerShell + 包注册）
    /// 又慢又可能失败，于是早期设计让勾选只改草稿、必须按「应用」才同步——
    /// 后果是用户改完勾选后系统菜单仍是旧内容，且界面上没有任何提示（实测缺陷：
    /// 设置里 3+3 项，右键菜单里却是上一次安装的全部 9 项）。
    ///
    /// 本方法把「改条目集」降为**一次小文件写入**（毫秒级、免管理员、无需重启
    /// 资源管理器），调用方即可在每次勾选时同步，彻底消除这个不一致窗口。
    ///
    /// 未安装时**退回 [`ContextMenu::install`]**：只写配置 = 装了个系统上看不见
    /// 的菜单，比不同步更糟。
    ///
    /// 默认实现即 `install`（非 Windows 后端的条目集与注册同源，拆不开）。
    fn sync(&self, spec: &MenuSpec) -> Result<(), MenuError> {
        self.install(spec).map(|_| ())
    }

    /// 只刷新**能力集**（`archive_exts`：引擎能读哪些扩展名）——已安装时生效。
    ///
    /// # 与 [`ContextMenu::sync`] 的区别
    ///
    /// `sync` 刷的是**用户勾选**（条目集），须由用户在设置页按「应用」触发；
    /// 本方法刷的是**引擎能力**（非选项，用户无从勾选），随构建升级而变化——
    /// 典型症状：新增 rar 支持后，`.rar` 右键里仍没有解压项（DLL 按旧清单判定
    /// 「不是档案」）。故应用启动时调一次即可保鲜，**不需要用户做任何操作**。
    ///
    /// 未安装（无配置文件）→ `Ok(false)` no-op。返回 `Ok(true)` = 已回写。
    /// 默认 no-op（非 Windows 后端不按扩展名清单判定档案）。
    fn refresh_capabilities(&self, _archive_exts: &[String]) -> Result<bool, MenuError> {
        Ok(false)
    }

    /// 消费扩展进程写入的**意图队列**（macOS FinderSync appex / Services provider）。
    ///
    /// # 为什么扩展不直接拼 argv
    ///
    /// appex 与 Services provider 运行在**沙盒**内，既读不到宿主私有配置，也
    /// 无法用 `NSWorkspace` 的 `arguments` 向宿主注入命令行（均已真机实证）。
    /// 故两侧的**唯一**通道是 App Group 容器里的一个追加式队列文件：扩展只写
    /// `{key, paths}`，由宿主（本方法调用方）用**同一个** [`VERBS`] 表展开成
    /// argv，再走与命令行、转发队列完全相同的意图解析路径。
    ///
    /// 幂等：整体读取后**清空内容但保留文件**（`inotify`/FSEvents 监听依附于
    /// 该路径，删文件会破坏监听——与 `hzip-forward-queue` 同款语义）。
    ///
    /// 默认恒空（非 macOS 无此通道）。
    fn take_extension_intents(&self) -> Vec<ExtensionIntent> {
        Vec::new()
    }

    /// 追加一条扩展意图（宿主进程内的 Services provider 用；appex 侧由 Swift
    /// 直接写文件，不经此方法）。
    ///
    /// 默认不支持（非 macOS 无此通道）。
    fn append_extension_intent(&self, _intent: &ExtensionIntent) -> Result<(), MenuError> {
        Err(MenuError::new(
            MenuErrorKind::Unsupported,
            "当前平台无 Finder 扩展意图通道",
        ))
    }

    /// 扩展意图队列的文件路径（macOS = App Group 容器内 `intents.jsonl`）。
    ///
    /// 供调用方挂**文件监听**（appex 在宿主已运行时写入队列后无其他通知手段——
    /// 它无法向宿主注入 argv，故只能靠宿主监听该文件）。默认 `None`。
    fn extension_intents_path(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// FinderSync appex / Services provider 投递的一次动作意图。
///
/// 字段名即 `intents.jsonl` 的线上字段（appex 侧 Swift 按同名字段编码）。
/// `key` ∈ [`VERBS`] 的 key；`paths` = 选中项绝对路径。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExtensionIntent {
    /// verb key（落盘 key，永不翻译）。
    pub key: String,
    /// 选中项绝对路径（appex 用 `URL.path`，Services 用 pasteboard 文件名）。
    pub paths: Vec<String>,
}

/// 全部 verb 定义（**argv 的单一事实源**；按 key 查表，见 [`super::backend::entry_for`]）。
///
/// 顺序**不决定**菜单渲染顺序——那是 `hzip-gui::ui::contracts::CTX_EXTRACT_KEYS`
/// / `CTX_COMPRESS_KEYS` 的职责（落进 `shell-menu.json` 的 `items` 数组，DLL 按
/// 数组顺序枚举）。此处仍与那份顺序保持一致，避免读代码的人以为有两套顺序。
///
/// `open_hzip` **不再**在 Windows 跳过。
///
/// 早期理由是「ProgId 的 `shell\open` 已提供同语义项」——但那只在 hzip 是该
/// 扩展名的默认处理器时成立；用户没勾文件关联时（`[ui].assoc_formats` 为空或
/// 只勾了部分），系统里根本没有那个顶层项，于是「设置里勾了『用 hzip 打开』，
/// 右键却什么都没有」（实测缺陷）。菜单项是我们自己子菜单里的一项，重复风险
/// 远小于「勾了不生效」。
pub const VERBS: [(&str, EntrySpec); 11] = [
    // 「用 hzip 打开」置顶（最常用；与设置页/子菜单展示顺序一致）
    (
        "open_hzip",
        EntrySpec {
            argv: &["--open", "%SEL"],
            scopes: &[MenuScope::ArchiveFile],
            win_skip: false,
        },
    ),
    (
        "extract_here",
        EntrySpec {
            argv: &["--extract-here", "%SEL"],
            scopes: &[MenuScope::ArchiveFile],
            win_skip: false,
        },
    ),
    (
        "auto_extract",
        EntrySpec {
            argv: &["--auto-extract", "%SEL"],
            scopes: &[MenuScope::ArchiveFile],
            win_skip: false,
        },
    ),
    (
        "extract_to",
        EntrySpec {
            argv: &["--extract-to", "%SEL"],
            scopes: &[MenuScope::ArchiveFile],
            win_skip: false,
        },
    ),
    (
        // 解压参数窗（右键「解压选项…」）：`%SELS` = 一次进程收下全部选中档案，
        // 参数窗只弹一次、参数共用（与 Bandizip 的「解压选项」一致）。
        "extract_options",
        EntrySpec {
            argv: &["--extract-options", "%SELS"],
            scopes: &[MenuScope::ArchiveFile],
            win_skip: false,
        },
    ),
    (
        "extract_here_del",
        EntrySpec {
            argv: &["--extract-here", "--delete", "%SEL"],
            scopes: &[MenuScope::ArchiveFile],
            win_skip: false,
        },
    ),
    (
        "extract_dir_del",
        EntrySpec {
            argv: &["--extract-dir", "--delete", "%SEL"],
            scopes: &[MenuScope::ArchiveFile],
            win_skip: false,
        },
    ),
    (
        "add_zip",
        EntrySpec {
            argv: &["--compress", "%SELS", "--format", "zip"],
            scopes: &[MenuScope::Any, MenuScope::DirectoryBackground],
            win_skip: false,
        },
    ),
    (
        "add_tar",
        EntrySpec {
            argv: &["--compress", "%SELS", "--format", "tar"],
            scopes: &[MenuScope::Any, MenuScope::DirectoryBackground],
            win_skip: false,
        },
    ),
    (
        "add_7z",
        EntrySpec {
            argv: &["--compress", "%SELS", "--format", "7z"],
            scopes: &[MenuScope::Any, MenuScope::DirectoryBackground],
            win_skip: false,
        },
    ),
    (
        "add_archive",
        EntrySpec {
            argv: &["--compress-to", "%SELS"],
            scopes: &[MenuScope::Any, MenuScope::DirectoryBackground],
            win_skip: false,
        },
    ),
];

/// 组件 CLSID（Windows 有；其他平台 `None`）。
///
/// 对外暴露的动机：构建脚本与验收测试需要与实现**同源**地拿到这个值。历史上
/// 曾出现「注册表写的 CLSID ≠ DLL 接受的 CLSID」→ 右键菜单静默不出现（详见
/// `win.rs::CLSID` 的说明）。任何需要该值的代码都应经此常量，不要另抄一份。
#[cfg(windows)]
pub const COMPONENT_CLSID: Option<&str> = Some(super::win::CLSID);

/// 组件 CLSID（非 Windows 平台无 COM 注册）。
#[cfg(not(windows))]
pub const COMPONENT_CLSID: Option<&str> = None;
