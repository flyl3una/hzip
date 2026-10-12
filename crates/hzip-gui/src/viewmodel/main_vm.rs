//! MainViewModel：浏览数据面（02 §11.1 VM1；T41）。
//!
//! `Engine::browse`（02 §9.10 同步轻量——不占线程池）的 UI 投影：条目缓存 /
//! 过滤 / 选择集 / 懒构建目录树 / 可疑编码计数。
//!
//! # 职责与边界（rustdoc）
//!
//! - `open()` 调用 `Engine::browse`（Browser 生命周期归本 VM）；`refresh()` 以
//!   同一路径重开（core `Browser::refresh` 需嵌套浏览器可重开前提且签名带
//!   source——本 VM 直接 browse() 重开，语义一致）；
//! - `entries()` 为 browse 一次快照（零 IO，红线 1 保障列表秒开）；
//! - `set_filter` 仅作用于缓存（过滤后视图克隆，调 0 次 reader）；
//! - `tree()` 懒构建（`core::browse::TreeNode::build`），首次请求树视图时算一次；
//! - `suspect_count`：`NameEncoding::Suspect(_)`/`Unknown` 条目数 → 编码修复
//!   预览入口徽标（02 §11.4）。
//!
//! 展示层职责（本地时区/i18n）归 T42；本层 mtime 存 RFC3339 UTC 占位字符串。

use std::collections::BTreeSet;
use std::path::Path;

use hzip_core::engine::Engine;
use hzip_core::services::browse::{Browser, TreeNode};
use hzip_error::HzipError;
use hzip_format::{EntryId, NameEncoding, OpenOptions, Source};

use super::rows::EntryRow;

/// 过滤条件（02 附录 B.1 ui 项语义；名称子串 / glob / 仅目录 / 隐藏 __MACOSX）。
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// 名称子串（None = 不限；按档案内名匹配，大小写不敏感）
    pub name_substr: Option<String>,
    /// glob 模式（None = 不限；语义同 `Browser::set_filter` 的段级匹配：
    /// 整名命中 **或** 任一 `/` 路径段命中）
    pub glob: Option<String>,
    /// 仅目录
    pub dirs_only: bool,
    /// 隐藏 `__MACOSX/` 前缀条目（Bandizip 惯例）
    pub hide_macosx: bool,
}

/// 浏览数据面（02 §11.1 VM1）。
/// （不 derive Debug——内含 `core::services::browse::Browser` 无 Debug。）
pub struct MainViewModel {
    /// 浏览会话（Browser 持有 reader + entries 缓存）
    browser: Option<Browser>,
    /// browse() 一次快照（列表零 IO）
    entries: Vec<EntryRow>,
    /// 原始条目数（不过滤）
    entry_count: usize,
    /// 过滤条件
    filter: Filter,
    /// 过滤派生形式（T-OPT 项12）：子串小写与 glob 编译结果**一次算好**。
    ///
    /// 旧实现在 `matches` / `dir_children` / `dir_name_hits_filter` /
    /// `emit_dir_nav` 里**逐条目**重做 `to_lowercase()` 与 `glob::Pattern::new()`
    /// ——每敲一个字符 = O(N) 次模式编译（大档案下过滤输入明显卡）。
    /// 派生与 `filter` 同批更新：`set_filter` 是唯一写入口（`new` 走默认）。
    ///
    /// 两个子串字段的口径**照抄各自调用点**（本轮不改语义）：`matches` 用
    /// ASCII 小写（`to_ascii_lowercase`），其余三处用 Unicode `to_lowercase`。
    filter_substr_ascii: Option<String>,
    filter_substr_unicode: Option<String>,
    filter_glob: Option<glob::Pattern>,
    /// 选择集（条目 ID；升序）
    selection: BTreeSet<EntryId>,
    /// 隐式目录选择集（T-SEL 走查 #1：tar/无目录条目档案的目录行
    /// `entry_id = None`——按**档案内路径**选择；与 `selection`（实体 id）
    /// 互补构成完整行选择。实体目录条目（zip 等）走 `selection`）
    dir_selection: BTreeSet<String>,
    /// 懒构建目录树（首次请求树视图时算一次）
    tree: Option<TreeNode>,
    /// 展开的目录路径集（段名 `/` 连接；当前浏览层）
    expanded: std::collections::BTreeSet<String>,
    /// 当前浏览目录（段名 `/` 连接；""=档案根）——双栏右列表数据面
    current_dir: String,
    /// 目录聚合大小表（全量 entries 一次计算；`dir_size_map` 产物——
    /// 目录路径 → (原始字节和, 打包字节和)）。#3 查看器 Tab 数据面：
    /// 右栏逐层浏览后行集只含一层，聚合必须基于全量而非可见行集。
    dir_sizes: std::collections::HashMap<String, (u64, u64)>,
    /// 目录子树大小表（T-OPT 项2：`dir_subtree_sizes` 的 O(1) 前置数据；
    /// 全量 entries 一次计算——旧实现每次调用对每个选中目录全量重扫
    /// entries，大档案 + 宽选择是 O(N×D)）。口径见 `dir_subtree_size_map`。
    dir_subtree_sizes_map: std::collections::HashMap<String, (u64, u64)>,
    /// 可疑编码（Suspect/Unknown）计数 → 编码修复预览入口徽标
    suspect_count: usize,
    /// 当前源路径（refresh 重开用）
    path: Option<std::path::PathBuf>,
}

impl Default for MainViewModel {
    fn default() -> Self {
        Self::new()
    }
}

impl MainViewModel {
    /// 新建（空浏览态）。
    pub fn new() -> Self {
        Self {
            browser: None,
            entries: Vec::new(),
            entry_count: 0,
            filter: Filter::default(),
            filter_substr_ascii: None,
            filter_substr_unicode: None,
            filter_glob: None,
            selection: BTreeSet::new(),
            dir_selection: BTreeSet::new(),
            tree: None,
            expanded: BTreeSet::new(),
            current_dir: String::new(),
            dir_sizes: std::collections::HashMap::new(),
            dir_subtree_sizes_map: std::collections::HashMap::new(),
            suspect_count: 0,
            path: None,
        }
    }

    /// 打开档案并浏览（02 §9.10 B→E：`Engine::browse` + 转行）。
    ///
    /// 头加密档案（HZ-3102）原样上抛——UI 提示后带密码重试（密码通道归
    /// [`super::router`]）。
    pub fn open(&mut self, engine: &Engine, path: &Path) -> Result<(), HzipError> {
        // 两个消费方都要**独占** PathBuf（`Source::File` 拿到所有权、`set_browser`
        // 存进 `self.path`）⇒ 这里必付两次路径分配；T-OPT 项12 复核后不改。
        let browser = engine.browse(Source::File(path.to_path_buf()), &OpenOptions::default())?;
        self.set_browser(browser, Some(path.to_path_buf()))
    }

