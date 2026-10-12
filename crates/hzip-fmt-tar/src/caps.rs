//! tar handler 能力矩阵与签名（02 §8.4 / §7.1；T22 §4.1 `caps` 模块）。
//!
//! caps 声明与 02 §8.4 逐字对照：
//!
//! | 02 §8.4 原文 | [`FormatCaps`] 字段 |
//! |---|---|
//! | `write=true` | `write: true` |
//! | `append=仅未压缩 tar` | `append: true`（实际追加仅在裸 tar File 源成立，见 [`crate::TarHandler`] rustdoc / §3.4 边界） |
//! | `edit=false` | `edit: false`（`open_editor` 恒 `Err Unsupported` HZ-5001） |
//! | 三变体全兼容读取 | `read: true` |
//!
//! 派生字段：`encryption=None`、`split=false`、`solid=false`、`comment=false`
//! （tar 无加密/无压缩/不分卷，02 §8.4 末句与 T22 §1）。

use hzip_format::Signature;
use hzip_format::caps::{EncryptionCaps, FormatCaps};

/// tar 格式 ID（02 §7.2）。
pub const FORMAT_ID: &str = "tar";

/// 签名：魔数为空串 + `offset=257`（02 §7.4 嗅探流程 `offset 257 = ustar?` 分支，
/// T22 §7-6：tar 的 `Signature` 只报 `ustar`@257——压缩组合的嗅探/装配归 registry）。
pub static SIGNATURE: Signature = Signature {
    magic: b"ustar",
    offset: 257,
    display: "tar",
    extensions: &["tar"],
};

/// 能力矩阵常量（02 §8.4 逐字对照，见模块文档）。
pub(crate) fn caps() -> FormatCaps {
    FormatCaps {
        read: true,
        write: true,
        append: true,
        edit: false,
        encryption: EncryptionCaps::None,
        split: false,
        solid: false,
        comment: false,
        // tar 是纯打包容器：无压缩层，故无可选压缩方式（02 §8.4「无压缩」）
        methods: &[],
        // 打包容器：格式无加密概念，写入不可加密
        write_encryption: &[],
    }
}
