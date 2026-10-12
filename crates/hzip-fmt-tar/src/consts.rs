//! TAR 格式编译期常量（02 附录 B.2 / T22 §4.1）。
//!
//! 出处：02 §8.4 三变体兼容读取、§4.1 模块结构表 `consts` 行。

/// 魔数偏移（02 附录 B.2；tar `ustar` @ 257，见 T22 §4.3 detect 函数）。
pub const TAR_MAGIC_OFFSET: usize = 257;

/// 单块字节数（512，标准 tar 头/数据块粒度）。
pub const BLOCK_BYTES: usize = 512;

/// 档案结尾连续零块数（GNU tar 兼容标准结尾：2 块）。
pub const TRAILER_BLOCKS: usize = 2;

/// 追加时尾部扫描窗口（GNU tar 兼容：10240 字节 = 20 块）。
pub const APPEND_TAIL_WINDOW: usize = 10240;

/// GNU 魔数（`b"ustar "`）+ GNU 版本（`b" \0"`，空格+空字符）。
///
/// 陷阱（T22 §7-2）：GNU version 域是 `0x20 0x00`，POSIX ustar 是 `0x00 0x30`
/// （`"00"`）——判定必须逐字节对照，禁止 `starts_with("ustar")` 混判。
pub(crate) const GNU_MAGIC: [u8; 8] = *b"ustar  \x00";
/// POSIX ustar 魔数+版本（`b"ustar\0"` + `b"00"`）。
pub(crate) const USTAR_MAGIC: [u8; 8] = *b"ustar\x0000";