    /// 用**后台线程已建好的** `Browser` 落账（R-GUI2-06）。
    ///
    /// `browse` 移出 UI 线程后，结果由调用方带回主线程安装——与 [`Self::open`]
    /// 完全同一落账路径（`set_browser`：转行 + 选择/树失效重置 + 根展开）。
    pub fn install_browser(&mut self, browser: Browser, path: &Path) -> Result<(), HzipError> {
        self.set_browser(browser, Some(path.to_path_buf()))
    }

    /// 按档案内路径取条目 ID（嵌套打开：把某个内层档案条目解出来）。
    ///
    /// 只认文件条目——目录条目没有可读数据流（`read_entry` 对目录返回错误）。
    pub fn entry_id_of_path(&self, path: &str) -> Option<EntryId> {
        self.entries
            .iter()
            .find(|e| !e.is_dir && e.name == path)
            .map(|e| e.id)
    }

    /// 读某条目的数据流（嵌套打开提取内层档案用；与 `preview` 同源 reader）。
    ///
    /// 调用方**不要持 AppData 锁**——大条目会读很久（见 `Browser::read_entry`）。
    /// `pw`：会话密码（加密条目用；`None` = 未加密/未缓存 → 加密条目 3101）。
    pub fn read_entry(
        &self,
        id: EntryId,
        pw: Option<&zeroize::Zeroizing<String>>,
    ) -> Result<Box<dyn std::io::Read + Send>, HzipError> {
        self.browser
            .as_ref()
            .ok_or_else(|| HzipError::Unsupported {
                what: "hz-0000 no-open-archive".into(),
            })?
            .read_entry(id, password_ref(pw))
    }

    /// 当前会话源路径（磁盘上的档案；嵌套层 = 解出来的临时文件）。
    pub fn source_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 重读（外部修改档案后的刷新语义）：以同一路径 `browse()` 重开。
    pub fn refresh(&mut self, engine: &Engine) -> Result<(), HzipError> {
        let Some(path) = self.path.clone() else {
            return Ok(()); // 无档案打开：无操作
        };
        // `self.path` 必须保留（重读失败后仍要能再试）⇒ 必付两次克隆；
        // T-OPT 项12 复核后不改。
        let browser = engine.browse(Source::File(path.clone()), &OpenOptions::default())?;
        self.set_browser(browser, Some(path))
    }

    /// 换浏览器（open/refresh 共用落账：转行 + 选择/树失效重置）。
    fn set_browser(
        &mut self,
        browser: Browser,
        path: Option<std::path::PathBuf>,
    ) -> Result<(), HzipError> {
        let infos = browser.entries();
        self.entry_count = browser.entry_count();
        self.entries = infos.into_iter().map(entry_row).collect();
        // 目录聚合大小表（全量一次；refresh_browse_rows 每次快照都查——不能按需重算）
        self.dir_sizes = crate::viewmodel::rows::dir_size_map(&self.entries);
        // 选中统计用的子树大小表同批算好（T-OPT 项2：状态栏每批快照都要查两处）
        self.dir_subtree_sizes_map = crate::viewmodel::rows::dir_subtree_size_map(&self.entries);
        self.suspect_count = self
            .entries
            .iter()
            .filter(|e| !matches!(e.name_encoding, NameEncoding::Utf8))
            .count();
        self.browser = Some(browser);
        self.path = path;
        self.selection.clear(); // 新档案选择集重置
        self.dir_selection.clear();
        self.tree = None; // 树失效（懒重建）
        self.expanded.clear();
        // 档案根默认展开（R-GUI4 #1/#2：根 = 压缩包节点，展开即露出顶层
        // 目录；子目录默认折叠。双击根可再收起——toggle 语义不变）
        self.expanded.insert(String::new());
        self.current_dir = String::new(); // 回档案根
        Ok(())
    }

    /// 当前视图状态快照（目录 + 树展开集）——编辑重开前取、重开后还。
    ///
    /// 用户口径 2026-09-29：重命名/新建会重写档案并重开浏览会话，而
    /// [`Self::set_browser`] 会把 `current_dir` 归零、`expanded` 清空 ⇒ 用户每次
    /// 改完名都被踢回档案根。故重开前记住、重开后种回。
    pub fn view_state(&self) -> (String, std::collections::BTreeSet<String>) {
        (self.current_dir.clone(), self.expanded.clone())
    }

    /// 种回视图状态（目录 + 展开集）；目录在新档案里不存在 → 回落档案根
    /// （例如改名的就是当前目录所在的路径）。
    pub fn restore_view_state(
        &mut self,
        dir: &str,
        expanded: std::collections::BTreeSet<String>,
    ) -> bool {
        self.expanded = expanded;
        // 目录存在判据：有同名目录条目，或存在 `dir/…` 前缀的子条目
        // （隐式目录：档案里没有独立目录条目）
        let prefix = format!("{dir}/");
        let exists = dir.is_empty()
            || self
                .entries
                .iter()
                .any(|e| e.name == dir || e.name.starts_with(&prefix));
        if exists {
            self.current_dir = dir.to_string();
        } else {
            self.current_dir = String::new();
            self.expanded.insert(String::new());
        }
        self.tree = None; // 树失效（懒重建）
        exists
    }

    /// 已打开（有浏览会话）。
    pub fn is_open(&self) -> bool {
        self.browser.is_some()
    }

    /// 释放浏览会话（关闭持有的档案文件句柄；EditBatch 原子替换前必须调用
    /// ——Windows 上目标被打开时 MoveFileExW REPLACE_EXISTING 报 err 5 拒绝
    /// 访问，任务必失败。保留 entries 缓存供 UI 显示；完成后经 refresh 重开）。
    pub fn release_browser(&mut self) {
        self.browser = None;
    }

    /// 全部条目（不过滤；browse 快照）。
    pub fn entries(&self) -> &[EntryRow] {
        &self.entries
    }

    /// 原始条目数（不过滤）。
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// 目录聚合大小表（全量 entries 计算；目录路径 → (原始, 打包)）。
    ///
    /// 消费方：`AppData::refresh_browse_rows`（`show_folder_size` 开时回填
    /// 右栏目录行）。键 = 档案内完整目录路径，与 `dir_children`/搜索扁平
    /// 行的 `path` 同一命名空间，直接查表即命中。
    pub fn dir_sizes(&self) -> &std::collections::HashMap<String, (u64, u64)> {
        &self.dir_sizes
    }

