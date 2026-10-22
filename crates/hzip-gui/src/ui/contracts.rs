//! UI 契约类型总表（docs/tasks/T-GUI/README.md §2 全量落地；T-G00 独占，W2/W3 只读）。
//!
//! **并行的前提**：跨任务只通过本文件中的纯数据类型与闭包注入口耦合。
//! 任何任务发现契约缺口 → 停下报告总控，追加 T-G0x 契约补丁任务，不得私自改他人文件。
//!
//! 消费方索引：
//! - [`MainTab`] / [`ToolbarAction`] / [`ShellHandlers`] → T-G10（chrome）/ T-G30（装配）
//! - [`Crumb`] / [`BrowseRow`] / [`SortColumn`] → T-G20（浏览页）
//! - [`DialogKind`] / [`DialogEvent`] → T-G11（对话框族）/ T-G30（消费）
//! - [`PreviewPayload`] / [`PreviewEvent`] → T-G12（预览窗口）
//! - [`SignalTx`] / [`SignalRx`] → T-G13（forwarder）/ T-G21（泵 notifier）/ T-G30（装配）
//! - [`AppStateUiSnapshot`] → T-G10（快照应用）/ T-G30（快照导出）

use crate::viewmodel::VmSignal;
use hzip_core::engine::Engine;

/// 主窗口页签（04 §4.1/4.2 原型）。
///
/// 消费方：T-G10（页签行渲染 + 状态）、T-G20/T-G21（内容插槽分支）、T-G30（装配）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainTab {
    /// 浏览页（面包屑 + 双栏）
    Browse,
    /// 任务中心页
    Jobs,
    /// 设置页（原为设置弹层；2026-09 改为可关闭页签——少一个弹窗）
    Settings,
}

/// 工具栏动作（主窗口 → AppData）。
///
/// 消费方：T-G10（工具栏按钮 + 菜单项发出）；T-G30（全量消费落地）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolbarAction {
    /// 打开档案（路径输入流程）
    Open,
    /// 「打开▾」：最近打开记录（档案路径）。
    ///
    /// 与菜单「文件 ▸ 最近的文件」同源同动作（`mru_items`，T-BAR3）。
    OpenRecent(String),
    /// 解压选中（选中空 → 回退全部，Bandizip 语义）
    Extract,
    /// 全部解压。
    ///
    /// 2026-09-29 用户口径：**工具栏不再出这个按钮**（改为「解压▾」承担全部语义）。
    /// 变体保留——键盘快捷键（`ctrl-alt-e`）与菜单仍派发它。
    ExtractAll,
    /// 「解压▾」：五种解压方式（T-BAR4；语义与右键动词家族逐条对齐）
    ExtractMode(ExtractMode),
    /// 添加文件到**档案内当前路径**（原 `Compress`：接压缩对话框，2026-09-29 改语义）
    AddFiles,
    /// 删除选中条目（与右键「删除」同一路径；无聚焦选中时置灰）
    Delete,
    /// 预览 = 右键「打开方式…」（系统询问对话框；加密条目先过密码门）
    Preview,
    /// 「查看▾」：切换某列显隐（参数 = `BrowseColumnId::key()`，契约键）
    ToggleColumn(String),
    /// 「打开▾」尾部：清空最近记录（与菜单同名项同一动作）
    ClearMru,
    /// 刷新
    Refresh,
    /// 测试完整性
    TestIntegrity,
    /// 设置
    Settings,
}

/// 解压方式（工具栏「解压▾」五种）。
///
/// 语义与右键动词家族（`LaunchIntent`：extract-here / auto-extract / extract-dir）
/// **逐条对齐**，唯一新增是桌面两项；`dest`/`smart` 的派生只写在
/// `ExtractPlan`/`extract_plan` 一处（T-BAR §3.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractMode {
    /// 自动解压（档案所在目录 + `smart=true`，即「智能解压」）
    Auto,
    /// 解压到当前文件夹（档案所在目录，`smart` 读配置）
    Here,
    /// 解压到「压缩文件名」文件夹（档案目录 / 去掉扩展名的档案名）
    Dir,
    /// 解压到桌面（`smart` 读配置）
    Desktop,
    /// 自动解压到桌面（桌面 + `smart=true`）
    AutoDesktop,
}

impl ExtractMode {
    /// 菜单顺序（= 需求给的顺序）。
    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::Here,
        Self::Dir,
        Self::Desktop,
        Self::AutoDesktop,
    ];

    /// i18n key（`chrome.toml` `[toolbar.extract_mode.*]`）。
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "toolbar.extract_mode.auto",
            Self::Here => "toolbar.extract_mode.here",
            Self::Dir => "toolbar.extract_mode.dir",
            Self::Desktop => "toolbar.extract_mode.desktop",
            Self::AutoDesktop => "toolbar.extract_mode.auto_desktop",
        }
    }

    /// 是否落桌面（桌面不可用时这些项置灰）。
    pub fn needs_desktop(self) -> bool {
        matches!(self, Self::Desktop | Self::AutoDesktop)
    }
}

/// 工具栏可用性投影（**唯一事实源**，T-BAR §3.2）。
///
/// 与右键菜单的可用性规则同口径（`browse::item_disabled`）：工具栏在 shell 侧
/// 渲染，吃不到表格委托的现态，只能由快照派生——故两条规则在此收敛，并由
/// 「对照表测试」锁定（`toolbar_enable_matches_context_menu_rules`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolbarEnable {
    /// 有打开的档案（解压/添加/删除/预览/查看 的前提）
    pub has_archive: bool,
    /// 解压可用（有档案即可；选中空 → 语义回退全部）
    pub can_extract: bool,
    /// 添加可用（档案可编辑：`caps.edit`，组合档/只读格式 → false）
    pub can_add: bool,
    /// 删除可用（可编辑 ∧ 有聚焦选中——用户口径：无聚焦置灰）
    pub can_delete: bool,
    /// 预览可用（恰好单选一个**文件**条目）
    pub can_preview: bool,
    /// 「查看」下拉可用（浏览页在场 = 有档案）
    pub can_columns: bool,
    /// 解压▾ 的桌面两项可用（取不到桌面目录 → 置灰，不报错）
    pub desktop_available: bool,
    /// 全局忙碌（叠加在上述各项之上：忙碌一律置灰）
    pub busy: bool,
}

