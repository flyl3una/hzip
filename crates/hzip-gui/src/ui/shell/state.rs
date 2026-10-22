//! `ShellState`（快照投影状态；`apply`/`to_snapshot` 往返 + 行集变化检测）
//! ——自 `shell/mod.rs` 拆分。

use std::sync::Arc;

use crate::ui::contracts::{AppStateUiSnapshot, JobPill, MainTab};

/// Shell 内部状态（T-G10 §3.1 字段集）。
#[derive(Debug)]
pub struct ShellState {
    /// 当前页签（默认 Browse）
    pub tab: MainTab,
    /// 设置页签是否打开（**UI 本地态**：设置不再是弹层，见 `present_tabs`）
    pub settings_open: bool,
    /// 浏览页签被用户关掉（档案仍开着、页签暂隐；开新档案自动复位）
    pub browse_hidden: bool,
    /// busy 指示（busy 条显隐）
    pub busy: bool,
    /// busy 文案
    pub busy_text: String,
    /// 当前档案可编辑（快照投影；`caps_edit()` 要嗅探档案，**不得**在每帧渲染里调）
    pub caps_edit: bool,
    /// 最近打开（快照投影；菜单 MRU 用，渲染路径不再锁 AppData）
    pub mru: Vec<String>,
    /// 工具栏可用性（快照派生；T-BAR：**唯一事实源**——与右键菜单同口径，
    /// 由 `AppStateUiSnapshot::toolbar_enable()` 在 apply 时算一次，渲染帧零查询）
    pub toolbar_enable: crate::ui::contracts::ToolbarEnable,
    /// 内部剪贴板非空（快照投影；菜单「粘贴」可用性）
    pub has_clipboard: bool,
    /// 行内名称编辑态（快照投影；用户口径 2026-09-28——浏览页据此把目标行
    /// 名称渲染成输入框）
    pub name_edit: Option<crate::ui::contracts::NameEditView>,
    /// 是否**嵌套会话**（快照投影；此前漏投 ⇒ 浏览页恒 false：嵌套会话里
    /// `..` 行不退出该层、且「可编辑」判定会误放行——嵌套层的内容不在顶层
    /// 档案里，改名/新建必须禁掉）
    pub nested: bool,
    /// 挂起的改动条数（手动写回模式；快照投影）
    pub pending_count: usize,
    /// 鼠标位置（窗口坐标；悬浮提示锚点）
    pub hover_pos: (i32, i32),
    /// 当前悬浮提示（自绘 tooltip；UI 本地态，不进快照）
    pub hover_tip: Option<crate::ui::widgets::hover_tip::HoverTip>,
    /// 手动写回模式（快照投影；保存按钮常驻依据）
    pub write_back_manual: bool,
    /// 档案名（标题栏 + 面包屑根；空 = 未打开）
    pub archive_name: String,
    /// 档案完整路径（标题 tooltip）
    pub archive_path: String,
    /// 当前浏览目录（档案内路径，""=根；面包屑派生 + 左树跟进 + `..` 目标段
    /// 推导源——T-SET 修复：旧实现无此字段，快照值在 apply 里被丢弃 →
    /// 浏览页恒收 ""，`..` 目标恒为根、左树恒定位根行）
    pub current_dir: String,
    /// 条目总数
    pub entry_count: usize,
    /// 已选数
    pub selection_count: usize,
    /// 已选条目 ID 集（T-PERF2 断链修复：contracts 快照早已携带、但旧
    /// ShellState 无此字段——apply 丢弃、to_snapshot 不投，图标/列表模式
    /// 选中高亮读 `snap.selection` 恒空）
    pub selection: Vec<u64>,
    /// 已选隐式目录路径集（T-SEL 走查 #1：目录行高亮第二数据源——
    /// apply 变化检测 + to_snapshot 回投，往返纪律同 selection）
    pub selection_dirs: Vec<String>,
    /// 选中统计（T-SEL2 状态栏：可见行交集口径——文件/文件夹数与两口径
    /// 字节数；apply 变化检测 + to_snapshot 回投，往返纪律同 selection）
    pub sel_stats: crate::ui::contracts::SelectionStats,
    /// 可疑编码计数
    pub suspect_count: usize,
    /// 任务中心行数（页签 ⚠ 徽标）
    pub jobs_badge: usize,
    /// 任务胶囊（第一条运行中任务；None = 隐藏）
    pub job_pill: Option<JobPill>,
    /// 左栏导航树行快照（T-G53 浏览页渲染输入；TreeRow 纯数据。
    /// T-PERF4 Arc：to_snapshot 投影是 refcount 克隆而非深拷贝）
    pub nav_rows: std::sync::Arc<Vec<crate::viewmodel::rows::TreeRow>>,
    /// 右栏当前目录行快照（T-G53 浏览页渲染输入；T-PERF4 Arc）
    pub dir_rows: std::sync::Arc<Vec<crate::viewmodel::rows::TreeRow>>,
    /// 面包屑段快照（T-G53 浏览页渲染输入）
    pub crumbs: Vec<crate::ui::contracts::Crumb>,
    /// 任务中心行快照（R-GUI4：旧实现无此字段——快照 job_rows 落在 apply
    /// 的 Default 里被丢弃，任务中心恒空；T-PERF4 Arc）
    pub job_rows: std::sync::Arc<Vec<crate::viewmodel::JobRow>>,
    /// `..` 跳上级行显隐（ui.show_dotdot；R2-04 消费——T-SET 修复：旧实现
    /// 无此字段，快照值在 apply 里被丢弃 → 浏览页恒收 false）
    pub show_dotdot: bool,
    /// 浏览页左树显隐（ui.show_tree；T-MENU5——apply/to_snapshot 往返纪律）
    pub show_tree: bool,
    /// 任务中心页签显隐（ui.show_jobs；默认 false = 只有浏览页、无页签栏）
    pub show_jobs: bool,
    /// 工具栏三态下标（T-MENU5）
    pub toolbar_style_ix: usize,
    /// 视图样式下标（T-MENU5）
    pub view_style_ix: usize,
    /// 浏览列配置（ui.columns；浏览页唯一列设置来源——T-SET 修复：旧实现经
    /// `columns_dirty` + `sync_browse_hold` 旁路写 Global，冷启动不回放）
    pub columns: crate::ui::contracts::ColumnsConfig,
    /// 配置缓存（T-PERF3：渲染帧的 MenuContext/工具栏样式读此缓存——
    /// 旧实现每帧 `Config::load()` 读盘+解析两次，动画帧 16ms 预算被吃掉）。
    /// 失效由泵驱动：批内 `SettingApplied(_)`/`MenuEpochBump` →
    /// [`AppShell::refresh_cfg`] 重读一次（低频信号路径，非渲染帧）。
    /// 覆盖面见 T-PERF README §5 B3：MRU 变化必伴随 epoch、视图偏好经
    /// SettingApplied；`tree_width` 写盘无信号但渲染帧不消费该字段。
    pub cfg: Arc<hzip_config::Config>,
    /// 菜单数据版本（设计 §1.4 第三重建条件）：MRU/视图偏好变化即递增，
    /// `MenuBar::sync` 消费重建快照。T-MENU1 落地机制；4/5 任务消费。
    pub menu_epoch: u64,
}

