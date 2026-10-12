//! clap 派生定义与值解析器（02 §10.3 骨架的完整展开 + §4.1/§4.2）。
//!
//! # 值解析器（§4.2）
//!
//! - [`FormatArg`]：格式白名单（值集见 [`FORMATS`]，与 registry 注册集对应）
//!   → [`FormatId`]；
//! - [`LevelArg`]：`store|fastest|fast|normal|maximum|ultra`（大小写不敏感）；
//! - [`ConflictArg`]：`ask|overwrite|skip|rename|newer`（02 附录 B.1
//!   `extract.conflict` 枚举）；
//! - [`HumanBytes`]：`"1m"/"500k"/"2g"/"1048576"` → 字节（二进制单位，
//!   Bandizip 惯例）；0/溢出/非法单位 → Err（HZ-5003 语义，clap 退出 2）；
//! - [`GlobArg`]：构造时校验 glob 语法（非法 → Err）；
//! - [`EntryNameArg`]：经 [`parse_entry_name`]（02 §7.2 语法校验）。
//!
//! 出处：02 §10.3、§4.1/§4.2 值解析器设计、附录 B.1。

use std::path::PathBuf;
use std::str::FromStr;

use clap::{ArgAction, Parser, Subcommand};
use hzip_format::parse_entry_name;

/// 压缩级别白名单（02 §6.3 表行名，大小写不敏感）。
pub const LEVELS: &[&str] = &["store", "fastest", "fast", "normal", "maximum", "ultra"];

/// 支持的格式白名单（02 §4.2 `FormatArg` 文案；格式能力经 core Engine 间接获得，
/// 本清单与 registry 注册集对应：tar/single(gz,bz2,xz,zst)/zip/7z）。
pub const FORMATS: &[&str] = &[
    "zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "gz", "bz2", "xz", "zst",
];

/// 冲突策略白名单（02 附录 B.1 `extract.conflict`；`newer` → NewerOnly）。
pub const CONFLICTS: &[&str] = &["ask", "overwrite", "skip", "rename", "newer"];