impl AppStateUiSnapshot {
    /// 工具栏可用性（唯一事实源；见 [`ToolbarEnable`]）。
    pub fn toolbar_enable(&self) -> ToolbarEnable {
        let has_archive = !self.archive_path.is_empty();
        let selected = self.sel_stats.files + self.sel_stats.dirs > 0;
        ToolbarEnable {
            has_archive,
            can_extract: has_archive,
            // `caps_edit` = **本会话可写性**（顶层 = 格式可编辑；嵌套 = 写回链能
            // 重建，T-IMG0 §5.3）——不再硬判 `nested`：被包住的镜像现在真能改。
            can_add: has_archive && self.caps_edit,
            can_delete: has_archive && self.caps_edit && selected,
            // 「恰好单选一个文件」= 右键菜单 `single_file` 的等价投影
            can_preview: self.sel_stats.files == 1 && self.sel_stats.dirs == 0,
            can_columns: has_archive,
            desktop_available: desktop_extract_available(),
            busy: self.busy,
        }
    }
}

/// 桌面目录是否可用（进程级缓存；`hzip_platform::dirs::desktop_dir()` 是系统
/// 查询——渲染路径不得每帧调用，故只问一次，见 07 §「渲染帧不做跨模块查询」）。
pub fn desktop_extract_available() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| hzip_platform::dirs::desktop_dir().is_some())
}

/// 面包屑段（点击跳转；`seg=""` = 档案根）。
///
/// 消费方：T-G20（渲染 + 派生）；T-G30（跳转落地）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    /// 展示段名（档案根时 = 档案名）
    pub label: String,
    /// 目标目录路径（"" = 根）
    pub seg: String,
    /// **嵌套层号**（0 = 当前会话；1 = 上一层容器；…）。
    ///
    /// 双击容器进入后，面包屑要把「外层档案 → 内层档案 → 目录」整条链画出来：
    /// 只有层号能让点击落到正确的会话上（段名在两层里可能同名——`a.zip/b.zip`）。
    pub level: usize,
}

/// 当前目录 → 面包屑段（`seg=""` = 档案根；`label` = 段名）。
///
/// T-GUI 结构重组自 binding.rs 迁入：`app::state` 快照导出与 UI 渲染双侧
/// 共用的纯派生（app 引用 ui 的唯一例外 = 本契约文件）。
pub fn crumbs_from_current_dir(archive_name: &str, current_dir: &str) -> Vec<Crumb> {
    let mut crumbs = vec![Crumb {
        label: archive_name.to_string(),
        seg: String::new(),
        level: 0,
    }];
    if current_dir.is_empty() {
        return crumbs;
    }
    let mut acc = String::new();
    for seg in current_dir.split('/').filter(|s| !s.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        crumbs.push(Crumb {
            label: seg.to_string(),
            seg: acc.clone(),
            level: 0,
        });
    }
    crumbs
}

/// 浏览行（右栏表格；由 binding.rs 从 EntryRow 派生）。
///
/// 消费方：T-G20（表格渲染 + 排序 + 选择）。
#[derive(Debug, Clone)]
pub struct BrowseRow {
    /// 条目 ID（None = 隐式目录）
    pub entry_id: Option<u64>,
    /// 段名 / 连接（导航定位）
    pub path: String,
    /// 展示名
    pub name: String,
    /// 是否目录
    pub is_dir: bool,
    /// 是否文件型容器（zip/7z/…）
    pub is_container: bool,
    /// 是否**磁盘镜像的分区行**：图标走 `HardDrive`、类型列显示文件系统、
    /// 无标签时显示语言化的「分区 N」（元数据来自 `VolumeRowMeta`，不靠名字猜）。
    pub is_partition: bool,
    /// 是否加密
    pub encrypted: bool,
    /// 可疑编码 ⚠ 徽标
    pub suspect: bool,
    /// 人类可读（12.4 KB / —）
    pub size_text: String,
    /// 打包后人类可读
    pub packed_text: String,
    /// UTF-8 / GBK / —
    pub encoding_text: String,
    /// 本地时区格式化
    pub mtime_text: String,
    /// 软链接标记（R-GUI1-02 §2.4；format 层 `EntryInfo.is_symlink` 上浮）
    pub is_symlink: bool,
    /// 软链接目标（None = 非软链接或未知；tooltip 展示）
    pub symlink_target: Option<String>,
    /// 类型列文本（文件夹/快捷方式/按扩展名；R-GUI4 #2）
    pub type_text: String,
    // —— 新五列（2026-09-25 用户口径；文本在 binding 层派生）——
    /// 压缩级别列文本（`6` / `—`）
    pub level_text: String,
    /// 加密算法列文本（`AES-256` / `—`）
    pub encryption_text: String,
    /// CRC 校验列文本（8 位十六进制 / `—`）
    pub crc_text: String,
    /// 文件属性列文本（只读/隐藏/系统/可执行标记拼串；无标记 = `—`）
    pub attrs_text: String,
}

/// 表格排序（UI 本地态，不进 MainViewModel——排序属展示层）。
///
/// 消费方：T-G20（列头点击 → 行视图重排；不改 VM entries 顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    /// 名称列
    Name,
    /// 类型列（R-GUI 走查：列头排序全列放开）
    Type,
    /// 大小列
    Size,
    /// 压缩后列
    Packed,
    /// 编码列
    Encoding,
    /// 修改时间
    Mtime,
    /// 压缩级别（数值序；未知排末）
    Level,
    /// 加密算法（字典序）
    Encryption,
    /// CRC 校验（数值序）
    Crc,
    /// 文件属性（标记数序）
    Attrs,
}

/// 排序方向。
///
/// 消费方：T-G20。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    /// 升序
    Asc,
    /// 降序
    Desc,
}

/// 对话框种类（Modal 显隐由 AppShell 持 `Option<DialogKind>`）。
///
/// 消费方：T-G10（显隐状态）；T-G11（渲染分支）；T-G30（接线驱动）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    /// 压缩
    Compress,
    /// 解压
    Extract,
    /// 密码
    Password,
    /// 冲突
    Conflict,
    /// 设置
    Settings,
}

