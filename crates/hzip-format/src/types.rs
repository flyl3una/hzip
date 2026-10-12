//! 数据类型层（02 §7.2；契约冻结）。
//!
//! `EntryName`/`FormatId` 承载于 hzip-error（T01 §7 约定），本 crate re-export
//! 并提供唯一校验构造入口 [`parse_entry_name`]（02 §7.2 语法校验 + 规范化，
//! 净化决策仍在 core 安全模块，02 §9.6）。

use std::path::PathBuf;

use hzip_codec::traits::CodecId;
use hzip_error::HzipError;
use time::OffsetDateTime;

pub use hzip_error::{EntryName, FormatId};

/// 条目 ID（entries() 结果的下标，0 起连续）。
pub type EntryId = usize;

/// 条目名编码状态（02 §7.2）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameEncoding {
    /// 明确 UTF-8（zip 的 UTF-8 flag 置位等）
    Utf8,
    /// 可疑非 UTF-8（携带检测出的编码；T45 批量修复后置为确定值）
    Suspect(Encoding),
    /// 未知（无法判定）
    Unknown,
}

/// 文件名编码集合（02 §8.1 chardetng 检测集合 + 手动指定）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// GBK / GB18030
    Gbk,
    /// Big5
    Big5,
    /// Shift_JIS
    ShiftJis,
    /// EUC-KR
    EucKr,
    /// Windows-1252
    Cp1252,
    /// UTF-8
    Utf8,
}

impl Encoding {
    /// chardetng 检测出的编码名 → [`Encoding`]（未知返回 None）。
    pub fn from_charset(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "gbk" | "gb18030" | "gb2312" | "cp936" => Some(Self::Gbk),
            "big5" | "big5-hkscs" | "cp950" => Some(Self::Big5),
            "shift_jis" | "sjis" | "cp932" => Some(Self::ShiftJis),
            "euc-kr" | "cp949" => Some(Self::EucKr),
            "windows-1252" | "cp1252" => Some(Self::Cp1252),
            "utf-8" | "utf8" => Some(Self::Utf8),
            _ => None,
        }
    }
}

/// 属性位（02 §5.3.2 映射表内部模型；档案内 mode + attrs 双字段，
/// 解压按目标平台语义恢复）。数据类：外部 crate（fmt 实现）需构造——
/// 不标 `#[non_exhaustive]`（扩展经 `..Default` 语义 + 公开字段）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryAttrs {
    /// 只读
    pub read_only: bool,
    /// 隐藏
    pub hidden: bool,
    /// 系统（仅 Windows 语义）
    pub system: bool,
    /// 可执行（None = 未知；Windows 按扩展名推断语义归 core）
    pub executable: Option<bool>,
}

/// 条目信息（02 §7.2 `entries()` 返回）。数据类：fmt 实现构造（可扩展
/// 字段经 `..Default`——字段全部公开，`Default` 语义在构造侧补全）。
#[derive(Debug, Clone)]
pub struct EntryInfo {
    /// 条目 ID（0 起连续）
    pub id: EntryId,
    /// 条目名（已规范化）
    pub name: EntryName,
    /// 是否目录
    pub is_dir: bool,
    /// 是否符号链接
    pub is_symlink: bool,
    /// 符号链接目标（is_symlink 时非空）
    pub symlink_target: Option<PathBuf>,
    /// 解压后原始字节数
    pub size_raw: u64,
    /// 打包字节数（档案内占用）
    pub size_packed: u64,
    /// 修改时间（格式无该域时为 None）
    pub mtime: Option<OffsetDateTime>,
    /// unix 权限位
    pub mode: Option<u32>,
    /// 平台属性位
    pub attributes: EntryAttrs,
    /// 是否加密
    pub encrypted: bool,
    /// 头部加密（无法列出条目）
    pub header_encrypted: bool,
    /// CRC32（格式提供时）
    pub crc32: Option<u32>,
    /// 压缩编码（02 §6.3）
    pub codec: Option<CodecId>,
    /// 文件名编码状态
    pub name_encoding: NameEncoding,
    /// 加密算法（浏览「加密算法」列；`None` = 未加密或格式未声明算法）。
    ///
    /// 仅当 [`EntryInfo::encrypted`] 为真时非 None；取值约定（永不本地化，
    /// 展示层查 `col.enc_*` 文案）：`ZipCrypto` / `AES-128` / `AES-192` /
    /// `AES-256` / `AES`（7z/rar 头不细分密钥长度时）。
    pub encryption: Option<&'static str>,
    /// 压缩级别数值（浏览「压缩级别」列；`None` = 格式不存储级别——
    /// zip 工具普遍不在头里写 deflate 级别，如实显示未知而非猜测）。
    /// zip64 存 1~9（zip crate `compression_level`）；7z LZMA/LZMA2 的级别
    /// 编码在 coder properties 里但 sevenz-rust 0.6.1 不导出 → None。
    pub compression_level: Option<u8>,
}

