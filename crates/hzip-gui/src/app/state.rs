//! AppData：UI 数据面（T-G30 独占实体；骨架态字段集与备份 state.rs 等价、
//! 全部旧框架类型引用剔除）。
//!
//! # 与备份（T42 state.rs）的差异
//!
//! - 原框架专属类型引用（ComponentHandle/SharedString 等）删除（零旧框架耦合）；
//! - 原 `make_notifier` / `WinRefs` **不恢复**——回投由 contracts
//!   [`SignalTx`](crate::ui::contracts::SignalTx) 承接（T-G13/G21 桥）；
//! - 方法体（快照导出/事件消费）归 T-G30 实体化；骨架只保证字段集一致 + 编译通过。
//!
//! # 设置消费（T-SET §1.2 统一流程）
//!
//! [`AppData::consume_dialog_event`] 消费 `DialogEvent::SettingChanged{id,value}`
//! 与 `SettingsReset`，落到 [`AppData::apply_setting`] / [`AppData::apply_reset`]：
//! 归一化 → 写 Config 节 + 镜像 → **恰好一次** `Config::save()` → 分发 Effect
//! （数据半场 + `VmSignal::SettingApplied` UI 半场）。写盘失败仅记 tracing warn，
//! 不 panic/unwrap；主题即时生效在持有 Window 的 UI 半场（runtime 泵消费信号）。
//!
//! 设置项清单/默认值/归一化策略全部在 [`crate::app::settings_registry`]——
//! 本文件只承载数据面（镜像字段 + 五步流程的编排）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::app::settings_registry;
use crate::ui::contracts::{ColumnsConfig, DialogEvent};
use crate::viewmodel::SettingId;
use crate::viewmodel::dialog_vm::{CompressDialogVm, ExtractDialogVm, SelectionArg};
use crate::viewmodel::jobs_vm::JobsViewModel;
use crate::viewmodel::main_vm::MainViewModel;
use crate::viewmodel::router::DialogRouter;
use crate::viewmodel::rows::TreeRow;
use hzip_core::engine::Engine;

/// 会话切换**原子闸**（一次只放行一个；用户口径 2026-09-24）。
/// 「会话切换」= 打开档案 / 进入容器 / 展开延迟层 / 弹出层——都是**有状态**的动作：
/// 两个并发跑会各自压栈、各自换会话，落账互相覆盖（第二个的 `begin_nested_session`
/// 与第一个的 `d.main = child` 交错 → 栈里多一层，会话却只有一个，`..` 退出来就
/// 对不上）。用户对此的原话是「加原子锁避免多次双击导致多次触发」。
///
/// 语义是**互斥**不是**排队**：拿到闸的跑，拿不到的**直接忽略**（连点两下想要的是
/// 「快点」，不是「开两次」）。闸票是 RAII 的——工作闭包持票，跑完（成功/失败/panic）
/// 随 `Drop` 放行。
#[derive(Clone, Default)]
pub struct SessionSwitchGate(Arc<std::sync::atomic::AtomicBool>);

impl SessionSwitchGate {
    /// 取闸：`None` = 已有会话切换在飞（调用方静默返回，别排队）。
    pub fn try_acquire(&self) -> Option<SessionSwitchGuard> {
        self.0
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok()
            .map(|_| SessionSwitchGuard(self.0.clone()))
    }

    /// 是否有会话切换在飞（诊断/测试面）。
    pub fn is_busy(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// 「换会话」目标：`key` = 去重/顶替判据（档案路径或 `路径::条目`），
/// `label` = 该流程进度窗的展示名（顶替时据此关掉旧窗）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchTarget {
    /// 去重与顶替判据
    pub key: String,
    /// 进度窗展示名（文件名 / 条目名）
    pub label: String,
}

/// 「换会话」发起结果（见 [`AppData::begin_session_switch`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchStart {
    /// 同目标已有在飞流程（连点）：调用方直接忽略本次触发。
    Duplicate,
    /// 获准发起。`epoch` 带进后台流程（落账前比对）；`superseded` = 被本次顶替的
    /// 旧目标**展示名**（调用方据此关掉旧目标的「正在打开」窗）。
    Started {
        /// 本次流程的代数
        epoch: u64,
        /// 被顶替的旧目标展示名（若有）
        superseded: Option<String>,
    },
}

/// 闸票（RAII）：`Drop` 即放行——早退、`?`、panic 展开都不会漏放。
pub struct SessionSwitchGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for SessionSwitchGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

// —— MRU（T-MENU4）：最近打开档案纯函数面（单测锚点） ——

/// MRU 容量（设计 §2.1：LRU cap 10）。
pub const MRU_CAP: usize = 10;

/// 追加路径到 MRU（去重置顶 + cap 裁尾）。
///
/// `paths` 为**规范字符串**（调用方负责统一分隔符/大小写策略——Windows
/// 路径大小写不敏感，这里按原样比较，与 `ui.recent_archives` 落盘值一致）。
pub fn mru_append(paths: &[String], path: &str) -> Vec<String> {
    let mut out: Vec<String> = paths
        .iter()
        .filter(|p| p.as_str() != path)
        .cloned()
        .collect();
    out.insert(0, path.to_string());
    out.truncate(MRU_CAP);
    out
}

/// 从 MRU 移除（文件失效/清空单项语义）。
pub fn mru_remove(paths: &[String], path: &str) -> Vec<String> {
    paths
        .iter()
        .filter(|p| p.as_str() != path)
        .cloned()
        .collect()
}

// —— 编辑变更集（T-MENU6/7/8）：纯函数面（单测锚点） ——

/// 选中条目 ID → 删除变更集（T-MENU7）。
///
/// `ids` 源自 `MainViewModel::selection()`（`BTreeSet<EntryId>` 投影）——
/// 天然去重且升序：同一选择集重复组装产出**逐字节相同**的 `ChangeSet`
/// （「重复执行幂等」的机制保证，而非依赖调用方纪律）。
pub fn delete_changeset(ids: &[hzip_format::EntryId]) -> hzip_format::ChangeSet {
    hzip_format::ChangeSet {
        changes: ids
            .iter()
            .copied()
            .map(hzip_format::Change::Delete)
            .collect(),
    }
}

/// 本地文件 → 追加变更集（T-MENU6/8）；返回 `(变更集, 同名冲突数)`。
///
/// `entries` = `(源路径, 档案内目标名)` 二元组：目标名由调用方组装
/// （可含 `/` 目录前缀——粘贴目录源按相对结构落位，见 shell 侧
/// `collect_paste_entries`；「添加文件」按 `paste_target_name(current_dir,
/// file_name)` 组装），本函数**不再**从 `file_name()` 取末段。
/// 冲突**不在此处裁决**：只统计，交由确认对话框（替换/跳过/取消）决定；
/// 与 `existing` 同名的条目在 `replace = true` 时按「先前删同名再追加」
/// 组装（`Change::Replace` 需 `EntryId`，跨格式不可用——Append 后置即可
/// 由 fmt 层覆盖同名旧条目）。
///
/// # 为什么是「判存闭包」而不是 `&[String]`（2026-09-24 审计 B18）
///
/// 调用点在 UI 线程且持 AppData 锁：旧签名要求调用方先把**全部条目名**克隆成
/// `Vec<String>`（大档案几十万条 ⇒ 每次添加/粘贴都白抄一遍）。改成闭包后由调用方
/// 直接借用 VM 内部条目名判存，零克隆。
///
/// # 目标名冲突口径（双向粘贴 P0）
///
/// 冲突按**目标全名**比较（含目录前缀）：`docs/a.txt` 与现有 `docs/a.txt`
/// 计冲突，与根下 `a.txt` 不计——判存语义 = 「提交后会不会覆盖已有条目」，
/// 只能看落位全名。无名条目（目标名为空串）跳过，不进变更集也不计冲突。
pub fn append_changeset(
    entries: &[(std::path::PathBuf, String)],
    is_existing: impl Fn(&str) -> bool,
) -> (hzip_format::ChangeSet, usize) {
    let mut cs = hzip_format::ChangeSet::default();
    let mut conflicts = 0usize;
    for (src, name) in entries {
        if name.is_empty() {
            continue;
        }
        if is_existing(name) {
            conflicts += 1;
        }
        cs.changes.push(hzip_format::Change::Append {
            src: src.clone(),
            name: hzip_error::EntryName::new(name.clone()),
        });
    }
    (cs, conflicts)
}

/// 粘贴目标名组装纯函数（T-MENU8 双向粘贴 P0；单测锚点）。
///
/// `(源路径, 源相对名)` → 档案内目标名：`current_dir` 非空时拼
/// `{current_dir}/{rel}`（分隔符与档案内条目名一致 = `/`，VM `current_dir`
/// 口径），根时 = `rel` 本身。**不**以 `Path::join` 组装——Windows 下会产出
/// `\` 分隔，与档案内 `/` 命名相撞。
pub fn paste_target_name(current_dir: &str, rel: &str) -> String {
    if current_dir.is_empty() {
        rel.to_string()
    } else {
        format!("{current_dir}/{rel}")
    }
}