/// 对话框事件（UI → AppData；语义与 T41 dialog_vm/router setter 一一对应）。
///
/// 消费方：T-G11（各对话框发出）；T-G30（全量消费 → VM/router/config）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogEvent {
    /// 压缩：选定格式（白名单下标，`COMPRESS_FORMATS` 对齐）
    CompressFormat(usize),
    /// 压缩：选定级别（白名单下标）
    CompressLevel(usize),
    /// 压缩：选定压缩方式（`labels::METHOD` 下标）
    ///
    /// 与 `CompressLevel` 正交：方式选编码器、级别选压多狠（见
    /// `hzip_codec::traits::CodecMethod` rustdoc）。候选子集由格式
    /// `FormatCaps::methods` 决定，越集由 core 以 5001 拒绝。
    CompressMethod(usize),
    /// 压缩：分卷预设（`labels::SPLIT` 下标；`custom` 项的自定义值走
    /// `CompressSplitText`）
    CompressSplitPreset(usize),
    /// 压缩：自定义分卷字节数（文本；空/`0` = 不分卷）
    CompressSplitText(String),
    /// 压缩：线程数（`labels::THREADS` 下标；value `0` = 自动）
    CompressThreads(usize),
    /// 压缩：固实开关（仅 `caps.solid` 格式可用——当前为 7z）
    CompressSolid(bool),
    /// 压缩：排除隐藏文件
    CompressExcludeHidden(bool),
    /// 压缩：跟随符号链接
    CompressFollowSymlinks(bool),
    /// 压缩：保留文件属性（时间/权限）
    CompressKeepMetadata(bool),
    /// 压缩：完成后校验档案
    CompressVerifyAfter(bool),
    /// 压缩：单条失败继续打包
    CompressBestEffort(bool),
    /// 压缩：加密方式（`labels::ENC_METHOD` 下标；当前仅记录，见写侧加密挂账）
    CompressEncMethod(usize),
    /// 压缩：加密文件名开关（仅 7z 头加密可用）
    CompressEncryptNames(bool),
    /// 压缩：切换「高级选项」折叠区
    CompressToggleAdvanced(bool),
    /// 压缩：切换「加密」折叠区
    CompressToggleEncryption(bool),
    /// 压缩：字典大小（`labels::DICT` 下标；仅 7z 可用）
    CompressDict(usize),
    /// 压缩：更新方式（`labels::UPDATE_MODE` 下标；`append` 仅 `caps.append` 格式可选）
    CompressUpdateMode(usize),
    /// 压缩：分卷字节（"0" = 不分卷）
    CompressSplit(String),
    /// 压缩：排除 glob（逗号/空白分隔）
    CompressExcludes(String),
    /// 压缩：密码（UI 态；Zeroizing 构造归 T-G30）
    CompressPassword(String),
    /// 压缩：浏览输出目录（T-GUI T33 修正——原误发 CompressCancel）
    CompressBrowse,
    /// 压缩：确认覆盖已存在的目标（参数窗内的覆盖确认态「覆盖」）
    ///
    /// 与 `CompressStart` 的区别只有一个：**不**对目标做自动改名（`unique_out_path`），
    /// 直接写用户指定的路径。
    CompressOverwriteAccept,
    /// 压缩：放弃覆盖（退出覆盖确认态，回到正常按钮区）
    CompressOverwriteCancel,
    /// 压缩：开始
    CompressStart,
    /// 压缩：取消/关闭
    CompressCancel,
    // —— 编辑提交确认（T-MENU6/7：添加/删除/同名冲突）——
    /// 添加文件：确认（`replace` = 同名条目是否替换）
    EditAddAccept { replace: bool },
    /// 添加文件：取消
    EditAddCancel,
    /// 删除文件：确认
    EditDeleteAccept,
    /// 删除文件：取消
    EditDeleteCancel,
    /// 同名冲突：替换同名条目
    EditConflictReplace,
    /// 同名冲突：跳过同名条目
    EditConflictSkip,
    /// 同名冲突：取消
    EditConflictCancel,
    /// 解压：目标目录
    ExtractDest(String),
    /// 解压：智能解压开关
    ExtractSmart(bool),
    /// 解压：冲突策略（白名单下标，`CONFLICT_POLICIES` 对齐）
    ExtractConflict(usize),
    /// 解压：文件名编码（白名单下标）
    ExtractCodepage(usize),
    /// 解压：尽力而为开关
    ExtractBestEffort(bool),
    /// 解压：浏览目录（路径选择器挂账，占位事件）
    ExtractBrowse,
    /// 解压：开始
    ExtractStart,
    /// 解压：取消/关闭
    ExtractCancel,
    /// 密码：提交（Submit 后 UI 立即清空输入，Zeroizing 通道不变）
    PasswordSubmit(String),
    /// 密码：取消
    PasswordCancel,
    /// 冲突：(决策, apply_all)
    ConflictAnswer(ConflictDecision, bool),
    // —— T-SET：设置统一流程（注册表化）——
    /// 设置：任一注册表项变更（`id` + 控件原始值）。
    ///
    /// 归一化/落盘/生效在 [`crate::app::state::AppData::apply_setting`] 内完成
    /// （五步流程 `normalize → store → persist → effect → 刷新`）；控件不感知
    /// 类型细节，取值类型由该注册表项的 [`crate::app::settings_registry::WidgetKind`]
    /// 约束（不匹配即忽略）。
    SettingChanged {
        /// 设置项标识（REGISTRY 键）
        id: crate::viewmodel::SettingId,
        /// 新值（bool / 白名单下标 / 文本 / 勾选集合 / 列设置）
        value: crate::app::settings_registry::SettingValue,
    },
    /// 设置：检查更新（T43 挂账——面板动作按钮只上行本事件）
    SettingsCheckUpdate,
    /// 设置：恢复默认（注册表驱动全量重置；落盘一次）
    SettingsReset,
    /// 设置：关闭
    SettingsClose,
    /// 设置：文件关联——「应用关联」（后台写 HKCU 注册表；数据面见
    /// [`crate::viewmodel::SettingId::Assoc`] 的 store）
    SettingsApplyAssoc,
    /// 设置：文件关联——「解除关联」
    SettingsRemoveAssoc,
    /// 设置：文件关联——打开系统的「默认应用设置」页（
    /// `hzip_platform::assoc::open_default_apps_settings`）。
    ///
    /// **批量**入口：Windows 11 深链到 hzip 的条目（一屏列出本程序注册的全部
    /// 扩展名），Windows 10 打开默认应用页。只在被占用的行旁边提供（Windows
    /// 特有场景）。Linux/macOS 没有这样的页面，也不需要有。
    SettingsAssocOpenAppSettings,
    /// 设置：上下文菜单——「应用」
    ///
    /// 把草稿落盘（若有）并在后台**完整安装**操作系统右键菜单——这是**唯一**
    /// 会把勾选写进系统的入口（见 `dispatch::ctx_menu_os_action` 契约）。
    ///
    /// 勾选/取消只改内存草稿（`Commit::Manual`），面板以「有未应用的更改」提示，
    /// 用户点本事件才落盘 + 装系统（用户明确要求的工作方式：「应用」前只改配置）。
    /// 因此本事件同时是**幂等的修复入口**：重建注册表、稀疏包与经典 verb
    /// （首次安装、被外部清理、或换了程序路径时用它）。
    SettingsContextMenuApply,
    /// 设置：上下文菜单——「清除」
    ///
    /// 撤销操作系统上本程序写入的全部右键菜单痕迹（不动文件关联），
    /// 并把两个勾选集合置空。
    SettingsContextMenuClear,
    // —— T-GUI T31：任务中心批量操作 ——
    /// 任务中心：全部取消（取消全部活跃任务）
    JobsCancelAll,
    /// 任务中心：取消**单个**任务（行内「取消」钮；`job_id`）。
    ///
    /// 幂等：终态行的句柄已回收 ⇒ 取消落在 `JobsVm::cancel` 的 `handles.get`
    /// 空转上；语义与进度窗的 `on_cancel` 完全一致（同一 `jobs.cancel`）。
    JobsCancel(u64),
    /// 任务中心：清除已完成（移除全部终态行）
    JobsClearFinished,
}