    /// 名称过滤判定（子串 + glob + 仅目录 + 隐藏 __MACOSX）。
    fn matches(&self, e: &EntryRow) -> bool {
        let name = e.name.as_str();
        if self.filter.dirs_only && !e.is_dir {
            return false;
        }
        if self.filter.hide_macosx && (name == "__MACOSX" || name.starts_with("__MACOSX/")) {
            return false;
        }
        if let Some(sub) = &self.filter_substr_ascii
            && !name.to_ascii_lowercase().contains(sub)
        {
            return false;
        }
        if let Some(p) = &self.filter_glob {
            let hit = p.matches(name)
                || (name.contains('/') && name.split('/').any(|seg| p.matches(seg)));
            if !hit {
                return false;
            }
        }
        true
    }

    /// 过滤派生形式重建（T-OPT 项12；`filter` 的**唯一**写入口
    /// [`Self::set_filter`] 调用；`new` 走默认值）。
    ///
    /// 非法 glob（`Pattern::new` Err）→ `None` = 该维度不限（与旧实现
    /// 「编译失败即跳过该约束」逐字一致）。
    fn refresh_filter_derived(&mut self) {
        self.filter_substr_ascii = self
            .filter
            .name_substr
            .as_ref()
            .map(|s| s.to_ascii_lowercase());
        self.filter_substr_unicode = self.filter.name_substr.as_ref().map(|s| s.to_lowercase());
        self.filter_glob = self
            .filter
            .glob
            .as_ref()
            .and_then(|g| glob::Pattern::new(g).ok());
    }

    /// 过滤后视图（应用 `self.filter`；返回行克隆——供列表模型重建）。
    pub fn filtered_entries(&self) -> Vec<EntryRow> {
        self.entries
            .iter()
            .filter(|e| self.matches(e))
            .cloned()
            .collect()
    }

    /// 应用过滤条件；返回过滤后视图（同 [`filtered_entries`](Self::filtered_entries)）。
    ///
    /// 选择集收缩到仍可见条目（隐藏条目自动取消选中）。
    pub fn set_filter(&mut self, f: Filter) -> Vec<EntryRow> {
        self.filter = f;
        self.refresh_filter_derived();
        // 先算保留集再 retain（避免闭包内 self 双重借用）
        let keep: Vec<EntryId> = self
            .selection
            .iter()
            .copied()
            .filter(|id| {
                self.entries
                    .get(*id)
                    .map(|e| self.matches(e))
                    .unwrap_or(false)
            })
            .collect();
        self.selection = keep.into_iter().collect();
        self.filtered_entries()
    }

    /// 当前过滤条件。
    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    /// 单条目（id 越界 → None）。
    pub fn entry(&self, id: EntryId) -> Option<&EntryRow> {
        self.entries.get(id)
    }

    /// 切换选择（选中移除 / 未选加入；越界 id 静默忽略）。
    pub fn toggle_select(&mut self, id: EntryId) {
        if !self.selection.remove(&id) && id < self.entries.len() {
            self.selection.insert(id);
        }
    }

    /// 选择集替换为单条目（越界静默忽略；搜索结果行点击定位用）。
    pub fn select_only(&mut self, id: EntryId) {
        self.selection.clear();
        if id < self.entries.len() {
            self.selection.insert(id);
        }
    }

    /// 选择集整组替换（右栏行点击上行：`RowAction::Select` → 数据面唯一
    /// 事实源；icon/list 模式选中高亮回显经快照 selection 消费同源）。
    /// 越界 ID 静默忽略；空集 = 清空（`..` 行点击）。隐式目录选择一并清空
    /// （整组替换语义——双通道同源替换）。
    pub fn select_ids(&mut self, ids: &[EntryId]) {
        self.select_rows(ids, &[]);
    }

    /// 双通道整组替换（T-SEL 走查 #1：实体行 id + 隐式目录行路径——
    /// 两集一次替换，行点击/右键预处理/全选共用出口）。
    pub fn select_rows(&mut self, ids: &[EntryId], dirs: &[String]) {
        self.selection = ids
            .iter()
            .copied()
            .filter(|id| *id < self.entries.len())
            .collect();
        self.dir_selection = dirs.iter().cloned().collect();
    }

    /// 隐式目录选择集（档案内路径升序）。
    pub fn dir_selection(&self) -> Vec<String> {
        self.dir_selection.iter().cloned().collect()
    }

    /// 隐式目录选择集**借用**（T-OPT 项2：快照导出每批都要读它——只读处
    /// 不该为比较/查存而克隆；需要独占所有权时才用 [`Self::dir_selection`]）。
    pub fn dir_selection_set(&self) -> &BTreeSet<String> {
        &self.dir_selection
    }

    /// 切换隐式目录选择（T-SEL：Ctrl+单击目录行增删；选中移除/未选加入）。
    pub fn toggle_select_dir(&mut self, path: &str) {
        if !self.dir_selection.remove(path) {
            self.dir_selection.insert(path.to_string());
        }
    }

    /// 有效实体 id 展开（T-SEL：实体选择 ∪ 隐式目录选择的**子树**——
    /// 目录按档案内路径前缀展开。解压选定/删除/复制等批量操作消费面：
    /// 选目录 = 选中其全部内容，资源管理器语义）。
    ///
    /// T-OPT 项2：旧实现「每个选中目录 × 全量 entries 前缀扫描」= O(N×D)
    /// （宽选择 + 大档案时单次动作可到秒级）。改为**单趟 entries + 逐层收缩
    /// 祖先段查选择集**（段边界天然成立，与 `"{dir}/"` 严格前缀同集合）：
    /// O(N × 深度 × log D)，语义逐项不变。
    pub fn effective_selection_ids(&self) -> Vec<EntryId> {
        if self.dir_selection.is_empty() {
            return self.selection.iter().copied().collect();
        }
        let mut set = self.selection.clone();
        for e in &self.entries {
            let mut end = e.name.len();
            while let Some(slash) = e.name[..end].rfind('/') {
                if self.dir_selection.contains(&e.name[..slash]) {
                    set.insert(e.id);
                }
                end = slash;
            }
        }
        set.into_iter().collect()
    }