/// 全局 CLI（02 §10.3 骨架）。
#[derive(Debug, Parser)]
#[command(name = "hzip", version, about = "hzip 压缩/解压命令行工具（02 §10）")]
pub struct Cli {
    /// JSON 输出：stdout 单文档结果 + stderr NDJSON 进度（02 §10.4）
    #[arg(long, global = true)]
    pub json: bool,
    /// 禁用 ANSI 颜色（NO_COLOR 环境变量亦生效）
    #[arg(long, global = true)]
    pub no_color: bool,
    /// 详细日志（可叠加；0=warn 1=info 2+=debug/trace）
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// 六子命令 + 别名（02 §10.3；c/x/l/p/t/cv）。
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// 压缩（别名 c）：把文件/目录打包为档案
    #[command(alias = "c")]
    Compress {
        /// 输出格式（zip/7z/tar/tar.gz/tar.bz2/tar.xz/tar.zst/gz/bz2/xz/zst）
        #[arg(short, long)]
        format: FormatArg,
        /// 压缩级别（store/fastest/fast/normal/maximum/ultra）
        #[arg(short = 'L', long)]
        level: Option<LevelArg>,
        /// 输出档案路径
        #[arg(short, long)]
        out: PathBuf,
        /// 密码环境变量名（三通道优先级最高；变量缺失 → HZ-5003 退出 2）
        #[arg(long)]
        password_env: Option<String>,
        /// 加密方式（zipcrypto/aes128/aes192/aes256；给 `--password-env` 或
        /// `HZIP_PASSWORD` 即隐式开启，缺省 aes256）
        #[arg(long, value_name = "METHOD")]
        encryption: Option<EncryptionArg>,
        /// 7z 不加密文件名（默认加密文件名 = 头加密 `-mhe=on`，设计稿 11 决策②）
        #[arg(long)]
        no_encrypt_names: bool,
        /// 排除 glob（可多次；`*.tmp` 按段、`sub/*` 剪枝）
        #[arg(long)]
        exclude: Vec<GlobArg>,
        /// 跟随符号链接（默认：链接条目记 target）
        #[arg(long)]
        follow_symlinks: bool,
        /// 分卷大小（二进制单位，如 1m/100k；zip/7z）
        #[arg(long)]
        split: Option<HumanBytes>,
        /// 输入（文件/目录，可多个）
        inputs: Vec<PathBuf>,
    },
    /// 解压（别名 x）：默认按原样目录结构展开到目标（不剥不包）
    #[command(alias = "x")]
    Extract {
        /// 档案路径（- 表示从 stdin 读取）
        archive: PathBuf,
        /// 解压目标目录（默认：档案所在目录；不存在则自动创建）
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// 优雅解压：多顶层/散文件（会污染目标根）时自动以压缩包名建目录
        /// 再展开；单顶层目录/单文件则原样展开。默认关闭 = 恒原样直展。
        #[arg(long)]
        smart: bool,
        /// 手动指定文件名编码（如 gbk/shift_jis）
        #[arg(long)]
        codepage: Option<String>,
        /// 只解压这些条目（可多次；档案内名）
        #[arg(long)]
        select: Vec<EntryNameArg>,
        /// 重名处理策略（ask/overwrite/skip/rename/newer）
        #[arg(long, default_value = "ask")]
        on_conflict: ConflictArg,
        /// 符号链接策略（deny-escape/skip/flatten/deny-all）
        #[arg(long, default_value = "deny-escape")]
        symlink: SymlinkArg,
        /// 密码环境变量名（三通道优先级最高；变量缺失 → HZ-5003 退出 2）
        #[arg(long)]
        password_env: Option<String>,
    },
    /// 列表（别名 l）：列出档案条目
    #[command(alias = "l")]
    List {
        /// 档案路径（- 表示 stdin）
        archive: PathBuf,
        /// 以目录树形式输出（02 §9.10）
        #[arg(long)]
        tree: bool,
        /// 只列这些条目（可多次）
        #[arg(long)]
        select: Vec<EntryNameArg>,
        /// 密码环境变量名（头加密档案列目录需密码；三通道同 x，变量缺失 → HZ-5003）
        #[arg(long)]
        password_env: Option<String>,
    },
    /// 预览条目（别名 p）：stdout 输出条目内容（截断）
    #[command(alias = "p")]
    Preview {
        /// 档案路径（- 表示 stdin）
        archive: PathBuf,
        /// 档案内条目名
        entry: EntryNameArg,
        /// 最大读取字节数（默认 256 KiB 文本 / 8 MiB 图片 / 4 KiB HEX）
        #[arg(long)]
        max_bytes: Option<u64>,
        /// 密码环境变量名（头加密档案/加密条目需密码；三通道同 x，缺失 → HZ-5003）
        #[arg(long)]
        password_env: Option<String>,
    },
    /// 测试完整性（别名 t）：逐条目 CRC 校验
    #[command(alias = "t")]
    Test {
        /// 档案路径（- 表示 stdin）
        archive: PathBuf,
        #[arg(long)]
        password_env: Option<String>,
    },
    /// 格式转换（别名 cv）：如 zip → 7z
    #[command(alias = "cv")]
    Convert {
        /// 源档案路径
        archive: PathBuf,
        /// 目标格式（zip/7z/tar/tar.gz/tar.bz2/tar.xz/tar.zst/gz/bz2/xz/zst）
        #[arg(long)]
        to: FormatArg,
        /// 目标压缩级别
        #[arg(short = 'L', long)]
        level: Option<LevelArg>,
        /// 输出路径（默认：{stem}.{to}）
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

/// 格式值解析器（02 §4.2）。
#[derive(Debug, Clone)]
pub struct FormatArg(pub hzip_format::FormatId);

impl FromStr for FormatArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        if FORMATS.contains(&lower.as_str()) {
            Ok(FormatArg(hzip_format::FormatId::from(lower.as_str())))
        } else {
            Err(format!("格式必须是 {} 之一（HZ-5003）", FORMATS.join("/")))
        }
    }
}

/// 压缩级别值解析器（02 §4.2；未知 → clap 退出 2，HZ-5003 语义）。
///
/// 载体为 `hzip_core::Level`（hzip-core 再导出 hzip-codec 的 Level——CLI 经
/// core 间接获得，02 §2.1 红线 2 合规；hzip-core 在 `lib.rs` 以
/// `pub use hzip_codec::level::Level` 追加再导出，T40 注明）。
#[derive(Debug, Clone)]
pub struct LevelArg(pub hzip_core::Level);

/// 加密方式（`c --encryption`；对应 `hzip_format::EncryptionRequest`）。
#[derive(Debug, Clone)]
pub struct EncryptionArg(pub hzip_format::EncryptionRequest);

impl FromStr for EncryptionArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use hzip_format::{AesKey, EncryptionRequest};
        match s.to_ascii_lowercase().as_str() {
            "zipcrypto" => Ok(Self(EncryptionRequest::ZipCrypto)),
            "aes128" => Ok(Self(EncryptionRequest::Aes(AesKey::Bits128))),
            "aes192" => Ok(Self(EncryptionRequest::Aes(AesKey::Bits192))),
            "aes256" => Ok(Self(EncryptionRequest::Aes(AesKey::Bits256))),
            other => Err(format!(
                "未知加密方式 {other}（可选：zipcrypto/aes128/aes192/aes256）"
            )),
        }
    }
}