// —— T-CM：右键菜单平台桥（app 层不依赖平台类型，映射收口在此）——

/// 查询操作系统上右键菜单的当前安装状态（**可能在 macOS 上起子进程**，勿在渲染帧内调用）。
pub fn query_ctx_menu_status() -> crate::app::state::CtxMenuStatus {
    use hzip_platform::context_menu::{MenuStatus, backend};
    match backend().status() {
        MenuStatus::NotInstalled => crate::app::state::CtxMenuStatus::NotInstalled,
        MenuStatus::Installed { mode, items } => crate::app::state::CtxMenuStatus::Installed {
            mode: map_install_mode(mode),
            items,
        },
        MenuStatus::NeedsRestart => crate::app::state::CtxMenuStatus::NeedsRestart,
        MenuStatus::Degraded { reason } => crate::app::state::CtxMenuStatus::Degraded { reason },
        MenuStatus::Unsupported { reason } => {
            crate::app::state::CtxMenuStatus::Unsupported { reason }
        }
    }
}

fn map_install_mode(m: hzip_platform::context_menu::InstallMode) -> crate::app::state::CtxMenuMode {
    use hzip_platform::context_menu::InstallMode as P;
    match m {
        P::ModernTopLevel => crate::app::state::CtxMenuMode::ModernTopLevel,
        P::LegacyMenu => crate::app::state::CtxMenuMode::LegacyMenu,
        P::FinderSync => crate::app::state::CtxMenuMode::FinderSync,
        P::Services => crate::app::state::CtxMenuMode::Services,
        P::Desktop => crate::app::state::CtxMenuMode::Desktop,
    }
}

/// 由当前勾选构造平台安装规格（**含本地化文案**——文案随界面语言落盘，
/// 故 DLL/servicemenu 侧不需要任何翻译表）。
pub fn build_menu_spec(
    d: &crate::app::state::AppData,
    exe_path: std::path::PathBuf,
) -> hzip_platform::context_menu::MenuSpec {
    use hzip_platform::context_menu::MenuEntry;
    let pick = |keys: &[(&'static str, &'static str)], checked: &[String]| -> Vec<MenuEntry> {
        keys.iter()
            .filter(|(k, _)| checked.iter().any(|c| c == k))
            .map(|(k, label_key)| MenuEntry {
                key: k,
                label: crate::tr(label_key),
            })
            .collect()
    };
    // Shell 扩展 DLL 路径（T-SEL 走查 #14）：`[ui].shell_ext_dll` 非空 =
    // 注册指向该路径；空 = None（平台层回落主程序同目录默认查找）
    let shell_dll = {
        let configured = d.settings.shell_ext_dll.trim();
        (!configured.is_empty()).then(|| std::path::PathBuf::from(configured))
    };
    hzip_platform::context_menu::MenuSpec {
        exe_path,
        extract: pick(&CTX_EXTRACT_KEYS, &d.settings.ctx_extract),
        compress: pick(&CTX_COMPRESS_KEYS, &d.settings.ctx_compress),
        shell_dll,
        // 必须是**引擎能解压的扩展名**（能力集），不是「文件关联页勾了哪些」
        // ——DLL 用它在非档案上隐藏解压组（`is_archive_path`）。两者语义不同：
        // 关联勾选 = 要不要把 hzip 注册成该类型的处理器，与「能不能解压」无关。
        // 曾经的错配（取 `settings.assoc`）：`assoc_formats = ["7z"]` 的机器上
        // 右键 .zip 只剩压缩项、解压项凭空消失。
        archive_exts: supported_open_extensions(&d.engine),
    }
}

/// 上下文解压菜单 key 白名单（README §2.3 契约；R2-03 UI 勾选 / R2-05 注册表 verb 共用）。
///
/// 第一字段 = 落盘 key（`[ui].ctx_extract` 白名单成员，永不翻译）；
/// 第二字段 = i18n key（展示文案经 `crate::tr` 查表——见 `ctxmenu.*`）。
///
/// **本数组顺序 = 展示顺序**（两处消费方共用同一份顺序，改这里两处同时生效）：
/// ① 设置页勾选格的排列；② `shell-menu.json` 的 `items` 落盘顺序 → DLL 按该顺序
/// 渲染子菜单 ⇒ 资源管理器里看到的顺序。用户要求「用 hzip 打开」置顶——它是最
/// 常用的动作，压在解压组后面每次要点三下。
pub const CTX_EXTRACT_KEYS: [(&str, &str); 7] = [
    ("open_hzip", "ctxmenu.open_hzip"),
    ("extract_here", "ctxmenu.extract_here"),
    ("auto_extract", "ctxmenu.auto_extract"),
    ("extract_to", "ctxmenu.extract_to"),
    ("extract_options", "ctxmenu.extract_options"),
    ("extract_here_del", "ctxmenu.extract_here_del"),
    ("extract_dir_del", "ctxmenu.extract_dir_del"),
];

/// 上下文压缩菜单 key 白名单（README §2.3 契约；字段语义同上）。
pub const CTX_COMPRESS_KEYS: [(&str, &str); 4] = [
    ("add_zip", "ctxmenu.add_zip"),
    ("add_tar", "ctxmenu.add_tar"),
    ("add_7z", "ctxmenu.add_7z"),
    ("add_archive", "ctxmenu.add_archive"),
];

// 冲突策略 / 代码页的 label↔value 唯一来源：`crate::app::labels`（T-SET2 收口——
// 此前 contracts 里另有一份大小写/取值都不同的镜像表，已删）。
// 注册表项：`SettingId::ExtractConflict` / `SettingId::ExtractCodepage`。

