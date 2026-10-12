//! 编译期常量（02 附录 B.2 归本 crate 的条目）。

/// 流拷贝/取消检查间隔（02 §7.3/附录 B.2）：1 MiB。
pub const COPY_CHUNK_BYTES: u64 = 1024 * 1024;

/// 嗅探读取量（02 §7.4）：512 字节头部。
pub const SNIFF_HEAD_BYTES: usize = 512;

/// 组合格式装配深度上限（02 §7.5）：3 层压缩；超限 → `Err Quota{MaxEntries}`
/// （HZ-4504 与条目数超限共用码）。
pub const COMPOSITE_DEPTH_MAX: u32 = 3;

/// 嗅探到 `Source::Stdin` 时的缓冲代理读取块（02 §7.4）。
pub const STDIN_PROBE_BUF: usize = 4096;