/// `TreeRow` 行集变化判定（TreeRow 无 PartialEq——长度 + 逐行字段对比）。
/// T-G53 apply 变化检测辅助；输入为克隆前后行集。
fn nav_rows_changed(
    before: &[crate::viewmodel::rows::TreeRow],
    after: &[crate::viewmodel::rows::TreeRow],
) -> bool {
    before.len() != after.len()
        || before.iter().zip(after).any(|(a, b)| {
            a.path != b.path
                || a.depth != b.depth
                || a.is_dir != b.is_dir
                || a.expanded != b.expanded
                || a.has_children != b.has_children
                || a.entry_id != b.entry_id
        })
}

/// `JobRow` 行集变化判定（JobRow 无 PartialEq——长度 + 关键字段对比；
/// R-GUI4：任务中心行流经 ShellState 的变化检测）。
fn job_rows_changed(
    before: &[crate::viewmodel::JobRow],
    after: &[crate::viewmodel::JobRow],
) -> bool {
    before.len() != after.len()
        || before.iter().zip(after).any(|(a, b)| {
            a.job_id != b.job_id
                || a.state != b.state
                || a.progress.map(f64::to_bits) != b.progress.map(f64::to_bits)
                || a.entries_done != b.entries_done
                || a.entries_total != b.entries_total
                || a.speed_bps != b.speed_bps
        })
}