/// 设置偏好快照（AppData 设置字段聚合；T-G54 面板 props 构造输入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPrefs {
    /// 主题下标（ThemeModeChoice 白名单）
    pub theme_ix: usize,
    /// 主题色 hex（`#RRGGBB`；空 = 主题默认——T-SEL 走查 #3；泵经
    /// `SettingApplied(ThemeColor)` 触发 `theme::apply_theme_replay`，值由此镜像取——
    /// 不读盘，见 `runtime::theme_replay_of`）
    pub theme_color: String,
    /// 语言下标（`labels::LANG` 白名单；`Effect::Locale` 经泵重设全局 locale）
    pub lang_ix: usize,
    /// 默认格式下标（FORMAT_LABELS 白名单）
    pub format_ix: usize,
    /// 默认级别下标（LEVEL_LABELS 白名单）
    pub level_ix: usize,
    /// 浏览列设置
    pub columns: ColumnsConfig,
    /// 显示 `..` 跳上级（ui.show_dotdot）
    pub show_dotdot: bool,
    /// 自动更新（ui.auto_update；检查逻辑挂账 T43）
    pub auto_update: bool,
    /// 文件关联勾选集合（ui.assoc_formats）
    pub assoc: Vec<String>,
    /// 上下文解压菜单勾选 key（ui.ctx_extract）
    pub ctx_extract: Vec<String>,
    /// 上下文压缩菜单勾选 key（ui.ctx_compress）
    pub ctx_compress: Vec<String>,
    /// 目录行聚合大小（ui.show_folder_size）
    pub show_folder_size: bool,
    /// 图像查看器路径（ui.image_viewer；空 = 系统默认）
    pub image_viewer: String,
    /// 编辑器路径（ui.editor；空 = 系统默认）
    pub editor: String,
    /// Shell 扩展 DLL 路径（ui.shell_ext_dll；空 = 主程序同目录的
    /// hzip-shell.dll——右键菜单注册 InProcServer32 的查找路径，T-SEL
    /// 走查 #14：DLL 与主程序分目录部署时经配置指定）
    pub shell_ext_dll: String,
    /// 浏览页左树显隐（ui.show_tree；T-MENU5）
    pub show_tree: bool,
    /// 任务中心页签显隐（ui.show_jobs；默认 false = 只有浏览页、无页签栏）
    pub show_jobs: bool,
    /// 工具栏三态下标（TOOLBAR_STYLE 白名单：large/small/hidden；T-MENU5）
    pub toolbar_style_ix: usize,
    /// 视图样式下标（VIEW_STYLE 白名单：detail/icon/list；T-MENU5/T-MENU9）
    pub view_style_ix: usize,
    /// 界面字体族（ui.font_family；空 = 主题默认）。泵经
    /// `SettingApplied(FontFamily)` 重放全局 Theme（字体族随 `Root` 下发）
    pub font_family: String,
    /// 界面字号 px（ui.font_size，已归一 10~24）。泵经
    /// `SettingApplied(FontSize)` 重设各窗口 rem 基准
    pub font_size: u32,
    /// 界面圆角弧度 px（ui.corner_radius，已归一 0~20）。泵经
    /// `SettingApplied(CornerRadius)` 重放全局 Theme（`radius`/`radius_lg`）
    pub corner_radius: u32,
    /// 布局密度下标（`labels::DENSITY` 白名单）。泵经
    /// `SettingApplied(Density)` 写 [`crate::ui::spacing::set_density`]
    pub density_ix: usize,
    /// 文件类型图标来源下标（`labels::FILE_ICONS` 白名单：0 = system、
    /// 1 = builtin）。泵经 `SettingApplied(FileTypeIcons)` 调
    /// [`crate::ui::widgets::file_icon::set_enabled`]
    pub file_type_icons_ix: usize,
    /// 快捷键绑定（`[keys]` 整节；T-KEYS）。
    ///
    /// 泵经 `SettingApplied(Keymap)` 重装绑定，**取值应从本镜像来**（与主题重放
    /// `theme_replay_of` 同款口径）：镜像在 `store` 步骤已就地更新，是「本进程
    /// 当前生效态」；盘上那份由异步写线程（`save_async`）稍后落定，读盘等于把
    /// 生效面挂到与落盘的赛跑上。
    pub keys: hzip_config::Keys,
}

impl Default for SettingsPrefs {
    fn default() -> Self {
        Self {
            theme_ix: 0,
            theme_color: String::new(),
            lang_ix: 0,
            format_ix: 0,
            level_ix: 3,
            columns: ColumnsConfig::default(),
            show_dotdot: true,
            auto_update: false,
            assoc: Vec::new(),
            ctx_extract: Vec::new(),
            ctx_compress: Vec::new(),
            show_folder_size: false,
            image_viewer: String::new(),
            editor: String::new(),
            shell_ext_dll: String::new(),
            show_tree: true,
            show_jobs: false,
            toolbar_style_ix: 0,
            view_style_ix: 0,
            font_family: String::new(),
            font_size: hzip_config::FONT_SIZE_DEFAULT,
            corner_radius: hzip_config::CORNER_RADIUS_DEFAULT,
            density_ix: crate::viewmodel::density::Density::Standard.index(),
            file_type_icons_ix: 0,
            // 全空 = 全部动作走内置默认键位（T-KEYS §2 三态之一）
            keys: hzip_config::Keys::default(),
        }
    }
}

/// 右键菜单勾选草稿（[`settings_registry::Commit::Manual`] 项的内存态）。
///
/// **不持久化**：`config` 里永远只存「已应用」的真相；草稿在会话内有效，
/// 退出即丢弃。这样「勾了但没点应用」不会污染下次启动的配置。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CtxMenuDraft {
    /// 解压菜单勾选（`CTX_EXTRACT_KEYS` 子集）
    pub extract: Vec<String>,
    /// 压缩菜单勾选（`CTX_COMPRESS_KEYS` 子集）
    pub compress: Vec<String>,
}

/// 右键菜单安装形态（平台 `InstallMode` 的 app 层镜像——app 层不依赖平台类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtxMenuMode {
    /// Windows 11 现代右键菜单顶层（稀疏包）
    ModernTopLevel,
    /// 传统菜单（Win10/Server；Windows 11 的「显示更多选项」）
    LegacyMenu,
    /// macOS Finder Sync 扩展
    FinderSync,
    /// macOS 服务菜单
    Services,
    /// Linux 桌面扩展 / 脚本
    Desktop,
}

/// 右键菜单安装状态（平台 `MenuStatus` 的 app 层镜像）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtxMenuStatus {
    /// 未安装
    NotInstalled,
    /// 已安装
    Installed {
        /// 生效形态
        mode: CtxMenuMode,
        /// 条目数
        items: usize,
    },
    /// 已安装但需重启文件管理器才可见
    NeedsRestart,
    /// 已安装但能力受限（如 Win11 上只有「显示更多选项」）
    Degraded {
        /// 面向用户的原因（已本地化）
        reason: String,
    },
    /// 当前环境不支持
    Unsupported {
        /// 面向用户的原因（已本地化）
        reason: String,
    },
}

/// 组合导航树的递归铺行（[`AppData::display_nav_rows`] 的实现体）。
///
/// `sessions`：`(会话, 层号, 该层要下钻的容器路径)`，自外向内排列。
/// `ix` 当前铺到哪个会话；`node` 该会话里要铺的子树；`prefix` 该会话内路径前缀。
fn emit_composite_rows(
    sessions: &[(&crate::viewmodel::MainViewModel, usize, Option<String>)],
    ix: usize,
    node: &hzip_core::services::browse::TreeNode,
    prefix: &str,
    depth: usize,
    out: &mut Vec<crate::viewmodel::rows::TreeRow>,
) {
    let (vm, level, descend_at) = &sessions[ix];
    let (level, descend_at) = (*level, descend_at.as_deref());
    for child in node.children.values() {
        let path = if prefix.is_empty() {
            child.name.to_string()
        } else {
            format!("{prefix}/{}", child.name)
        };
        let facts = child.entry.and_then(|id| vm.entry_facts(id));
        let is_dir = facts.is_none_or(|(d, _)| d);
        // 容器行判定（唯一来源 = 名字命中容器扩展名表）：组合档的内层容器层
        // （`init.cpio`）现在是 core 给的**真实条目**（延迟展开，见
        // `hzip_core::services::browse::Browser::deferred`），走的就是这条。
        let is_container =
            !is_dir && crate::viewmodel::MainViewModel::is_container_name(&child.name);
        let descend = descend_at == Some(path.as_str());
        // 树只收目录与「本层要下钻的容器」；其余文件不占位置
        if !is_dir && !descend {
            continue;
        }
        if is_dir && !vm.dir_name_hits_filter(&child.name) {
            continue;
        }
        // 展开判定：会话自己的展开集 **或** 该节点是下钻容器的祖先链
        // （后者保证「当前所在的内层」在树里一定可见——用户可能在外层把那个
        // 目录收起来了，但既然已经进去了，树就不该把它藏起来）。
        let on_descend_path = descend_at.is_some_and(|d| {
            d.len() > path.len() && d.starts_with(&path) && d.as_bytes()[path.len()] == b'/'
        });
        let expanded = vm.is_expanded(&path) || on_descend_path;
        out.push(crate::viewmodel::rows::TreeRow {
            path: path.clone(),
            name: child.name.to_string(),
            depth,
            is_dir,
            entry_id: child.entry,
            expanded,
            has_children: !child.children.is_empty() || descend,
            is_container,
            encrypted: facts.is_some_and(|(_, e)| e),
            level,
            // 格式名列：容器行由名字派生（`browse_row_from_tree_row` 同一判定）
            container_format: is_container
                .then(|| hzip_format::exts::container_handler(&child.name).map(str::to_string))
                .flatten(),
            // 组合树行只收目录/下钻容器，文件叶不进树 → 其余上浮列（大小/时间/
            // 软链接/新五列）恒空（Default 基线）
            ..Default::default()
        });
        if !expanded {
            continue;
        }
        if descend {
            // 内层压缩包：把子会话的树挂在这一层下面（层号 -1，深度 +1）
            if let Some((child_vm, _, _)) = sessions.get(ix + 1)
                && let Some(child_tree) = child_vm.tree_ref()
            {
                emit_composite_rows(sessions, ix + 1, child_tree, "", depth + 1, out);
            }
        } else {
            emit_composite_rows(sessions, ix, child, &path, depth + 1, out);
        }
    }
}

/// 嵌套打开的**父层帧**（双击容器进入下层时压栈，`..`/面包屑回退时弹栈）。
///
/// 为什么整份 `MainViewModel` 都存下来：用户从 `a.zip › sub › b.7z › docs` 退回
/// `a.zip › sub` 时，期望看到离开时的样子（当前目录、选中项、展开态、树），
/// 而不是被送回档案根。
pub struct OpenFrame {
    /// 该层自己的面包屑段（不含更外层；渲染时按层号拼接）
    pub crumbs: Vec<crate::ui::contracts::Crumb>,
    /// **进入下一层时双击的容器条目**在本层内的路径（组合树据此把子会话挂在它下面）
    pub container_path: String,
    /// 该层会话
    pub vm: crate::viewmodel::MainViewModel,
    /// 该层会话的**源档案**（嵌套层 = 从父层解出来的临时文件；弹栈时删除）
    pub temp: Option<PathBuf>,
}

/// 行内名称编辑态（用户口径 2026-09-28：右键「重命名」「新建文件夹」）。
///
/// 数据面只存**纯数据**（无 gpui 实体）：UI 侧按本状态把对应行的名称渲染成
/// 输入框，Enter/失焦提交时回读文本并调 [`AppData::commit_name_edit`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEdit {
    /// 目标所在目录（档案内路径；空串 = 档案根）
    pub dir: String,
    /// 改名源条目名（末段；`None` = 新建文件夹）
    pub from: Option<String>,
    /// 行内输入初值（改名 = 原名末段；新建 = `新建文件夹`）
    pub text: String,
    /// 进入编辑时的**会话代数**（[`AppData::session_epoch`]）：提交时不一致 =
    /// 会话已换（换了档案/进了另一层）→ 放弃，绝不把旧目标的改名落到新档案。
    pub session_epoch: u64,
}

/// 单个路径段校验（重命名/新建目录的输入）。
///
/// 只允许**一个名字段**：非空、无分隔符、不是 `.`/`..`、且经契约归一化器
/// （[`hzip_format::parse_entry_name`]）原样通过——否则拒绝（控制字符/空段等）。
fn name_is_valid_segment(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    match hzip_format::parse_entry_name(name) {
        Ok(parsed) => parsed.as_str() == name,
        Err(_) => false,
    }
}

