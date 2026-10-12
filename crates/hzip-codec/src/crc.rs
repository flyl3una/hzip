//! crc32 流式增量（02 §6.2 旁路累计；EntrySink drop 时校验）。

/// 基于 crc32fast 的流式增量（02 §6.2 旁路累计；EntrySink drop 时校验）。
///
/// 维护 `(state, bytes)`，支持**中途查询** `finalize()`/`bytes()`——zip EntrySink
/// drop 校验需要「写到哪校验到哪」。内部持有一个 `crc32fast::Hasher` 增量累计
/// （热路径不产生新 Hasher）；`finalize(&self)` 通过克隆哈希器取值（O(1)，非热路径）。
///
/// 空输入 `finalize() == 0`。
#[derive(Debug, Clone)]
pub struct Crc32 {
    hasher: crc32fast::Hasher,
    bytes: u64,
}

impl Crc32 {
    /// 新建：以 0 初始（crc32fast 终结异或内建）。
    pub fn new() -> Self {
        Self {
            hasher: crc32fast::Hasher::new(),
            bytes: 0,
        }
    }

    /// 追加一段字节到累计值。
    pub fn update(&mut self, buf: &[u8]) {
        self.hasher.update(buf);
        self.bytes += buf.len() as u64;
    }

    /// 当前已消费字节的 CRC（中途查询语义，不消费 `self`）。
    pub fn finalize(&self) -> u32 {
        self.hasher.clone().finalize()
    }

    /// 已累计字节数。
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}
