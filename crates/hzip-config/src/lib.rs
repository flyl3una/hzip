//! hzip 用户配置（02 §12.1；B.1 全键）。
//!
//! # 加载链（高→低）
//!
//! 命令行参数 > 环境变量（HZIP_*）> 配置文件 > 默认值。
//!
//! 本 crate 提供**配置文件层**：`load()` 读
//! `{config_dir}/config.toml`（路径经 `HZIP_CONFIG` 覆盖）；缺失/损坏 →
//! 默认值 + 不阻塞（02 §12.1）。未知键忽略（serde 不 deny_unknown）；
//! 首次运行把仓库默认模板（`etc/hzip.toml`，编译期内嵌单一来源）落盘到
//! config_dir 供用户编辑。
//!
//! # 结构
//!
//! [`Config`] 全字段带默认（B.1 表）；serde `#[serde(default)]` 使文件缺键/
//! 半截文件自然回落默认。上层（CLI/GUI/hzip-log）只调本 crate API，
//! 不自行解析 toml。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};

/// 默认配置模板（仓库 `etc/hzip.toml`；编译期内嵌——单一来源防漂移）。
pub const DEFAULT_TEMPLATE: &str = include_str!("../../../etc/hzip.toml");

// ===== 配置模型（B.1 全键；serde 反序列化默认回落） =====

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub defaults: Defaults,
    pub security: Security,
    pub extract: Extract,
    pub compress: Compress,
    pub jobs: Jobs,
    pub preview: Preview,
    pub ui: Ui,
    pub update: Update,
    pub logging: Logging,
    /// 快捷键（T-KEYS `[keys]`）；见 [`Keys`] 的三态语义。
    pub keys: Keys,
}

/// 在线更新检查（`[update]`）。
///
/// 地址放配置里而非编进产物：换更新源（自建镜像、别的托管）**不必重新打包**，
/// 与 `ui.auto_update` 同源（该开关的检查逻辑挂账 T43）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Update {
    /// 更新检查地址（发布元数据端点；T43 的检查流程读它——当前「帮助 ▸ 检查更新」
    /// 与设置页按钮仍是占位提示，尚未发起任何网络请求）。
    pub check_url: String,
}