impl FromStr for LevelArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        let idx = LEVELS
            .iter()
            .position(|&l| l == lower)
            .ok_or_else(|| format!("级别必须是 {} 之一（HZ-5003）", LEVELS.join("/")))?;
        use hzip_core::Level;
        let level = match idx {
            0 => Level::Store,
            1 => Level::Fastest,
            2 => Level::Fast,
            3 => Level::Normal,
            4 => Level::Maximum,
            _ => Level::Ultra,
        };
        Ok(LevelArg(level))
    }
}

/// 冲突策略值解析器（02 附录 B.1 `extract.conflict`）。
#[derive(Debug, Clone)]
pub struct ConflictArg(pub hzip_format::ConflictPolicy);

impl FromStr for ConflictArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        let policy = match lower.as_str() {
            "ask" => hzip_format::ConflictPolicy::Ask,
            "overwrite" => hzip_format::ConflictPolicy::Overwrite,
            "skip" => hzip_format::ConflictPolicy::Skip,
            "rename" => hzip_format::ConflictPolicy::RenameAuto,
            "newer" => hzip_format::ConflictPolicy::NewerOnly,
            _ => {
                return Err(format!(
                    "冲突策略必须是 {} 之一（HZ-5003）",
                    CONFLICTS.join("/")
                ));
            }
        };
        Ok(ConflictArg(policy))
    }
}

/// 符号链接策略值解析器（02 附录 B.1 `security.symlink`）。
#[derive(Debug, Clone)]
pub struct SymlinkArg(pub hzip_format::SymlinkPolicy);

impl FromStr for SymlinkArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        let policy = match lower.as_str() {
            "deny-escape" => hzip_format::SymlinkPolicy::DenyEscape,
            "deny-all" => hzip_format::SymlinkPolicy::DenyAll,
            "flatten" => hzip_format::SymlinkPolicy::Flatten,
            "skip" => hzip_format::SymlinkPolicy::Skip,
            _ => {
                return Err(
                    "符号链接策略必须是 deny-escape/skip/flatten/deny-all 之一（HZ-5003）".into(),
                );
            }
        };
        Ok(SymlinkArg(policy))
    }
}

/// 人类可读字节数（02 §4.2；`1m`/`500k`/`2g`/`1048576` → 字节；二进制单位 k=KiB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumanBytes(pub u64);

impl FromStr for HumanBytes {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("分卷大小不能为空（HZ-5003）".into());
        }
        let split_at = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (num_part, unit_part) = s.split_at(split_at);
        let num: u64 = num_part
            .parse()
            .map_err(|_| format!("非法大小 `{s}`（HZ-5003）"))?;
        if num == 0 {
            return Err("分卷大小必须大于 0（HZ-5003）".into());
        }
        let mult: u64 = match unit_part.to_ascii_lowercase().as_str() {
            "" => 1,
            "k" => 1024,
            "m" => 1024 * 1024,
            "g" => 1024 * 1024 * 1024,
            "t" => 1024u64 * 1024 * 1024 * 1024,
            _ => return Err(format!("非法单位 `{unit_part}`（HZ-5003）")),
        };
        let bytes = num
            .checked_mul(mult)
            .ok_or_else(|| format!("大小溢出 `{s}`（HZ-5003）"))?;
        Ok(HumanBytes(bytes))
    }
}

/// glob 排除模式（02 §4.2；构造时校验语法，非法 → Err HZ-5003）。
///
/// 载体为 `hzip_core::consts` 再导出的 glob 校验入口（hzip-core 依赖 glob
/// crate——CLI 经 core 间接获得校验能力，02 §2.1 红线外不新增依赖；
/// T40 在 hzip-core `lib.rs`/`consts` 追加 `validate_glob`）。
#[derive(Debug, Clone)]
pub struct GlobArg(pub String);

impl FromStr for GlobArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if hzip_core::consts::validate_glob(s).is_err() {
            return Err(format!("非法 glob `{s}`（HZ-5003）"));
        }
        Ok(GlobArg(s.to_string()))
    }
}

/// 条目名（02 §7.2 语法校验 + 规范化；`--select`/p 子命令）。
#[derive(Debug, Clone)]
pub struct EntryNameArg(pub hzip_format::EntryName);

impl FromStr for EntryNameArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match parse_entry_name(s) {
            Ok(n) => Ok(EntryNameArg(n)),
            Err(e) => Err(format!("非法条目名 `{s}`: {e}（HZ-5003）")),
        }
    }
}

impl EntryNameArg {
    /// 取内部条目名。
    pub fn name(&self) -> &hzip_format::EntryName {
        &self.0
    }
}
