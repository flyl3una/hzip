//! GPT 分区表夹具（保护性 MBR + 主头 + 条目数组 + 备份头；CRC32 现场算）。
//!
//! 手写而不是录字节的两条理由：
//!
//! 1. **CRC32 必须真算**——分区探测会校验主头 CRC 与条目数组 CRC，抄一份常量
//!    在磁盘几何变化（`total_bytes` 不同）时就自相矛盾；
//! 2. **四段结构要自洽**：保护性 MBR ↔ 主头 ↔ 条目数组 ↔ 备份头，字段互相回指
//!    （`backup_lba`、`last_usable_lba`、备份头自己的 `current_lba`），手写能保证一致。
//!
//! 不引第三方 crc 依赖：IEEE 反射型 CRC32（多项式 `0xEDB88320`）十行足够。

/// 逻辑扇区字节数（GPT 的 LBA 单位）。
pub const GPT_SECTOR: u64 = 512;
/// GPT 条目数组容量（规范建议值，也是我们写的值）。
pub const GPT_ENTRIES: usize = 128;
/// 单个分区块字节数（含 72 B UTF-16LE 名字）。
pub const GPT_ENTRY_BYTES: usize = 128;
/// 条目数组占用扇区数（16 KiB / 512）。
pub const GPT_ENTRIES_SECTORS: u64 = (GPT_ENTRIES as u64 * GPT_ENTRY_BYTES as u64) / GPT_SECTOR;

/// 分区类型（只列测试要用的两种；GUID 是**混合端序**，与 RFC 4122 文本序不同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionType {
    /// EFI 系统分区（ESP）。
    EfiSystem,
    /// Linux 文件系统。
    LinuxFilesystem,
}

impl PartitionType {
    /// GPT 类型 GUID 的磁盘字节序（前三段小端）。
    #[must_use]
    pub fn guid(self) -> [u8; 16] {
        match self {
            // C12A7328-F81F-11D2-BA4B-00A0C93EC93B
            Self::EfiSystem => [
                0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
                0xC9, 0x3B,
            ],
            // 0FC63DAF-8483-4772-8E79-3D69D8477DE4
            Self::LinuxFilesystem => [
                0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47,
                0x7D, 0xE4,
            ],
        }
    }
}

/// 一个 GPT 分区项。
#[derive(Debug, Clone)]
pub struct GptPartition {
    /// 类型 GUID（语义）。
    pub ty: PartitionType,
    /// 首扇区（LBA，绝对）。
    pub start_lba: u64,
    /// 扇区数。
    pub len_lba: u64,
    /// 名字（UTF-16LE 落盘，超 36 个码元截断）。
    pub name: &'static str,
}

