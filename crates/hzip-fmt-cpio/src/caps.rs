//! cpio handler 能力矩阵与签名（02 §8.7）。
//!
//! # caps 声明
//!
//! | 操作 | caps | 行为 |
//! |---|---|---|
//! | read | `true` | newc / crc / odc / bin 四变体全兼容 |
//! | write | `false` | [`CpioHandler::create_writer`] 恒 `Err Unsupported` HZ-5001 |
//! | append | `false` | 同上（cpio 无追加语义） |
//! | edit | `false` | [`CpioHandler::open_editor`] 恒 `Err Unsupported` HZ-5001 |
//!
//! 读能力的定位与 rar 同档：cpio 是**只读引入格式**——现实世界的用例是
//! 解包 Linux initramfs / rpm / 各类发行版镜像（`newc`），而 hzip 自身不产出
//! cpio（写侧无需求）。派生字段：`encryption=None`、`split=false`、`solid=false`、
//! `comment=false`、`methods=[]`（纯打包容器，无压缩层）。
//!
//! 组合形态（`.cpio.gz` 等）**不在此声明**：压缩层归 registry 组合装配
//! （02 §7.5），本 handler 只认裸 cpio 字节流。

use hzip_format::Signature;
use hzip_format::caps::{EncryptionCaps, FormatCaps};

/// cpio 格式 ID（写侧 [`FormatId`](hzip_error::FormatId) 字符串）。
pub const FORMAT_ID: &str = "cpio";

/// 签名：newc 魔数在偏移 0（odc/crc/bin 由嗅探层按变体细分，02 §7.4）。
pub static SIGNATURE: Signature = Signature {
    magic: b"070701",
    offset: 0,
    display: "cpio",
    extensions: &["cpio"],
};

/// 能力矩阵常量（见模块文档）。
pub(crate) fn caps() -> FormatCaps {
    FormatCaps {
        read: true,
        write: false,
        append: false,
        edit: false,
        encryption: EncryptionCaps::None,
        split: false,
        solid: false,
        comment: false,
        // 纯打包容器：无压缩层，故无可选压缩方式
        methods: &[],
        // 无加密概念
        write_encryption: &[],
    }
}

#[cfg(test)]
#[path = "tests/caps.rs"]
mod tests;
