//! 纯数据行类型（02 §11.1 红线载体；T41）。
//!
//! # 红线（02 §11.1 原文照抄进 rustdoc）
//!
//! 本模块 **禁止 import gpui/gpui_kit 类型**——纯数据 + `Clone`，供 T-G20/T-G30 桥接转换成 gpui_kit
//! 行；`cargo test -p hzip-gui --lib` 无图形环境可跑（gpui 宏展开会引入需要
//! 原生后端初始化的类型与线程亲和约束）。
//!
//! # 与 core 状态机映射（02 §9.3）
//!
//! [`JobRowState`] 与 core jobs 状态机（Queued/Running/WaitingInput/…）一一对应，
//! **枚举闭包在本 crate**（不 import core 内部状态枚举，避免跨 crate 私有类型
//! 泄漏）；映射函数 `from_event` 在 [`super::jobs_vm`] 维护。
//!
//! # 展示层职责剥离
//!
//! `EntryRow.mtime` 存 RFC3339 UTC 字符串占位（02 §1.2：内部 `OffsetDateTime` UTC，
//! 展示格式化——本地时区/i18n——刻意留给 T42）；`FailureRow.message` 为 core
//! i18n key + 参数由 T42 查表（VM 不渲染文案，测试不依赖 locale）。

use std::time::Duration;

use hzip_format::{EntryId, NameEncoding};

/// 条目行（浏览列表视图；纯数据）。
#[derive(Clone, Debug)]
pub struct EntryRow {
    /// 条目 ID（entries() 下标，0 起连续）
    pub id: EntryId,
    /// 条目名（`/` 分隔，已规范化）
    pub name: String,
    /// 是否目录
    pub is_dir: bool,
    /// 解压后原始字节数
    pub size_raw: u64,
    /// 打包字节数（档案内占用）
    pub size_packed: u64,
    /// 修改时间（RFC3339 UTC 展示占位；本地时区格式化归 T42）
    pub mtime: Option<String>,
    /// 是否加密
    pub encrypted: bool,
    /// 头部加密（无法列出条目）
    pub header_encrypted: bool,
    /// 条目名编码状态（`Suspect(_)`/`Unknown` → UI 打「可疑编码」标记）
    pub name_encoding: NameEncoding,
    /// 软链接标记（R-GUI1-02 §2.4；format 层 `EntryInfo.is_symlink` 上浮）
    pub is_symlink: bool,
    /// 软链接目标（is_symlink 时非空；tooltip 展示）
    pub symlink_target: Option<String>,
    /// 加密算法名（浏览「加密算法」列；None = 未加密/格式未声明）
    pub encryption: Option<&'static str>,
    /// 压缩级别数值（浏览「压缩级别」列；None = 格式不存储级别）
    pub compression_level: Option<u8>,
    /// CRC32 校验值（浏览「CRC 校验」列；None = 格式未声明）
    pub crc32: Option<u32>,
    /// 平台属性位（浏览「文件属性」列数据源）
    pub attrs: hzip_format::EntryAttrs,
}

/// 任务行状态（02 §9.3 图 + GUI 任务中心视图；枚举闭包在本 crate）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobRowState {
    /// 已入队
    Queued,
    /// 执行中
    Running,
    /// 等待用户决策（密码/冲突弹窗）
    WaitingInput,
    /// 取消中（瞬时中间态）
    Cancelling,
    /// 全部成功
    Done,
    /// 部分失败（best_effort 聚合）
    PartialDone,
    /// 失败
    Failed,
    /// 已取消
    Cancelled,
}

/// 任务终局摘要（任务中心行尾态展示；[`JobRow.finished`]）。
#[derive(Clone, Debug)]
pub enum JobRowOutcome {
    /// 全部成功（`Outcome::Done(stats)`）
    Done {
        /// 处理字节数
        bytes: u64,
    },
    /// 部分失败（`Outcome::Partial`）
    Partial {
        /// 成功条目数
        entries_done: u64,
        /// 失败明细（下钻数据面）
        failures: Vec<FailureRow>,
    },
    /// 失败（`Outcome::Failed(e)`）；错误码 + i18n key + 原文承载（02 §3.3）
    Failed {
        /// HZ-xxxx 数字部分
        code: u32,
        /// i18n key（T42 查表渲染；本层不渲染文案）
        i18n_key: String,
        /// 错误**原文**（`HzipError::Display`：含具体路径/os 文案，例如
        /// 「IO 错误: 拒绝访问。 (os error 5) (E:\Downloads\x.zip)」）。
        ///
        /// 为什么必须有（用户口径 2026-09-29）：「任务中心看不出来具体失败原因」
        /// ——只有 i18n key 时，单元格只能显示「IO 错误」这类类别文案，定位不了
        /// 到底哪个文件出了什么事。密码类错误的 Display 不含明文（只有 target）。
        detail: String,
    },
    /// 已取消（`Outcome::Cancelled` / `RolledBack`）
    Cancelled,
}