/// 造一整块**带 GPT 的盘**：长度 `total_bytes`，含保护性 MBR、主头（CRC 正确）、
/// 条目数组、盘尾备份条目 + 备份头。
///
/// 盘太小时**直接 panic**：GPT 的最小可用布局是 `34 + 33` 个扇区，装不下就没有
/// 「退化成 MBR」这种宽容语义（夹具要能代表真实 GPT 盘，不能悄悄降级）。
#[must_use]
pub fn gpt_only(parts: &[GptPartition], total_bytes: u64) -> Vec<u8> {
    let total_lba = total_bytes / GPT_SECTOR;
    assert!(
        total_lba >= 34 + 1 + GPT_ENTRIES_SECTORS + 1 + parts.len() as u64,
        "盘太小，装不下 GPT（{total_lba} 扇区）"
    );
    let entries_lba = 2u64;
    let first_usable = entries_lba + GPT_ENTRIES_SECTORS;
    let backup_lba = total_lba - 1;
    let backup_entries_lba = backup_lba - GPT_ENTRIES_SECTORS;
    let last_usable = backup_entries_lba - 1;

    // 保护性 MBR：单条 0xEE 覆盖整盘（扇区数超过 u32 时按规范封顶）。
    let protected = (total_lba - 1).min(u64::from(u32::MAX)) as u32;
    let mut disk = super::mbr_only(&[(0xEE, 1, protected)], total_bytes);

    // —— 条目数组（主 + 备份，两份内容相同）——
    let mut entries = vec![0u8; GPT_ENTRIES * GPT_ENTRY_BYTES];
    for (i, p) in parts.iter().enumerate() {
        let base = i * GPT_ENTRY_BYTES;
        assert!(
            p.len_lba > 0
                && p.start_lba >= first_usable
                && p.start_lba + p.len_lba - 1 <= last_usable,
            "分区 {i} 越界：LBA {}..{} 不在可用区 {}..{} 内",
            p.start_lba,
            p.start_lba + p.len_lba,
            first_usable,
            last_usable
        );
        entries[base..base + 16].copy_from_slice(&p.ty.guid());
        // 唯一 GUID：按序号造固定值（夹具确定性；不取随机数）
        let mut uniq = [0u8; 16];
        uniq[..8].copy_from_slice(&0x1122_3344_5566_7788u64.to_be_bytes());
        uniq[8..10].copy_from_slice(&(0x9900_u16 + i as u16).to_be_bytes());
        entries[base + 16..base + 32].copy_from_slice(&uniq);
        entries[base + 32..base + 40].copy_from_slice(&p.start_lba.to_le_bytes());
        entries[base + 40..base + 48].copy_from_slice(&(p.start_lba + p.len_lba - 1).to_le_bytes());
        // attributes 8 B 留零
        let mut name_utf16 = Vec::new();
        for u in p.name.encode_utf16().take(36) {
            name_utf16.extend_from_slice(&u.to_le_bytes());
        }
        entries[base + 56..base + 56 + name_utf16.len()].copy_from_slice(&name_utf16);
    }
    let entries_crc = crc32(&entries);

    let enc = GPT_SECTOR as usize;
    disk[entries_lba as usize * enc..(entries_lba + GPT_ENTRIES_SECTORS) as usize * enc]
        .copy_from_slice(&entries);
    disk[backup_entries_lba as usize * enc
        ..(backup_entries_lba + GPT_ENTRIES_SECTORS) as usize * enc]
        .copy_from_slice(&entries);

    // —— 两个头：字段互为镜像，各自 CRC 独立 ——
    let disk_guid = [
        0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
        0x0C,
    ];
    let primary = gpt_header(
        1,
        backup_lba,
        first_usable,
        last_usable,
        &disk_guid,
        entries_lba,
        entries_crc,
    );
    let backup = gpt_header(
        backup_lba,
        1,
        first_usable,
        last_usable,
        &disk_guid,
        backup_entries_lba,
        entries_crc,
    );
    disk[GPT_SECTOR as usize..GPT_SECTOR as usize + primary.len()].copy_from_slice(&primary);
    disk[backup_lba as usize * enc..backup_lba as usize * enc + backup.len()]
        .copy_from_slice(&backup);
    disk
}

/// 造一个 92 B 的 GPT 头（含 CRC32）。
fn gpt_header(
    current_lba: u64,
    other_lba: u64,
    first_usable: u64,
    last_usable: u64,
    disk_guid: &[u8; 16],
    entries_lba: u64,
    entries_crc: u32,
) -> [u8; 92] {
    let mut h = [0u8; 92];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // revision 1.0
    h[12..16].copy_from_slice(&92u32.to_le_bytes()); // header_size
    // 16..20 = header CRC（先留零，最后回填）
    h[24..32].copy_from_slice(&current_lba.to_le_bytes());
    h[32..40].copy_from_slice(&other_lba.to_le_bytes());
    h[40..48].copy_from_slice(&first_usable.to_le_bytes());
    h[48..56].copy_from_slice(&last_usable.to_le_bytes());
    h[56..72].copy_from_slice(disk_guid);
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&(GPT_ENTRIES as u32).to_le_bytes());
    h[84..88].copy_from_slice(&(GPT_ENTRY_BYTES as u32).to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let crc = crc32(&h);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// IEEE 反射型 CRC32（多项式 `0xEDB88320`；位运算实现，无查表、无依赖）。
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let lsb = crc & 1;
            crc >>= 1;
            if lsb != 0 {
                crc ^= 0xEDB8_8320;
            }
        }
    }
    !crc
}