/// 档案内路径拼接（目录为空 = 档案根）。
fn join_entry_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// UI 数据面（无窗口句柄——gpui 侧经 contracts ShellHandlers/SignalTx 通信）。
pub struct AppData {
    /// Engine 句柄
    pub engine: Arc<Engine>,
    /// 浏览数据面
    pub main: MainViewModel,
    /// 任务队列
    pub jobs: Arc<Mutex<JobsViewModel>>,
    /// 请求路由（密码/冲突）
    pub router: Arc<Mutex<DialogRouter>>,
    /// 压缩对话框参数
    pub compress: CompressDialogVm,
    /// 解压对话框参数
    pub extract: ExtractDialogVm,
    /// 当前档案路径
    pub archive: Option<PathBuf>,
    /// 外部编辑器编辑会话（双击物化 → 外部程序保存 → 写回档案）。
    ///
    /// 见 [`crate::app::external_edit`]：登记/判重/在飞串行/「不可写回」告知都在
    /// 那里，本结构只持有清单；`watch` 句柄归 `AppShell`（UI 线程半场），
    /// 避免 AppData ↔ 回调的自引用环。
    pub external_edits: Vec<crate::app::external_edit::ExternalEdit>,
    /// 解压对话框待确认对象（档案, 选择范围；确认后消费）
    pub pending_scope: Option<(PathBuf, SelectionArg)>,
    /// 编辑提交待确认变更集（T-MENU6/7 添加上传/删除确认挂起期载荷）
    ///
    /// 与 [`Self::pending_scope`] 同款：确认对话框打开期间持有，确认出口
    /// （`EditAddAccept`/`EditDeleteAccept`）提交后台 `EditBatch` 任务后清空；
    /// 取消则丢弃。放 AppData 而非 AppShell 的原因：对话框事件闭包
    /// （`Fn(DialogEvent)`）拿不到 `Context<AppShell>`，而 AppData 有句柄。
    pub pending_edit: Option<(PathBuf, hzip_format::ChangeSet)>,
    /// **行内名称编辑态**（重命名/新建文件夹；用户口径 2026-09-28）。
    ///
    /// `Some` 期间：UI 把目标行（或新建的草稿行）名称渲染成输入框；Enter/失焦
    /// → [`Self::commit_name_edit`] 提交 `EditBatch`（重写档案）；空串/Esc → 取消。
    /// 导航（进目录/换档案）会清空——旧目标的路径没有意义。
    pub name_edit: Option<NameEdit>,
    /// 编辑重开前的**视图状态**（当前目录 + 树展开集；用户口径 2026-09-29）。
    ///
    /// 编辑保存必须先释放浏览会话（Windows 原子替换前提），重开会把会话状态归零
    /// ⇒ 用户每次改完名都被踢回档案根。此处记住，`finish_browse_reopen` 种回。
    pub reopen_view_state: Option<(String, std::collections::BTreeSet<String>)>,
    /// **会话密码缓存**（用户口径 2026-09-28 / 2026-09-29；**进程级**共享）。
    ///
    /// 用户为某档案输入的密码，经核实**正确**后缓存在这里（键 = 档案路径：顶层
    /// 会话 = `archive`，嵌套会话 = 该层源档案）；同一档案后续「打开文件/解压/
    /// 编辑（新建、重命名、删除）」直接复用，不再弹密码框——即用户说的「该压缩包
    /// 处于解密中状态」。
    ///
    /// 多窗口共享同一份（`run_ui` 注入，见 [`Self::share_password_cache`]）：
    /// 用户口径是「只要 hzip-gui 程序没关闭就继续用上一次的密码」。`Zeroizing`
    /// 持有（02 §1.2），进程内**不落盘**、无过期；嵌套会话退出按路径清除。
    pub session_passwords: std::sync::Arc<crate::app::password_cache::PasswordCache>,
    /// **待验证**的任务密码（用户口径 2026-09-29）。
    ///
    /// 用户在任务链里被问到的密码（编辑保存/解压/转换的 `JobRequest::Password`
    /// 应答，或提交时参数窗已填的值），在任务**成功结束**（Done/Partial——成功即
    /// 证明密码对该档案正确）后才写进 [`Self::session_passwords`]；失败/取消一律
    /// 丢弃。这样「输入一次」就够，且**绝不把没验过的密码喂给后续操作**。
    pending_job_passwords:
        std::collections::HashMap<hzip_progress::JobId, (PathBuf, zeroize::Zeroizing<String>)>,
    /// 设置偏好（T-G54：主题/语言/默认参数/列设置；即时保存语义）
    pub settings: SettingsPrefs,
    /// 右键菜单勾选草稿（`Commit::Manual` 项；None = 无未提交改动）
    pub ctx_draft: Option<CtxMenuDraft>,
    /// 右键菜单安装状态（None = 尚未查询/未安装；由 dispatch 后台回写）
    pub ctx_menu_status: Option<CtxMenuStatus>,
    /// 嵌套打开的父层栈（空 = 当前是顶层档案会话）。
    pub open_stack: Vec<OpenFrame>,
    /// 当前会话的**源档案**是不是临时文件（嵌套层 = 是；弹栈时删除）。
    pub current_temp: Option<PathBuf>,
    /// 会话临时目录（嵌套打开把内层档案解到这儿；进程级一份，退出时清理）。
    pub temp_area: Option<std::sync::Arc<hzip_platform::temp_area::TempArea>>,
    /// 文件关联系统侧**最后一次失败**的原因（None = 无失败）。
    ///
    /// 与 [`Self::ctx_menu_status`] 同款：关联/「打开方式」失败只进日志的话，
    /// 用户面对的是「点了按钮没反应」——原因必须上浮到设置页状态行。
    /// 成功的一次会清空（失败态不陈旧滞留）。
    pub assoc_error: Option<String>,
    /// 左栏导航行缓存（仅目录；行索引 → TreeRow；T-PERF4 Arc——export_snapshot
    /// refcount 克隆，泵每周期导出零深拷贝；重建时整体替换）
    pub nav_rows: std::sync::Arc<Vec<TreeRow>>,
    /// 右栏当前目录行缓存（行索引 → TreeRow；T-PERF4 Arc，同上）
    pub dir_rows: std::sync::Arc<Vec<TreeRow>>,
    /// 当前选中树节点路径
    pub selected_path: Option<String>,
    /// 当前选中条目集（右栏表格选择写回）
    pub selection: Vec<u64>,
    /// 过滤输入显示值
    pub filter_text: String,
    /// 后台打开进行中（busy 指示）
    pub busy: bool,
    /// 会话切换原子闸（打开档案 / 进入容器 / 展开延迟层 / 弹出层；见 [`SessionSwitchGate`]）
    pub session_switch: SessionSwitchGate,
    /// 会话重开在飞标志（编辑释放会话后的保活重开走后台线程；见
    /// [`Self::browse_session_reopen_request`]）。
    pub browse_reopen_in_flight: Arc<std::sync::atomic::AtomicBool>,
    /// 会话重开**期望标志**（2026-09-26 用户实测「打开文件后目录跳回根」根因
    /// 修复）：只有编辑提交（`commit_edit_batch` → `release_browser`）真正释放
    /// 会话时才武装。`take_browser`（预览/外部打开/复制的三段式摘出）**不武装**
    /// ——否则泵的保活兜底会把「临时摘出」误判成「已释放」，重开一个新会话
    /// 把 `current_dir`/`expanded` 全部打回档案根。
    browse_reopen_armed: std::cell::Cell<bool>,
    /// 会话切换**代数**（每次发起有目标的换会话 +1；见 [`Self::begin_session_switch`]）。
    session_epoch: u64,
    /// 在飞的会话切换目标：`(键, 展示名)`（同键连点去重；不同键 = 顶替）。
    session_target: Option<(String, String)>,
    /// 浏览行重建在飞标志（O(行集) 派生只在后台线程跑；见
    /// [`Self::rebuild_browse_rows`]）。
    rows_rebuild_in_flight: Arc<std::sync::atomic::AtomicBool>,
    /// `caps_edit` 记忆化（键 = 档案路径）。
    ///
    /// `caps_edit()` 要嗅探档案（开文件 + 读头），而它被**每帧**的菜单投影调用
    /// （`AppShell::render`）——每帧一次文件 IO 是动画帧预算杀手。caps 只取决于
    /// **格式**（由路径决定），故路径不变即结果不变，路径一变自然失效。
    /// 用 `Mutex` 是因为只读路径（`export_snapshot(&self)`）也要写缓存。
    caps_edit_cache: Mutex<Option<(PathBuf, bool)>>,
    /// 跨线程信号发送端（泵/forwarder → UI；T-G30 在 run_ui 建 channel 装配）
    pub signal_tx: Option<Arc<crate::ui::contracts::SignalTx>>,
    /// 最近打开（LRU 序，cap [`MRU_CAP`]）——**进程内唯一真源**，落盘镜像在
    /// `ui.recent_archives`（启动自配置读入，每次变更同步写回）。
    ///
    /// 菜单直接读它（`MenuContext.mru`）：旧实现每帧从 `ShellState.cfg` 取，
    /// 而那份缓存只在泵收到 `SettingApplied`/`MenuEpochBump` 时重读配置——
    /// 外部入口（资源管理器双击/右键）打开档案后一个信号都不发，于是「刚打开的
    /// 档案在菜单里看不到」。放这里还有一处收益：用例在 `persist` 零落盘
    /// （`cfg!(test)`）下也能断言「这次打开记了没有」。
    pub recent_archives: Vec<String>,
}