// —— R2 契约补丁（T-G50 §3.4；R2-W2 五卡共用，只读）——

/// 浏览表格列（T-G53 渲染 / T-G54 设置；持久化于 hzip-config）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseColumnId {
    Name,
    Type,
    Size,
    Packed,
    Encoding,
    Mtime,
    /// 压缩级别（默认隐藏）
    Level,
    /// 加密算法（默认隐藏）
    Encryption,
    /// CRC 校验（默认隐藏）
    Crc,
    /// 文件属性（默认隐藏）
    Attrs,
}

impl BrowseColumnId {
    /// 默认全序（06 §4.1 列序 + R-GUI4 #2 类型列居名称后 + 2026-09-25 四列追加）。
    pub const ALL: [Self; 10] = [
        Self::Name,
        Self::Type,
        Self::Size,
        Self::Packed,
        Self::Encoding,
        Self::Mtime,
        Self::Level,
        Self::Encryption,
        Self::Crc,
        Self::Attrs,
    ];

    /// 默认隐藏列（2026-09-25 用户口径：四列默认不显示——设置页手动开启）。
    pub const DEFAULT_HIDDEN: [Self; 4] = [Self::Level, Self::Encryption, Self::Crc, Self::Attrs];

    /// 列身份 key（config 落盘值 + gpui 元素 id 片段；**永不本地化**——
    /// 切语言不得让 `[ui.columns]` 读不回来、元素 id 漂移）。
    pub fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Type => "type",
            Self::Size => "size",
            Self::Packed => "packed",
            Self::Encoding => "encoding",
            Self::Mtime => "mtime",
            Self::Level => "level",
            Self::Encryption => "encryption",
            Self::Crc => "crc",
            Self::Attrs => "attrs",
        }
    }

    /// 列头文案（本地化展示名；zh-CN 与改造前逐字节一致）。
    pub fn label(self) -> String {
        match self {
            Self::Name => crate::tr("col.name"),
            Self::Type => crate::tr("col.type"),
            Self::Size => crate::tr("col.size"),
            Self::Packed => crate::tr("col.packed"),
            Self::Encoding => crate::tr("col.encoding"),
            Self::Mtime => crate::tr("col.mtime"),
            Self::Level => crate::tr("col.level"),
            Self::Encryption => crate::tr("col.encryption"),
            Self::Crc => crate::tr("col.crc"),
            Self::Attrs => crate::tr("col.attrs"),
        }
    }

    /// 列排序键（列头点击 → `sort_browse_rows` 消费；全列可排）。
    pub fn sort_column(self) -> SortColumn {
        match self {
            Self::Name => SortColumn::Name,
            Self::Type => SortColumn::Type,
            Self::Size => SortColumn::Size,
            Self::Packed => SortColumn::Packed,
            Self::Encoding => SortColumn::Encoding,
            Self::Mtime => SortColumn::Mtime,
            Self::Level => SortColumn::Level,
            Self::Encryption => SortColumn::Encryption,
            Self::Crc => SortColumn::Crc,
            Self::Attrs => SortColumn::Attrs,
        }
    }
}

/// 列设置（显隐 + 顺序；Name 列强制显示——无名称列表无意义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnsConfig {
    /// 显示顺序（含隐藏列——hidden 只影响渲染剔除）
    pub order: Vec<BrowseColumnId>,
    /// 隐藏列集合
    pub hidden: Vec<BrowseColumnId>,
}

impl Default for ColumnsConfig {
    fn default() -> Self {
        Self {
            order: Self::default_order(),
            // 2026-09-25 五列默认不显示（用户口径）；老配置无这些 key 时由
            // `columns_from_pod` 补全 order + hidden 保持一致语义
            hidden: BrowseColumnId::DEFAULT_HIDDEN.to_vec(),
        }
    }
}

impl ColumnsConfig {
    fn default_order() -> Vec<BrowseColumnId> {
        BrowseColumnId::ALL.to_vec()
    }

    /// 渲染序列：按 order 排列、剔除 hidden、Name 强制保留。
    pub fn visible(&self) -> Vec<BrowseColumnId> {
        self.order
            .iter()
            .copied()
            .filter(|c| *c == BrowseColumnId::Name || !self.hidden.contains(c))
            .collect()
    }
}

/// 工具栏动作派发闭包（UI → AppShell `dispatch_toolbar`；T-G50 布局件注入口）。
pub type ToolbarEmit =
    std::rc::Rc<dyn Fn(&ToolbarAction, &mut gpui_kit::Window, &mut gpui_kit::App)>;

/// 支持打开的扩展名清单（T-G52 选择器过滤 + 右键菜单「档案判定」能力集）。
///
/// 来源 = [`hzip_format::exts::FORMAT_EXTS`] 表 ∩ 本构建**实际注册**的 handler
/// （feature 裁剪时不广告未编入的格式）；含 `tgz`/`tbz2`/`txz`/`tzst` 等别名与
/// `tar.gz` 这类复合写法。小写、去重、排序稳定。
pub fn supported_open_extensions(engine: &Engine) -> Vec<String> {
    table_exts(engine, false)
}

/// 可做 **Windows 文件关联**的扩展名（文件关联页候选清单）。
///
/// 与 [`supported_open_extensions`] 同源，但只保留单点扩展名（Windows 关联
/// 以最后一个点为准，`tar.gz` 实际走 `.gz`），故候选里是 `tgz` 而非 `tar.gz`。
pub fn associable_extensions(engine: &Engine) -> Vec<String> {
    table_exts(engine, true)
}

/// 表 → 扩展名清单；`assoc_only` = 只要可关联的单点扩展名。
fn table_exts(engine: &Engine, assoc_only: bool) -> Vec<String> {
    let registry = engine.registry();
    let mut out: Vec<String> = hzip_format::exts::FORMAT_EXTS
        .iter()
        .filter(|e| {
            hzip_format::exts::components(e.format)
                .all(|c| registry.handler(c).is_some_and(|h| h.caps().read))
        })
        .flat_map(|e| e.exts.iter().copied())
        .filter(|x| !assoc_only || !x.contains('.'))
        .map(str::to_lowercase)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 预览视图种类。
///
/// 消费方：T-G12（三视图渲染）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewView {
    /// 文本（虚拟滚动行渲染）
    Text,
    /// 图片（RGBA8 → ImageSource）
    Image,
    /// HEX（16 字节/行）
    Hex,
}

