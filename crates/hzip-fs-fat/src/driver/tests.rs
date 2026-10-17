//! `driver` 模块的单元用例（T-STD 规则②：业务文件里不写内联 `mod tests` 体）。

use super::is_fat_boot_sector;

#[test]
fn short_or_unlabeled_head_is_not_fat() {
    assert!(!is_fat_boot_sector(&[]));
    assert!(!is_fat_boot_sector(&[0u8; 511]));
    let mut head = vec![0u8; 512];
    head[510] = 0x55;
    head[511] = 0xAA;
    assert!(
        !is_fat_boot_sector(&head),
        "无 BPB 字段的裸 0x55AA 不算 FAT"
    );
}

#[test]
fn exfat_and_ntfs_are_refused() {
    let mut head = vec![0u8; 512];
    head[3..11].copy_from_slice(b"EXFAT   ");
    head[510] = 0x55;
    head[511] = 0xAA;
    assert!(!is_fat_boot_sector(&head));
    head[3..11].copy_from_slice(b"NTFS    ");
    assert!(!is_fat_boot_sector(&head));
}