    /// 隐式目录子树大小合计（T-SEL 状态栏统计：目录选中大小的口径——
    /// 全量 entries 前缀求和，不受 `show_folder_size` 开关影响）。
    ///
    /// T-OPT 项2：改查 `set_browser` 期算好的 [`Self::dir_subtree_sizes_map`]
    /// （同一前缀口径、同样不跳过目录条目），每次调用从 O(N×D) 降到 O(D)。
    /// 表里没有的目录 = 无子孙 → 0（与旧实现空和一致）。
    pub fn dir_subtree_sizes(&self, dirs: &[String]) -> (u64, u64) {
        let (mut raw, mut packed) = (0, 0);
        for dir in dirs {
            if let Some((r, p)) = self.dir_subtree_sizes_map.get(dir) {
                raw += r;
                packed += p;
            }
        }
        (raw, packed)
    }

    /// 全选/清空（`clear=true` 清空；否则全选过滤后可见条目——实体条目
    /// 全集 + 清隐式目录选择：目录内容已由子实体覆盖，目录行本身无需选中）。
    pub fn select_all(&mut self, clear: bool) {
        if clear {
            self.selection.clear();
            self.dir_selection.clear();
        } else {
            self.selection = self
                .entries
                .iter()
                .filter(|e| self.matches(e))
                .map(|e| e.id)
                .collect();
            self.dir_selection.clear();
        }
    }

    /// 反选（T-GUI T34「反选」菜单接线：可见条目内选中/未选中互换；
    /// 隐式目录选择一并清空——混合反选语义无自然定义，从实体集）。
    pub fn select_invert(&mut self) {
        let visible: Vec<EntryId> = self
            .entries
            .iter()
            .filter(|e| self.matches(e))
            .map(|e| e.id)
            .collect();
        let next: BTreeSet<EntryId> = visible
            .iter()
            .copied()
            .filter(|id| !self.selection.contains(id))
            .collect();
        self.selection = next;
        self.dir_selection.clear();
    }

    /// 选择集（升序 ID 列表）。
    pub fn selection(&self) -> Vec<EntryId> {
        self.selection.iter().copied().collect()
    }

    /// 选择集**借用**（T-OPT 项2：快照导出同一批里要读三处——每次物化成
    /// `Vec`/`HashSet` 是纯浪费；需要所有权时才用 [`Self::selection`]）。
    pub fn selection_set(&self) -> &BTreeSet<EntryId> {
        &self.selection
    }

    /// 是否已选中。
    pub fn is_selected(&self, id: EntryId) -> bool {
        self.selection.contains(&id)
    }

    /// 选择集大小（实体 + 隐式目录两通道行数）。
    pub fn selection_count(&self) -> usize {
        self.selection.len() + self.dir_selection.len()
    }

    /// 选中条目名（解压 `SelectionArg::Names` 数据面——T-SEL：隐式目录
    /// 选择经子树展开并入，选目录 = 选其全部内容）。
    pub fn selection_names(&self) -> Vec<String> {
        let ids: std::collections::BTreeSet<EntryId> =
            self.effective_selection_ids().into_iter().collect();
        ids.iter()
            .filter_map(|id| self.entries.get(*id))
            .map(|e| e.name.clone())
            .collect()
    }

    /// 懒构建目录树（首次请求算一次；过滤生效于叶——命中文件的祖先目录
    /// 保留可见，core §3.2 语义）。返回树视图引用（T42 转 gpui_kit 树行）。
    pub fn tree(&mut self) -> &Option<TreeNode> {
        if self.tree.is_none()
            && let Some(b) = &self.browser
        {
            // 组合档的「内层容器」不再由本层**合成**：core 的 `browse` 对其**延迟
            // 展开**（用户口径 2026-09-24——打开 `.cpio.gz` 不该立刻把 cpio 展开），
            // 列表/树里那一行 `*.cpio` 是 core 给的**真实（合成）条目**，双击才加载
            // （`Browser::enter_deferred` ⟵ shell 的 `open_container` 分支）。
            self.tree = Some(b.tree());
        }
        &self.tree
    }

    /// 本会话是否停在**延迟组合层**（`init.cpio.gz` 的 `init.cpio`；用户口径
    /// 2026-09-24）：是则名字对上的那一行由 [`Self::enter_deferred_layer`] 展开。
    pub fn deferred_layer(&self) -> Option<&str> {
        self.browser.as_ref().and_then(|b| b.deferred_layer())
    }

    /// **摘下**会话浏览器（重活在锁外跑的唯一手法）。
    ///
    /// 展开延迟层这类操作耗时按秒~分钟计（装配解码链 + 内层容器落 spool + 索引），
    /// 而 `Browser` 只被本会话独占——把它**移出** AppData 就能在**不持 AppData 锁**
    /// 的后台线程上跑（持锁跑 = UI 每帧读快照时堵在锁上 = 界面冻住，与用户口径
    /// 2026-09-24「后台线程 + 进度窗」正相反）。
    ///
    /// 摘下期间会话呈**无浏览态**：列表/树渲染走 `entries` 缓存（不受影响），
    /// 而 `deferred_layer()` 等读到 `None`——同一行的再次双击由
    /// `AppData::session_switch` 闸挡掉，不会误走嵌套容器路径。
    ///
    /// **放回**见 [`Self::restore_browser`]（失败路径必须放回：core 在失败时把延迟态
    /// 放回了 `Browser` 自己，还回去即可原样重试）。
    pub fn take_browser(&mut self) -> Option<Browser> {
        self.browser.take()
    }

    /// 放回浏览器（[`Self::take_browser`] 的逆操作；含失败路径）。
    pub fn restore_browser(&mut self, browser: Browser) {
        self.browser = Some(browser);
    }

    /// 树节点引用（无浏览/未构建为 None）。
    pub fn tree_ref(&self) -> Option<&TreeNode> {
        self.tree.as_ref()
    }

    /// 目录是否展开（组合导航树跨层铺平用：每层各有自己的展开集）。
    pub fn is_expanded(&self, path: &str) -> bool {
        self.expanded.contains(path)
    }

    /// 目录名是否命中本会话的名称过滤（组合树各层沿用自己会话的过滤条件）。
    pub fn dir_name_hits_filter(&self, name: &str) -> bool {
        match &self.filter_substr_unicode {
            Some(sub) if !sub.is_empty() => name.to_lowercase().contains(sub),
            _ => true,
        }
    }

    /// 条目事实（组合树建行用）：`(是否目录, 是否加密/头加密)`。
    pub fn entry_facts(&self, id: EntryId) -> Option<(bool, bool)> {
        self.entries
            .get(id)
            .map(|e| (e.is_dir, e.encrypted || e.header_encrypted))
    }

    /// 文件型容器扩展名白名单（core `Browser::enter` 可进；与 core
    /// `container_handler_id` 同表——`hzip_format::exts` 表驱动，02 §9.10）。
    pub fn is_container_name(name: &str) -> bool {
        hzip_format::exts::container_handler(name).is_some()
    }