/// 写入方向的单条目元数据（02 §7.3 `next_entry` 入参）。数据类：fmt 实现构造，
/// 不标 `#[non_exhaustive]`。
#[derive(Debug, Clone)]
pub struct EntryMeta {
    /// 条目名（已规范化）
    pub name: EntryName,
    /// 是否目录
    pub is_dir: bool,
    /// 是否符号链接
    pub is_symlink: bool,
    /// 符号链接目标
    pub symlink_target: Option<PathBuf>,
    /// 原始字节数（已知则预声明，tar/zip 头需要；None = 流式未知）
    pub size_raw: Option<u64>,
    /// 修改时间
    pub mtime: Option<OffsetDateTime>,
    /// unix 权限位
    pub mode: Option<u32>,
    /// 平台属性位
    pub attrs: EntryAttrs,
    /// 请求加密（caps 不支持 → `next_entry` 返回 Unsupported）
    pub encrypted: bool,
}

impl Default for EntryMeta {
    fn default() -> Self {
        Self {
            name: EntryName::new(""),
            is_dir: false,
            is_symlink: false,
            symlink_target: None,
            size_raw: None,
            mtime: None,
            mode: None,
            attrs: EntryAttrs::default(),
            encrypted: false,
        }
    }
}

/// 语法校验 + 规范化构造（02 §7.2；规则表见 crate 文档 §3.4）。
///
/// fmt 实现解码原始名后**必须**经由此函数（除 §7-3 注明的 `\`→`/` 预转换外，
/// 不自行改名义）。
///
/// # 规则
///
/// - N1 非空；N2 不含 `\`；N3 不含 NUL 与 C0/C1 控制字符 → `Err Format{BadHeader}`；
/// - N4 无前导 `./` → 构造时剥除；
/// - N5 无空段（`//`、尾 `/`）→ 空段丢弃（目录性由 `is_dir` 表达）；
/// - N6 前导 `/`、盘符、`..` 段、Windows 保留名、段尾空格/句点 → **放行**
///   （净化决策归 `core::security`，02 §9.6）。
pub fn parse_entry_name(raw: &str) -> Result<EntryName, HzipError> {
    use hzip_error::FormatErrorKind;
    use hzip_error::HzipError;

    let bad = || {
        Err(HzipError::Format {
            format: FormatId::from("?"),
            kind: FormatErrorKind::BadHeader,
        })
    };
    if raw.is_empty() {
        return bad(); // N1
    }
    if raw.contains('\\') {
        return bad(); // N2（zip 侧在调用前做 \→/ 预转换）
    }
    // N3：NUL 与 C0/C1 控制字符
    for ch in raw.chars() {
        let c = ch as u32;
        if ch == '\0' || (c <= 0x1F) || c == 0x7F || (0x80..=0x9F).contains(&c) {
            return bad();
        }
    }
    // N4/N5：按 '/' 切段，剥空段与前导 '.' 段；空段丢弃
    let mut kept: Vec<&str> = Vec::new();
    for seg in raw.split('/') {
        if seg.is_empty() {
            continue; // N5：空段丢弃
        }
        if seg == "." {
            continue; // N4：前导 ./ 剥除（任意位置的 . 段同样丢弃）
        }
        kept.push(seg);
    }
    if kept.is_empty() {
        // "//"、"a/"、"…/./" 之类剥净 → 空名非法
        return bad();
    }
    Ok(EntryName::new(kept.join("/")))
}

/// 归档根的**自我描述条目**判定（`"."` / `"./"` / `"././"`）。
///
/// 标准工具会写出这样一个条目表示归档根：`find . | cpio -o`（cpio）与
/// `tar cf x.tar .`（tar）都产出它。
///
/// 它**不承载任何条目语义**（不代表任何文件），且规范化后是空名——
/// [`parse_entry_name`] 按 N1/N5 必然拒绝（`"//"`、`"a/"` 之类剥净同理）。
/// 容器 reader 因此必须在解析名字**之前**用它判定并跳过，否则「用户拿标准工具
/// 产出的档案」会被判 2004 打不开（cpio 与 tar 两侧都踩过这个坑）。
///
/// 返回 `true` 当且仅当按 `/` 切分后**每一段都为空或恰为 `.`**，且至少有一段是
/// `.`。**不**命中 `".."`（那是真实条目名，`parse_entry_name` 按 N6 放行，
/// 净化决策归 `core::security`，02 §9.6）、也不命中 `".hidden"`。
pub fn is_root_entry_name(raw: &[u8]) -> bool {
    let mut saw_dot = false;
    for seg in raw.split(|&b| b == b'/') {
        if seg.is_empty() {
            continue;
        }
        if seg == b"." {
            saw_dot = true;
        } else {
            return false;
        }
    }
    saw_dot
}

#[cfg(test)]
#[path = "tests/types.rs"]
mod tests;
