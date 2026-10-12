//! 压缩/解压对话框 ViewModel（02 §11.4 对话框参数实时预览数据源；T41）。
//!
//! 参数 → [`JobSpec`] 构建：压缩（`JobKind::Compress`，core `CompressSpec` 载体）、
//! 解压（`JobKind::Extract`，`JobSpec::extract_job` + `ExtractOptions` 载体）。
//! 格式/级别/编码/冲突策略取值语义参考 CLI 值解析器（02 §4.2 白名单 +
//! HZ-5003）与 02 附录 B.1 配置默认——本层为纯数据参数容器（UI 下拉值由 gpui/gpui_kit
//! 侧取白名单常量），不实现文本解析。

use std::path::PathBuf;

use hzip_core::CodecMethod;
use hzip_core::Level;
use hzip_core::jobs::{JobKind, JobSpec};
use hzip_core::services::compress::CompressSpec;
use hzip_core::services::extract::ExtractOptions;
use hzip_error::HzipError;
use hzip_format::{ConflictPolicy, Encoding, EncryptionRequest, FormatId, Selection};
use std::path::Path;
use zeroize::Zeroizing;

/// 对话框默认格式（02 附录 B.1 `defaults.format = "zip"`；UI 首次打开值）。
pub const DEFAULT_FORMAT: &str = "zip";

/// 压缩格式白名单（02 §4.2 `FormatArg`；可写格式集）。
pub const COMPRESS_FORMATS: &[&str] = &[
    "zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "gz", "bz2", "xz", "zst",
];

/// 冲突策略白名单（02 附录 B.1 `extract.conflict`）。
pub const CONFLICT_POLICIES: &[&str] = &["ask", "overwrite", "skip", "rename", "newer"];

/// config `[extract].conflict` 字符串 → core 冲突策略（README §2.5 契约）。
///
/// 值域与 hzip-cli `CONFLICTS` 白名单对齐：`"ask"/"overwrite"/"skip"/"rename"/"newer"`
///（CLI cli.rs:209 同名映射，`rename` → [`ConflictPolicy::RenameAuto`]，
/// `newer` → [`ConflictPolicy::NewerOnly`]）。未知值回落默认 [`ConflictPolicy::Ask`]。
pub fn conflict_from_str(s: &str) -> ConflictPolicy {
    match s {
        "overwrite" => ConflictPolicy::Overwrite,
        "skip" => ConflictPolicy::Skip,
        "rename" => ConflictPolicy::RenameAuto,
        "newer" => ConflictPolicy::NewerOnly,
        _ => ConflictPolicy::Ask,
    }
}

/// config codepage 字符串 → [`Encoding`]（README §2.5 契约）。
///
/// `None`/`"auto"`/空串/未知 → `None`（自动检测）；白名单与 hzip-cli
/// `parse_codepage`（ops.rs:540）及 core [`Encoding::from_charset`] 对齐，
/// 大小写不敏感。
pub fn codepage_from_str(s: Option<&str>) -> Option<Encoding> {
    let s = s?;
    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
        return None;
    }
    Encoding::from_charset(s)
}

/// config `security.symlink` 字符串 → [`SymlinkPolicy`]（README §2.5 契约）。
///
/// 白名单与 hzip-cli `SymlinkArg`（cli.rs `SYMLINKS` 语义）对齐：
/// `"deny-escape"/"skip"/"flatten"/"deny-all"`；未知值回落默认
/// [`SymlinkPolicy::DenyEscape`]（02 §9.6 默认档）。
pub fn symlink_from_str(s: &str) -> hzip_format::SymlinkPolicy {
    match s {
        "skip" => hzip_format::SymlinkPolicy::Skip,
        "flatten" => hzip_format::SymlinkPolicy::Flatten,
        "deny-all" => hzip_format::SymlinkPolicy::DenyAll,
        // deny-escape 与一切未知值 → 默认档（向后兼容）
        _ => hzip_format::SymlinkPolicy::DenyEscape,
    }
}

/// 参数非法（未选格式 / 无输入 / 未知编码名）→ 5003 语义错误。
fn arg_err(what: impl Into<String>) -> HzipError {
    HzipError::Unsupported {
        what: format!("hz-5003 {}", what.into()).into(),
    }
}

