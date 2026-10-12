//! CPIO 格式编译期常量（02 §8.7）。
//!
//! 四种变体的头长/对齐/魔数都在这里定义，[`crate::variant`] 与
//! [`crate::reader`] 只引用常量，不写魔法数字。

/// newc（SVR4，`070701`）与 crc（`070702`）头字节数：6 魔数 + 13×8 十六进制域。
pub const NEWC_HEADER_BYTES: usize = 110;

/// odc（旧可移植 ASCII，`070707`）头字节数：6 魔数 + 7×6 + 11 + 6 + 11 八进制域。
pub const ODC_HEADER_BYTES: usize = 76;

/// bin（旧二进制，`0x71C7`）头字节数：13 个 16 位字（mtime/filesize 各占两字）。
pub const BIN_HEADER_BYTES: usize = 26;

/// newc 魔数（SVR4，ASCII）。
pub const NEWC_MAGIC: &[u8; 6] = b"070701";
/// crc 魔数（SVR4 + 数据字节和校验）。
pub const CRC_MAGIC: &[u8; 6] = b"070702";
/// odc 魔数（旧可移植 ASCII）。
pub const ODC_MAGIC: &[u8; 6] = b"070707";

/// bin 魔数小端字节序（`c7 71`，x86/ARM 等 LE 主机产物）。
pub const BIN_MAGIC_LE: [u8; 2] = [0xC7, 0x71];
/// bin 魔数大端字节序（`71 c7`，PDP-11/m68k 等 BE 主机产物）。
pub const BIN_MAGIC_BE: [u8; 2] = [0x71, 0xC7];

/// newc/crc 变体的对齐粒度（头+名字、数据区均按 4 字节对齐）。
pub const NEWC_ALIGN: usize = 4;
/// bin 变体的对齐粒度（按 16 位字对齐）。
pub const BIN_ALIGN: usize = 2;
/// odc 变体无对齐（名字与数据紧接）。
pub const ODC_ALIGN: usize = 1;

/// 结尾条目的名字（含尾 NUL 由 `namesize` 表达，本常量为去掉 NUL 的正文）。
pub const TRAILER_NAME: &[u8] = b"TRAILER!!!";

/// 名字长度上限（防 `namesize` 字段构造的内存炸弹；合法路径远小于此）。
pub const MAX_NAME_BYTES: u64 = 64 * 1024;

/// 索引条目数上限（与 [`hzip_format::QuotaLimits::max_entries`] 默认值同口径；
/// 超出 → `Err Quota{MaxEntries}`，绝不无界分配）。
pub const MAX_INDEX_ENTRIES: usize = 1_000_000;

/// 符号链接目标读取上限：cpio 的链接目标存放在**数据区**，索引期为填
/// `EntryInfo::symlink_target` 需读该条目的数据；上限之外的超大「链接」按目标
/// 未知处理（`None`），保证 `entries()` 的耗时与数据区大小无关（红线 1）。
pub const MAX_SYMLINK_TARGET_BYTES: u64 = 4096;

/// 文件类型掩码（`mode & S_IFMT`）。
pub const S_IFMT: u32 = 0o170000;
/// 目录（`S_IFDIR`）。
pub const S_IFDIR: u32 = 0o040000;
/// 符号链接（`S_IFLNK`）。
pub const S_IFLNK: u32 = 0o120000;

// 归档根的自我描述条目（`.` / `./`）判定**不在此处**：统一走
// `hzip_format::is_root_entry_name`（tar/cpio 共用一个判定，见其 rustdoc 对
// 「为什么必须跳过」的说明）。