/// 预览载荷（独立窗口输入；T-G30 从 core preview 数据构造）。
///
/// 消费方：T-G12。
///
/// # 为什么是 `Arc`（2026-09-24 UI 线程阻塞审计 B16/B17）
///
/// 预览窗**每次重绘**都调 `view_element()`：旧实现那里 `self.payload.text.clone()`
/// / `hex_lines.clone()` 是**整表 String 克隆**（HEX 4 MB 载荷 ≈ 256k 行 ≈ 12 MB
/// 副本/帧），图片则每帧 `rgba.clone()` + 重建 `RenderImage`（≈8 MB 拷贝 + BGRA
/// swap）。现在：文本/HEX 行是 `Arc<Vec<String>>`（渲染只克隆引用），图片
/// **在载荷构造时（后台线程）就封成 `Arc<RenderImage>`**，渲染只是引用。
#[derive(Debug, Clone)]
pub struct PreviewPayload {
    /// 条目名（窗口标题）
    pub entry_name: String,
    /// 初始视图
    pub view: PreviewView,
    /// 文本行（已按行切分——虚拟滚动；Arc 共享，重绘零克隆）
    pub text: std::sync::Arc<Vec<String>>,
    /// 预解码图片（已封好的 gpui `RenderImage`；None = 无法解码/非图片）
    pub image: Option<std::sync::Arc<gpui_kit::RenderImage>>,
    /// HEX 行（每行 16 字节：偏移 + hex + ASCII；Arc 共享）
    pub hex_lines: std::sync::Arc<Vec<String>>,
    /// 是否截断（文本 256 KB / 图片 8 MB / HEX 4 KB，02 §9.10）
    pub truncated: bool,
    /// 完整大小人类可读
    pub total_size: String,
}

/// 预览事件（预览窗口 → 主窗口）。
///
/// 消费方：T-G12（发出）；T-G30（OpenExternal → hzip_platform 系统打开）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewEvent {
    /// 切换视图
    Switch(PreviewView),
    /// 外部打开
    OpenExternal,
    /// 关闭窗口
    Close,
}

/// `VmSignal` 跨线程回投载体（T-G13/G21 与 T-G30 共用；机制 = Spike#7 结论）。
///
/// 注意：`VmSignal` 实际路径为 `crate::viewmodel::VmSignal`（viewmodel/mod.rs 再导出）。
pub type SignalTx = smol::channel::Sender<VmSignal>;

/// 文件关联的程序标识（Windows ProgId，双击打开的落点）。
///
/// **单一来源在平台层**：[`hzip_platform::assoc::PROG_ID`]——那里同时被
/// 「写注册表」与「回读实况判 `ours`」使用，两处各写一份字符串迟早对不上，
/// 而症状（状态行说已生效/实际是别人打开）极难排查。Linux（`hzip.desktop`）与
/// macOS（bundle id）的后端自持标识，不用这个常量。
pub use hzip_platform::assoc::PROG_ID;

// —— 关联 / 右键菜单**系统状态缓存**（2026-09-24 UI 线程阻塞审计 B7/B10）——
//
// 这两项都要问操作系统（Windows 注册表；macOS 起 `pluginkit` 子进程，秒级），
// 而它们被「设置页装配」与「设置页 `reload`」两处同步调用——`reload` 会在每次
// 设置生效 / 切语言时跑。故加 TTL 缓存：命中即返回（零系统调用），未命中才真查
// 并写入缓存；应用/清除右键菜单后显式失效 + 后台预热。
//
// 首查仍在调用线程（面板需要值才能建行）；靠 `warm_assoc_status_cache` 在启动期
// 与失效期用**后台线程**预热，让常态路径（首次打开设置页）命中热缓存。

/// 缓存条目：`(写入时刻, 右键菜单状态, 各扩展名关联态, 扩展名集键)`。
type AssocCacheEntry = (
    std::time::Instant,
    crate::app::state::CtxMenuStatus,
    Vec<(String, hzip_platform::assoc::ExtAssocState)>,
    String,
);

/// 缓存有效期（超过即视为陈旧，由下次查询刷新）。
const ASSOC_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// 系统状态缓存（`None` = 尚未查询）。
static ASSOC_STATUS_CACHE: std::sync::Mutex<Option<AssocCacheEntry>> = std::sync::Mutex::new(None);

/// 扩展名集键（顺序无关：先排序再拼，避免配置里的顺序变化误判为陈旧）。
fn assoc_exts_key(exts: &[String]) -> String {
    let mut v: Vec<&str> = exts.iter().map(|e| e.as_str()).collect();
    v.sort_unstable();
    v.join(",")
}

/// 命中缓存的右键菜单状态（TTL 内且非空）。
pub fn cached_ctx_menu_status() -> Option<crate::app::state::CtxMenuStatus> {
    let c = ASSOC_STATUS_CACHE.lock().ok()?;
    let (at, status, _, _) = c.as_ref()?;
    (at.elapsed() < ASSOC_CACHE_TTL).then(|| status.clone())
}

/// 命中缓存的各扩展名关联态（TTL 内 + 扩展名集键一致）。
pub fn cached_assoc_ext_states(
    exts: &[String],
) -> Option<Vec<(String, hzip_platform::assoc::ExtAssocState)>> {
    let c = ASSOC_STATUS_CACHE.lock().ok()?;
    let (at, _, states, key) = c.as_ref()?;
    (at.elapsed() < ASSOC_CACHE_TTL && *key == assoc_exts_key(exts)).then(|| states.clone())
}

/// 写缓存（查询完成后调用）。
fn store_assoc_status(
    status: crate::app::state::CtxMenuStatus,
    states: Vec<(String, hzip_platform::assoc::ExtAssocState)>,
    exts: &[String],
) {
    if let Ok(mut c) = ASSOC_STATUS_CACHE.lock() {
        *c = Some((
            std::time::Instant::now(),
            status,
            states,
            assoc_exts_key(exts),
        ));
    }
}

/// 显式失效（应用/清除右键菜单/关联后调用——那之后系统实况已变）。
pub fn invalidate_assoc_status_cache() {
    if let Ok(mut c) = ASSOC_STATUS_CACHE.lock() {
        *c = None;
    }
}

/// **后台**预热缓存（启动期 / 失效后调用；查询本身可能秒级——macOS `pluginkit`）。
pub fn warm_assoc_status_cache(cfg_exts: Vec<String>) {
    let spawned = std::thread::Builder::new()
        .name("hzip-assoc-warm".into())
        .spawn(move || {
            let status = query_ctx_menu_status();
            let states = assoc_ext_states(&cfg_exts);
            store_assoc_status(status, states, &cfg_exts);
        });
    if let Err(e) = spawned {
        tracing::warn!("关联状态预热线程创建失败（下次查询退回同步）: {e}");
    }
}