/// 单条目失败明细（02 §11.4 任务中心失败条目下钻数据面）。
#[derive(Clone, Debug)]
pub struct FailureRow {
    /// 失败条目名
    pub entry: String,
    /// HZ-xxxx 错误码数字部分
    pub code: u32,
    /// i18n key + 参数（T42 查表；测试不依赖 locale）
    pub message: String,
}

/// 已处理条目摘要（T-PW 进度窗口文件列表数据面；纯数据）。
#[derive(Clone, Debug)]
pub struct RecentEntry {
    /// 条目名（`/` 分隔完整路径）
    pub name: String,
    /// 条目字节数（EntryDone 实际字节数）
    pub bytes: u64,
    /// CRC 是否通过（false = 失败行，渲染侧标红）
    pub ok: bool,
    /// 失败原因 i18n key（ok=true 为空串；core 逐条目失败由 pump 在
    /// `JobFinished` 时从 Outcome 并入——渲染侧查表翻译，测试不依赖 locale）
    pub reason: String,
    /// 错误码数字部分（ok=true 为 0；tail 渲染 HZ-xxxx 用）
    pub code: u32,
}

/// 任务行（任务中心队列视图；纯数据）。
#[derive(Clone, Debug)]
pub struct JobRow {
    /// 任务 ID
    pub job_id: u64,
    /// 任务种类（extract/compress/test/…；展示文案归 T42）
    pub kind: &'static str,
    /// 行状态
    pub state: JobRowState,
    /// 进度 0..1（无 total 时为 None）
    pub progress: Option<f64>,
    /// EWMA 瞬时速度 B/s（预热期内 None）
    pub speed_bps: f64,
    /// 剩余时间（无法估计为 None）
    pub eta: Option<Duration>,
    /// 已完成字节数
    pub done_bytes: u64,
    /// 总字节数
    pub total_bytes: u64,
    /// 已完成条目数
    pub entries_done: u64,
    /// 总条目数
    pub entries_total: u64,
    /// 逐条目失败明细（运行中累积；终局后由 outcome 落账）
    pub failures: Vec<FailureRow>,
    /// 终局摘要（运行中为 None）
    pub finished: Option<JobRowOutcome>,
    // —— R-GUI4 #4 任务中心列扩展（纯数据面）——
    /// 输入对象显示名（解压 = 档案文件名；压缩 = 源名）
    pub src: String,
    /// 当前正在处理的条目名（EntryStarted 置值 / 终局清空；T-PW）
    pub current_entry: Option<String>,
    /// 终局失败摘要行（Failed/PartialDone 终局后非 None；i18n key +
    /// 错误码——进度窗口文件列表持续输出终局结果，T-PW）
    pub final_line: Option<FailureRow>,
    /// 已处理条目摘要（EntryDone 追加；cap 500 淘汰旧项——虚拟列表不吃量；T-PW）
    pub recent_entries: std::collections::VecDeque<RecentEntry>,
    /// 输出对象显示名（解压 = 目标目录；压缩 = 产物路径）
    pub dest: Option<String>,
    /// 档案格式（解压 = 源扩展名；压缩 = 输出格式；test 空串）
    pub format: String,
    /// 开始时间（Running 起算；未开始为 None）
    pub started_at: Option<std::time::SystemTime>,
    /// 总耗时（终局落账；运行中为 None——展示层现算）
    pub elapsed: Option<Duration>,
}

