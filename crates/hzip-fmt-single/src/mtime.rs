//! mtime 来源与提取（02 §8.5 / T23 §3.2 / §4.4）。
//!
//! | 格式 | 读端 | 写端 |
//! |---|---|---|
//! | gz | gzip 头 mtime 字段（4 B，`0` = 无 → `None`） | `GzBuilder::mtime`（秒） |
//! | zst | 帧可选：hzip 私有 skippable 帧标记（读端尽力） | 默认不写（字节兼容） |
//! | xz / bz2 | 无（容器无 mtime 字段）→ `None` | 不写 |
//!
//! 02/03 的 mtime 冲突裁定（T23 §7-1）：xz 容器无标准 mtime 字段，本 crate 按
//! 02 §8.5（规范唯一来源）只实现 gzip 头与 zstd 可选路径，xz/bz2 恒 `None`。

use time::OffsetDateTime;

/// hzip zstd 私有 mtime skippable 帧的载荷前缀（T23 §4.4）。
const HZIP_MTIME_MAGIC: &[u8; 10] = b"hzip-mtime";
/// 载荷布局 tag（版本 1；hzip 私有，外部工具不可见）。
const HZIP_MTIME_TAG: u32 = 1;

/// 私有 skippable 帧最小长度：`[b"hzip-mtime"][4B tag][8B unix_millis]`。
const HZIP_MTIME_PAYLOAD_LEN: usize = 10 + 4 + 8;

/// gzip 头 mtime 字段 → `Option<OffsetDateTime>`（`0` = 无 → `None`，Unix 秒）。
#[cfg(feature = "gz")]
pub(crate) fn mtime_from_gzip_field(secs: u32) -> Option<OffsetDateTime> {
    if secs == 0 {
        return None;
    }
    OffsetDateTime::from_unix_timestamp(i64::from(secs)).ok()
}

/// `EntryMeta.mtime` → gzip 头 mtime 秒值（None / 越界 → 0 = 不写时间戳）。
#[cfg(feature = "gz")]
pub(crate) fn gzip_mtime_field(mtime: Option<OffsetDateTime>) -> u32 {
    match mtime {
        Some(t) => {
            let secs = t.unix_timestamp();
            u32::try_from(secs).unwrap_or(0)
        }
        None => 0,
    }
}

/// 解析 hzip 私有 zstd skippable 帧载荷 → mtime（Unix 毫秒 → `OffsetDateTime`）。
///
/// 载荷布局：`[b"hzip-mtime"][u32 LE tag][u64 LE unix_millis]`。非 hzip 布局 /
/// tag 不符 / 长度不足 → `None`（读端尽力，非 hzip 帧一律忽略，02 §8.5）。
pub(crate) fn parse_hzip_mtime_payload(payload: &[u8]) -> Option<OffsetDateTime> {
    if payload.len() < HZIP_MTIME_PAYLOAD_LEN || !payload.starts_with(HZIP_MTIME_MAGIC) {
        return None;
    }
    let mut tag = [0u8; 4];
    tag.copy_from_slice(&payload[10..14]);
    if u32::from_le_bytes(tag) != HZIP_MTIME_TAG {
        return None;
    }
    let mut millis = [0u8; 8];
    millis.copy_from_slice(&payload[14..22]);
    let millis = u64::from_le_bytes(millis);
    OffsetDateTime::from_unix_timestamp_nanos(millis.saturating_mul(1_000_000) as i128).ok()
}

/// 供写端诊断/测试的私有帧构造辅助（`#[cfg(test)]` 与外部测试用）。
#[cfg(test)]
pub(crate) fn build_hzip_mtime_payload(millis: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(HZIP_MTIME_PAYLOAD_LEN);
    v.extend_from_slice(HZIP_MTIME_MAGIC);
    v.extend_from_slice(&HZIP_MTIME_TAG.to_le_bytes());
    v.extend_from_slice(&millis.to_le_bytes());
    v
}

#[cfg(test)]
#[path = "tests/mtime.rs"]
mod tests;