/// 已勾选格式的双击实况（关联页状态行数据源）。
///
/// 三平台各有采集途径（Windows 注册表三处；Linux `mimeapps.list`；macOS
/// `LSHandlers`——后者会起子进程读偏好），故调用方**在 AppData 锁外**调用
/// （同 [`query_ctx_menu_status`] 的取舍），并靠 [`warm_assoc_status_cache`]
/// 在后台预热。
pub fn assoc_ext_states(exts: &[String]) -> Vec<(String, hzip_platform::assoc::ExtAssocState)> {
    // 批量入口（T-OPT B5.3）：macOS 后端一次 `plutil` 快照内逐扩展判定，
    // 旧实现逐扩展调 `ext_state` = N 对子进程；Windows/Linux 语义不变。
    hzip_platform::assoc::ext_states(exts)
}
/// `VmSignal` 接收端（`spawn_ui_forwarder` 消费侧）。
pub type SignalRx = smol::channel::Receiver<VmSignal>;

/// `ConflictDecision` re-export（对话框事件不需要 VM 依赖——README §2.3）。
pub use hzip_core::jobs::ConflictDecision;

/// 行选择上行载荷（T-SEL 走查 #1：双通道——`..` 行/隐式目录行的
/// `entry_id` 为 None，隐式目录按**档案内路径**选择；两集整组替换 VM）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionUpdate {
    /// 实体行条目 id 集（升序）
    pub ids: Vec<u64>,
    /// 隐式目录行路径集（升序）
    pub dirs: Vec<String>,
}

/// Shell 回调注入口（T-G30 在 `AppShell::new` 处填充；T-G10 自测用 no-op）。
///
/// 渲染输入经 [`AppShell::update_from_state`]（app.rs）；本结构只承载动作上行。
pub struct ShellHandlers {
    /// 工具栏/菜单动作
    pub on_toolbar: Box<dyn Fn(ToolbarAction)>,
    /// 页签切换
    pub on_tab: Box<dyn Fn(MainTab)>,
    /// 面包屑段点击（seg = 目标目录）
    pub on_crumb: Box<dyn Fn(Crumb)>,
    /// 对话框事件
    pub on_dialog_event: Box<dyn Fn(DialogEvent)>,
    /// 预览事件（外部打开/关闭转发主窗口）
    pub on_preview_event: Box<dyn Fn(PreviewEvent)>,
    /// 浏览行选择变化（双通道载荷——T-SEL 走查 #1：实体 id + 隐式目录路径）
    pub on_selection_changed: Box<dyn Fn(SelectionUpdate)>,
    /// 浏览行激活（双击/回车；path = 行路径）
    pub on_row_activate: Box<dyn Fn(String)>,
    /// 目录树行点击（进入目录）
    pub on_tree_click: Box<dyn Fn(String)>,
    /// 目录树行箭头（展开/收起）
    pub on_tree_toggle: Box<dyn Fn(String)>,
    /// 「仅目录」开关
    pub on_dirs_only: Box<dyn Fn(bool)>,
    /// 浏览过滤输入变化（T-MENU1 查找；空串 = 清除过滤）
    pub on_filter_changed: Box<dyn Fn(String)>,
}

impl std::fmt::Debug for ShellHandlers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellHandlers").finish_non_exhaustive()
    }
}

impl Default for ShellHandlers {
    /// 全 no-op 默认（T-G10 自测 + headless 测试用）。
    fn default() -> Self {
        Self {
            on_toolbar: Box::new(|_| {}),
            on_tab: Box::new(|_| {}),
            on_crumb: Box::new(|_| {}),
            on_dialog_event: Box::new(|_| {}),
            on_preview_event: Box::new(|_| {}),
            on_selection_changed: Box::new(|_| {}),
            on_row_activate: Box::new(|_| {}),
            on_tree_click: Box::new(|_| {}),
            on_tree_toggle: Box::new(|_| {}),
            on_dirs_only: Box::new(|_| {}),
            on_filter_changed: Box::new(|_| {}),
        }
    }
}

/// 行内名称编辑态的渲染投影（[`crate::app::state::NameEdit`] 的 UI 面）。
///
/// 只带渲染需要的字段：目标目录 + 源条目名（输入框实体在 shell 侧）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEditView {
    /// 目标所在目录（档案内路径；空串 = 档案根）
    pub dir: String,
    /// 改名源条目名（末段；`None` = 新建文件夹草稿行）
    pub from: Option<String>,
}