/// 文件树可见行（T42 GUI 树视图数据面；纯数据gpui/gpui_kit 零耦合）。
///
/// 树以「平铺可见行 + depth 缩进」表达：VM 依展开集（目录路径集）把
/// `TreeNode` 深度优先铺平。`path` 为节点路径（段名 `/` 连接）；目录展开
/// 判定经路径；文件/目录选择经 `entry_id`（`None` = 隐式目录无实体条目）。
///
/// `Default` = **无实体条目行**的基线（隐式目录/导航行/组合层行：不加密、
/// 零字节、无软链接、非容器、level 0）：各处构造点用 `..Default::default()`
/// 收掉「全部字段置空」的长清单（T-OPT 项11）。
///
/// **新增字段时**：`..Default::default()` 不会强制你在这 3 处做决定 ⇒ 请回看
/// [`crate::viewmodel::MainViewModel::nav_rows`]、`emit_dir_nav`、
/// [`crate::app::state::display_nav_rows`] 的 `emit_composite_rows`——新列若在
/// 合成行有非空语义，必须显式赋值（其余 3 处实体行构造走
/// [`TreeRow::entry_columns`]，一处改完全生效）。
#[derive(Clone, Debug, Default)]
pub struct TreeRow {
    /// 节点路径（段名 `/` 连接；顶层文件即其名）
    pub path: String,
    /// 本段名（展示）
    pub name: String,
    /// 缩进层级（0=顶层）
    pub depth: usize,
    /// 是否目录
    pub is_dir: bool,
    /// 实体条目 id（隐式目录为 None）
    pub entry_id: Option<EntryId>,
    /// 目录已展开（显示 ▼）
    pub expanded: bool,
    /// 是否有子节点
    pub has_children: bool,
    /// 是否文件型容器（zip/7z/tar…可进入浏览）
    pub is_container: bool,
    /// **卷行元数据**（磁盘镜像会话的顶层行；其余恒 `None`）。
    ///
    /// 来源 `hzip_format::VolumeRowMeta`（reader 的 `volume_rows()`）：分区标签、
    /// 文件系统、类型描述、序号。界面据此画分区图标/类型列/语言化「分区 N」；
    /// 名字本身（`path`/`name`）保持 ASCII 稳定，不参与本地化。
    pub volume: Option<hzip_format::VolumeRowMeta>,
    /// **嵌套层号**（0 = 当前会话；1 = 上一层容器；…；与 `Crumb.level` 同标尺）。
    ///
    /// 左栏树跨嵌套层展示（外层档案为根、内层压缩包当子目录），同名的
    /// `docs/` 可能同时存在于内外两层 ⇒ 光靠 `path` 分不清是哪一层的节点，
    /// 点击/展开也必须知道该作用于哪个会话。
    pub level: usize,
    /// 加密
    pub encrypted: bool,
    /// 解压后原始字节数（展示 string 由绑定层格式化；目录行可经
    /// [`dir_size_map`] 聚合回填）
    pub size_raw: u64,
    /// 打包字节数（目录行可被 [`dir_size_map`] 聚合回填）
    pub size_packed: u64,
    /// 修改时间 RFC3339（展示格式化归绑定层）
    pub mtime: Option<String>,
    /// 软链接标记（R-GUI1-02 §2.4；由对应 EntryRow 上浮，隐式目录恒 false）
    pub is_symlink: bool,
    /// 加密算法名（浏览「加密算法」列；None = 未加密/格式未声明/隐式目录）
    pub encryption: Option<&'static str>,
    /// 压缩级别数值（浏览「压缩级别」列；None = 格式不存储级别）
    pub compression_level: Option<u8>,
    /// CRC32 校验值（浏览「CRC 校验」列；None = 格式未声明）
    pub crc32: Option<u32>,
    /// 文件属性标记集（浏览「文件属性」列；只读/隐藏/系统/可执行——
    /// `TreeRow` 纯数据面持标记位，展示文本由绑定层拼）
    pub attrs: hzip_format::EntryAttrs,
    /// **合成容器层的格式 id**（`init.cpio` → `cpio`；`data.tar` → `tar`）。
    ///
    /// 只有组合档内层容器层（`TreeNode::with_composite_layer` 插的那种合成节点）
    /// 是 `Some`——它没有实体条目（`entry_id = None`），靠名字判不出「合成」还是
    /// 「隐式目录」，故由 VM 显式标注。类型列据此显示格式名（用户口径 2026-09-24：
    /// 「cpio 目录层级在列表里的类型应该显示为 cpio 而不是 目录」）。
    ///
    /// 导航语义仍是**目录**（`is_dir = true` → 树行单击展开、右栏双击进层），
    /// 与「显示成容器」并不冲突：图标/类型按容器显示，行为按目录走。
    pub container_format: Option<String>,
}

impl TreeRow {
    /// 行操作类别（VM 判定后 UI 路由：目录 toggle/容器 enter/文件 preview）。
    pub fn kind(&self) -> TreeRowKind {
        if self.is_dir {
            TreeRowKind::Dir
        } else if self.is_container {
            TreeRowKind::Container
        } else {
            TreeRowKind::File
        }
    }

