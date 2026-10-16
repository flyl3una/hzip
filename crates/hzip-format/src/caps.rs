//! 能力矩阵（02 §7.2；契约冻结）。

use std::fmt;

/// 压缩方式（`FormatCaps::methods` 的元素类型；**re-export** 使该公开字段类型
/// 可被调用方命名，无需额外依赖 hzip-codec）。
pub use hzip_codec::traits::CodecMethod;

/// 加密能力（02 §7.2 注释展开）：读取方向 = 可解密方案；写入请求见
/// [`EncryptionRequest`]。`AesHeader` = 7z 头加密（HZ-3102，无法列出条目）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionCaps {
    /// 无加密
    None,
    /// ZipCrypto（传统 zip 2.0 加密）
    ZipCrypto,
    /// WinZip AES（密钥长度可选）
    Aes(AesKey),
    /// 头加密（7z）
    AesHeader,
}

/// AES 密钥长度。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AesKey {
    /// AES-128
    Bits128,
    /// AES-192
    Bits192,
    /// AES-256
    Bits256,
}

/// 写入加密请求（`caps` 不满足 → `create_writer` 返回 `Err Unsupported`）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionRequest {
    /// ZipCrypto
    ZipCrypto,
    /// WinZip AES
    Aes(AesKey),
}

/// 卷类格式的**写回模式**（磁盘镜像 / 裸卷；用户裁定 2026-10-16：默认 `Overlay`，
/// 设置页可切 `InPlace`/只读）。经 [`CreateOptions::volume_write`] 由 core 下发到
/// 格式实现（设备链末端选择见 T-IMG2 §device）；非卷格式忽略本字段。
///
/// [`CreateOptions::volume_write`]: crate::CreateOptions::volume_write
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VolumeWriteMode {
    /// 覆盖层暂存 → 提交时只回放脏块（默认；提交前原文件一字节不动）
    #[default]
    Overlay,
    /// 直接写原文件（快；原子性由格式/文件系统自持）
    InPlace,
    /// 只读（不写任何字节）
    ReadOnly,
}

/// 格式能力矩阵（02 §7.2）。数据类：fmt 实现以常量提供（可扩展字段经
/// `Default` 补全——`Default` 全 false/None，构造侧显式置位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatCaps {
    /// 可读
    pub read: bool,
    /// 可写
    pub write: bool,
    /// 可追加（tar 裸档 / zip）
    pub append: bool,
    /// 可编辑（zip/7z 重写）
    pub edit: bool,
    /// 加密能力
    pub encryption: EncryptionCaps,
    /// 分卷
    pub split: bool,
    /// 固实（solid）
    pub solid: bool,
    /// 档案注释
    pub comment: bool,
    /// 写侧**可选压缩方式**（`&[]` = 该格式无可选方式）。
    ///
    /// - 仅打包格式（tar）恒空；
    /// - 单一编码器的格式（gz/bz2/xz/zst）列**唯一**一项（UI 上表现为锁定值）；
    /// - 多编码器格式（zip/7z）按实装逐项列出。
    ///
    /// 语义为「格式允许的集合」：具体取值还须经 `CreateOptions::method` 传入，
    /// 越集即 `Unsupported`（校验点见 `hzip-core::services::compress::writer_for`）。
    pub methods: &'static [CodecMethod],
    /// 写侧**可设置的加密方案**（`&[]` = 该格式写入不可加密）。
    ///
    /// 与 `encryption`（读能力，单值）分离：zip 可读 ZipCrypto+AES 双方案、
    /// 亦可写双方案，单值无法同时表达。UI「加密方式」候选与
    /// `enc_writable` 门控以本字段为唯一来源（设计稿 11 §3.1 B）。
    /// 传入 `CreateOptions::encryption` 不在集合内 → `Unsupported`（校验点在
    /// 各 fmt `create_writer`）。
    pub write_encryption: &'static [EncryptionRequest],
}

impl Default for FormatCaps {
    fn default() -> Self {
        Self {
            read: false,
            write: false,
            append: false,
            edit: false,
            encryption: EncryptionCaps::None,
            split: false,
            solid: false,
            comment: false,
            methods: &[],
            write_encryption: &[],
        }
    }
}

impl fmt::Display for FormatCaps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "read={} write={} append={} edit={}",
            self.read, self.write, self.append, self.edit
        )
    }
}