/// 主窗口 UI 快照（T-G30 持 AppData 锁后导出；T-G10 `update_from_state` 应用）。
///
/// T-G00 骨架态字段集 = T-G10 §3.3 要求（status/tab/busy/archive_name/dialog_open）
/// + T-G20/T-G21 渲染输入（rows/crumbs/job_rows）；W3/W4 发现缺口提请 T-G0x 补丁。
#[derive(Debug, Clone, Default)]
pub struct AppStateUiSnapshot {
    /// 当前页签
    pub tab: Option<MainTab>,
    /// busy 指示（后台打开/退出宽限）
    pub busy: bool,
    /// busy 文案（状态条）
    pub busy_text: String,
    /// 档案名（标题栏 + 面包屑根；空 = 未打开）
    pub archive_name: String,
    /// 档案完整路径（面包屑右端/标题 tooltip）
    pub archive_path: String,
    /// 当前目录（面包屑派生源）
    pub current_dir: String,
    /// 手动写回模式下**挂起的改动条数**（0 = 没有；浏览页据此显隐「保存」按钮）。
    pub pending_count: usize,
    /// 当前是否是**手动写回**模式（`ui.write_back_immediate = false`）。
    ///
    /// 按钮按它**常驻**：只在「有挂起」时出现的话，用户看到通知去找按钮却找不到
    /// （2026-10-19 实测两次），常驻 + 计数 0/ n 才是可发现的状态。
    pub write_back_manual: bool,
    /// 是否处于**嵌套会话**（双击容器进入的层级 > 0）。
    ///
    /// 右栏 `` `..` `` 行的语义随之不同：嵌套会话的档案根再往上 = **退出该层**
    /// （回到外层档案），而顶层会话的档案根往上无处可去（no-op）。
    pub nested: bool,
    /// 面包屑段
    pub crumbs: Vec<Crumb>,
    /// 左栏树行快照（nav_rows；T-PERF4 Arc 共享——快照投影是 refcount 克隆
    /// 而非深拷贝，泵每周期导出不再付出 O(行集) 克隆成本）
    pub nav_rows: std::sync::Arc<Vec<crate::viewmodel::rows::TreeRow>>,
    /// 右栏当前目录行快照（dir_rows → binding 派生 BrowseRow；T-PERF4 Arc）
    pub dir_rows: std::sync::Arc<Vec<crate::viewmodel::rows::TreeRow>>,
    /// 条目总数
    pub entry_count: usize,
    /// 已选数
    pub selection_count: usize,
    /// 已选条目 ID 集（T-TVS3：icon/list 模式选中高亮回显；与
    /// `selection_count` 同源于 `MainViewModel::selection`）
    pub selection: Vec<u64>,
    /// 已选隐式目录路径集（T-SEL 走查 #1：目录行高亮第二数据源——
    /// 与 `selection` 互补构成完整行选择）
    pub selection_dirs: Vec<String>,
    /// 选中统计（T-SEL2 状态栏：selection ∩ 当前目录可见行——文件数/
    /// 文件夹数/原始与压缩后字节数；`selection` 全局集合的可见行交集口径）
    pub sel_stats: SelectionStats,
    /// 可疑编码计数
    pub suspect_count: usize,
    /// 弹窗显隐（T-G30 接线驱动）
    pub dialog_open: Option<DialogKind>,
    /// 任务中心行快照（JobRow → binding 派生 JobsRowUi 由 T-G21 承接；
    /// T-PERF4 Arc——进度 tick 高频导出零深拷贝）
    pub job_rows: std::sync::Arc<Vec<crate::viewmodel::JobRow>>,
    /// 状态栏任务胶囊（第一条运行中任务；None = 隐藏）
    pub job_pill: Option<JobPill>,
    /// `..` 跳上级行显隐（ui.show_dotdot；R2-04 消费）
    pub show_dotdot: bool,
    /// 浏览页左树显隐（ui.show_tree；T-MENU5 菜单/设置双入口）
    pub show_tree: bool,
    /// 任务中心页签显隐（ui.show_jobs；默认 false = 只有浏览页、无页签栏）
    pub show_jobs: bool,
    /// 工具栏三态下标（TOOLBAR_STYLE：icon_text/icon_only/hidden；T-TVS2）
    pub toolbar_style_ix: usize,
    /// 视图样式下标（VIEW_STYLE：detail/icon/list；icon/list 渲染归 T-MENU9）
    pub view_style_ix: usize,
    /// 浏览列配置（ui.columns；浏览页**唯一**列设置来源——T-SET 修复：
    /// 旧实现经 `columns_dirty` 旗标 + `sync_browse_hold` 旁路写 Global，
    /// 冷启动不回放）
    pub columns: ColumnsConfig,
    /// 请求-应答弹窗待决（router 投影；R-GUI4 #1）
    pub dialog: Option<UiDialogPrompt>,
    /// 当前档案是否可编辑（`AppData::caps_edit` 投影，**已记忆化**）。
    ///
    /// 为什么进快照（T-PERF5）：`caps_edit()` 要**嗅探档案**（开文件读头），而
    /// 菜单投影是**每帧**构建的——每帧一次文件 IO 在慢盘/网络盘上直接表现为
    /// 界面卡顿。快照由泵按信号批次导出（不是每帧），且 `AppData` 侧按档案路径
    /// 记忆化，故一次打开只嗅探一次。
    pub caps_edit: bool,
    /// 行内名称编辑态（重命名/新建文件夹；用户口径 2026-09-28）。
    ///
    /// 渲染侧据此把目标行的名称换成输入框（输入框实体在 shell 侧持有）；
    /// `None` = 无编辑。
    pub name_edit: Option<NameEditView>,
    /// 最近打开列表（菜单 MRU 投影；渲染路径不再为此锁 AppData）
    pub mru: Vec<String>,
    /// 内部剪贴板非空（菜单「粘贴」可用性投影）。
    ///
    /// 为什么进快照（2026-09-24 审计 B14）：它此前在**每帧**渲染里查一次
    /// （`hzip_platform::clipboard::internal_is_empty()`——虽只是进程内 Mutex，
    /// 但渲染帧不该有任何跨模块查询）。
    pub has_clipboard: bool,
}

/// 选中集统计（T-SEL2 状态栏）。
///
/// 口径：`MainViewModel::selection`（全局集合）与当前目录可见行
/// （`dir_rows`）的**交集**——导航后的残留选中 id 不计入；目录行计入
/// `dirs` 且 `size_raw` 为目录聚合大小。`size_packed` 合计为 0 = 未计算
/// （显示层占位「—」，与列口径一致）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectionStats {
    /// 选中文件数
    pub files: usize,
    /// 选中文件夹数
    pub dirs: usize,
    /// 原始字节数合计（目录 = 聚合大小）
    pub size_raw: u64,
    /// 压缩后字节数合计
    pub size_packed: u64,
}

/// 状态栏任务胶囊（进度 + 文案；点击切任务中心页签）。
#[derive(Debug, Clone)]
pub struct JobPill {
    /// 任务 ID
    pub job_id: u64,
    /// 文案（解压 demo.zip · 12/408 条）
    pub summary: String,
    /// 进度 0..1
    pub progress: f32,
}

/// 请求-应答弹窗待决投影（router → UI；R-GUI4 #1：解压 decision-timeout 根因
/// ——泵把密码/冲突请求收进 router 后 UI 半场从未接线，120 s 无人应答）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiDialogPrompt {
    /// 密码请求（target None = 整档/头加密）
    Password {
        /// 任务 ID
        job_id: u64,
        /// 目标条目名
        target: Option<String>,
        /// 重试次数（0 = 首问）
        attempts: u8,
    },
    /// 冲突请求（目标路径 + 现存条目）
    Conflict {
        /// 任务 ID
        job_id: u64,
        /// 待写路径（展示）
        path: String,
        /// 现存是否目录
        existing_is_dir: bool,
        /// 现存大小（目录为 0）
        existing_size: u64,
    },
}

impl UiDialogPrompt {
    /// 弹窗身份（新弹窗判定：身份变化 = 旧关新开）
    pub fn identity(&self) -> String {
        match self {
            Self::Password {
                job_id,
                target,
                attempts,
            } => {
                format!("pw:{job_id}:{}:{attempts}", target.as_deref().unwrap_or(""))
            }
            Self::Conflict { job_id, path, .. } => format!("cf:{job_id}:{path}"),
        }
    }
}

#[cfg(test)]
#[path = "tests/contracts.rs"]
mod tests;