impl AppData {
    /// 骨架构造（T-G30 run_ui 装配真实依赖）。
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            main: MainViewModel::new(),
            jobs: Arc::new(Mutex::new(JobsViewModel::new())),
            router: Arc::new(Mutex::new(DialogRouter::new())),
            compress: CompressDialogVm::new(),
            extract: ExtractDialogVm::new(),
            archive: None,
            external_edits: Vec::new(),
            pending_scope: None,
            pending_edit: None,
            name_edit: None,
            reopen_view_state: None,
            session_passwords: std::sync::Arc::new(crate::app::password_cache::PasswordCache::new()),
            pending_job_passwords: std::collections::HashMap::new(),
            settings: SettingsPrefs::default(),
            nav_rows: std::sync::Arc::new(Vec::new()),
            dir_rows: std::sync::Arc::new(Vec::new()),
            selected_path: None,
            selection: Vec::new(),
            filter_text: String::new(),
            busy: false,
            session_switch: SessionSwitchGate::default(),
            browse_reopen_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            browse_reopen_armed: std::cell::Cell::new(false),
            session_epoch: 0,
            session_target: None,
            rows_rebuild_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            caps_edit_cache: Mutex::new(None),
            // 草稿与右键菜单状态都是会话内的瞬时态：启动即空
            ctx_draft: None,
            ctx_menu_status: None,
            open_stack: Vec::new(),
            current_temp: None,
            temp_area: None,
            assoc_error: None,
            signal_tx: None, // run_ui 建通道后注入（T-G30 装配）
            // 最近打开：启动即从配置读入（此后以本字段为准）
            recent_archives: hzip_config::Config::load().ui.recent_archives,
        }
    }

    // —— 会话密码缓存（用户口径 2026-09-28 / 2026-09-29）——

    /// 接入**进程级**密码缓存（`run_ui` 装配，多窗口共享一份；见
    /// [`crate::app::password_cache`]）。
    pub fn share_password_cache(
        &mut self,
        cache: std::sync::Arc<crate::app::password_cache::PasswordCache>,
    ) {
        self.session_passwords = cache;
    }

    /// 取会话密码（`None` = 该档案尚未缓存 → 读加密条目会 3101，需过密码门）。
    pub fn cached_password(&self, archive: &std::path::Path) -> Option<zeroize::Zeroizing<String>> {
        self.session_passwords.get(archive)
    }

    /// 缓存**已验证**的会话密码（同一档案后续打开/解压/编辑直接复用，不再询问）。
    pub fn remember_password(&mut self, archive: &std::path::Path, pw: zeroize::Zeroizing<String>) {
        self.session_passwords.remember(archive, pw);
    }

    /// 丢弃某档案的会话密码（嵌套会话弹栈/密码失效自愈）。
    pub fn forget_password(&mut self, archive: &std::path::Path) {
        self.session_passwords.forget(archive);
    }

    /// 登记「任务正在用这份密码」——任务**成功结束**后才进缓存（见
    /// [`Self::commit_verified_job_passwords`]）。
    ///
    /// 两个来源：① 用户应答任务密码询问（[`crate::ui::shell::prompt`]）；
    /// ② 提交时参数窗已填（解压/压缩/编辑）。同一任务重复登记以最后一次为准。
    pub fn note_job_password(
        &mut self,
        job_id: hzip_progress::JobId,
        archive: PathBuf,
        pw: zeroize::Zeroizing<String>,
    ) {
        self.pending_job_passwords.insert(job_id, (archive, pw));
    }

    /// 任务终局收口：成功的（Done/Partial）密码写进缓存，失败/取消丢弃。
    ///
    /// 每个 UI 批次调用一次（便宜：表通常 0~1 条）。判定用任务行的终局摘要——
    /// **任务成功即密码正确**（该任务正是拿着它去读写该档案的）。
    pub fn commit_verified_job_passwords(&mut self) {
        if self.pending_job_passwords.is_empty() {
            return;
        }
        let mut verdicts: Vec<(hzip_progress::JobId, crate::viewmodel::jobs_vm::JobVerify)> =
            Vec::new();
        {
            let Ok(jobs) = self.jobs.lock() else {
                return;
            };
            for id in self.pending_job_passwords.keys() {
                verdicts.push((*id, jobs.verify_of(*id)));
            }
        }
        for (id, verdict) in verdicts {
            use crate::viewmodel::jobs_vm::JobVerify;
            match verdict {
                JobVerify::Active => {} // 还在跑：等终局
                JobVerify::Succeeded => {
                    if let Some((key, pw)) = self.pending_job_passwords.remove(&id) {
                        tracing::info!(
                            job = id,
                            archive = %key.display(),
                            "任务成功 → 密码进会话缓存（后续同档案操作不再问）"
                        );
                        self.session_passwords.remember(&key, pw);
                    }
                }
                JobVerify::Failed | JobVerify::Gone => {
                    if let Some((key, _)) = self.pending_job_passwords.remove(&id) {
                        tracing::debug!(
                            job = id,
                            archive = %key.display(),
                            ?verdict,
                            "任务未成功 → 密码不进缓存（不喂没验过的密码）"
                        );
                    }
                }
            }
        }
    }

    /// 取待执行的门动作（主线程泵消费：密码验证通过后重跑原操作）。
    ///
    /// 门状态本身在 [`Self::router`]（`take_pending_gate_action`）；本方法只是
    /// 数据面收口，避免 UI 层直接触碰 router 锁序。
    pub fn take_pending_gate_action(&self) -> Option<crate::viewmodel::GateAction> {
        self.router
            .lock()
            .ok()
            .and_then(|mut r| r.take_pending_gate_action())
    }

    /// 档案是否可编辑（T-MENU6 caps_edit 投影；MenuContext 消费）。
    ///
    /// = registry caps.edit 且非组合格式（tar/single/rar/组合 → false）。
    pub fn caps_edit(&self) -> bool {
        let Some(archive) = &self.archive else {
            return false;
        };
        // 记忆化（见 `caps_edit_cache` 文档）：命中路径即返回，不再嗅探。
        if let Ok(cache) = self.caps_edit_cache.lock()
            && let Some((path, value)) = cache.as_ref()
            && path == archive
        {
            return *value;
        }
        let value = self.caps_edit_sniff(archive);
        if let Ok(mut cache) = self.caps_edit_cache.lock() {
            *cache = Some((archive.clone(), value));
        }
        value
    }

    /// 真实嗅探（[`Self::caps_edit`] 的未缓存内核；只在路径变化时调用）。
    fn caps_edit_sniff(&self, archive: &std::path::Path) -> bool {
        // 嗅探（读头部，开销 O(1)；失败 = 不可编辑）
        let sniffed = self
            .engine
            .registry()
            .sniff(&hzip_format::Source::File(archive.to_path_buf()));
        match sniffed {
            Ok(hzip_format::Sniffed::Single(id, _)) => {
                let edit = self
                    .engine
                    .registry()
                    .handler(id.as_str())
                    .map(|h| h.caps().edit)
                    .unwrap_or(false);
                tracing::debug!(format = %id, edit, "caps_edit 投影");
                edit
            }
            other => {
                tracing::debug!(?other, "caps_edit 投影：组合/Raw/嗅探失败 → false");
                false // 组合/Raw → 不可编辑（02 §7.5）
            }
        }
    }

    /// 从 hzip-config 恢复设置偏好（启动装配；T-G30 在 run_ui 调用）。
    ///
    /// 归一化策略与注册表 `store` 同源（[`settings_registry::index_of_value_ci`] /
    /// [`settings_registry::columns_from_pod`]）；未知配置值回落默认（向后兼容）。
    pub fn load_settings(&mut self, config: &hzip_config::Config) {
        let theme =
            settings_registry::index_of_value_ci(&crate::app::labels::THEME, &config.ui.theme);
        let lang = settings_registry::index_of_value_ci(&crate::app::labels::LANG, &config.ui.lang);
        // 界面下标域 = UI_FORMAT（组合格式屏蔽）；存量配置里的 `tar.gz` 等
        // 组合值不在 UI 表 → `unwrap_or(0)` 回落 zip（不重写用户配置文件）。
        let format = crate::app::labels::UI_FORMAT
            .iter()
            .position(|(_, v)| v.eq_ignore_ascii_case(&config.defaults.format))
            .unwrap_or(0);
        let level = settings_registry::index_of_value_ci(
            &crate::app::labels::LEVEL,
            &config.defaults.level,
        );
        let ui = &config.ui;
        self.settings = SettingsPrefs {
            theme_ix: theme,
            theme_color: ui.theme_color.trim().to_string(),
            lang_ix: lang,
            format_ix: format,
            level_ix: level,
            columns: settings_registry::columns_from_pod(&ui.columns),
            show_dotdot: ui.show_dotdot,
            auto_update: ui.auto_update,
            assoc: ui.assoc_formats.clone(),
            ctx_extract: ui.ctx_extract.clone(),
            ctx_compress: ui.ctx_compress.clone(),
            show_folder_size: ui.show_folder_size,
            image_viewer: ui.image_viewer.clone(),
            editor: ui.editor.clone(),
            shell_ext_dll: ui.shell_ext_dll.trim().to_string(),
            show_tree: ui.show_tree,
            show_jobs: ui.show_jobs,
            toolbar_style_ix: settings_registry::index_of_value_ci(
                &crate::app::labels::TOOLBAR_STYLE,
                &ui.toolbar_style,
            ),
            view_style_ix: settings_registry::index_of_value_ci(
                &crate::app::labels::VIEW_STYLE,
                &ui.view_style,
            ),
            font_family: ui.font_family.trim().to_string(),
            font_size: ui.font_size_px(),
            corner_radius: ui.corner_radius_px(),
            density_ix: crate::viewmodel::density::Density::from_config(&ui.density).index(),
            file_type_icons_ix: settings_registry::index_of_value_ci(
                &crate::app::labels::FILE_ICONS,
                &ui.file_type_icons,
            ),
            // 快捷键整节原样搬入镜像（本层不校验键位串：三态判读与非法回落
            // 都在 `ui::shell::keymap`——装配时判一次，错误值原样留在盘上）
            keys: config.keys.clone(),
        };
    }

    /// 记录打开成功（T-MENU4）：MRU append 去重置顶 + 内存镜像更新 + 落盘 + 菜单刷新。
    ///
    /// 调用方约定：**open_archive 成功后**才调用（失败不记）。**所有**打开路径都要调
    /// ——对话框/最近列表/工具栏，以及资源管理器双击/右键（`LaunchIntent::Open`）
    /// 与右键解压家族（`open_then_extract*`，它们同样把档案装进浏览会话）；
    /// 唯一的例外是**档案内的子压缩包**（进容器 = 嵌套会话，不是一次「打开档案」，
    /// 见 `open_container`/`begin_nested_session`——那里不调用本方法）。
    pub fn record_mru_open(&mut self, path: &std::path::Path) {
        let path_str = path.to_string_lossy().into_owned();
        self.recent_archives = mru_append(&self.recent_archives, &path_str);
        let mut config = hzip_config::Config::load();
        config.ui.recent_archives = self.recent_archives.clone();
        persist(&config);
        // 菜单数据源是本字段（`MenuContext.mru`），但菜单**重建**要等一帧信号；
        // 没有这一步时外部入口打开档案后菜单仍显示旧列表（用户实测报告）。
        if let Some(tx) = &self.signal_tx {
            let _ = tx.try_send(crate::viewmodel::VmSignal::MenuEpochBump);
        }
    }

    /// 清空 MRU（T-MENU4「清空所有历史记录」）：置空 + 落盘。
    pub fn clear_mru(&mut self) {
        self.recent_archives.clear();
        let mut config = hzip_config::Config::load();
        config.ui.recent_archives.clear();
        persist(&config);
    }

    /// 移除单条 MRU（点击失效条目时调用）+ 落盘。
    pub fn remove_mru(&mut self, path: &str) {
        self.recent_archives = mru_remove(&self.recent_archives, path);
        let mut config = hzip_config::Config::load();
        config.ui.recent_archives = self.recent_archives.clone();
        persist(&config);
    }

    /// 暂存待确认的编辑变更集（T-MENU6/7 确认对话框挂起期；确认后提交）。
    pub fn stage_edit_batch(
        &mut self,
        archive: std::path::PathBuf,
        changes: hzip_format::ChangeSet,
    ) {
        self.pending_edit = Some((archive, changes));
    }

    /// 丢弃待确认变更集（对话框取消/关闭）。
    pub fn discard_edit_batch(&mut self) {
        self.pending_edit = None;
    }

    /// 提交暂存的编辑变更集（确认对话框「确认」出口）。
    ///
    /// 走 [`crate::ui::shell::dispatch::submit_edit_batch`]（唯一编辑提交点）：
    /// `EditBatch` 后台任务 + 任务泵（进度/取消/密码链复用 EditSave 链路）；
    /// 完成后回投 `JobsChanged`（任务行 + 进度窗）。返回 job_id。
    pub fn commit_edit_batch(&mut self) -> Result<u64, String> {
        let Some((archive, changes)) = self.pending_edit.take() else {
            return Err("没有待提交的变更".into());
        };
        // 关键：先释放浏览会话（见 `release_session_for_edit` 的说明）。
        self.release_session_for_edit(&archive);
        crate::ui::shell::dispatch::submit_edit_batch_locked(self, &archive, changes)
    }

    /// 编辑提交前的浏览会话释放（**唯一实现**，两个入口共用）。
    ///
    /// 必须：`Browser` 持着档案文件句柄，而 Windows 上 `MoveFileExW
    /// REPLACE_EXISTING` 对被打开的目标返回 err 5（拒绝访问）⇒ 编辑任务的原子替换
    /// 必失败。释放后 UI 仍显示 entries 缓存，任务完成后泵钩子按 archive 路径
    /// refresh 重开（保活重开只在**真正释放**处武装——三段式临时摘出不武装，
    /// 避免误重开把目录打回根）。
    ///
    /// 调用方：① 菜单/右键的批量编辑（[`Self::commit_edit_batch`]）
    /// ② 外部编辑器保存回写（`ui::shell::external_edits`）。
    pub(crate) fn release_session_for_edit(&mut self, archive: &std::path::Path) {
        if self.archive.as_deref() == Some(archive) {
            // 重开前记住视图状态（当前目录 + 树展开集）——重开会话会把它们归零
            self.reopen_view_state = Some(self.main.view_state());
            self.main.release_browser();
            // 武装保活重开（2026-09-26）：只有这里（编辑提交真正释放会话）才
            // 允许泵的 `browse_session_reopen_request` 重开。
            self.browse_reopen_armed.set(true);
        }
    }

    /// 进入行内名称编辑态（重命名/新建文件夹；用户口径 2026-09-28）。
    ///
    /// UI 侧负责：把目标行的名称渲染成输入框并聚焦；Enter/失焦时回读文本调
    /// [`Self::commit_name_edit`]。数据面只记「在编辑什么」。
    pub fn begin_name_edit(&mut self, edit: NameEdit) {
        self.name_edit = Some(edit);
    }

    /// 当前会话代数（换会话 +1；见 [`Self::begin_session_switch`]）。
    pub(crate) fn session_epoch(&self) -> u64 {
        self.session_epoch
    }

    /// 退出编辑态（Esc/导航/换会话）：不改档案。
    pub fn cancel_name_edit(&mut self) {
        self.name_edit = None;
    }

    /// 提交行内名称（Enter/失焦）。
    ///
    /// 返回值：`Ok(Some(job_id))` = 已提交 `EditBatch` 重写任务（后台改档，完成后
    /// 泵重开浏览会话）；`Ok(None)` = 无动作（空名/未改名 = 放弃）；`Err(文案)` =
    /// 校验失败——**保持编辑态**让用户改（UI 侧弹 toast）。
    pub fn commit_name_edit(&mut self, text: &str) -> Result<Option<u64>, String> {
        let Some(edit) = self.name_edit.clone() else {
            return Ok(None); // 编辑态已结束（重复提交/blur 尾随）→ 幂等 no-op
        };
        if edit.session_epoch != self.session_epoch {
            self.name_edit = None; // 会话已换：目标名不再指向同一档案
            return Ok(None);
        }
        // 嵌套会话（进了内层容器）禁编辑：内层内容不在顶层档案里，按同名路径
        // 落到顶层档案只会改错东西（用户口径 2026-09-28 的「不要改动其他信息」
        // 直接要求这条）。
        if !self.open_stack.is_empty() {
            self.name_edit = None;
            return Err(crate::tr("browse.rename.nested"));
        }
        let name = text.trim();
        if name.is_empty() {
            self.name_edit = None; // 清空 = 放弃（与 Esc 同义）
            return Ok(None);
        }
        if !name_is_valid_segment(name) {
            return Err(crate::tr("browse.rename.invalid"));
        }
        // 名字没变 → 无动作（**必须早于重名判定**：不改名直接回车/失焦时，
        // 重名判定会把「自己」当成冲突而报错）
        if edit.from.as_deref() == Some(name) {
            self.name_edit = None;
            return Ok(None);
        }
        // 目录树是**懒构建**的：重名判定前确保已构建（否则 `dir_children` 恒空
        // ⇒ 重名检查形同虚设）
        self.main.tree();
        if self.dir_has_name(&edit.dir, name) {
            return Err(crate::tr("browse.rename.exists"));
        }
        let new_full = join_entry_path(&edit.dir, name);
        let changes = match &edit.from {
            Some(from) => vec![hzip_format::Change::Rename {
                from: join_entry_path(&edit.dir, from).into(),
                to: new_full.into(),
            }],
            None => vec![hzip_format::Change::MakeDir {
                name: new_full.into(),
            }],
        };
        let archive = self
            .archive
            .clone()
            .ok_or_else(|| crate::tr("browse.rename.canceled"))?;
        self.name_edit = None;
        self.stage_edit_batch(archive, hzip_format::ChangeSet { changes });
        self.commit_edit_batch().map(Some)
    }

    /// 编辑目标目录下是否已有同名条目。
    ///
    /// 取 VM 的**当前目录子行**（`dir_children`）而非渲染行缓存：后者带过滤
    /// 投影——被过滤隐藏的同名条目必须照样算冲突（否则改名会撞名）。
    fn dir_has_name(&self, dir: &str, name: &str) -> bool {
        if dir != self.main.current_dir() {
            return false; // 非当前目录不查（编辑只在当前目录发起，防御分支）
        }
        self.main.dir_children().iter().any(|r| r.name == name)
    }

    /// 消费对话框事件中的设置事件（T-G54 §3.3；即时保存语义）。
    ///
    /// 返回是否命中设置事件（未命中的事件由 T-G30 其余消费分支处理）。
    /// 写盘失败仅 warn 不 panic；主题即时应用（apply_theme）由持有 Window 的
    /// UI 半场接线（AppShell dispatch），本方法只落数据面。
    pub fn consume_dialog_event(&mut self, event: &DialogEvent) -> bool {
        match event {
            DialogEvent::SettingChanged { id, value } => {
                self.apply_setting(*id, value);
            }
            DialogEvent::SettingsReset => {
                self.apply_reset();
            }
            DialogEvent::SettingsApplyAssoc
            | DialogEvent::SettingsRemoveAssoc
            | DialogEvent::SettingsAssocOpenAppSettings
            | DialogEvent::SettingsCheckUpdate => {
                // 数据面无动作：关联写注册表/检查更新由主控与 UI 半场处理
                // （勾选集合等数据面已由 `SettingChanged` 落盘）。
            }
            DialogEvent::JobsCancelAll => {
                // T-GUI T31「全部取消」：逐活跃任务幂等 cancel；终局以
                // JobFinished{Cancelled} 落账（core 唯一事实来源）
                if let Ok(mut jobs) = self.jobs.lock() {
                    jobs.cancel_all();
                }
            }
            DialogEvent::JobsCancel(id) => {
                // 任务中心行内「取消」：与进度窗 `on_cancel` 同一通道
                //（`jobs.cancel`，幂等；终态行句柄已回收 = 空转）
                if let Ok(mut jobs) = self.jobs.lock() {
                    jobs.cancel(*id);
                }
            }
            DialogEvent::JobsClearFinished => {
                // T-GUI T31「清除已完成」：移除全部终态行（活跃行保留）
                if let Ok(mut jobs) = self.jobs.lock() {
                    let _ = jobs.clear_finished();
                }
            }
            DialogEvent::SettingsClose => {
                // 即时保存语义下无批量应用步骤；Close 仅收起（无数据面动作）
            }
            _ => return false,
        }
        true
    }

    /// 单项设置变更（T-SET §1.2 五步流程；框架尾步固定）。
    ///
    /// ```text
    /// type guard → (def.store)(cfg, self, value)   // normalize + 写 Config + 镜像
    ///            → Config::save()                 // 恰好一次（cfg!(test) 不落盘）
    ///            → dispatch_effect(def.effect)     // 数据半场 + UI 半场信号
    /// ```
    ///
    /// 取值类型与该控件不符 → 整项忽略（防跨类型串用；不 panic）。
    pub fn apply_setting(&mut self, id: SettingId, value: &settings_registry::SettingValue) {
        let def = settings_registry::def(id);
        if !def.widget.accepts(value) {
            tracing::warn!("设置项 {id:?} 取值类型不符，已忽略");
            return;
        }
        // 手动提交项（右键菜单）：只改内存草稿——不落盘、不碰操作系统。
        // 真正的落盘 + OS 写入由「应用」按钮走 commit_ctx_draft。
        if def.commit == settings_registry::Commit::Manual {
            self.stage_ctx_draft(id, value);
            return;
        }
        let mut config = hzip_config::Config::load();
        (def.store)(&mut config, self, value);
        persist(&config);
        self.dispatch_effect(id, def.effect);
    }

    /// 把一次控件变更写进右键菜单草稿（白名单收敛，与 store 同源）。
    fn stage_ctx_draft(&mut self, id: SettingId, value: &settings_registry::SettingValue) {
        use crate::ui::contracts::{CTX_COMPRESS_KEYS, CTX_EXTRACT_KEYS};
        let Some(set) = value.as_str_set() else {
            return;
        };
        let draft = self.ctx_draft.get_or_insert_with(|| CtxMenuDraft {
            // 首次改动：以「已应用」值为草稿基线，避免其余项被误清空
            extract: self.settings.ctx_extract.clone(),
            compress: self.settings.ctx_compress.clone(),
        });
        match id {
            SettingId::CtxExtract => {
                draft.extract = settings_registry::whitelist_order(&CTX_EXTRACT_KEYS, set);
            }
            SettingId::CtxCompress => {
                draft.compress = settings_registry::whitelist_order(&CTX_COMPRESS_KEYS, set);
            }
            _ => {}
        }
    }

    /// 提交右键菜单草稿：落盘一次，返回是否确有改动。
    ///
    /// 返回 `true` 时调用方（`dispatch`）应后台执行
    /// `hzip_platform::context_menu::backend().install(...)`。
    /// 无草稿（用户没改过任何勾选）→ `false`，不触发 OS 写入。
    pub fn commit_ctx_draft(&mut self) -> bool {
        let Some(draft) = self.ctx_draft.take() else {
            return false;
        };
        let mut config = hzip_config::Config::load();
        config.ui.ctx_extract = draft.extract.clone();
        config.ui.ctx_compress = draft.compress.clone();
        self.settings.ctx_extract = draft.extract;
        self.settings.ctx_compress = draft.compress;
        persist(&config);
        // UI 半场信号：面板重载（草稿已清，显示回到「已应用」值）
        if let Some(tx) = &self.signal_tx {
            let _ = tx.try_send(crate::viewmodel::VmSignal::SettingApplied(
                SettingId::CtxExtract,
            ));
        }
        true
    }

    /// 丢弃草稿（「恢复默认」后调用：草稿不能比 config 活得更久）。
    pub fn discard_ctx_draft(&mut self) {
        self.ctx_draft = None;
    }

    /// 「清除」：把两个勾选集合置空并落盘，同时丢弃草稿。
    ///
    /// 系统侧的撤销由调用方（`dispatch`）在后台跑 `backend().uninstall()`；
    /// 本方法只负责数据面——**config 与系统必须同步归零**，否则面板会显示
    /// 「勾着但系统里没有」的假象。
    pub fn clear_ctx_menu_selection(&mut self) {
        self.ctx_draft = None;
        self.settings.ctx_extract.clear();
        self.settings.ctx_compress.clear();
        let mut config = hzip_config::Config::load();
        config.ui.ctx_extract.clear();
        config.ui.ctx_compress.clear();
        persist(&config);
    }

    /// 恢复默认（T-SET8：注册表驱动全量重置；同样只落盘一次）。
    pub fn apply_reset(&mut self) {
        // 草稿必须随「恢复默认」一起清：否则面板显示默认值、草稿还活着，
        // 下次点「应用」会把旧草稿写回去（用户看到的就是「恢复默认没生效」）。
        self.ctx_draft = None;
        let mut config = hzip_config::Config::load();
        settings_registry::apply_defaults(&mut config, self);
        persist(&config);
        // UI 半场：主题即时生效（全局 Theme，泵按 id 分派）；列设置等其余项
        // 由同一次 `SettingApplied` 触发的快照刷新带出（泵批次非空即全量
        // 重读快照——不必逐项补发信号）。
        self.dispatch_effect(SettingId::Theme, settings_registry::Effect::Theme);
        // 界面字体族/字号与主题同挂全局 `Theme`（`font.family`/`font.size`），
        // 但泵只在**收到对应 id** 时才重放并 `refresh_windows`（字号的 rem 基准
        // 由各窗口 `Root` 渲染帧套用，不重绘就不生效）——故须显式补发。
        self.dispatch_effect(SettingId::FontFamily, settings_registry::Effect::Theme);
        self.dispatch_effect(SettingId::FontSize, settings_registry::Effect::Theme);
        // 圆角同挂全局 Theme（`radius`/`radius_lg`）——不补发就「数据面复位了、
        // 界面还是旧圆角」。
        self.dispatch_effect(SettingId::CornerRadius, settings_registry::Effect::Theme);
        // 布局密度：UI 线程级间距刻度，UI 半场单独分派
        self.dispatch_effect(SettingId::Density, settings_registry::Effect::Density);
        // 文件类型图标来源：默认复位到 `system`——不补发信号则数据面已复位、
        // 图标系统还是关着的（泵按 `FileTypeIcons` 调 `file_icon::set_enabled`）。
        self.dispatch_effect(
            SettingId::FileTypeIcons,
            settings_registry::Effect::FileIconSource,
        );
        // 快捷键：`apply_defaults` 已把 `[keys]` 清成全空（= 全默认），但键位
        // 生效面是 `App` 级 keymap（全局态，快照不携带）——不补发信号则「配置复位
        // 了、按下仍是旧快捷键」（与主题/字体/圆角同款缺口，泵按本 id 重装绑定）。
        self.dispatch_effect(SettingId::Keymap, settings_registry::Effect::DataOnly);
    }

    /// 设置视图投影（面板逐帧回读 / 主窗口刷新共用；候选集现算）。
    ///
    /// `assoc_candidates` 由调用方给出（`ui::contracts::supported_open_extensions`
    /// 派生——app 层不依赖 ui 的 Engine 能力封装之外的东西）。
    ///
    /// T-OPT 项6：系统实况为**缓存优先**（与设置面板自身 `effective_ctx_status` /
    /// `assoc_states_now` 同口径，60 s TTL）——不是严格「活读」：旧实现每次直调
    /// `query_ctx_menu_status()`（macOS 起子进程）+ `assoc_ext_states()`（Windows
    /// 每扩展一次注册表读），绕开了 T-PERF 走查 #13 建的缓存。需要**即时**实况的
    /// 调用方：先 `ui::contracts::invalidate_assoc_status_cache()` 再调本方法。
    /// 未命中缓存仍真查并顺手后台预热。
    ///
    /// **调用方**负责把 `Config::load()` 的读盘留在锁外（见
    /// `ui::views::settings_layout::read_view` 的取舍）；本方法自身不做该拆分。
    pub fn settings_view(&self, assoc_candidates: Vec<String>) -> settings_registry::SettingsView {
        let exts = &self.settings.assoc;
        let status = match crate::ui::contracts::cached_ctx_menu_status() {
            Some(hit) => hit,
            None => {
                crate::ui::contracts::warm_assoc_status_cache(exts.clone());
                crate::ui::contracts::query_ctx_menu_status()
            }
        };
        let assoc_states = match crate::ui::contracts::cached_assoc_ext_states(exts) {
            Some(hit) => hit,
            None => {
                let states = crate::ui::contracts::assoc_ext_states(exts);
                crate::ui::contracts::warm_assoc_status_cache(exts.clone());
                states
            }
        };
        settings_registry::SettingsView::load(
            self,
            &hzip_config::Config::load(),
            assoc_candidates,
            Some(status),
            assoc_states,
        )
    }

    /// 生效分发（Effect 声明 → 两侧半场消费）。
    ///
    /// **数据半场**在本方法内（`columns_dirty` 已删——列设置改走快照，
    /// 见 [`Self::export_snapshot`]）；**UI 半场**一律由主线程泵按
    /// `SettingApplied(id)` 分派（主题/语言需 `&mut App`，app 层不可达）。
    fn dispatch_effect(&mut self, id: SettingId, effect: settings_registry::Effect) {
        use settings_registry::Effect;
        match effect {
            // 声明即文档：这几类的生效动作全在 UI 半场（泵按 id 分派），
            // 数据面在 `store` 中已完成——此处无额外数据动作。
            // 密度同此：间距刻度是进程级原子量，写它须在 UI 半场（布局构建期
            // 没有 `App`）⇒ 此处只发信号。
            Effect::DataOnly
            | Effect::Theme
            | Effect::Locale
            | Effect::BackgroundAssoc
            | Effect::Density
            | Effect::FileIconSource => {}
            // 延后项（默认格式/级别）：只更新压缩对话框模板，不逐帧刷新面板
            // （T-SET1 §1——面板在打开期不被外部状态覆盖）
            Effect::Deferred => {}
        }
        // UI 半场信号（主线程泵消费：主题 apply / locale 重设 / 快照刷新）。
        if let Some(tx) = &self.signal_tx {
            let _ = tx.try_send(crate::viewmodel::VmSignal::SettingApplied(id));
        }
    }

    /// 浏览会话保活检查（**便宜**：`is_open` 判断 + 一次 `exists`）。
    ///
    /// 浏览会话被释放（EditBatch 提交前释放句柄保原子替换）而源档案仍在 → 返回
    /// 需要重开的路径并**置在飞标志**；真正的重开（`engine.browse`——与档案索引
    /// 规模成正比的活）由后台线程执行，见
    /// [`crate::ui::shell::spawn_browse_reopen`]。
    ///
    /// 为什么不再就地重开（2026-09-24 UI 线程阻塞审计 B3）：本函数被**泵每
    /// wake** 调用，旧实现在 UI 线程持 AppData 锁里跑完整 `browse` —— 编辑（添加/
    /// 删除/粘贴）完成后必现卡顿，而且它同时堵住渲染帧（渲染要那把锁）。
    pub fn browse_session_reopen_request(&mut self) -> Option<PathBuf> {
        if self.main.is_open() {
            // 会话在（正常态或 `take_browser` 三段式的摘出间隙）——不重开。
            // 后者见 `browse_reopen_armed` 文档（2026-09-26 用户实测缺陷）。
            self.browse_reopen_armed.set(false);
            return None;
        }
        // **编辑任务在飞 → 先不重开**（保持武装，下一次 wake 再试）：重开会重新
        // 打开源档案，而编辑任务随后要原子替换同一路径——Windows 上目标被打开时
        // 替换失败 os 5（2026-09-29 用户实测：加密档重命名必失败，日志
        // `IO 错误: 拒绝访问。 (os error 5)`）。
        if self
            .jobs
            .lock()
            .map(|j| j.has_active_edit())
            .unwrap_or(false)
        {
            return None;
        }
        if !self.browse_reopen_armed.replace(false) {
            return None; // 未武装：会话不是被编辑提交释放的（防御 + 竞态收口）
        }
        if self
            .browse_reopen_in_flight
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None; // 已有重开在飞：等它（成败都会清标志）
        }
        // 当前会话源（嵌套层 = 解出来的临时档案；用 `archive` 会去重开外层档案）
        let path = self
            .main
            .source_path()
            .map(|p| p.to_path_buf())
            .filter(|p| p.exists())?;
        self.browse_reopen_in_flight
            .store(true, std::sync::atomic::Ordering::Release);
        Some(path)
    }

    /// 后台重开收口（成功装会话 + 重建行；失败清空行集）。
    ///
    /// 由 [`crate::ui::shell::spawn_browse_reopen`] 在锁内调用；**无论成败都清
    /// 在飞标志**（否则会话保活会永久停摆）。
    pub fn finish_browse_reopen(&mut self, result: Result<(), hzip_error::HzipError>) {
        match result {
            Ok(()) => {
                tracing::info!("编辑后重开浏览会话");
                // 种回编辑前的目录/展开集（用户口径 2026-09-29：改完名不该被踢回
                // 档案根）；目录在新档案里已不存在 → 回落根（`restore_view_state`
                // 内部处理）
                if let Some((dir, expanded)) = self.reopen_view_state.take() {
                    let kept = self.main.restore_view_state(&dir, expanded);
                    if !kept {
                        tracing::info!("编辑后重开：原目录 {dir:?} 已不存在，回落档案根");
                    }
                }
                self.rebuild_browse_rows();
            }
            Err(e) => {
                tracing::warn!("编辑后重开失败: {e}");
                self.reopen_view_state = None;
                std::sync::Arc::make_mut(&mut self.nav_rows).clear();
                std::sync::Arc::make_mut(&mut self.dir_rows).clear();
                self.main.go_root_dir();
            }
        }
        self.browse_reopen_in_flight
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// 行重建在飞标志（泵/后台线程共用；去重「多窗口 + 多信号」的重复请求）。
    pub fn rows_rebuild_in_flight(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.rows_rebuild_in_flight.clone()
    }

    /// 发起一次**有目标的换会话**（打开档案 / 进入容器 / 刷新）。
    ///
    /// # 为什么需要（用户实测 2026-09-24）
    ///
    /// 「我打开了 cpio.gz（没展开里面的 cpio），然后再打开一个 qcow2.gz，结果
    /// 冒出来的进度窗还在打开那个 cpio 文件」——后台流程之间**没有次序保证**：
    /// 旧流程可能后落地，把新打开的档案会话盖回去（还带着自己的进度窗/记录）。
    ///
    /// 本方法与原子闸 [`Self::session_switch`] 分工不同：
    ///
    /// - 原子闸管**无目标**的重活（复制物化 / 弹出层）：同一时刻只允许一个；
    /// - 本代数管**有目标**的换会话之间的次序：同目标连点 → [`SwitchStart::Duplicate`]
    ///   （去重，不重复起线程）；不同目标 → 顶替（代数 +1），旧流程落账前会发现
    ///   代数已变而**丢弃结果**（不装会话、不发信号、不留任务中心记录、关掉自己的窗）。
    pub fn begin_session_switch(&mut self, target: SwitchTarget) -> SwitchStart {
        if let Some((key, _)) = self.session_target.as_ref()
            && *key == target.key
        {
            return SwitchStart::Duplicate;
        }
        self.session_epoch = self.session_epoch.wrapping_add(1);
        let superseded = self
            .session_target
            .replace((target.key, target.label))
            .map(|(_, label)| label);
        SwitchStart::Started {
            epoch: self.session_epoch,
            superseded,
        }
    }

    /// 后台流程落账前比对：`false` = 已被更新的换会话顶替（必须丢弃结果）。
    pub fn session_switch_is_current(&self, epoch: u64) -> bool {
        self.session_epoch == epoch
    }

    /// 流程收尾（成功/失败/被顶替都要调）：仅当自己仍是最新时清在飞目标
    /// （被顶替的情况下新目标由新流程持有，不能误清）。
    pub fn end_session_switch(&mut self, epoch: u64) {
        if self.session_epoch == epoch {
            self.session_target = None;
        }
    }

    /// 顶层打开新档案：清空**嵌套会话栈**（旧档案的内层会话与临时文件）。
    ///
    /// 不清的后果（用户实测 2026-09-24「不应该被老压缩包内容干扰」）：面包屑/
    /// 左树会把旧档案的内层层级一起画出来，`..` 还能退回已经不存在的旧会话。
    /// 临时文件一并尽力删除（不删也不致命——会话级 TempArea 退出时兜底清理，
    /// 但大档案会在会话期间白占盘）。
    pub fn reset_nested_sessions(&mut self) {
        for frame in self.open_stack.drain(..) {
            if let Some(temp) = frame.temp {
                let _ = std::fs::remove_file(&temp);
            }
        }
        if let Some(temp) = self.current_temp.take() {
            let _ = std::fs::remove_file(&temp);
        }
    }

    /// 浏览行重建（nav 树 + 当前层/搜索行 + 目录聚合大小回填）。
    ///
    /// **只允许在后台线程调用**（测试除外）：这是 O(行集) 的纯 CPU 派生，在 UI
    /// 线程上跑就是卡顿（2026-09-24 审计 B5）。泵批内只**请求**重建
    /// （[`Self::rows_rebuild_in_flight`] + [`crate::ui::shell::spawn_rows_rebuild`]）。
    pub(crate) fn rebuild_browse_rows(&mut self) {
        // 懒建树（nav_rows/visible_rows 依赖 tree_ref；未建则两查询皆空）。
        self.main.tree();
        // 左栏树跨嵌套层：外层档案为根、内层压缩包当容器节点
        self.nav_rows = std::sync::Arc::new(self.display_nav_rows());
        // 搜索激活 → 全档扁平匹配结果；否则 = 当前目录子项（dir_children）
        self.dir_rows = std::sync::Arc::new(self.main.visible_rows());
        // 目录行聚合大小（#3 查看器 Tab 开关；R2-04 §3.6）。全量聚合表回填
        // ——旧实现 fold_dir_sizes 在行集内求和，右栏逐层浏览后行集只含一层、
        // 子孙不在集内 → 恒 0（用户勾选后看不到目录大小的根因）。两条行源
        // （逐层/搜索扁平）的 path 均为档案内完整路径，直接查表命中。
        // T-PERF4：Arc 就地改（独占 → make_mut 无克隆）。
        if self.settings.show_folder_size {
            let sizes = self.main.dir_sizes();
            for row in std::sync::Arc::make_mut(&mut self.dir_rows) {
                if row.is_dir
                    && let Some(&(raw, packed)) = sizes.get(&row.path)
                {
                    row.size_raw = raw;
                    row.size_packed = packed;
                }
            }
        }
    }

    // —— 嵌套会话栈（R-GUI2-06：双击容器当目录进）——

    /// 进入容器前压栈：把**当前会话整体**存起来，`main` 交给内层档案。
    ///
    /// 调用方随后用 [`crate::viewmodel::MainViewModel::install_browser`] 把内层
    /// 会话装进 `main`。临时文件交接：当前会话若是嵌套层，它的源档案要跟着它
    /// 一起进栈（返回内层时还要用）；新会话的临时文件由调用方经
    /// [`Self::adopt_temp_source`] 登记。
    pub fn begin_nested_session(&mut self, container_path: &str) {
        let mut crumbs = crate::ui::contracts::crumbs_from_current_dir(
            &self.current_archive_name(),
            self.main.current_dir(),
        );
        // 容器不在当前目录时（用户在档案根双击 `sub/child.tar`）补一段：面包屑
        // 要能看出「这个内层档案在外层的 sub/ 里」，而且点那一段能回去——外层帧
        // 只记到 `current_dir`，缺这一段就会出现 `outer.tar › child.tar` 这种
        // 丢失中间路径的链。
        let container_dir = container_path
            .rsplit_once('/')
            .map(|(d, _)| d)
            .unwrap_or("");
        if !container_dir.is_empty() && container_dir != self.main.current_dir() {
            crumbs.push(crate::ui::contracts::Crumb {
                label: container_dir
                    .rsplit('/')
                    .next()
                    .unwrap_or(container_dir)
                    .to_string(),
                seg: container_dir.to_string(),
                level: 0,
            });
        }
        let mut parent = std::mem::take(&mut self.main);
        let temp = self.current_temp.take();
        // 容器节点随进入即展开（否则树上看不到内层内容，得像目录那样再点一下）
        parent.toggle_expand(container_path);
        self.open_stack.push(OpenFrame {
            crumbs,
            container_path: container_path.to_string(),
            vm: parent,
            temp,
        });
        self.selected_path = None;
    }

    /// 登记新会话的临时档案（嵌套层专用；弹栈时删除）。
    pub fn adopt_temp_source(&mut self, temp: PathBuf) {
        self.current_temp = Some(temp);
    }

    /// 弹栈一层：恢复父层会话 + 删掉内层的临时档案。返回是否真的弹了。
    pub fn pop_open_frame(&mut self) -> bool {
        let Some(frame) = self.open_stack.pop() else {
            return false;
        };
        let child_temp = std::mem::replace(&mut self.current_temp, frame.temp);
        self.main = frame.vm;
        self.selected_path = None;
        if let Some(t) = child_temp {
            // 该层会话的密码缓存随会话退出丢弃（临时路径不复用，留着只会是垃圾）
            self.forget_password(&t);
            // 删不掉也不致命（TempArea 退出时会整目录清），但必须留痕
            if let Err(e) = std::fs::remove_file(&t) {
                tracing::warn!(temp = %t.display(), error = %e, "嵌套会话临时档案清理失败");
            }
        }
        true
    }

    /// **组合导航树**（跨嵌套层）：最外层档案为根，内层压缩包当**容器节点**挂在其所属目录下。
    ///
    /// 用户口径：进内层后目录树不能改根——最初那个压缩包永远是根，子压缩包只是
    /// 它下面的一个「能展开的目录」。故铺行时逐层拼：
    ///
    /// ```text
    /// outer.tar            ← 根（最外层档案）
    ///   ├ sub/             ← 外层目录
    ///   │ └ child.tar      ← 容器节点（FolderZip 图标；展开 = 内层会话的行）
    ///   │     └ deep/      ← 内层目录（层号 0）
    ///   └ keep.txt …       ← 普通文件不进树
    /// ```
    ///
    /// 每行的 `level` 标明它属于哪个会话（0 = 当前会话），点击/展开据此路由
    /// ——同名的 `deep/` 可能内外各有一份，光靠路径分不出层。
    pub fn display_nav_rows(&mut self) -> Vec<crate::viewmodel::rows::TreeRow> {
        if self.open_stack.is_empty() {
            return self.main.nav_rows(); // 顶层：既有单层行为，逐字不变
        }
        // 先补齐各会话的树（懒建：树随会话建过一次即缓存，但释放句柄/重装等路径
        // 可能留 None——漏补的后果是组合树只剩一个根行）。
        for frame in &mut self.open_stack {
            frame.vm.tree();
        }
        self.main.tree();
        // 会话序列：最外层帧 → 当前会话；`descend_at` = 该层里要下钻的容器路径
        let depth = self.open_stack.len();
        let mut sessions: Vec<(&crate::viewmodel::MainViewModel, usize, Option<String>)> =
            Vec::with_capacity(depth + 1);
        // 帧 k 的 `container_path` = 从**该帧会话**进入下一层的容器 ⇒ 下钻目标
        // 就是它自己记的那条路径（最后一帧的下一层 = 当前 main，同样适用）。
        for (ix, frame) in self.open_stack.iter().enumerate() {
            sessions.push((&frame.vm, depth - ix, Some(frame.container_path.clone())));
        }
        sessions.push((&self.main, 0, None));

        let mut out = Vec::new();
        let root_vm = sessions[0].0;
        // 根行：最外层档案（虚拟节点，path 空）
        out.push(crate::viewmodel::rows::TreeRow {
            volume: None,
            path: String::new(),
            name: root_vm
                .source_path()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| crate::tr("vm.root_default_name")),
            depth: 0,
            is_dir: true,
            entry_id: None,
            expanded: root_vm.is_expanded(""),
            has_children: root_vm.tree_ref().is_some_and(|t| !t.children.is_empty()),
            is_container: false,
            encrypted: false,
            size_raw: 0,
            size_packed: 0,
            mtime: None,
            is_symlink: false,
            encryption: None,
            compression_level: None,
            crc32: None,
            attrs: Default::default(),
            level: depth,
            container_format: None,
        });
        if root_vm.is_expanded("")
            && let Some(tree) = root_vm.tree_ref()
        {
            emit_composite_rows(&sessions, 0, tree, "", 1, &mut out);
        }
        out
    }

    /// 切换某层某节点的展开态（左栏树双击）。层号越界/无该会话 → no-op。
    pub fn toggle_tree_expand(&mut self, level: usize, path: &str) {
        let depth = self.open_stack.len();
        if level == 0 {
            self.main.toggle_expand(path);
        } else if level <= depth {
            self.open_stack[depth - level].vm.toggle_expand(path);
        }
        // 不在这里重建行（2026-09-24 审计 B9）：展开态是 O(1) 数据面变更，
        // 行重建是 O(行集) 派生——后者由调用方发 `EntriesChanged` 后由泵**后台**
        // 重建（`spawn_rows_rebuild`）。旧实现在持锁的 UI 线程里直接重建。
    }

    /// **当前会话**的源档案名（嵌套层 = 内层档案名；空 = 未打开）。
    ///
    /// 组合档**就地展开**出的那一层（`a.cpio.gz` → `a.cpio`）没有自己的文件：
    /// 名字取展开链末段，否则面包屑/标题会把外层档案名重复一遍
    /// （`a.cpio.gz / a.cpio.gz`），而设计口径是与「双击内层压缩包」同构的
    /// `a.cpio.gz / a.cpio`（03 TC-GUI-115/116）。
    pub fn current_archive_name(&self) -> String {
        if let Some(name) = self.main.in_place_layer() {
            return name.to_string();
        }
        self.main
            .source_path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// 完整面包屑链：外层帧（层号 ≥1）+ 当前会话（层号 0）。
    ///
    /// 层号在**这里**统一赋值——帧里存的段自带 level=0（`crumbs_from_current_dir`
    /// 的产物），拼装时才按「距当前会话多远」改写。
    pub fn display_crumbs(&self) -> Vec<crate::ui::contracts::Crumb> {
        let mut out = Vec::new();
        let depth = self.open_stack.len();
        for (ix, frame) in self.open_stack.iter().enumerate() {
            let level = depth - ix;
            out.extend(
                frame
                    .crumbs
                    .iter()
                    .cloned()
                    .map(|c| crate::ui::contracts::Crumb { level, ..c }),
            );
        }
        out.extend(crate::ui::contracts::crumbs_from_current_dir(
            &self.current_archive_name(),
            self.main.current_dir(),
        ));
        out
    }

    /// 导出 UI 快照（镜像 [`crate::ui::shell::ShellState::apply`] 消费的字段集；
    /// R-GUI1-02 §2.1——tab/dialog_open/job_pill 归 UI 侧所有权，恒 None）。
    pub fn export_snapshot(&self) -> crate::ui::contracts::AppStateUiSnapshot {
        // 档案名/路径取**当前会话**的源档案：嵌套会话下 `self.archive` 仍是
        // 顶层档案（解压目标/MRU 的语义要它），而标题与面包屑根要的是内层名。
        let source = self.main.source_path().map(|p| p.to_path_buf());
        // 标题/面包屑根 = 本会话层名（就地展开的延迟层没有自己的文件，名字取自
        // 展开链——见 `current_archive_name`）
        let archive_name = self.current_archive_name();
        // 路径（标题 tooltip）用**外层实体档案**：嵌套会话的源是临时文件，
        // 把它塞进 tooltip 只会让人一头雾水；嵌套层级由面包屑负责表达。
        let archive_path = if self.open_stack.is_empty() {
            source
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            self.archive
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        // 可编辑投影（记忆化：同一档案只嗅探一次，见 `caps_edit_cache`）
        let caps_edit = self.caps_edit();
        // 最近打开（菜单 MRU；渲染路径直接读快照，不再锁 AppData）
        let mru = self.recent_archives.clone();
        // 内部剪贴板可用性（审计 B14：渲染帧不再自查）
        let has_clipboard = !hzip_platform::clipboard::internal_is_empty();
        // 任务中心行快照：锁成功才取（锁中毒退化空列表——快照不 panic）
        // Arc 句柄（2026-09-24 审 ⑦）：行集未变时与上一次导出 `ptr_eq` 相等——
        // 既省掉全量深拷贝，也让任务中心视图能据此跳过派生行重建（审 ⑲）。
        let job_rows = self
            .jobs
            .lock()
            .map(|vm| vm.rows_arc())
            .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));
        // 状态栏任务胶囊（R-GUI4 #3：第一条运行中任务——不切页签也能看进度）
        let job_pill = job_rows
            .iter()
            .find(|r| matches!(r.state, crate::viewmodel::JobRowState::Running))
            .map(|r| crate::ui::contracts::JobPill {
                job_id: r.job_id,
                summary: {
                    let kind = crate::ui::binding::job_kind_label(r.kind);
                    let kind: &str = &kind;
                    crate::tr_with(
                        "vm.job_pill_summary",
                        &[
                            ("kind", kind),
                            ("done", &r.entries_done.to_string()),
                            ("total", &r.entries_total.max(r.entries_done).to_string()),
                        ],
                    )
                },
                progress: r.progress.unwrap_or(0.0) as f32,
            });
        // 请求-应答弹窗待决投影（router；R-GUI4 #1——旧快照恒 None，
        // 密码/冲突请求 120 s 无人应答 → decision-timeout 失败的根因）
        let dialog = self.router.lock().ok().and_then(|r| {
            // 浏览密码门（设计稿 11 §4.3）：头加密档案打开失败 → 无任务弹窗
            //（job_id = u64::MAX 哨兵；answer_prompt 门分支先于 active_job）。
            if let Some(attempts) = r.gate_attempts() {
                return Some(crate::ui::contracts::UiDialogPrompt::Password {
                    job_id: u64::MAX,
                    target: None,
                    attempts,
                });
            }
            if let Some(p) = r.conflict_prompt() {
                return Some(crate::ui::contracts::UiDialogPrompt::Conflict {
                    job_id: p.job_id,
                    path: p.path.display().to_string(),
                    existing_is_dir: p.existing.is_dir,
                    existing_size: p.existing.size,
                });
            }
            match r.password_state() {
                crate::viewmodel::router::PasswordDialogState::Awaiting {
                    job_id, target, ..
                } => Some(crate::ui::contracts::UiDialogPrompt::Password {
                    job_id: *job_id,
                    target: target.as_ref().map(|t| t.to_string()),
                    attempts: 0,
                }),
                crate::viewmodel::router::PasswordDialogState::Retry {
                    job_id,
                    target,
                    attempts,
                    ..
                } => Some(crate::ui::contracts::UiDialogPrompt::Password {
                    job_id: *job_id,
                    target: target.as_ref().map(|t| t.to_string()),
                    attempts: *attempts,
                }),
                crate::viewmodel::router::PasswordDialogState::Hidden => None,
            }
        });
        // 面包屑跨会话：外层帧段（level ≥1）+ 当前会话段，层号在 AppData 里定
        let crumbs = self.display_crumbs();
        let nested = !self.open_stack.is_empty();
        crate::ui::contracts::AppStateUiSnapshot {
            tab: None, // UI 侧所有权：tab 切换由 AppShell 本地态持有
            busy: self.busy,
            busy_text: String::new(), // busy 文案由发起方（T-G30 泵）随 busy 置位旁路传递
            archive_name,
            nested,
            archive_path,
            current_dir: self.main.current_dir().to_string(),
            crumbs,
            nav_rows: std::sync::Arc::clone(&self.nav_rows),
            dir_rows: std::sync::Arc::clone(&self.dir_rows),
            entry_count: self.main.entry_count(),
            selection_count: self.main.selection_count(),
            selection: self
                .main
                .selection_set()
                .iter()
                .map(|id| *id as u64)
                .collect(),
            selection_dirs: self.main.dir_selection_set().iter().cloned().collect(),
            name_edit: self
                .name_edit
                .as_ref()
                .map(|e| crate::ui::contracts::NameEditView {
                    dir: e.dir.clone(),
                    from: e.from.clone(),
                }),
            // T-SEL2 选中统计：全局选择集 ∩ 当前目录可见行（导航残留 id
            // 不计入；实体目录条目 = 行聚合大小；隐式目录行 = 子树求和
            // ——T-SEL 走查 #1：目录选择与文件同口径进状态栏）
            sel_stats: {
                // T-OPT 项2：选择集**借用**查存（旧实现同一批里把 BTreeSet 物化
                // 成 Vec<u64>/Vec<String>/HashSet/… 共 4 份——泵每批都要付）
                let sel = self.main.selection_set();
                let dir_sel = self.main.dir_selection_set();
                let mut stats = crate::ui::contracts::SelectionStats::default();
                let mut visible_dirs: Vec<String> = Vec::new();
                for row in self.dir_rows.iter() {
                    if let Some(id) = row.entry_id
                        && sel.contains(&id)
                    {
                        if row.is_dir {
                            stats.dirs += 1;
                        } else {
                            stats.files += 1;
                        }
                        stats.size_raw += row.size_raw;
                        stats.size_packed += row.size_packed;
                    } else if row.is_dir && row.entry_id.is_none() && dir_sel.contains(&row.path) {
                        // 隐式目录行（entry_id None）：按路径选中——大小走
                        // 子树求和（不受 show_folder_size 开关影响）
                        stats.dirs += 1;
                        visible_dirs.push(row.path.clone());
                    }
                }
                if !visible_dirs.is_empty() {
                    let (raw, packed) = self.main.dir_subtree_sizes(&visible_dirs);
                    stats.size_raw += raw;
                    stats.size_packed += packed;
                }
                stats
            },
            suspect_count: self.main.suspect_count(),
            dialog_open: None, // UI 侧所有权：Modal 显隐归 AppShell
            job_rows,
            job_pill, // 第一条运行中任务（R-GUI4 #3）
            show_dotdot: self.settings.show_dotdot,
            show_tree: self.settings.show_tree,
            show_jobs: self.settings.show_jobs,
            toolbar_style_ix: self.settings.toolbar_style_ix,
            view_style_ix: self.settings.view_style_ix,
            columns: self.settings.columns.clone(),
            dialog,
            caps_edit,
            mru,
            has_clipboard,
        }
    }
}