impl ShellState {
    /// 初始态（未打开档案）。
    pub fn new() -> Self {
        Self {
            tab: MainTab::Browse,
            settings_open: false,
            browse_hidden: false,
            busy: false,
            busy_text: String::new(),
            caps_edit: false,
            toolbar_enable: crate::ui::contracts::ToolbarEnable::default(),
            has_clipboard: !hzip_platform::clipboard::internal_is_empty(),
            name_edit: None,
            nested: false,
            pending_count: 0,
            hover_pos: (0, 0),
            hover_tip: None,
            write_back_manual: false,
            // 初值从配置读（首个泵快照到达前菜单不至于空）；此后以快照为准
            mru: hzip_config::Config::load().ui.recent_archives,
            archive_name: String::new(),
            archive_path: String::new(),
            current_dir: String::new(),
            entry_count: 0,
            selection_count: 0,
            selection: Vec::new(),
            selection_dirs: Vec::new(),
            sel_stats: crate::ui::contracts::SelectionStats::default(),
            suspect_count: 0,
            jobs_badge: 0,
            job_pill: None,
            nav_rows: std::sync::Arc::new(Vec::new()),
            dir_rows: std::sync::Arc::new(Vec::new()),
            crumbs: Vec::new(),
            job_rows: std::sync::Arc::new(Vec::new()),
            // hzip-config 默认（[ui].show_dotdot = true / columns 全序）：
            // 首个泵批次到达前浏览页不渲染（archive_name 空 → 空态），
            // 故此初值只在极短窗口内可见
            show_dotdot: true,
            show_tree: true,
            show_jobs: false,
            toolbar_style_ix: 0,
            view_style_ix: 0,
            columns: crate::ui::contracts::ColumnsConfig::default(),
            // 窗口创建期读一次（T-PERF3：之后渲染帧零磁盘 I/O）
            cfg: Arc::new(hzip_config::Config::load()),
            menu_epoch: 0,
        }
    }

    /// 当前**打开的**页签集（有序；唯一事实源）。
    ///
    /// 页面不是常量：由活态派生（用户口径 2026-09-23）——
    ///
    /// | 页签 | 出现条件 |
    /// |---|---|
    /// | 浏览 | 有打开的档案 且 未被用户关掉（`browse_hidden`） |
    /// | 任务中心 | `ui.show_jobs`（视图菜单勾选 / 页签关闭钮同一事实源） |
    /// | 设置 | 「设置」动作开过（`settings_open`，UI 本地态） |
    ///
    /// 刚启动 = 全空（无档案、show_jobs 默认 false）→ 页签栏不渲染、内容区空态。
    pub fn present_tabs(&self) -> Vec<MainTab> {
        let mut tabs = Vec::with_capacity(3);
        if !self.archive_name.is_empty() && !self.browse_hidden {
            tabs.push(MainTab::Browse);
        }
        if self.show_jobs {
            tabs.push(MainTab::Jobs);
        }
        if self.settings_open {
            tabs.push(MainTab::Settings);
        }
        tabs
    }

    /// 页签是否存在（内容分支/切换门槛；顺序即 `present_tabs` 顺序）。
    pub fn tab_present(&self, tab: MainTab) -> bool {
        self.present_tabs().contains(&tab)
    }