    /// 可见树行（当前浏览层；依 `expanded` 铺平——含子目录展开下钻）。
    /// 过滤（`matches`）生效于叶——命中文件祖先目录保留。
    pub fn tree_rows(&self) -> Vec<crate::viewmodel::rows::TreeRow> {
        let mut out = Vec::new();
        // 顶层 = 档案内直接条目（entries 快照，非目录树——树从 entries 根建）
        let Some(root) = self.tree_ref() else {
            return out;
        };
        self.emit_node(root, "", 0, &mut out);
        out
    }

    /// 递归铺平节点（目录展开才下钻）。
    fn emit_node(
        &self,
        node: &TreeNode,
        prefix: &str,
        depth: usize,
        out: &mut Vec<crate::viewmodel::rows::TreeRow>,
    ) {
        let name = node.name.as_str();
        // 根节点名空（name=""）→ 不输出自身，直接遍历子
        if name.is_empty() {
            for child in node.children.values() {
                self.emit_node(child, "", 0, out);
            }
            return;
        }
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        // 实体条目元数据（隐式目录 None → 默认）
        let er = node.entry.and_then(|id| self.entries.get(id));
        let entry_id = node.entry;
        let is_dir = er.is_none_or(|e| e.is_dir);
        let expanded = self.expanded.contains(&path);
        let has_children = !node.children.is_empty();
        out.push(crate::viewmodel::rows::TreeRow {
            path: path.clone(),
            name: name.to_string(),
            depth,
            is_dir,
            entry_id,
            expanded,
            has_children,
            // 实体条目上浮列（T-OPT 项11：与 dir_children/visible_rows 同一构造器）
            ..crate::viewmodel::rows::TreeRow::entry_columns(er, name)
        });
        // 目录展开且(过滤可见或目录自身命中过滤链)——此处简化：展开即下钻全子
        if is_dir && expanded {
            for child in node.children.values() {
                self.emit_node(child, &path, depth + 1, out);
            }
        }
    }

