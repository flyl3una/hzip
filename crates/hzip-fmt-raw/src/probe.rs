//! `raw` 镜像的**内容探针**（raw 没有魔数，靠结构证据认领）。
//!
//! # 为什么需要探针
//!
//! `raw`/`img`/`dd` 只是一段字节：没有任何头部标识。判定它「像不像磁盘镜像」
//! 只能看**内容结构**，而这件事必须做得**保守**——把普通文件误判成镜像，用户会看到
//! 一个打不开的「卷」；把镜像漏判，用户看到的是「未知格式」（可退回用扩展名打开）。
//! 故本模块只认三种强证据，认不出就 `None`，交给扩展名回退（`Confidence::Low`）。
//!
//! # 三种证据（顺序即优先级）
//!
//! | 证据 | 位置 | 说明 |
//! |---|---|---|
//! | MBR 签名 `55 AA` + 表项合理 | 偏移 510 | 至少一条非空表项（type/起始/长度都非 0） |
//! | GPT 头签名 `EFI PART` | 偏移 512 | 规范字符串 |
//! | 裸文件系统签名 | 偏移 0 或 1080 | 复用 [`hzip_vol::partition::fs_kind_in_head`]（单一事实源） |
//!
//! 文件系统魔数的偏移口径（尤其 ext4 的 1080 而非 1024）由 `hzip-vol` 那一份负责，
//! 本模块**不重复**写第二遍——魔数抄两份是本仓库明确踩过的坑。

use hzip_format::Confidence;

/// 探针窗口（GPT 头在 512、ext4 超级块到 1080 ⇒ 2048 足够且不浪费）。
pub const PROBE_HEAD_BYTES: usize = 2048;

/// 内容探针：`Some(置信度)` = 「像磁盘镜像」。
#[must_use]
pub fn raw_probe(head: &[u8]) -> Option<Confidence> {
    if head.len() < 512 {
        return None; // 头都不够：没证据（不猜）
    }
    if head[510..512] == [0x55, 0xAA] && mbr_entries_plausible(&head[446..510]) {
        return Some(Confidence::High);
    }
    if head.len() >= 1024 && &head[512..520] == b"EFI PART" {
        return Some(Confidence::High);
    }
    if hzip_vol::partition::fs_kind_in_head(head).is_some() {
        return Some(Confidence::High);
    }
    None
}

/// MBR 表项是否「像回事」：至少一条 type ≠ 0 且起始扇区/扇区数都非 0。
///
/// 不校验是否越界（探针只看头部，不知道盘长）；`55 AA` + 一条像样的表项已足够把
/// 「这是磁盘镜像」与「普通文件恰好以 55 AA 结尾」区分开。
fn mbr_entries_plausible(table: &[u8]) -> bool {
    table.chunks_exact(16).any(|e| {
        let ty = e[4];
        let start = u32::from_le_bytes([e[8], e[9], e[10], e[11]]);
        let len = u32::from_le_bytes([e[12], e[13], e[14], e[15]]);
        ty != 0 && start != 0 && len != 0
    })
}