/// 唯一的写盘尾步（T-SET §1.2 第三步）：`store` 之后恰好落盘一次。
///
/// 失败仅 warn 不 panic（内存态已更新）；测试进程不落盘
/// （`cfg!(test)`——防单测污染真实用户 `config.toml`：R-GUI4 曾发现
/// image_viewer/ctx_* 测试值被写进用户配置）。
fn persist(config: &hzip_config::Config) {
    if cfg!(test) {
        return;
    }
    // **异步**落盘（2026-09-24 审计 B6）：本函数在 UI 线程、且多数调用点在
    // AppData 锁内；`save()` 的同步 IO（序列化 + 写盘）连点设置时就是一连串
    // 帧内写盘。`save_async` 只记最新一份，由后台线程合并写。
    config.clone().save_async();
}

/// 直接子项判定（`dir`="" 取顶层名不含 '/'；`entry_name` 为档案内完整路径）。
///
/// dir_rows 从 entries 前缀过滤派生的核心谓词（R-GUI1-02 §2.1）；dir_children
/// 落地为 VM 方法后本谓词降级为单测面纯函数（防回归锚点），故 allow dead_code。
#[cfg_attr(not(test), allow(dead_code))]
fn is_direct_child(dir: &str, entry_name: &str) -> bool {
    match dir {
        "" => !entry_name.contains('/'),
        d => entry_name
            .strip_prefix(d)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|rest| !rest.contains('/')),
    }
}

#[cfg(test)]
#[path = "tests/state.rs"]
mod tests;
