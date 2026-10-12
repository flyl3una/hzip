//! 格式 handler 契约（02 §7.1/§7.3；契约冻结）。
//!
//! `FormatHandler` 是格式层的统一入口：嗅探 → `open_reader`/`create_writer`/
//! `open_editor` 返回 trait 对象。core 只操作 trait 对象，不再感知具体格式
//! （02 §7.6 注册表时序）。

use std::io::{Read, Write};
use std::path::PathBuf;

use hzip_error::HzipError;

use crate::caps::{EncryptionRequest, FormatCaps};
use crate::editor::ArchiveEditor;
use crate::reader::ArchiveReader;
use crate::types::EntryName;
use crate::types::{Encoding, FormatId};
use crate::writer::ArchiveWriter;

/// 格式签名描述（02 §7.1）。`&'static` 由各 handler 以常量提供，registry 嗅探用。
/// 数据类。
#[derive(Debug, Clone, Copy)]
pub struct Signature {
    /// 魔数（tar 等偏移型可为空，用 `offset` 定位）
    pub magic: &'static [u8],
    /// 魔数偏移（tar: 257）
    pub offset: usize,
    /// 人读格式名
    pub display: &'static str,
    /// 扩展名回退表（`.zipx` 等，02 §7.4）
    pub extensions: &'static [&'static str],
}

/// 输入源（02 §7.4：文件/stdin/卷集合；组合装配内层为已解压流，02 §7.5）。
pub enum Source {
    /// 单文件
    File(PathBuf),
    /// stdin（经 512 B 缓冲代理，保留嗅探已读字节）
    Stdin,
    /// 分卷（首卷在前；缺失由 fmt 层报 2005）
    Volumes {
        /// 首卷
        first: PathBuf,
        /// 其余卷
        rest: Vec<PathBuf>,
    },
    /// registry 组合装配专用：内层已解压流
    Stream(Box<dyn Read + Send>),
}

impl Source {
    /// 供嗅探/错误诊断的显示名。
    pub fn display(&self) -> String {
        match self {
            Self::File(p) => p.display().to_string(),
            Self::Stdin => "<stdin>".into(),
            Self::Volumes { first, .. } => first.display().to_string(),
            Self::Stream(_) => "<stream>".into(),
        }
    }
}

/// 输出端（02 §8.2/§9.5：文件/stdout/分卷；组合写入内层为压缩包装流，02 §7.5）。
pub enum Sink {
    /// 单文件
    File(PathBuf),
    /// stdout（管道输出；7z 头回写受限，02 §8.2）
    Stdout,
    /// 分卷（字节切分，02 §8.1）
    Split {
        /// 卷前缀
        prefix: PathBuf,
        /// 每卷字节数
        bytes_per_volume: u64,
    },
    /// registry 组合写入专用
    Stream(Box<dyn Write + Send>),
}

/// 打开选项（读取方向）。数据类。
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// 手动指定编码（优先于自动检测，02 §7.3）
    pub codepage: Option<Encoding>,
    /// stdin 单文件流条目名覆盖（CLI `--entry-name`，02 §8.5）
    pub entry_name: Option<EntryName>,
    /// 头加密档案的打开密码（设计稿 11 §3.1 A）：头加密档案
    /// `entries()` 在打开期即需密码解头，否则 HZ-3102。由调用方
    /// （core 打开重试 / CLI `l`/`p` 通道 / GUI 密码门）提供；
    /// 未加密档案忽略本字段。条目级密码仍走 `PasswordRef`/`PasswordSource`。
    pub password: Option<zeroize::Zeroizing<String>>,
    /// **组合档延迟展开**（用户口径 2026-09-24）：`true` 时打开 `.cpio.gz`/`.tar.gz`
    /// 这类「编码层 + 容器尾」档案**只嗅探**，列表里先给一行内层容器
    /// （`init.cpio`），双击那一行才装配解码链、落临时缓冲并索引
    /// （`hzip_core::services::browse::Browser::enter_deferred`）。
    ///
    /// 交互式界面（GUI）置 `true`——打开因此从 10+ 秒变毫秒级，且不再一打开就
    /// 占下与内层容器等量的临时盘；命令行（`l`/`p`/`t` 等一次性取数）保持
    /// `false`（无「双击」语义，直接给最终结构）。
    pub defer_composite: bool,
}

/// 创建选项（写入方向）。数据类。
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// 统一压缩级别（02 §6.3）
    pub level: hzip_codec::level::Level,
    /// 压缩方式（`None` = 沿用格式默认编码器）。
    ///
    /// 须 ∈ [`FormatCaps::methods`]；越集由调用方（core）以 `Unsupported`（HZ-5001）
    /// 拒绝，各 fmt 的 `create_writer` 只需处理"已被允许的方式"。
    pub method: Option<hzip_codec::traits::CodecMethod>,
    /// 线程数
    pub threads: u32,
    /// LZMA2 **字典大小覆写**（`None` = 随级别派生；仅 7z/LZMA2 消费）。
    ///
    /// 越界（< 4096 / > 256 MiB，编码器合法域）由消费方钳制——字典大小是
    /// 编码器参数而非格式约束，故不在此层拒绝。
    pub dict_size: Option<u64>,
    /// 加密请求（`caps.write_encryption` 不支持 → Unsupported）
    pub encryption: Option<EncryptionRequest>,
    /// 「加密文件名」（仅 7z = 头加密 `-mhe=on`；设计稿 11 §3.1 C）。
    /// `None` = 格式默认（7z 默认 `true`——安全默认，文件名不泄露）；
    /// 其余格式忽略本字段。
    pub encrypt_names: Option<bool>,
    /// 固实（`caps.solid` 才有效，7z）
    pub solid: bool,
    /// 档案注释（`caps.comment` 才有效）
    pub comment: Option<String>,
    /// 追加模式（`caps.append=false` → `Err Unsupported` HZ-5001，TC-CONF-005）
    pub append: bool,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            level: hzip_codec::level::Level::Normal,
            method: None,
            threads: 0,
            dict_size: None,
            encryption: None,
            encrypt_names: None,
            solid: false,
            comment: None,
            append: false,
        }
    }
}

/// 格式处理器契约（02 §7.3 冻结签名）。实现须 `Send + Sync`。
pub trait FormatHandler: Send + Sync {
    /// 格式 ID（"zip"/"7z"/"tar"/…）
    fn id(&self) -> FormatId;
    /// 签名描述（嗅探表用）
    fn signature(&self) -> &'static Signature;
    /// 能力矩阵
    fn caps(&self) -> FormatCaps;
    /// 打开读取器
    fn open_reader(
        &self,
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveReader>, HzipError>;
    /// 创建写入器
    fn create_writer(
        &self,
        dst: Sink,
        opts: &CreateOptions,
    ) -> Result<Box<dyn ArchiveWriter>, HzipError>;
    /// 打开编辑器（`caps.edit` 才实现；否则恒 `Err Unsupported`）
    fn open_editor(
        &self,
        src: Source,
        opts: &OpenOptions,
    ) -> Result<Box<dyn ArchiveEditor>, HzipError>;
}
