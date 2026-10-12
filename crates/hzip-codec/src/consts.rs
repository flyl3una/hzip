//! 级别表常量与默认线程规则（02 §6.3）。
//!
//! 常量供 [`crate::level`] 表驱动映射使用，避免散落魔法数字（02 附录 B.2 精神）。

/// lzma2 字典尺寸常量（02 §6.3「7z lzma2 dict」列）。单位：字节。
pub const LZMA2_DICT_STORE: u64 = 0;
/// 64 KB
pub const LZMA2_DICT_FASTEST: u64 = 64 * 1024;
/// 1 MB
pub const LZMA2_DICT_FAST: u64 = 1024 * 1024;
/// 16 MB
pub const LZMA2_DICT_NORMAL: u64 = 16 * 1024 * 1024;
/// 64 MB
pub const LZMA2_DICT_MAXIMUM: u64 = 64 * 1024 * 1024;
/// 256 MB（02 §6.3；解码 window 内存 ~512 MB 峰值风险，rustdoc 标注于 [`crate::xz`]）
pub const LZMA2_DICT_ULTRA: u64 = 256 * 1024 * 1024;

/// 默认线程数：`min(请求值, 可用核数)` 规则的「全部可用核」语义（02 §6.3）。
///
/// 各编码器在 `threads == 0` 时以本值作为 worker 数（0 亦会被上游视为 auto）。
pub fn default_threads() -> u32 {
    crate::level::clamp_threads(u32::MAX)
}
