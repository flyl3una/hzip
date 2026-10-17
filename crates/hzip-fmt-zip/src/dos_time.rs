//! `time::OffsetDateTime` ⇄ `zip::DateTime`（DOS 时间）。
//!
//! zip 9 收掉了与 `OffsetDateTime` 的直转 impl（4.x 有 `TryFrom<OffsetDateTime>` /
//! `TryFrom<DateTime> for OffsetDateTime` 四条），只留 `PrimitiveDateTime` 桥。
//! 本模块就是那座桥——语义与 4.x **逐字一致**，避免升级时把时间戳口径改了：
//! 两个方向都只搬**墙钟**（`date()` + `time()`），一个不折算时区、一个按 UTC 解读
//!（zip 头里存的本来就是无时区的当地墙钟）。
//!
//! 对照 zip 4.6.1 `src/types.rs:440,465`。

use time::{OffsetDateTime, PrimitiveDateTime};

/// `OffsetDateTime` → DOS 时间（越界返回 `None`：调用方沿用条目原值，不猜时间）。
pub(crate) fn to_dos(mt: OffsetDateTime) -> Option<zip::DateTime> {
    zip::DateTime::try_from(PrimitiveDateTime::new(mt.date(), mt.time())).ok()
}

/// DOS 时间 → `OffsetDateTime`（按 UTC 解释；`None` = 分量非法，调用方当无时间）。
pub(crate) fn from_dos(dt: zip::DateTime) -> Option<OffsetDateTime> {
    PrimitiveDateTime::try_from(dt)
        .ok()
        .map(PrimitiveDateTime::assume_utc)
}

/// 当前时刻的 DOS 表示（越界回落 `DateTime::default()` = 1980-01-01）。
pub(crate) fn now() -> zip::DateTime {
    to_dos(OffsetDateTime::now_utc()).unwrap_or_default()
}