    /// 实体条目**上浮列**基线（T-OPT 项11）。
    ///
    /// 「条目元数据 → 行展示列」这组公式原先在 `emit_node` / `dir_children` /
    /// `visible_rows` 三处逐字重复（11 个字段 × 3 份；新增列时曾同改三处），
    /// 收口在此。**位置相关**字段（`path`/`name`/`depth`/`is_dir`/`entry_id`/
    /// `expanded`/`has_children`/`level`）仍由各构造点给出——那才是三处的差异。
    ///
    /// `name`：容器扩展名判定用的名字串。三处调用点原本各写一遍（树/子项用
    /// **本段名**，搜索扁平行用**完整档案内路径**）；判定只看扩展名，两者同值，
    /// 故统一由调用点给出一个字符串（不做归一/拼接）。
    ///
    /// `er = None`（隐式目录/合成层）→ 各列取 [`Default`] 基线。
    pub fn entry_columns(er: Option<&EntryRow>, name: &str) -> Self {
        let is_container =
            er.is_some_and(|e| !e.is_dir && hzip_format::exts::container_handler(name).is_some());
        Self {
            is_container,
            encrypted: er.is_some_and(|e| e.encrypted || e.header_encrypted),
            size_raw: er.map(|e| e.size_raw).unwrap_or(0),
            size_packed: er.map(|e| e.size_packed).unwrap_or(0),
            mtime: er.and_then(|e| e.mtime.clone()),
            is_symlink: er.is_some_and(|e| e.is_symlink),
            encryption: er.and_then(|e| e.encryption),
            compression_level: er.and_then(|e| e.compression_level),
            crc32: er.filter(|e| !e.is_dir).and_then(|e| e.crc32),
            attrs: er.map(|e| e.attrs).unwrap_or_default(),
            // 类型列 = 容器格式名；非容器行恒空
            container_format: is_container
                .then(|| hzip_format::exts::container_handler(name).map(str::to_string))
                .flatten(),
            ..Default::default()
        }
    }
}

/// 树行操作类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeRowKind {
    Dir,
    Container,
    File,
}

/// 目录聚合大小表（#3 查看器 Tab 数据面；R2-04 §3.6 语义修正）。
///
/// 基于全量 entries 计算 `目录路径 → (原始字节和, 打包字节和)`：每个**文件**
/// 条目向其全部祖先目录累加（目录条目自身大小不计——zip 目录条目 size 恒 0
/// 语义）。无文件子孙的目录不在表中（调用方保持原值 → 展示层回 `NO_VALUE`）。
///
/// 祖先枚举按段边界（`a/bc/x` 计入 `a` 与 `a/bc`，不计入 `a/b`；`abc/x`
/// 与 `ab` 无包含关系）。纯 Rust 零 gpui（viewmodel/ 红线合规）。
///
/// **为什么取代旧 `fold_dir_sizes`**（行集内前缀后序求和）：旧算法要求行集
/// 是全量扁平树；右栏改逐层浏览后行集只含当前目录一层，目录行的子孙文件
/// 不在集内 → 聚合恒 0（用户勾选「目录行显示大小」看不到大小的根因）。
/// 本表以全量 entries 计算，与可见层无关，对逐层浏览与搜索扁平两路都成立。
pub fn dir_size_map(entries: &[EntryRow]) -> std::collections::HashMap<String, (u64, u64)> {
    let mut map = std::collections::HashMap::new();
    for e in entries {
        if e.is_dir {
            continue; // 目录条目自身不计（zip 目录条目 size 恒 0 语义）
        }
        // 祖先目录枚举：`a/b/c.txt` → `a/b`、`a`（rfind '/' 逐层收缩；段边界天然成立）
        let mut end = e.name.len();
        while let Some(slash) = e.name[..end].rfind('/') {
            let slot = map.entry(e.name[..slash].to_string()).or_insert((0, 0));
            slot.0 += e.size_raw;
            slot.1 += e.size_packed;
            end = slash;
        }
    }
    map
}

/// 目录**子树**大小表：`dir_subtree_sizes` 的 O(1) 前置数据（T-OPT 项2）。
///
/// 与 [`dir_size_map`] 的唯一差别：**不跳过目录条目**——后者依赖「目录条目
/// size 恒 0」（目录行聚合展示用）；本表服务选中统计口径（`"{dir}/"` 前缀求和），
/// 逐字对应旧实现的 `entries.starts_with("{dir}/")` 全量扫描，故不引入该假设。
/// 展开规则同前：`a/b/c.txt` → 计入 `a/b`、`a`（严格祖先），与 `"{dir}/"` 前缀
/// 命中的集合逐项相同。
pub fn dir_subtree_size_map(entries: &[EntryRow]) -> std::collections::HashMap<String, (u64, u64)> {
    let mut map = std::collections::HashMap::new();
    for e in entries {
        let mut end = e.name.len();
        while let Some(slash) = e.name[..end].rfind('/') {
            let slot = map.entry(e.name[..slash].to_string()).or_insert((0, 0));
            slot.0 += e.size_raw;
            slot.1 += e.size_packed;
            end = slash;
        }
    }
    map
}

#[cfg(test)]
#[path = "tests/rows.rs"]
mod tests;