    /// 从快照迁移（`update_from_state` 调用；返回是否变化——测试断言面）。
    ///
    /// T-PERF2：**比较先行**（先逐字段对比、再赋值——删除旧「先克隆 4 份
    /// 行集留底再对比」的写法，10 Hz 空转周期不再付出克隆成本）；返回值
    /// 是 `update_from_state` 的 notify 门控——漏检字段 = 界面静默不刷新，
    /// 新增消费字段必须同步进检测清单（往返测试守纪律）。
    pub fn apply(&mut self, snap: &AppStateUiSnapshot) -> bool {
        // 派生修态前的页签集/活动页签（末尾补比用，见函数尾注）
        let tab_before = self.tab;
        let tabs_before = self.present_tabs();
        // **打开/更换档案**判据（用户实测缺陷 2026-09-24：在任务中心/设置页时打开
        // 压缩包，页签停在原处 ⇒ 看不到刚打开的内容）。比较在**克隆之前**做：
        // `archive_name` 是 String，逐帧 clone 比一次 `&str` 比较贵（T-PERF 纪律）。
        // 只在「名字变了且非空」时成立——同一档案的快照逐帧不变，不得反复复位
        //（否则用户在浏览中切到任务中心会被每帧拽回来）。
        let archive_opened =
            self.archive_name != snap.archive_name && !snap.archive_name.is_empty();
        // TreeRow 无 PartialEq——变化检测逐字段比较（避免派生污染 VM 纯数据面）
        let jobs_badge_new = snap
            .job_rows
            .iter()
            .filter(|r| matches!(r.state, crate::viewmodel::JobRowState::Failed))
            .count();
        let job_pill_changed = match (&self.job_pill, &snap.job_pill) {
            (Some(a), Some(b)) => {
                a.job_id != b.job_id
                    || a.progress.to_bits() != b.progress.to_bits()
                    || a.summary != b.summary
            }
            (None, None) => false,
            _ => true,
        };
        let changed = snap.tab.is_some_and(|t| t != self.tab)
            || self.busy != snap.busy
            || self.busy_text != snap.busy_text
            || self.caps_edit != snap.caps_edit
            || self.toolbar_enable != snap.toolbar_enable()
            || self.mru != snap.mru
            || self.has_clipboard != snap.has_clipboard
            || self.name_edit != snap.name_edit
            || self.nested != snap.nested
            || self.pending_count != snap.pending_count
            || self.write_back_manual != snap.write_back_manual
            || self.archive_name != snap.archive_name
            || self.archive_path != snap.archive_path
            || self.current_dir != snap.current_dir
            || self.entry_count != snap.entry_count
            || self.selection_count != snap.selection_count
            || self.selection != snap.selection
            || self.selection_dirs != snap.selection_dirs
            || self.sel_stats != snap.sel_stats
            || self.suspect_count != snap.suspect_count
            || self.jobs_badge != jobs_badge_new
            || job_pill_changed
            || self.show_dotdot != snap.show_dotdot
            || self.show_tree != snap.show_tree
            || self.show_jobs != snap.show_jobs
            || self.toolbar_style_ix != snap.toolbar_style_ix
            || self.view_style_ix != snap.view_style_ix
            || self.columns != snap.columns
            || self.crumbs != snap.crumbs
            || nav_rows_changed(&self.nav_rows, &snap.nav_rows)
            || nav_rows_changed(&self.dir_rows, &snap.dir_rows)
            || job_rows_changed(&self.job_rows, &snap.job_rows);
        if let Some(tab) = snap.tab {
            self.tab = tab;
        }
        self.busy = snap.busy;
        self.busy_text.clone_from(&snap.busy_text);
        self.caps_edit = snap.caps_edit;
        self.toolbar_enable = snap.toolbar_enable();
        self.mru.clone_from(&snap.mru);
        self.has_clipboard = snap.has_clipboard;
        self.name_edit.clone_from(&snap.name_edit);
        self.nested = snap.nested;
        self.pending_count = snap.pending_count;
        self.write_back_manual = snap.write_back_manual;
        self.archive_name.clone_from(&snap.archive_name);
        self.archive_path.clone_from(&snap.archive_path);
        // 当前目录（T-SET 修复：旧实现无此字段 → 浏览页 `..` 目标恒根）
        self.current_dir.clone_from(&snap.current_dir);
        self.entry_count = snap.entry_count;
        self.selection_count = snap.selection_count;
        self.selection.clone_from(&snap.selection);
        self.selection_dirs.clone_from(&snap.selection_dirs);
        self.sel_stats = snap.sel_stats;
        self.suspect_count = snap.suspect_count;
        self.jobs_badge = jobs_badge_new;
        self.nav_rows.clone_from(&snap.nav_rows);
        self.dir_rows.clone_from(&snap.dir_rows);
        self.crumbs.clone_from(&snap.crumbs);
        self.job_pill = snap.job_pill.clone();
        // 任务中心行（R-GUI4：旧实现丢弃——jobs 页恒空）
        self.job_rows.clone_from(&snap.job_rows);
        // 浏览页设置类输入（T-SET 修复：旧实现两字段均被丢弃——
        // `..` 行恒隐、列设置仅靠旁路 Global 写回且冷启动不回放）
        self.show_dotdot = snap.show_dotdot;
        // T-MENU5 视图偏好（同款断链防护——apply 必收，to_snapshot 必投）
        self.show_tree = snap.show_tree;
        self.show_jobs = snap.show_jobs;
        // 任务中心被关掉时当前页签回浏览（页签不再存在——停留会让内容分支
        // 落在隐藏页上；唯一事实源是快照，故修态收在此处）
        if !self.show_jobs && self.tab == MainTab::Jobs {
            self.tab = MainTab::Browse;
        }
        // 新档案打开 → 浏览页签复位（用户手动关过也要回来，否则「打开了却
        // 看不到」）。
        if !self.archive_name.is_empty() {
            self.browse_hidden = false;
        }
        // 新档案打开 → **聚焦浏览页**（用户口径 2026-09-24）：打开压缩包就该看到
        // 内容，而不是留在任务中心/设置页上。「页签存在」由上面的复位保证。
        if archive_opened && self.tab_present(MainTab::Browse) {
            self.tab = MainTab::Browse;
        }
        // 内容分支必须落在**存在**的页签上：页签被关（或档案关掉后浏览页签
        // 消失）→ 回落到首个存在的页签；全无 → 由 render 走空态。
        if !self.tab_present(self.tab) {
            self.tab = self
                .present_tabs()
                .first()
                .copied()
                .unwrap_or(MainTab::Browse);
        }
        self.toolbar_style_ix = snap.toolbar_style_ix;
        self.view_style_ix = snap.view_style_ix;
        self.columns.clone_from(&snap.columns);
        // **派生页签集也要进变化检测**：本函数里的修态（开档案复位
        // `browse_hidden`、把 `tab` 收到存在的页签上）**不来自快照**——快照逐字
        // 不变时上面的字段对比全为假，`update_from_state` 就不 notify，界面永不
        // 重画 ⇒ 「关掉页签后再打开同一档案，页签不出现」（用户实测缺陷
        // 2026-09-23）。补比「页签集 + 活动页签」即可覆盖全部派生修态。
        changed || self.tab != tab_before || self.present_tabs() != tabs_before
    }