/// 级别白名单表（**正向/逆向解析共用同一份**；T-OPT 项12：原实现两个函数
/// 各内联一份逐字相同的 `[Level; 6]`，增删档位时容易只改一处）。
const LEVEL_ORDER: [Level; 6] = [
    Level::Store,
    Level::Fastest,
    Level::Fast,
    Level::Normal,
    Level::Maximum,
    Level::Ultra,
];

/// config `[defaults].level` / 对话框级别字符串 → core 级别（02 §6.3）。
///
/// 值域取 [`crate::app::labels::LEVEL`]——**唯一来源**：UI 下拉、配置文件、
/// 本解析器共用同一张表（历史上冲突策略曾在两处各写一份，T-SET2 已收口；
/// 这里不重蹈）。未知值回落 `normal`（02 附录 B.1 缺省）。
pub fn level_from_str(s: &str) -> Level {
    let lower = s.to_ascii_lowercase();
    let ix = crate::app::labels::LEVEL
        .iter()
        .position(|(_, v)| *v == lower)
        .unwrap_or(3); // 3 = normal
    LEVEL_ORDER[ix]
}

/// 级别 → [`crate::app::labels::LEVEL`] 下标（[`level_from_str`] 的逆；参数窗回显用）。
pub fn level_index(level: Level) -> usize {
    // 逆映射必须与正向同表（`level_from_str_matches_labels_table_order` 互锁）
    LEVEL_ORDER.iter().position(|l| *l == level).unwrap_or(3)
}

/// 压缩输出的建议路径：「源同级目录 / 源主名.<ext>」。
///
/// - 单选文件：`dir/a.tar.gz` + `zip` → `dir/a.tar.zip`（取主名，与 7-Zip 一致）
/// - 单选目录：`dir/photos` + `zip` → `dir/photos.zip`
/// - 多选：取**首项所在目录的目录名**（`dir/a.txt`+`dir/b.txt` → `dir/dir.zip`），
///   与 7-Zip/Bandizip 的「添加到…」同观感
pub fn default_out_path(sources: &[PathBuf], ext: &str) -> PathBuf {
    let first = sources.first();
    let dir = first
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let stem = if sources.len() > 1 {
        dir.file_name().map(|s| s.to_string_lossy().into_owned())
    } else {
        first.and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
    }
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "archive".to_string());
    dir.join(format!("{stem}.{ext}"))
}