    /// 展开/收起目录（路径定位；不存在静默）。
    pub fn toggle_expand(&mut self, path: &str) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_string());
        }
    }

    /// 顶层路径展开（打开档案后调用：默认展开第一层目录；先懒建树）。
    pub fn expand_top_level(&mut self) {
        self.tree(); // 懒建（tree_rows 依赖 tree_ref）
        let rows = self.tree_rows();
        for r in rows {
            if r.is_dir && r.depth == 0 {
                self.expanded.insert(r.path);
            }
        }
    }

    // —— 双栏浏览（左导航树 = 仅目录；右内容列表 = 当前目录直接子项）——

    /// 当前浏览目录（""=档案根；右栏标题/面包屑数据面）。
    pub fn current_dir(&self) -> &str {
        &self.current_dir
    }

    /// 进入目录（右栏双击目录/左栏单击目录共用；仅记录路径，行由调用方刷）。
    pub fn enter_dir(&mut self, path: &str) {
        self.current_dir = path.to_string();
    }

    /// 返回上级目录（根时不变；返回是否发生移动）。
    pub fn go_up_dir(&mut self) -> bool {
        if self.current_dir.is_empty() {
            return false;
        }
        match self.current_dir.rfind('/') {
            Some(i) => self.current_dir.truncate(i),
            None => self.current_dir.clear(),
        }
        true
    }

    /// 回档案根目录。
    pub fn go_root_dir(&mut self) {
        self.current_dir.clear();
    }

    /// 进入目录并保证其父链全部展开（左栏可见）；父链未展开时补展开。
    pub fn expand_to(&mut self, path: &str) {
        // 逐段补展开（a/b/c → 展开 a、a/b）
        let mut acc = String::new();
        for seg in path.split('/') {
            if seg.is_empty() {
                continue;
            }
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            if acc != path {
                self.expanded.insert(acc.clone());
            }
        }
        // 自身展开（左栏后续导航可见其子）
        self.expanded.insert(path.to_string());
    }

    /// 当前目录直接子项行（右栏内容列表；目录在前、文件按名排序——
    /// BTreeMap 遍历序即名序，目录优先经双 pass）。
    pub fn dir_children(&self) -> Vec<crate::viewmodel::rows::TreeRow> {
        let mut out = Vec::new();
        let Some(root) = self.tree_ref() else {
            return out;
        };
        // `..` 占位行归 UI 委托合成（browse.rs DataRowRef::Parent）——快照保持
        // 纯实体行。曾在此混入字面 ".." 行：表格出现第二个可双击的 `..` 行，
        // 双击后 enter_dir("..") 进字面目录导致无法跳上级（R-GUI 走查 Fix3）。
        // 沿 current_dir 段下行取节点
        let mut node = root;
        if !self.current_dir.is_empty() {
            for seg in self.current_dir.split('/') {
                match node.children.get(seg) {
                    Some(n) => node = n,
                    None => return out, // 路径失效（刷新后）→ 空列表
                }
            }
        }
        let prefix = if self.current_dir.is_empty() {
            String::new()
        } else {
            format!("{}/", self.current_dir)
        };
        // 目录先行（各自名序），再文件
        for pass in 0..2 {
            for child in node.children.values() {
                let child_is_dir = match child.entry {
                    Some(id) => self.entries.get(id).is_some_and(|e| e.is_dir),
                    None => true,
                };
                if (pass == 0) != child_is_dir {
                    continue;
                }
                // 过滤（名称子串/glob 段匹配；02 §11.4 视图过滤）——
                // 派生形式在 `set_filter` 期算好（T-OPT 项12：旧实现每个子项
                // 都做一次 `to_lowercase` + `glob::Pattern::new`）
                let hit =
                    self.filter_substr_unicode.as_ref().is_none_or(|sub| {
                        sub.is_empty() || child.name.to_lowercase().contains(sub)
                    }) && self
                        .filter_glob
                        .as_ref()
                        .is_none_or(|p| p.matches(&child.name));
                if !hit {
                    continue;
                }
                let path = format!("{}{}", prefix, child.name);
                let er = child.entry.and_then(|id| self.entries.get(id));
                let entry_id = child.entry;
                out.push(crate::viewmodel::rows::TreeRow {
                    path,
                    name: child.name.to_string(),
                    depth: 0,
                    is_dir: child_is_dir,
                    entry_id,
                    expanded: false,
                    has_children: !child.children.is_empty(),
                    // 实体条目上浮列（T-OPT 项11：与 emit_node/visible_rows 同一构造器）
                    ..crate::viewmodel::rows::TreeRow::entry_columns(er, &child.name)
                });
            }
        }
        out
    }

    /// 搜索结果行（过滤激活时右栏**全档扁平**匹配；无过滤 → 与
    /// [`Self::dir_children`] 等价的当前目录视图）。
    ///
    /// 与 `dir_children` 的段名过滤不同：这里用 [`Self::matches`] 对
    /// **完整条目路径**判定（`docs/src/a.txt` 也能被 "a.txt" 命中），
    /// 结果行 `path` 带完整档案内路径——单击行即可定位到文件所在目录
    /// （面包屑 + 左树展开跟随，2026-09-15 用户报障）。
    pub fn visible_rows(&self) -> Vec<crate::viewmodel::rows::TreeRow> {
        let has_search = self
            .filter
            .name_substr
            .as_ref()
            .is_some_and(|s| !s.is_empty());
        if !has_search {
            return self.dir_children();
        }
        let mut out: Vec<crate::viewmodel::rows::TreeRow> = self
            .entries
            .iter()
            .filter(|e| self.matches(e))
            .map(|e| crate::viewmodel::rows::TreeRow {
                path: e.name.clone(),
                name: e.name.rsplit('/').next().unwrap_or(&e.name).to_string(),
                depth: 0,
                is_dir: e.is_dir,
                entry_id: Some(e.id),
                expanded: false,
                has_children: false,
                // 实体条目上浮列（T-OPT 项11：与 emit_node/dir_children 同一构造器；
                // 本支的名字串按原样用**完整档案内路径**——容器扩展名判定同其在
                // 单段名上的结果）
                ..crate::viewmodel::rows::TreeRow::entry_columns(Some(e), &e.name)
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// 左栏导航树可见行（仅目录；依 `expanded` 铺平）。
    ///
    /// 首行 = 档案根（压缩包名，depth 0）：无子目录时树不至空白（R-GUI3 #1）。
    /// 子目录行 depth +1；`..`/文件不进树（R-GUI2 树仅目录语义）。
    pub fn nav_rows(&self) -> Vec<crate::viewmodel::rows::TreeRow> {
        let mut out = Vec::new();
        let Some(root) = self.tree_ref() else {
            return out;
        };
        // 档案根行（压缩包名含扩展名——R-GUI4 #1：用户要求根显示完整档案
        // 文件名；虚拟节点——树里可选中/可展开，无对应 entry）
        let root_name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| crate::tr("vm.root_default_name"));
        out.push(crate::viewmodel::rows::TreeRow {
            path: String::new(),
            name: root_name,
            depth: 0,
            is_dir: true,
            entry_id: None,
            expanded: self.expanded.contains(""),
            has_children: !root.children.is_empty(),
            // 档案根行：无实体条目 → 上浮列全空（Default 基线）
            ..Default::default()
        });
        for child in root.children.values() {
            self.emit_dir_nav(child, "", 1, &mut out);
        }
        out
    }

    /// 导航树递归铺平（只下钻目录；过滤生效于目录名）。
    fn emit_dir_nav(
        &self,
        node: &TreeNode,
        prefix: &str,
        depth: usize,
        out: &mut Vec<crate::viewmodel::rows::TreeRow>,
    ) {
        // 文件叶不进导航树（左栏仅目录）
        let is_dir = match node.entry {
            Some(id) => self.entries.get(id).is_some_and(|e| e.is_dir),
            None => true,
        };
        if !is_dir {
            return;
        }
        let path = if prefix.is_empty() {
            node.name.to_string()
        } else {
            format!("{prefix}/{}", node.name)
        };
        // 目录名过滤（子串不命中则剪枝——目录树只做名称过滤）
        if let Some(sub) = &self.filter_substr_unicode {
            let lower = node.name.to_lowercase();
            if !sub.is_empty() && !lower.contains(sub) {
                return;
            }
        }
        let entry_id = node.entry;
        let encrypted = entry_id
            .and_then(|id| self.entries.get(id))
            .map(|e| e.encrypted || e.header_encrypted)
            .unwrap_or(false);
        let expanded = self.expanded.contains(&path);
        out.push(crate::viewmodel::rows::TreeRow {
            path: path.clone(),
            name: node.name.to_string(),
            depth,
            is_dir: true,
            entry_id,
            expanded,
            has_children: !node.children.is_empty(),
            encrypted,
            // 导航树仅目录行：其余上浮列（容器/格式名/新五列）全空（Default 基线）
            ..Default::default()
        });
        if expanded {
            for child in node.children.values() {
                self.emit_dir_nav(child, &path, depth + 1, out);
            }
        }
    }

    /// 可疑编码（Suspect/Unknown）计数 → 编码修复预览入口徽标。
    pub fn suspect_count(&self) -> usize {
        self.suspect_count
    }

    /// 可疑编码条目（编码修复预览对话框数据面；修复名经 dialog_vm 计算）。
    pub fn suspect_entries(&self) -> Vec<EntryRow> {
        self.entries
            .iter()
            .filter(|e| !matches!(e.name_encoding, NameEncoding::Utf8))
            .cloned()
            .collect()
    }

    /// 预览请求参数（02 §9.10：`(entry_id, size_raw)`——调用方据条目类型选
    /// PreviewMode 后经 `Browser::preview` 读取；本 VM 只提供数据面）。
    pub fn preview_request(&self, id: EntryId) -> Option<(EntryId, u64)> {
        self.entry(id).map(|e| (e.id, e.size_raw))
    }

    /// 读条目预览数据（转发 `Browser::preview`；文本默认上限由 core 常量）。
    pub fn preview_data(
        &self,
        id: EntryId,
        mode: hzip_core::services::preview::PreviewMode,
        max_bytes: Option<u64>,
        pw: Option<&zeroize::Zeroizing<String>>,
    ) -> Result<hzip_core::services::preview::PreviewData, HzipError> {
        let b = self
            .browser
            .as_ref()
            .ok_or_else(|| HzipError::Unsupported {
                what: crate::tr("msg.preview_no_archive").into(),
            })?;
        Self::preview_in(b, id, mode, max_bytes, pw)
    }

    /// 读预览数据（**静态形态**：`&Browser` 只读，不含会话状态）。
    ///
    /// 存在的理由：预览要**解压读条目**（大条目秒级），而 `MainViewModel` 在
    /// `AppData` 里、读它必须持锁——持锁读会把每帧渲染（要锁）堵住 ⇒ 主界面
    /// 冻住。调用方按 `take_browser` → 锁外 `preview_in` → `restore_browser`
    /// 三段式使用（2026-09-24 用户实测「主界面卡住」的根因同类）。
    ///
    /// `pw`：会话密码（加密条目用；`None` = 未加密/未缓存 → 3101）。
    pub fn preview_in(
        b: &Browser,
        id: EntryId,
        mode: hzip_core::services::preview::PreviewMode,
        max_bytes: Option<u64>,
        pw: Option<&zeroize::Zeroizing<String>>,
    ) -> Result<hzip_core::services::preview::PreviewData, HzipError> {
        b.preview(id, mode, max_bytes, password_ref(pw))
    }

    /// 复制物化（T-MENU8）：选中条目 → 进程级复制区稳定路径清单。
    ///
    /// 路径布局 `{area}/{sha256(档案)[..8]}/{安全名}`（与 core 编辑服务物化
    /// 同源；TC-SEC-015 净化）；目录条目经 `_dir` 后缀防与同名文件碰撞。
    /// 返回 `(物化路径清单, 失败条目名)`——单条失败不放弃整批（best-effort：
    /// 部分成功仍可粘贴，失败条目名归调用方提示）。
    pub fn materialize_entries(
        &self,
        ids: &[EntryId],
        archive: &std::path::Path,
        pw: Option<&zeroize::Zeroizing<String>>,
    ) -> Result<(Vec<std::path::PathBuf>, Vec<String>), HzipError> {
        let b = self
            .browser
            .as_ref()
            .ok_or_else(|| HzipError::Unsupported {
                what: crate::tr("msg.copy_no_archive").into(),
            })?;
        Self::materialize_in(b, ids, archive, pw)
    }

    /// 物化条目到进程复制区（**静态形态**：[`Self::materialize_entries`] 的内核）。
    ///
    /// 与 [`Self::preview_in`] 同理：物化要**把整条内容解出来**（大条目可到分钟
    /// 级），持 `AppData` 锁跑会让每帧渲染干等 ⇒ 主界面无响应。调用方三段式：
    /// 锁内 `take_browser` → 锁外本函数 → 锁内 `restore_browser`。
    ///
    /// `pw`：会话密码（加密条目用）。**密码错一律上抛**（`HzipError::Password`）
    /// ——它是档案级失败，不是「这条坏了」；上抛让调用方补密码重试
    /// （用户口径 2026-09-28 密码门自愈）。其余逐条错误仍 best-effort 累积。
    ///
    /// # 目录递归展开（T-MENU8 语义升级）
    ///
    /// 入参 `ids` 来自 [`Self::effective_selection_ids`]——隐式目录选择已按
    /// 「档案内路径前缀」展开为**全部后代条目 id**（选目录 = 选中其全部内容，
    /// 资源管理器语义），但**哪些 id 是目录根**的信息在展开时丢失。本函数从
    /// Browser 的全量条目缓存按相对关系反推：
    ///
    /// - **目录根** = 显式 `is_dir` 条目，或（隐式目录，tar 无目录条目档案）
    ///   其在档案内的**全部**后代条目都在选中集合——半选（只选了目录里部分
    ///   文件）不构成目录根，否则会把单个选中文件误判成壳；
    /// - **纯目录展开判别**（选中集指纹）：仅当全部选中 id 都落在某个根子树
    ///   内才进入展开模式（目录行不是实体，`effective_selection_ids` 的展开
    ///   产物里没有「根外散件」）；手工多选文件即使恰好盖满某目录（根外另有
    ///   条目）→ 整体按旧语义平铺，行为与升级前一致；
    /// - 选中文件 → `rel` = 自身末段名；目录根 D → 目录壳 `rel` = D 末段，
    ///   D 下条目 `rel` = `{D末段}/{去掉 D 前缀的档案内路径}`（`/` 连接，与
    ///   档案内 [`hzip_error::EntryName`] 分隔符口径一致——hzip-error §EntryName
    ///   契约「内部统一 '/' 分隔」）；
    /// - 后代枚举只用 Browser 的 `entries` 缓存（browse 一次快照即全量扁平
    ///   清单，零 IO）——**不走 Job/Engine 链**；
    /// - 逐条委托 `hzip_platform::clipboard::materialize_entry`（契约 B）：
    ///   多段 rel 逐段净化、中间目录自动创建；单条失败进 `failed` 不放弃整批。
    pub fn materialize_in(
        b: &Browser,
        ids: &[EntryId],
        archive: &std::path::Path,
        pw: Option<&zeroize::Zeroizing<String>>,
    ) -> Result<(Vec<std::path::PathBuf>, Vec<String>), HzipError> {
        let mut out = Vec::new();
        let mut failed = Vec::new();
        // 全量条目快照（档案内路径 → (id, is_dir)）：隐式目录根判定与后代
        // 枚举共用；BTreeMap 键序稳定（去重 + 确定性物化顺序）。
        let mut all: std::collections::BTreeMap<String, (EntryId, bool)> =
            std::collections::BTreeMap::new();
        for i in 0..b.entry_count() {
            if let Some(info) = b.entry(i) {
                all.insert(info.name.as_str().to_string(), (i, info.is_dir));
            }
        }
        let selected: std::collections::BTreeSet<EntryId> = ids.iter().copied().collect();
        // 目录根集（档案内路径）：
        // 1) 显式 is_dir 选中条目——自身即目录壳；
        // 2) 隐式目录——tar 无目录条目，目录只以「条目名的路径前缀」存在
        //    （`root/a.txt` 蕴含 `root`）。候选 = 选中条目名的全部祖先段；
        //    判定 = 档案内以 `{候选}/` 打头的**全部**条目都在选中集合（与
        //    effective_selection_ids 的子树展开特征互逆：只有「整目录选择」
        //    才产生这个形态；半选保持平铺文件语义，单文件不会被误判成壳）。
        let mut dir_roots: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        // 显式目录条目：直接进根集（自身壳 + 后代展开）。
        for (name, (id, is_dir)) in &all {
            if *is_dir && selected.contains(id) {
                dir_roots.insert(name.clone());
            }
        }
        // 隐式候选：选中条目名的祖先段（`root/sub/b.txt` → root, root/sub）。
        let mut candidates: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for id in &selected {
            if let Some(info) = b.entry(*id) {
                if info.is_dir {
                    continue; // 显式目录已处理；隐式只看文件条目名前缀
                }
                let name = info.name.as_str();
                let mut acc = String::new();
                for seg in name.split('/') {
                    if !acc.is_empty() {
                        acc.push('/');
                    }
                    acc.push_str(seg);
                    // 最后一段 = 条目自身，不是祖先目录
                    if acc.len() < name.len() {
                        candidates.insert(acc.clone());
                    }
                }
            }
        }
        for cand in candidates {
            let prefix = format!("{cand}/");
            let fully_selected = all
                .iter()
                .filter(|(n, _)| n.starts_with(&prefix))
                .all(|(_, (oid, _))| selected.contains(oid));
            // 至少有一个后代才谈得上目录（无后代前缀 = 空目录名，无实体）。
            let has_descendant = all.keys().any(|n| n.starts_with(&prefix));
            if has_descendant && fully_selected {
                dir_roots.insert(cand);
            }
        }
        // 纯目录展开判别（选中集指纹）：effective_selection_ids 的目录展开
        // 产物 = 全部 id 都落在某个根子树内（目录行不是实体，选择集里没有
        // 「根外散件」）。手工多选文件即使恰好盖满某目录，也会有根外条目
        // （如全选 top.txt + docs/a.txt）——此时按旧语义平铺，不展开成壳。
        let name_of =
            |sid: EntryId| -> &str { b.entry(sid).map(|i| i.name.as_str()).unwrap_or("") };
        let roots_cover_all = !dir_roots.is_empty()
            && selected.iter().all(|sid| {
                dir_roots
                    .iter()
                    .any(|r| name_of(*sid).starts_with(format!("{r}/").as_str()))
            });
        if !roots_cover_all {
            dir_roots.clear();
        }
        // 物化顺序：先目录根（壳 + 整棵子树），后剩余平铺选中项。隐式根
        // 不是条目（tar 无目录条目），**不能**靠逐 id 循环命中——逐根处理。
        // 嵌套根去重：内层根（root/sub）是外层根（root）前缀子树，跳过。
        let mut covered: std::collections::BTreeSet<EntryId> = std::collections::BTreeSet::new();
        let top_roots: Vec<&String> = dir_roots
            .iter()
            .filter(|r| {
                !dir_roots.iter().any(|other| {
                    other.as_str() != r.as_str() && r.starts_with(format!("{other}/").as_str())
                })
            })
            .collect();
        for root in top_roots {
            let name = root.as_str();
            // 目录根：壳 rel = 末段；后代 rel = `{壳落地名}/{去前缀档案内路径}`。
            // 壳经契约 B 落地时叶子段可能带净化后缀（目录 `_dir` 防碰撞）——
            // 后代前缀取**壳返回路径的末段**（后缀已应用），保证后代嵌进壳
            // 内同一棵树，而非另起一份净化前的同名目录。
            let base = last_segment(name);
            let Some(shell) =
                hzip_platform::clipboard::materialize_entry(archive, base, true, std::io::empty())
            else {
                failed.push(name.to_string());
                continue;
            };
            out.push(shell.clone());
            let shell_leaf = shell
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| base.to_string());
            let prefix = format!("{name}/");
            for (desc, (did, is_dir_leaf)) in &all {
                if !desc.starts_with(&prefix) {
                    continue;
                }
                covered.insert(*did);
                let rel = format!("{shell_leaf}/{}", &desc[prefix.len()..]);
                // 密码错一律上抛（档案级失败，与平铺分支同口径）。
                match b.read_entry(*did, password_ref(pw)).and_then(|s| {
                    hzip_platform::clipboard::materialize_entry(archive, &rel, *is_dir_leaf, s)
                        .ok_or_else(|| HzipError::Unsupported {
                            what: "clipboard-materialize-entry".into(),
                        })
                }) {
                    Ok(p) => out.push(p),
                    Err(e @ HzipError::Password { .. }) => return Err(e),
                    Err(_) => failed.push(desc.clone()),
                }
            }
        }
        // 剩余平铺选中项：未被任何目录根子树覆盖的 id（半选文件 / 单文件）。
        for id in ids {
            if covered.contains(id) {
                continue; // 已随目录根展开
            }
            let Some(info) = b.entry(*id) else {
                failed.push(format!("#{id}"));
                continue;
            };
            let name = info.name.as_str();
            if info.is_dir {
                // 显式目录条目（半选残留 / 空目录）：按旧语义物化目录壳。
                match hzip_platform::clipboard::materialize_entry(
                    archive,
                    last_segment(name),
                    true,
                    std::io::empty(),
                ) {
                    Some(p) => out.push(p),
                    None => failed.push(name.to_string()),
                }
            } else {
                // 选中文件：rel = 自身末段名（与旧 `materialize_for_copy` 同布局）。
                // 密码错一律上抛（档案级失败，见函数文档 `pw` 口径）。
                let rel = last_segment(name);
                let stream = match b.read_entry(*id, password_ref(pw)) {
                    Ok(s) => s,
                    Err(e @ HzipError::Password { .. }) => return Err(e),
                    Err(_) => {
                        failed.push(name.to_string());
                        continue;
                    }
                };
                match hzip_platform::clipboard::materialize_entry(archive, rel, false, stream)
                    .ok_or_else(|| HzipError::Unsupported {
                        what: "clipboard-materialize-entry".into(),
                    }) {
                    Ok(p) => out.push(p),
                    Err(_) => failed.push(name.to_string()),
                }
            }
        }
        Ok((out, failed))
    }
}

