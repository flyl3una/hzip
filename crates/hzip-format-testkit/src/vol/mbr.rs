//! MBR 分区表夹具（字节级手写，512 B）。
//!
//! 只写**读侧真正会看**的字段：表项的 `type` / 起始 LBA / 扇区数 + 末尾 `55 AA`。
//! 起止 CHS 一律填 `FE FF FF`（「见 LBA 字段」，LBA 寻址的常规写法）——夹具不构造
//! CHS 语义，分区探测也不看它。

/// MBR 只有四个主分区项。
pub const MBR_MAX_ENTRIES: usize = 4;

/// 引导签名（`55 AA`）在 MBR 内的偏移。
pub const MBR_SIGNATURE_OFFSET: usize = 510;

/// 造 512 B MBR：`(类型字节, 起始 LBA, 扇区数)` 落四个表项（不足补零），末尾写 `55 AA`。
///
/// 超过四项**直接 panic**：夹具宁可炸测试，也不静默丢掉调用方想造的分区
/// （静默丢项会让「分区数」这个断言值失真，那是最难查的一类夹具缺陷）。
#[must_use]
pub fn mbr(entries: &[(u8, u32, u32)]) -> [u8; 512] {
    assert!(
        entries.len() <= MBR_MAX_ENTRIES,
        "MBR 只有四个主分区项，给了 {}",
        entries.len()
    );
    let mut out = [0u8; 512];
    for (i, (ty, start_lba, sectors)) in entries.iter().enumerate() {
        let e = 446 + i * 16;
        out[e] = 0x00; // 引导标志：非活动分区
        out[e + 1..e + 4].copy_from_slice(&[0xFE, 0xFF, 0xFF]); // 起始 CHS：见 LBA
        out[e + 4] = *ty;
        out[e + 5..e + 8].copy_from_slice(&[0xFE, 0xFF, 0xFF]); // 结束 CHS：见 LBA
        out[e + 8..e + 12].copy_from_slice(&start_lba.to_le_bytes());
        out[e + 12..e + 16].copy_from_slice(&sectors.to_le_bytes());
    }
    out[MBR_SIGNATURE_OFFSET..].copy_from_slice(&[0x55, 0xAA]);
    out
}

/// 整盘字节：长度 `total_bytes`，前 512 B 是 [`mbr`]，其余全零（只测分区层时用）。
#[must_use]
pub fn mbr_only(entries: &[(u8, u32, u32)], total_bytes: u64) -> Vec<u8> {
    let mut disk = vec![0u8; usize::try_from(total_bytes).expect("整盘长度超出 usize")];
    disk[..512].copy_from_slice(&mbr(entries));
    disk
}