impl Default for Update {
    fn default() -> Self {
        Self {
            check_url: "https://api.github.com/repos/flyl3una/hzip/releases/latest".into(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// 缺省压缩格式（"zip"/"7z"/"tar"/...；白名单在调用方）
    pub format: String,
    /// 缺省压缩级别（"store"/"fastest"/"fast"/"normal"/"maximum"/"ultra"）
    pub level: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            format: "zip".into(),
            level: "normal".into(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Security {
    pub max_file_gib: u64,
    pub max_total_gib: u64,
    pub max_ratio: u32,
    pub max_entries: u64,
    pub min_free_disk_pct: u8,
    /// deny-escape / deny-all / flatten
    pub symlink: String,
    pub follow_symlinks: bool,
}

impl Default for Security {
    fn default() -> Self {
        Self {
            max_file_gib: 32,
            max_total_gib: 128,
            max_ratio: 200,
            max_entries: 1_000_000,
            min_free_disk_pct: 5,
            symlink: "deny-escape".into(),
            follow_symlinks: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Extract {
    pub smart: bool,
    /// ask / overwrite / skip / rename / newer
    pub conflict: String,
    pub best_effort: bool,
    pub delete_after: bool,
    /// 强制代码页（gbk/big5/sjis/euc-kr/cp1252/utf-8；null=自动）
    pub codepage: Option<String>,
}

impl Default for Extract {
    fn default() -> Self {
        Self {
            smart: false,
            conflict: "ask".into(),
            best_effort: false,
            delete_after: false,
            codepage: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Compress {
    pub keep_metadata: bool,
    pub verify_after: bool,
    pub solid_block_mib: u32,
}

impl Default for Compress {
    fn default() -> Self {
        Self {
            keep_metadata: true,
            verify_after: true,
            solid_block_mib: 64,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Jobs {
    pub max_parallel: u8,
    /// 任务完成后发送系统通知（R-GUI4 #3 任务完成流程开关）
    pub notify_on_finish: bool,
    /// 压缩/解压时弹出独立进度窗口（T-PW；GUI 提交点判定）
    pub progress_window: bool,
    /// 全部任务完成后退出 hzip（设置页「完成后退出」与进度窗底部复选框同一配置）
    pub exit_on_finish: bool,
    /// 全部任务完成后、退出前的等待毫秒数（进度到 100% 后停留展示；
    /// **仅配置文件可调，界面无此项**。0 = 立即退出）
    pub exit_delay_ms: u64,
    /// **打开档案**耗时超过多少毫秒才弹「正在打开」进度窗。
    ///
    /// 用户口径是「打开很慢才需要弹」：小档案几十毫秒就开完，弹一个一闪而过的
    /// 窗口纯属打扰；大档案（固实 7z、超大 tar.gz）则必须让用户看见它在动，
    /// 否则界面看着就是卡死。**仅配置文件可调，界面无此项**；0 = 立即弹。
    pub open_show_delay_ms: u64,
    /// 「正在打开」进度窗在打开**结束**后停留多少毫秒再自动关闭
    /// （用户口径 0.1 s；**仅配置文件可调**，且不受「完成后退出/自动关窗」
    /// 那套设置影响——打开不是任务，它自己管自己的窗口）。0 = 立即关。
    pub open_close_delay_ms: u64,
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            notify_on_finish: false,
            progress_window: true,
            exit_on_finish: false,
            exit_delay_ms: 1000,
            open_show_delay_ms: 250,
            open_close_delay_ms: 100,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Preview {
    pub max_text_kb: u32,
    pub max_image_mb: u32,
    pub max_hex_kb: u32,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            max_text_kb: 256,
            max_image_mb: 8,
            max_hex_kb: 4,
        }
    }
}

/// 界面字号下限 px（再小：中文字形开始糊成一团）。
pub const FONT_SIZE_MIN: u32 = 10;
/// 界面字号上限 px（再大：固定高度的工具条/状态栏装不下）。
pub const FONT_SIZE_MAX: u32 = 24;
/// 界面字号默认 px（= 浏览器/尾风基准 16 ⇒ rem 布局的 1rem）。
pub const FONT_SIZE_DEFAULT: u32 = 16;
/// 界面圆角下限 px（0 = 直角）。
pub const CORNER_RADIUS_MIN: u32 = 0;
/// 界面圆角上限 px（再大按钮/输入框就成了胶囊，行高有限的控件会明显变形）。
pub const CORNER_RADIUS_MAX: u32 = 20;
/// 界面圆角默认 px（0 = 直角——hzip 既有观感，用户可自行调圆）。
pub const CORNER_RADIUS_DEFAULT: u32 = 0;
/// 布局密度默认值（`[ui].density` 契约值）。
pub const DENSITY_DEFAULT: &str = "standard";
/// 文字行高倍率默认值（`[ui].line_height`；× 字号 = 行框高）。
///
/// 取 1.2 = 用户口径的「紧」端：它**低于**字体字形盒（本机主题字体
/// `ascent + |descent| ≈ 1.3em`），故生效值由渲染层夹到字形盒下限——
/// 真正决定观感的是「不低于字形盒」，配置只负责**不比它更紧**。
pub const LINE_HEIGHT_DEFAULT: f32 = 1.2;
/// 行高倍率下限（低于 1.0 会把行框压到字号以下：任何字体都会裁字形）。
pub const LINE_HEIGHT_MIN: f32 = 1.0;
/// 行高倍率上限（3.0 已是非常松的排布；再大就是每行一个字）。
pub const LINE_HEIGHT_MAX: f32 = 3.0;

/// 用户界面偏好（B.1 `[ui]`）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Ui {
    /// system / light / dark
    pub theme: String,
    /// 主题色（品牌色 hex `#RRGGBB`；空 = 主题默认——T-SEL 走查 #3
    /// 设置页 ColorPicker 写入；覆盖 primary/accent 语义槽）
    pub theme_color: String,
    /// system / zh-CN / en
    pub lang: String,
    /// 缺省压缩格式（与 defaults.format 白名单一致；GUI 设置页写入）
    pub default_format: String,
    /// 缺省压缩级别（store/fastest/fast/normal/maximum/ultra）
    pub default_level: String,
    /// 浏览表格列显隐/顺序（GUI 设置页写入）
    pub columns: Columns,
    /// 压缩文件中显示 `..` 跳上级（R-GUI2 #4）
    pub show_dotdot: bool,
    /// **改动后立即写回**（默认 true = 自动保存）。
    ///
    /// `false` = 手动保存：改动先挂起（浏览页工具栏出现「保存 (n)」），由用户
    /// 一次性合并写回——组合链/大档案每次写回都要全量重压，改多个文件时这一步
    /// 很贵（2026-10-19 用户口径）。
    pub write_back_immediate: bool,
    /// 自动更新开关（检查逻辑挂账 T43；R-GUI2 #3）
    pub auto_update: bool,
    /// 文件关联勾选扩展名集合（R-GUI2 #3 存偏好 / R2-05 写注册表）
    pub assoc_formats: Vec<String>,
    /// 上下文解压菜单勾选 key（白名单见 GUI contracts CTX_EXTRACT_KEYS）
    pub ctx_extract: Vec<String>,
    /// 上下文压缩菜单勾选 key
    pub ctx_compress: Vec<String>,
    /// 目录行显示聚合大小（R-GUI2 #4）
    pub show_folder_size: bool,
    /// 图像查看器路径（空 = 系统默认）
    pub image_viewer: String,
    /// 编辑器路径（空 = 系统默认）
    pub editor: String,
    /// Shell 扩展 DLL 路径（空 = 主程序同目录的 `hzip-shell.dll`；非空 =
    /// 右键菜单注册 `InProcServer32` 指向该路径——T-SEL 走查 #14：DLL
    /// 与主程序分目录部署时经配置指定）
    pub shell_ext_dll: String,
    /// 最近打开档案（LRU 序，cap 10；T-MENU4 菜单「最近的文件」数据源）
    pub recent_archives: Vec<String>,
    /// 浏览页左树显隐（T-MENU5 菜单/设置双入口）
    pub show_tree: bool,
    /// 任务中心页签显隐（默认 **false** = 界面只有浏览页、连页签栏都不显示；
    /// 菜单「视图 ▸ 任务中心」/ 设置双入口同源）
    pub show_jobs: bool,
    /// 工具栏三态：icon_text / icon_only / hidden（T-TVS2 内容语义；
    /// 旧值 large/small 读取时无感回落 icon_text）
    pub toolbar_style: String,
    /// 视图样式：detail / icon / list（T-MENU5/T-MENU9）
    pub view_style: String,
    /// 文件类型图标来源：`system` / `builtin`（T-ICON 决策 #1）。
    ///
    /// `system` = 明细名称列 / 图标网格 / 紧凑列表 / 左树的行图标取操作系统
    /// 文件管理器同款图标（当前仅 Windows 生效；其余平台拿不到即回退 `builtin`）；
    /// `builtin` = 只用内置图标集（关闭系统图标探测，零平台调用）。
    ///
    /// 本层**不归一**：与 `theme` / `lang` / `view_style` 同惯例直存原字符串
    /// （白名单校验在消费方，大小写不敏感解析，未知/空值按 `system` 处理）。
    pub file_type_icons: String,
    /// 浏览页左树宽度 px（R-GUI2 #4 拖动持久化）
    pub tree_width: u32,
    /// 界面字体族（空 = 主题默认 `.SystemUIFont`；取值由 GUI 从系统字体列表
    /// 给出，写入时不经白名单校验——手改配置里的未知字体由渲染层回落默认）
    pub font_family: String,
    /// 界面字号 px（rem 基准；有效域 [`FONT_SIZE_MIN`]..=[`FONT_SIZE_MAX`]，
    /// 0/越界按 [`FONT_SIZE_DEFAULT`] 处理——见 [`Ui::font_size_px`]）
    pub font_size: u32,
    /// 界面圆角弧度 px（0 = 直角；有效域 [`CORNER_RADIUS_MIN`]..=[`CORNER_RADIUS_MAX`]，
    /// 越界按 [`CORNER_RADIUS_DEFAULT`] 处理——见 [`Ui::corner_radius_px`]）。
    ///
    /// 作用于 gpui-kit 按 `theme.radius` 取圆角的组件（菜单/按钮/输入框/页签
    /// 关闭钮/弹层容器）；弹层专用档 `radius_lg` 按同比例派生，见 GUI
    /// `ui::theme::apply_theme_cfg`。
    pub corner_radius: u32,
    /// 布局密度：compact / standard / loose（未知值按 standard 处理；
    /// 只缩放**组件间距**，不改变字号——见 GUI `ui::spacing`）
    pub density: String,
    /// 文字行高倍率（× 字号 = 行框高；文件列表 / 目录树 / 复选标签行）：默认
    /// [`LINE_HEIGHT_DEFAULT`]，有效域 [`LINE_HEIGHT_MIN`]..=[`LINE_HEIGHT_MAX`]。
    ///
    /// 渲染层把生效值**夹到字体字形盒下限**（`ascent + |descent|`）——低于它的
    /// 行框会顶掉降部（`g` 看起来像 `a`，见 GUI `ui/spacing::line_height_px`）。
    /// 只影响**行框高度**（行与行的疏密），不改变间距刻度 [``density`]。
    pub line_height: f32,
}
impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            theme_color: String::new(),
            lang: "system".into(),
            default_format: "zip".into(),
            default_level: "normal".into(),
            columns: Columns::default(),
            show_dotdot: true,
            write_back_immediate: true,
            auto_update: false,
            assoc_formats: Vec::new(),
            ctx_extract: Vec::new(),
            ctx_compress: Vec::new(),
            show_folder_size: false,
            image_viewer: String::new(),
            editor: String::new(),
            shell_ext_dll: String::new(),
            recent_archives: Vec::new(),
            show_tree: true,
            show_jobs: false,
            toolbar_style: "icon_text".into(),
            view_style: "detail".into(),
            file_type_icons: "system".into(),
            tree_width: 260,
            font_family: String::new(),
            font_size: FONT_SIZE_DEFAULT,
            corner_radius: CORNER_RADIUS_DEFAULT,
            density: DENSITY_DEFAULT.into(),
            line_height: LINE_HEIGHT_DEFAULT,
        }
    }
}

impl Ui {
    /// 归一后的界面字号 px：0 / 越界 → [`FONT_SIZE_DEFAULT`]。
    ///
    /// 手改配置写 0、「999」或漏键都落回默认，不 panic、不把窗口做废
    /// （GUI 上行值已经过同一归一，故这里只管手改/旧配置文件）。
    pub fn font_size_px(&self) -> u32 {
        if (FONT_SIZE_MIN..=FONT_SIZE_MAX).contains(&self.font_size) {
            self.font_size
        } else {
            FONT_SIZE_DEFAULT
        }
    }

    /// 归一后的界面圆角 px：越界（手改 999 等）→ [`CORNER_RADIUS_DEFAULT`]。
    ///
    /// 与 [`Ui::font_size_px`] 的差别：这里 **0 是合法值**（= 直角）而不是
    /// 「漏键」，故只夹越界，不做 0→默认 的回落。
    pub fn corner_radius_px(&self) -> u32 {
        if (CORNER_RADIUS_MIN..=CORNER_RADIUS_MAX).contains(&self.corner_radius) {
            self.corner_radius
        } else {
            CORNER_RADIUS_DEFAULT
        }
    }

    /// 归一后的行高倍率：NaN / 越界 / 漏键 → [`LINE_HEIGHT_DEFAULT`]。
    ///
    /// 同 [`Ui::font_size_px`]：手改配置写 `0`、`-1`、`999` 都不 panic，也不把
    /// 界面做成一条线。**字形盒下限在渲染层夹**（那里才拿得到字体度量）。
    pub fn line_height_em(&self) -> f32 {
        if self.line_height.is_finite()
            && (LINE_HEIGHT_MIN..=LINE_HEIGHT_MAX).contains(&self.line_height)
        {
            self.line_height
        } else {
            LINE_HEIGHT_DEFAULT
        }
    }
}

/// 浏览表格列设置 POD（`ui.columns`；不依赖任何上层 crate）。
///
/// `order` 为显示顺序（含隐藏列）；`hidden` 为隐藏列名集合。列名白名单：
/// `name` / `type` / `size` / `packed` / `encoding` / `mtime` / `level` /
/// `encryption` / `crc` / `attrs`（未知名忽略——上层转换时过滤）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Columns {
    /// 显示顺序（列名小写）
    pub order: Vec<String>,
    /// 隐藏列集合（列名小写；不含 `name`——名称列强制显示）
    pub hidden: Vec<String>,
}

impl Default for Columns {
    fn default() -> Self {
        Self {
            // 与 UI 默认列序一致（`ColumnsConfig::default()`；R-GUI4 #2 起含类型列
            // ——缺 type 会让「恢复默认」写出与出厂默认不同的列序）。2026-09-25
            // 追加 level/encryption/crc/attrs 四列——默认隐藏（hidden 与 UI 侧
            // `BrowseColumnId::DEFAULT_HIDDEN` 同集；备注列 2026-09-26 下线）
            order: [
                "name",
                "type",
                "size",
                "packed",
                "encoding",
                "mtime",
                "level",
                "encryption",
                "crc",
                "attrs",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            hidden: ["level", "encryption", "crc", "attrs"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Logging {
    /// debug / info / warn / error
    pub level: String,
    /// 滚动保留天数
    pub retention_days: u8,
    /// 日志目录（空/省略 = 平台默认 `hzip_platform::dirs::log_dir()`）
    pub dir: String,
    /// plain / detailed（detailed = 带源文件名 + 代码行 + target）
    pub format: String,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: "info".into(),
            retention_days: 7,
            dir: String::new(),
            format: "detailed".into(),
        }
    }
}

/// 快捷键配置（`[keys]`；T-KEYS 需求 2）。
///
/// 每个字段 = 一个**可配置动作**，字段名与 TOML 键名、GUI 的
/// `ui::shell::keymap::KeyActionDef::config_key` 三者同字面；**声明顺序**即
/// 设置页「快捷键」页签的行序（GUI `keymap::ACTIONS` 逐条对应）。
///
/// # 三态（每个字段一个 `String`）
///
/// | 值 | 语义 | 行为 |
/// |---|---|---|
/// | `""`（或缺键） | 用**内置默认** | 绑该动作的默认键位（各字段文档里那条；GUI 侧 `default_spec`） |
/// | `"none"` | **禁用**该动作 | 不注册任何键盘绑定；菜单项仍可点击、仍能执行，只是不再有快捷键、菜单也不显示提示 |
/// | 其它 | 自定义键位 | ASCII 键位串（如 `ctrl-shift-n`）；**解析失败回落默认并 `tracing::warn!`** |
///
/// # 为什么是哨兵字符串，而不是 `Option<String>` / 枚举
///
/// - TOML 没有 `null`：用户在配置文件里表达「这个动作别用快捷键了」，最直白也
///   最不容易写错的就是 `open = "none"`；而 `open = ""` 必须仍表示「用默认」，
///   两者得能区分。
/// - `""` = 「未设置 → 回落默认」是本文件既有惯例（`ui.image_viewer`、
///   `ui.editor`、`update.check_url` 同口径），沿用同一口径就不必让用户学第二套
///   空值语义；也让 `#[serde(default)]` 缺键回落自然等于「全默认」。
/// - **判定顺序在消费方**：必须先认哨兵/空串，再进 `Keystroke::parse`。把
///   `"none"` 直接丢给解析器**不会报错**（它只是把 `none` 当成一个叫 `none`
///   的普通键名，而物理键盘永远送不出这个名字），于是「禁用」会被静默当成
///   「绑了个永不触发的键」——看着生效、实则与默认绑定被清掉不是一回事。
///
/// # 本层不校验、不归一
///
/// 与 `theme` / `lang` / `file_type_icons` 同惯例：本层**直存**用户写下的字符串。
/// 「合法键位串」只有 GUI 拿得到判定（`gpui` 的 `Keystroke::parse`），且「非法串
/// 回落默认」应当是**每次装配时**判一次而不是在读取时把原文改掉——手改的错误值
/// 原样留在盘上，用户回头才看得出自己写错在哪（若这里悄悄改成默认，用户会以为
/// 自己写的东西被程序吃了）。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Keys {
    /// 打开档案（默认 `ctrl-o`）
    pub open: String,
    /// 解压（默认 `ctrl-e`）
    pub extract: String,
    /// 新建窗口（默认 `ctrl-shift-n`）
    pub new_window: String,
    /// 全选（默认 `ctrl-a`；T-SEL2 起「全选/反选」只作用于**可见行**）
    pub select_all: String,
    /// 反选（默认 `ctrl-i`）
    pub select_invert: String,
    /// 复制（默认 `ctrl-c`）
    pub copy: String,
    /// 粘贴（默认 `ctrl-v`）
    pub paste: String,
    /// 查找（默认 `ctrl-f`）
    pub find: String,
    /// 测试完整性（默认 `ctrl-t`）
    pub test_integrity: String,
    /// 打开设置（默认 `ctrl-,`）
    pub settings: String,
    /// 任务中心（默认 `ctrl-j`）
    pub show_jobs: String,
    /// 刷新（默认 `f5`）
    pub refresh: String,
    /// 删除（默认 `delete`）
    pub delete: String,
    /// 退出（默认 `ctrl-q`）
    pub quit: String,
}

impl Keys {
    /// 按 TOML 键名取单个键位原文（`None` = 该键名不属于本表）。
    ///
    /// 为什么拿字符串键来索引：调用方（GUI）的世界里这套动作是按
    /// `config_key: &'static str` 组织的（设置注册表行、`keymap::ACTIONS`、
    /// 冲突检测全走同一个字面量），本 crate 不依赖 GUI 类型 ⇒ 这一处字符串边界
    /// 换来上层零映射表。
    ///
    /// 为什么是显式 match 而不是宏/反射：14 条是冻结契约，分支逐条写出来才能在
    /// 改字段名时被编译器盯住；`field` / `field_mut` 两个 match 的键名集合由单测
    /// 互锁（见 `tests/lib_rs.rs::keys_field_roundtrip_all_known_keys`）。
    pub fn field(&self, config_key: &str) -> Option<&str> {
        match config_key {
            "open" => Some(self.open.as_str()),
            "extract" => Some(self.extract.as_str()),
            "new_window" => Some(self.new_window.as_str()),
            "select_all" => Some(self.select_all.as_str()),
            "select_invert" => Some(self.select_invert.as_str()),
            "copy" => Some(self.copy.as_str()),
            "paste" => Some(self.paste.as_str()),
            "find" => Some(self.find.as_str()),
            "test_integrity" => Some(self.test_integrity.as_str()),
            "settings" => Some(self.settings.as_str()),
            "show_jobs" => Some(self.show_jobs.as_str()),
            "refresh" => Some(self.refresh.as_str()),
            "delete" => Some(self.delete.as_str()),
            "quit" => Some(self.quit.as_str()),
            _ => None,
        }
    }

    /// 按 TOML 键名取单个键位的**可变**引用（设置页「录制」写回整份 `Keys` 时用；
    /// `None` = 未知键）。
    ///
    /// 与 [`Keys::field`] 是同一批键名，只是可写——两个 match 必须同步演进。
    pub fn field_mut(&mut self, config_key: &str) -> Option<&mut String> {
        match config_key {
            "open" => Some(&mut self.open),
            "extract" => Some(&mut self.extract),
            "new_window" => Some(&mut self.new_window),
            "select_all" => Some(&mut self.select_all),
            "select_invert" => Some(&mut self.select_invert),
            "copy" => Some(&mut self.copy),
            "paste" => Some(&mut self.paste),
            "find" => Some(&mut self.find),
            "test_integrity" => Some(&mut self.test_integrity),
            "settings" => Some(&mut self.settings),
            "show_jobs" => Some(&mut self.show_jobs),
            "refresh" => Some(&mut self.refresh),
            "delete" => Some(&mut self.delete),
            "quit" => Some(&mut self.quit),
            _ => None,
        }
    }
}

/// 进程内配置缓存条目：`(路径, 文件戳, 解析结果)`。
///
/// 文件戳为 `Option`：**盘上还没有文件**（首次运行）也要能缓存刚写入的值
/// ——见 [`Config::save_async`]（写后立读）。
type CacheEntry = (PathBuf, Option<Stamp>, Config);

/// 文件戳：`(mtime, 长度)`（`None` = 文件不存在/不可 stat）。
type Stamp = (std::time::SystemTime, u64);

/// 进程内配置缓存（见 [`Config::load`]）。
static CONFIG_CACHE: std::sync::Mutex<Option<CacheEntry>> = std::sync::Mutex::new(None);

/// 待落盘配置（**最新一份**覆盖；见 [`Config::save_async`]）。
static PENDING_SAVE: std::sync::Mutex<Option<Config>> = std::sync::Mutex::new(None);

/// 异步写线程是否在跑（避免每个请求都起线程）。
static SAVE_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// [`Config::flush_pending_saves`] 的等待上限。
pub const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

impl Config {
    /// 全默认（无文件时的回落值；等价 B.1 表）。
    pub fn defaults() -> Self {
        Self::default()
    }

    /// 加载配置：`HZIP_CONFIG` 覆盖路径 → 否则 `config_dir()/config.toml`。
    /// 文件缺失/不可读/损坏 → 默认值（损坏场景记录 tracing warn，不阻塞）。
    ///
    /// # 进程内缓存（mtime/size 守卫）
    ///
    /// `load()` 是「读盘 + TOML 解析」，而 GUI 里有四十来处调用点，其中若干在
    /// **渲染帧**与**每次操作**路径上（菜单/进度窗/分栏/设置…）——2026-09-24
    /// UI 线程阻塞审计把这类同步读盘列为明显阻塞点。故加一层进程内缓存：
    /// 每次调用只 `metadata()`（stat，微秒级）比对 mtime + 长度，命中直接返回
    /// 克隆，未命中才真读盘解析。
    ///
    /// 为什么用 mtime 而不是「只在 save() 时失效」：配置文件也可能被**别的进程**
    /// （CLI、编辑器）改写，stat 守卫让这种情形照样立刻可见——语义与「每次都读」
    /// 实质等价，只省掉了解析与读。
    ///
    /// # 写后立读（2026-10-09 修）
    ///
    /// 落盘是**异步**的（[`Self::save_async`]），而调用方常在写后**立刻**读
    /// （GUI 的设置生效半场、菜单 cfg 缓存、面板重载）——只靠 stat 守卫会读到
    /// **上一拍**的盘上值（实测 200/200 陈旧 ⇒ 设置「点一次不生效、再点一次
    /// 才生效」）。故 `save_async` 入队在**同一时刻**把新值铺进本缓存
    /// （戳 = 写入前的当前戳）：落盘前 `load()` 命中新值，落盘后戳变化自然
    /// miss → 读到盘上同一份（幂等，不破坏「外部改写立刻可见」）。
    pub fn load() -> Self {
        let path = Self::path();
        let stamp = Self::stamp_of(&path);
        if let Ok(cache) = CONFIG_CACHE.lock()
            && let Some((cached_path, cached_stamp, cfg)) = cache.as_ref()
            && cached_path == &path
            && *cached_stamp == stamp
        {
            return cfg.clone();
        }
        let cfg = Self::load_uncached(&path);
        Self::seed_cache(&path, stamp, cfg.clone());
        cfg
    }

    /// 文件戳（`None` = 文件不存在/不可 stat）。
    fn stamp_of(path: &std::path::Path) -> Option<Stamp> {
        std::fs::metadata(path)
            .map(|m| {
                (
                    m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    m.len(),
                )
            })
            .ok()
    }

    /// 铺进程内缓存（`stamp` = 与该份配置对应的**当时**文件戳；`None` = 盘上
    /// 尚无文件）。缓存键 = `(路径, 戳)`：戳一变即自动 miss 去读盘。
    fn seed_cache(path: &std::path::Path, stamp: Option<Stamp>, cfg: Config) {
        if let Ok(mut cache) = CONFIG_CACHE.lock() {
            *cache = Some((path.to_path_buf(), stamp, cfg));
        }
    }

    /// 真读盘 + 解析（[`Self::load`] 的未缓存内核）。
    fn load_uncached(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    tracing::warn!("配置文件损坏，使用默认值: {} ({e})", path.display());
                    Config::defaults()
                }
            },
            Err(_) => Config::defaults(),
        }
    }

    /// **异步**落盘（去抖 + 后台线程合并写）。
    ///
    /// 为什么需要（2026-09-24 UI 线程阻塞审计 B6）：GUI 的写盘点（设置变更 /
    /// 右键勾选草稿 / MRU / 分栏宽度）都在 UI 线程上，而 [`Self::save`] 是
    /// `create_dir_all` + 序列化 + `fs::write` 的**同步 IO**，连点设置时就是
    /// 一连串帧内写盘。本方法只把「最新一份」记进待写槽（配置是整体快照，
    /// 丢中间态无副作用 ⇒ 最新覆盖即可，不需要队列），由一个后台线程写。
    ///
    /// # 写后立读（2026-10-09 修）
    ///
    /// 入队**同时**把新值铺进进程内缓存（见 [`Self::load`] 文档），使落盘前
    /// 紧跟着的 `load()` 也拿到新值。旧实现只等后台线程落盘：GUI 的持久化
    /// 尾步是 `save_async`，而生效半场/菜单 cfg 缓存**紧接着**就 `load()`，
    /// 于是读到上一拍的值（实测 200/200 陈旧）⇒ 表现为「设置点一次不生效、
    /// 再点一次才生效」。
    ///
    /// 残留窗口（知情）：后台线程正在写**旧一份**时会把缓存指向旧一份（若期间
    /// 已有更新份入队）——更新份随即写入并覆盖之，窗口 = 一次写盘时长（毫秒级），
    /// 且需两次入队落在同一窗口内才可能被观察到。GUI 的字号/圆角/主题色
    /// **不依赖**本缓存（走内存镜像，见 `hzip_gui::ui::theme::ThemeReplay`）。
    ///
    /// 退出前请调 [`Self::flush_pending_saves`]（否则可能丢最后一次写）。
    pub fn save_async(self) {
        let path = Self::path();
        Self::seed_cache(&path, Self::stamp_of(&path), self.clone());
        if let Ok(mut pending) = PENDING_SAVE.lock() {
            *pending = Some(self);
        }
        if SAVE_RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return; // 已有写线程：它会（在清标志后）再看一眼待写槽
        }
        let spawned = std::thread::Builder::new()
            .name("hzip-cfg-write".into())
            .spawn(|| {
                loop {
                    let next = PENDING_SAVE.lock().ok().and_then(|mut p| p.take());
                    let Some(cfg) = next else {
                        // 先清标志再复检：清标志与请求方「看到 true 就不起线程」
                        // 之间若插入新请求，本轮复检必须接住它（否则丢写）。
                        SAVE_RUNNING.store(false, std::sync::atomic::Ordering::Release);
                        if PENDING_SAVE.lock().map(|p| p.is_none()).unwrap_or(true) {
                            break;
                        }
                        SAVE_RUNNING.store(true, std::sync::atomic::Ordering::Release);
                        continue;
                    };
                    if let Err(e) = cfg.save() {
                        tracing::warn!("配置写盘失败（内存态已更新）: {e}");
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("配置写线程创建失败，降级为当前线程同步写: {e}");
            if let Some(cfg) = PENDING_SAVE.lock().ok().and_then(|mut p| p.take()) {
                let _ = cfg.save();
            }
            SAVE_RUNNING.store(false, std::sync::atomic::Ordering::Release);
        }
    }

    /// 阻塞到待写内容落盘（退出前调用；上限 [`FLUSH_TIMEOUT`] 防卡死）。
    ///
    /// 返回 `true` = 已无待写；`false` = 超时（内容仍在磁盘上留有不一致风险，
    /// 但不阻塞退出）。
    pub fn flush_pending_saves() -> bool {
        let deadline = std::time::Instant::now() + FLUSH_TIMEOUT;
        while std::time::Instant::now() < deadline {
            let idle = PENDING_SAVE.lock().map(|p| p.is_none()).unwrap_or(true)
                && !SAVE_RUNNING.load(std::sync::atomic::Ordering::Acquire);
            if idle {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }

    /// 配置文件路径（`HZIP_CONFIG` 覆盖 → config_dir/config.toml）。
    pub fn path() -> PathBuf {
        if let Ok(p) = std::env::var("HZIP_CONFIG")
            && !p.is_empty()
        {
            return PathBuf::from(p);
        }
        hzip_platform::dirs::config_dir().join("config.toml")
    }

    /// 首次运行落默认模板：config_dir 无 config.toml → 写内嵌模板
    /// （etc/hzip.toml 单一来源）。已有文件不动。返回是否新建。
    pub fn ensure_default_config() -> bool {
        let path = Self::path();
        if path.exists() {
            return false;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, DEFAULT_TEMPLATE) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("默认配置写入失败（忽略，用内置默认）: {e}");
                false
            }
        }
    }

    /// 显式从给定文件加载（测试/嵌入式用）。
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Config>(&text).unwrap_or_else(|e| {
                tracing::warn!("配置损坏（用默认）: {e}");
                Config::defaults()
            }),
            Err(_) => Config::defaults(),
        }
    }

    /// 日志级别（logging.level 有效值否则默认 "info"）。
    pub fn log_level(&self) -> &str {
        match self.logging.level.as_str() {
            "debug" | "info" | "warn" | "error" | "trace" => &self.logging.level,
            _ => "info",
        }
    }

    /// 序列化保存到当前配置路径（`Config::path()`）。
    ///
    /// 设置页「即时保存」通道：调用方（hzip-gui）在事件消费处触发；失败仅
    /// 返回错误（由调用方决定提示/降级），不 panic。父目录不存在时先建。
    pub fn save(&self) -> Result<(), std::io::Error> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(&path, text)?;
        // 落盘后把缓存指向**刚写下的这一份**（戳 = 写后戳）：写后立读零额外
        // 读盘，且同秒内改写也踩不到旧条目（旧实现是显式失效 → 再读盘）。
        Self::seed_cache(&path, Self::stamp_of(&path), self.clone());
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/lib_rs.rs"]
mod tests;