/// `Option<&Zeroizing<String>>` → `PasswordRef`（会话密码传递的统一收口）。
fn password_ref(pw: Option<&zeroize::Zeroizing<String>>) -> hzip_format::PasswordRef<'_> {
    match pw {
        Some(p) => hzip_format::PasswordRef::new(p),
        None => hzip_format::PasswordRef::none(),
    }
}

/// 条目名末段（`/` 分隔；EntryName 契约：档案内名统一 `/` 分隔、无 `\`）。
fn last_segment(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// `EntryInfo` → 纯数据行（mtime → RFC3339 UTC 占位；展示层本地时区格式化
/// 归 T42）。不引入 `time` 依赖——UTC 字段手工格式化。
fn entry_row(e: &hzip_format::EntryInfo) -> EntryRow {
    EntryRow {
        id: e.id,
        name: e.name.as_str().to_string(),
        is_dir: e.is_dir,
        size_raw: e.size_raw,
        size_packed: e.size_packed,
        mtime: e.mtime.map(|t| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                t.year(),
                t.month() as u8,
                t.day(),
                t.hour(),
                t.minute(),
                t.second()
            )
        }),
        encrypted: e.encrypted,
        header_encrypted: e.header_encrypted,
        name_encoding: e.name_encoding,
        is_symlink: e.is_symlink,
        symlink_target: e
            .symlink_target
            .as_ref()
            .map(|t| t.to_string_lossy().into_owned()),
        encryption: e.encryption,
        compression_level: e.compression_level,
        crc32: if e.is_dir { None } else { e.crc32 },
        attrs: e.attributes,
    }
}

#[cfg(test)]
#[path = "tests/main_vm.rs"]
mod tests;