    /// 渲染投影（ShellState → 视图快照；`AppShell::render` 唯一调用方）。
    ///
    /// **修复纪律**：`AppData::export_snapshot` 的字段若被视图消费，就必须在
    /// `apply` 里接收、在此处回投——漏掉任一段即断链（历史事故：`show_dotdot` /
    /// `columns` / `current_dir` 三个字段被丢弃，表现为「设置改了没用」「`..`
    /// 跳根」「左树不跟随」）。往返测试 `snapshot_round_trips_through_shell_state`
    /// 守住这条纪律。
    ///
    /// 刻意**不**投的快照字段：`dialog_open`（Modal 显隐归 AppShell 本地态）、
    /// `dialog`（请求-应答弹窗由泵直接消费 `AppData` 快照的投影，不经 ShellState）。
    pub fn to_snapshot(&self) -> AppStateUiSnapshot {
        AppStateUiSnapshot {
            tab: Some(self.tab),
            busy: self.busy,
            busy_text: self.busy_text.clone(),
            archive_name: self.archive_name.clone(),
            archive_path: self.archive_path.clone(),
            current_dir: self.current_dir.clone(),
            crumbs: self.crumbs.clone(),
            nav_rows: self.nav_rows.clone(),
            dir_rows: self.dir_rows.clone(),
            entry_count: self.entry_count,
            selection_count: self.selection_count,
            selection: self.selection.clone(),
            selection_dirs: self.selection_dirs.clone(),
            sel_stats: self.sel_stats,
            suspect_count: self.suspect_count,
            job_pill: self.job_pill.clone(),
            job_rows: self.job_rows.clone(),
            show_dotdot: self.show_dotdot,
            show_tree: self.show_tree,
            show_jobs: self.show_jobs,
            toolbar_style_ix: self.toolbar_style_ix,
            view_style_ix: self.view_style_ix,
            columns: self.columns.clone(),
            // —— 渲染真正消费、但此前漏投的字段 ——
            // 漏投的症状：浏览页恒拿默认值 ⇒ 嵌套会话的 `..` 语义失效
            // （`nested=false`）、行内编辑的目标行匹配不上（`name_edit=None`）、
            // 菜单「重命名/新建文件夹」恒禁用（`caps_edit=false`）。
            nested: self.nested,
            caps_edit: self.caps_edit,
            // 手动写回的挂起条数：漏投 ⇒ 过滤框左侧「保存 (n)」按钮永远不出现
            //（2026-10-19 用户实测：通知说「点保存」，界面上却没有按钮）
            pending_count: self.pending_count,
            write_back_manual: self.write_back_manual,
            name_edit: self.name_edit.clone(),
            ..Default::default()
        }
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}