/// 输出路径去重：目标已存在则追加 `_1`、`_2`…，**绝不覆盖**既有档案。
///
/// 右键「添加到 X.zip」没有任何确认弹窗，若直接覆盖，用户原有的同名档案会
/// 不可逆丢失——同名时追加序号（与 7-Zip 的「添加到」行为一致）。
pub fn unique_out_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for n in 1u32.. {
        let name = if ext.is_empty() {
            format!("{stem}_{n}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("存在无限个不重名候选（循环在首个空闲名处返回）")
}

/// 压缩对话框参数模型（T-G30 双向绑定 gpui_kit 控件；09 §3 扩面）。
///
/// # 默认值来源（互锁，勿改成 `#[derive(Default)]`）
///
/// [`Default`] 逐字段取 [`CompressSpec::default`] 的同名字段，只覆盖两处
/// 「对话框语义」差异：
/// - `format: None`（未选 → `build_spec` 拒绝 HZ-5003）—— spec 默认为 tar；
/// - `out: None`（未填路径）—— spec 默认为空 `PathBuf`。
///
/// 这样 `keep_metadata` / `verify_after` 这类「spec 默认开」的字段不会因为本结构
/// 自己写一份默认值而悄悄变成关（两处各写一份默认值 = 迟早漂移）。
#[derive(Debug, Clone)]
pub struct CompressDialogVm {
    /// 目标格式（`None` = 未选 → build_spec 拒绝 HZ-5003）
    pub format: Option<FormatId>,
    /// 压缩方式（`None` = 沿用格式默认编码器；须 ∈ 格式 `caps().methods`，
    /// 越集由 core 以 HZ-5001 拒绝）
    pub method: Option<CodecMethod>,
    /// 统一压缩级别（默认 Normal）
    pub level: Level,
    /// 档案级密码（Zeroizing 持有，02 §1.2；与 `encryption` **成对**进 spec，
    /// 单边携带由 core 入口以 HZ-5003 拒绝）
    pub password: Option<Zeroizing<String>>,
    /// 加密方式（`None` = 不加密；进 spec 后由 core `writer_for` 按 caps 校验）
    pub encryption: Option<EncryptionRequest>,
    /// 7z「加密文件名」= 头加密（默认开——安全默认，设计稿 11 决策②）
    pub encrypt_names: bool,
    /// 分卷大小字节（None = 单文件）
    pub split: Option<u64>,
    /// 线程数（0 = 自动）
    pub threads: u32,
    /// LZMA2 字典大小覆写（`None` = 随级别派生；仅 7z 消费）
    pub dict_size: Option<u64>,
    /// 固实（`caps.solid` 才有效，当前仅 7z）
    pub solid: bool,
    /// 更新方式：追加到既有档案（`false` = 新建；仅 `caps.append` 格式有意义）
    pub append: bool,
    /// 排除 glob（`*.tmp` 按段、`sub/*` 剪枝，02 §9.5 B）
    pub excludes: Vec<String>,
    /// 排除隐藏文件（默认 false）
    pub exclude_hidden: bool,
    /// 跟随符号链接（默认 false：链接条目记 target）
    pub follow_symlinks: bool,
    /// 保留文件属性（mtime/mode/attrs；默认 true）
    pub keep_metadata: bool,
    /// finish 后重开自检（默认 true；大文件可关）
    pub verify_after: bool,
    /// 写向 best_effort（默认 false = fail-fast）
    pub best_effort: bool,
    /// 输出路径（用户选择）
    pub out: Option<PathBuf>,
}

impl Default for CompressDialogVm {
    fn default() -> Self {
        // 与 CompressSpec::default() 逐字段对齐（见类型 rustdoc「默认值来源」）
        let spec = CompressSpec::default();
        Self {
            format: None,
            method: spec.method,
            level: spec.level,
            password: None,
            encryption: spec.encryption,
            encrypt_names: spec.encrypt_names.unwrap_or(true),
            split: spec.split,
            threads: spec.threads,
            dict_size: spec.dict_size,
            solid: spec.solid,
            append: spec.append,
            excludes: spec.exclude_globs.clone(),
            exclude_hidden: spec.exclude_hidden,
            follow_symlinks: spec.follow_symlinks,
            keep_metadata: spec.keep_metadata,
            verify_after: spec.verify_after,
            best_effort: spec.best_effort,
            out: None,
        }
    }
}

impl CompressDialogVm {
    /// 新建（默认 zip + Normal——02 附录 B.1 缺省）。
    pub fn new() -> Self {
        Self {
            format: Some(FormatId::from(DEFAULT_FORMAT)),
            ..Default::default()
        }
    }

    /// 构建压缩任务规格（02 §11.4 对话框职责：参数 → JobSpec）。
    ///
    /// `inputs`：待打包路径（文件/目录）；`out`：输出档案路径。
    ///
    /// # 密码携带（设计稿 11）
    ///
    /// 密码以 `Zeroizing<String>` 持有（02 §1.2），与 `encryption` **成对**
    /// 进入 `CompressSpec`；提交侧校验空密码（方式≠无 → 禁提交），core 入口
    /// 再兜一层 HZ-5003（单边携带拒绝），fmt `writer_for` 按 caps 校验方案。
    pub fn build_spec(&self, inputs: &[PathBuf], out: &Path) -> Result<JobSpec, HzipError> {
        let fmt = self
            .format
            .clone()
            .ok_or_else(|| arg_err(crate::tr("msg.compress_no_format")))?;
        if inputs.is_empty() {
            return Err(arg_err(crate::tr("msg.compress_no_inputs")));
        }
        let c = CompressSpec {
            inputs: inputs.to_vec(),
            format: fmt,
            out: out.to_path_buf(),
            split: self.split,
            level: self.level,
            method: self.method,
            threads: self.threads,
            dict_size: self.dict_size,
            encryption: self.encryption,
            password: self.password.clone(),
            encrypt_names: Some(self.encrypt_names),
            solid: self.solid,
            append: self.append,
            exclude_globs: self.excludes.clone(),
            exclude_hidden: self.exclude_hidden,
            follow_symlinks: self.follow_symlinks,
            verify_after: self.verify_after,
            best_effort: self.best_effort,
            keep_metadata: self.keep_metadata,
            comment: None,
        };
        let mut spec = JobSpec::new(JobKind::Compress, inputs[0].clone());
        spec.compress = Some(c);
        Ok(spec)
    }
}

// —— 09 压缩对话框：派生量（纯函数，UI 与摘要共用同一份真值）——

/// 格式 → 输出扩展名。
///
/// 单流格式（gz/bz2/xz/zst）当前**不含** `.tar.` 组合前缀：压缩路径无 tar 组合写
/// （见 [`is_stream_format`]），界面据实显示，不承诺写不出的名字。
pub fn default_ext(format: &str) -> &'static str {
    match format {
        "zip" => "zip",
        "7z" => "7z",
        "tar" => "tar",
        // 组合格式：后缀就是「.tar.<codec>」（产物名 `x.tar.gz`）
        "tar.gz" => "tar.gz",
        "tar.bz2" => "tar.bz2",
        "tar.xz" => "tar.xz",
        "tar.zst" => "tar.zst",
        "gz" => "gz",
        "bz2" => "bz2",
        "xz" => "xz",
        "zst" => "zst",
        // 白名单外（配置写坏）回落 zip，与 `DEFAULT_FORMAT` 一致
        _ => "zip",
    }
}

/// 单文件流格式：**恰一条目**语义，多文件/目录无法保留目录结构。
///
/// 压缩路径 `writer_for` 直接按格式取 handler（core §9.5），**没有** tar 组合写
/// （组合写只存在于 `services::convert`）——故这些格式当前只能承载单文件，
/// 界面须对多文件/目录给出明确提示（09 §7.2 / §13 决策 ②）。
pub fn is_stream_format(format: &str) -> bool {
    matches!(format, "gz" | "bz2" | "xz" | "zst")
}

/// 文件名非法字符（Windows 保留集；`None` = 合法）。
///
/// 提交前把非法名挡在对话框内，而不是让 core 在建文件时抛 IO 错（后者文案对
/// 用户没有指向性）。
pub fn invalid_name_char(name: &str) -> Option<char> {
    name.chars()
        .find(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
}

/// 分卷**预设** value → 字节数（`none` / `custom` / 未知 → `None`）。
pub fn split_preset_bytes(v: &str) -> Option<u64> {
    const MB: u64 = 1024 * 1024;
    match v {
        "10m" => Some(10 * MB),
        "100m" => Some(100 * MB),
        "700m" => Some(700 * MB),
        "1g" => Some(1024 * MB),
        "4g" => Some(4096 * MB),
        _ => None,
    }
}

/// 自定义分卷文本 → 字节数（**单位 MB**，与界面后缀一致；空/0/非数字 → `None`）。
pub fn split_custom_bytes(text: &str) -> Option<u64> {
    text.trim()
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .map(|mb| mb * 1024 * 1024)
}

/// 字典大小白名单 value → 字节（`auto`/未知 → `None` = 随级别派生）。
///
/// 越出编码器合法域（4096 B ~ 256 MiB）的值不在白名单内；写侧仍会钳制一次
/// （见 `hzip-fmt-7z::coder::lzma2_options`），本函数不重复该职责。
pub fn dict_from_str(v: &str) -> Option<u64> {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    match v {
        "64k" => Some(64 * KB),
        "1m" => Some(MB),
        "4m" => Some(4 * MB),
        "16m" => Some(16 * MB),
        "64m" => Some(64 * MB),
        "256m" => Some(256 * MB),
        // auto / 未知 / 配置写坏 → 随级别派生（安全默认）
        _ => None,
    }
}

/// 更新方式白名单 value → `append`（`append` 才为真；其余/未知 → 新建）。
///
/// 未知值回落「新建」而非「追加」：追加会改动既有档案，**默认必须是最保守的那支**。
pub fn append_from_str(v: &str) -> bool {
    matches!(v, "append")
}

/// 可用核数（线程数下拉的「自动（N 核）」文案与上限）。
pub fn threads_available() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

/// 级别在某格式下的**具体实参**（供 UI 渲染提示行；纯数据，不含文案/i18n）。
///
/// 数值全部取自 `hzip_codec::level` 的映射表——与真正写入时用的是**同一份**
/// 表，故界面显示的字典/级别不会与实际写入不同。
///
/// # 为什么按格式（而非仅按压缩方式）分派
///
/// `xz` 与 `7z` 同为 LZMA2，但旋钮不同（preset vs 字典）：容器规格决定该显示
/// 哪一个数。当前各格式只有**唯一**编码器，故 `method` 不参与分派；将来多编码器
/// 落地后本函数须改为 method 优先、格式仅决定容器相关展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelDetail {
    /// Deflate 级别（0..9）
    Deflate {
        /// deflate 级别
        level: u32,
    },
    /// LZMA2 字典 + 模式
    Lzma2 {
        /// 字典字节
        dict: u64,
        /// 压缩模式
        mode: hzip_core::level::Mode,
    },
    /// xz preset（+ 极限标志）
    Xz {
        /// preset 0..9
        preset: u32,
        /// 是否 -e（extreme，仅极限档）
        extreme: bool,
    },
    /// zstd 级别
    Zstd {
        /// zstd 级别
        level: i32,
    },
    /// bzip2 块大小
    BZip2 {
        /// 块 0..9
        block: u32,
    },
    /// 该格式不压缩（tar 仅打包）
    None,
}

/// 格式 + 级别 → [`LevelDetail`]（见该类型 rustdoc 的分派说明）。
pub fn level_detail(format: &str, level: Level) -> LevelDetail {
    use hzip_core::level as lv;
    match format {
        "zip" | "gz" => LevelDetail::Deflate {
            level: lv::deflate_level(level),
        },
        "7z" => {
            let p = lv::lzma2_params(level);
            LevelDetail::Lzma2 {
                dict: p.dict,
                mode: p.mode,
            }
        }
        "xz" => LevelDetail::Xz {
            preset: lv::xz_preset(level),
            extreme: lv::xz_extreme(level),
        },
        "zst" => LevelDetail::Zstd {
            level: lv::zstd_level(level),
        },
        "bz2" => LevelDetail::BZip2 {
            block: lv::bzip2_level(level),
        },
        // tar：无压缩层；白名单外（配置写坏）同样按不压缩显示
        _ => LevelDetail::None,
    }
}

/// 解压对话框参数模型（02 §11.4：目标选择/智能开关/冲突策略/编码修复预览）。
#[derive(Debug, Clone)]
pub struct ExtractDialogVm {
    /// 解压目标根（None → 源档案所在目录；core 语义）
    pub dest: Option<PathBuf>,
    /// 优雅解压（v2；默认 false = 恒原样直展；true = 多顶层自动包档案名壳）
    pub smart: bool,
    /// 冲突策略（默认 Ask → 运行时冲突经请求-应答弹窗）
    pub conflict: ConflictPolicy,
    /// 手动指定文件名编码（02 §8.1 B0；默认 None = 自动检测）
    pub codepage: Option<Encoding>,
    /// best-effort（02 §3.5；默认 false = fail-fast）
    pub best_effort: bool,
    /// 符号链接策略（02 §9.6；默认 DenyEscape；GUI 读 config security.symlink）
    pub symlink: hzip_format::SymlinkPolicy,
    /// 解压密码（可选；用户先在解压选项里填的候选）。语义：core **首个候选**——
    /// 成功即零弹窗，未填/不对回落到运行时询问链（`ExtractOptions.password`）。
    pub password: Option<Zeroizing<String>>,
    /// 编码修复预览对（原始名, 修复名）——Suspect 条目在解压前的展示
    /// （02 §11.4「编码修复预览」；由 [`ExtractDialogVm::preview`] 填充，
    /// 空 = 无可修复条目）
    pub preview: Vec<(String, String)>,
}

impl Default for ExtractDialogVm {
    fn default() -> Self {
        Self {
            dest: None,
            smart: false,
            conflict: ConflictPolicy::Ask,
            codepage: None,
            best_effort: false,
            symlink: hzip_format::SymlinkPolicy::DenyEscape,
            password: None,
            preview: Vec::new(),
        }
    }
}

/// 选择范围（解压操作对象；由 MainViewModel 选择集构造）。
#[derive(Debug, Clone)]
pub enum SelectionArg {
    /// 全部条目
    All,
    /// 按名字（MainViewModel 选中条目的档案内名）
    Names(Vec<String>),
}

impl ExtractDialogVm {
    /// 新建（全默认：smart=false 原样直展 / Ask / 无 codepage）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 按指定编码重映射名（修复预览用；非法字节序列 → None——保留原名 +
    /// Suspect 语义，02 §8.1「不乱猜」）。
    ///
    /// 预览把编码修复后的候选名与原 Suspect 名配对展示；实际落盘修复
    /// （T45 fmt 层字节级）以 core `ExtractOptions.codepage` 驱动为准——
    /// 本函数只提供 UI 预览数据对。
    pub fn repaired_name(&self, raw: &str) -> Option<String> {
        let enc = self.codepage?;
        // 含 U+FFFD 的名字 = fmt 层 lossy 产物（原始字节已不可恢复）→ 不乱猜
        // （TC-CORE-016 语义；GBK 字节级修复需 core 透传原始字节，挂 TODO）
        if raw.contains('\u{FFFD}') {
            return None;
        }
        let rs = to_encoding_rs(enc)?;
        let (cow, _, had_errors) = rs.decode(raw.as_bytes());
        if had_errors {
            return None;
        }
        let fixed = cow.into_owned();
        // 语法校验（02 §7.2 parse_entry_name）——修复名须可作档案条目名
        if hzip_format::parse_entry_name(&fixed).is_err() {
            return None;
        }
        Some(fixed)
    }

    /// 构建解压任务规格（02 §11.4 对话框职责：参数 → JobSpec）。
    ///
    /// `archive`：源档案路径；`sel`：解压范围（SelectionArg）。
    pub fn build_spec(&self, archive: &Path, sel: &SelectionArg) -> Result<JobSpec, HzipError> {
        if !archive.exists() {
            let path = archive.display().to_string();
            return Err(arg_err(crate::tr_with(
                "msg.extract_source_missing",
                &[("path", &path)],
            )));
        }
        let selection = match sel {
            SelectionArg::All => Selection::All,
            SelectionArg::Names(names) => Selection::Names(
                names
                    .iter()
                    .map(|n| hzip_format::EntryName::new(n.as_str()))
                    .collect(),
            ),
        };
        let opts = ExtractOptions {
            dest_root: self.dest.clone(),
            selection,
            conflict: self.conflict,
            smart: self.smart,
            best_effort: self.best_effort,
            codepage: self.codepage,
            password: self.password.clone(),
            sanitize: hzip_format::SanitizeConfig {
                symlink: self.symlink,
            },
            ..Default::default()
        };
        Ok(JobSpec::extract_job(archive.to_path_buf(), opts))
    }
}

/// hzip-format `Encoding` ↔ encoding_rs 静态编码。
fn to_encoding_rs(e: Encoding) -> Option<&'static encoding_rs::Encoding> {
    match e {
        Encoding::Gbk => Some(encoding_rs::GBK),
        Encoding::Big5 => Some(encoding_rs::BIG5),
        Encoding::ShiftJis => Some(encoding_rs::SHIFT_JIS),
        Encoding::EucKr => Some(encoding_rs::EUC_KR),
        Encoding::Cp1252 => Some(encoding_rs::WINDOWS_1252),
        Encoding::Utf8 => Some(encoding_rs::UTF_8),
        // #[non_exhaustive]（T20 冻结）：未知扩展编码 → None（调用方回退保留）
        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/dialog_vm.rs"]
mod tests;
