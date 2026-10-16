//! qcow2 的**魔数探针**：`QFI\xfb` + 版本 ∈ {2, 3}。
//!
//! # 为什么这里只需要 8 个字节
//!
//! qcow2 与 `raw` 相反：它有**规范固定的魔数**，且头部字段一律**大端**（规格如此）。
//! 「前 4 字节 = `QFI\xfb` 且紧随的版本号 ∈ {2,3}」已经是强证据——普通文件恰好以
//! 这 8 个字节开头的概率不必纳入考虑，故探针恒 [`Confidence::High`]，
//! 扩展名（`.qcow2`/`.qcow`）只作回退。
//!
//! # ⚠ 大端
//!
//! `QFI\xfb` 之后的 4 字节是**大端** u32。写成小端读会把 v3（落盘 `00 00 00 03`）
//! 读成 `0x0300_0000` 而漏判——这是本模块唯一容易写错的地方，故版本号的读取单独
//! 成 [`be32`]，并把「大端」写进函数名（改坏时看名字就知道错在哪）。

use hzip_format::Confidence;

/// 探针窗口：v3 头的固定字段到 104 字节（含 `compression_type` 到 112）。
///
/// 取 512 是因为它是嗅探层的默认头长——与魔数嗅探**同一次读**，不额外做 IO。
pub const PROBE_HEAD_BYTES: usize = 512;

/// 魔数 `QFI\xfb`（规格里写作整型 `0x5146_49fb`，落盘就是这 4 个字节）。
const QCOW2_MAGIC: [u8; 4] = [0x51, 0x46, 0x49, 0xFB];

/// 内容探针：`Some(High)` = 「这是 qcow2」。
///
/// 只认「魔数对 + 版本 ∈ {2,3}」；其余（含头长不足 8）一律 `None`——不猜。
#[must_use]
pub fn qcow2_probe(head: &[u8]) -> Option<Confidence> {
    if head.len() < 8 || head[..4] != QCOW2_MAGIC {
        return None;
    }
    matches!(be32(&head[4..8]), 2 | 3).then_some(Confidence::High)
}

/// 大端 u32（qcow2 头字段全部大端）。
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
