//! hzip-fs-fat：FAT12/16/32 文件系统驱动（`hzip_vol::FsDriver` 实现）。
//!
//! # 读与写
//!
//! 读：`entries()` 递归列目录（LFN 与 8.3 并存，CJK 名经 LFN 往返）；`read_at()` 按
//! 偏移惰性读（乱序/重复可用 → 契约红线 2）。写：五类变更映射到 `fatfs` 的
//! 建/删/改名/建目录与文件写入。
//!
//! # 无日志 ⇒ 风险如实标注
//!
//! FAT 没有日志：目录项、FAT 链、数据簇之间**不是原子的**。故能力矩阵里给
//! `vol.warn.no-journal` 提示，并依赖 `hzip-vol` 的默认覆盖写（提交前原档不变）。
//! 读路径必须关掉「访问日期更新」（`FsOptions::update_accessed_date(false)`）——
//! **读不能改档案**，这是 hzip 的既有红线。
//!
//! # 计划与验收
//!
//! `docs/tasks/T-IMG/T-IMG12-fat.md`（三种宽度的夹具、LFN/CJK、ESP 形态、4 GiB 上限）。
//!
//! # 与计划的两处落地差异（均为硬约束下的选择，详见各常量/函数文档）
//!
//! | 计划 | 落地 | 原因 |
//! |---|---|---|
//! | 在 `hzip-vol/src/error.rs` 新增 `vol.err.too-large` 变体 | 经 `VolError::Unsupported("vol.err.too-large")` 承载 | 本波次**不允许改 `hzip-vol`**；稳定键一字不改，UI 按键查 locale 不受影响 |
//! | `audit()` 返回 `AuditSummary::not_implemented("vol.warn.no-journal")` | 同左，并**额外**扫「根目录 + 一层子目录」做轻量自检 | 计划 §2.2 要求额外自检；自检是抽样，`skipped` 仍为真（不冒充 fsck） |

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod driver;
pub mod io;
pub mod volume;

pub use driver::{FatDriver, is_fat_boot_sector};
pub use io::BlockDeviceSeek;
pub use volume::{
    ERR_DIR_NOT_EMPTY, ERR_ENTRY_GONE, ERR_EXISTS, ERR_NAME_TOO_LONG, ERR_SOURCE_NOT_FILE,
    ERR_TOO_LARGE, ERR_UNKNOWN_CHANGE, ERR_UNKNOWN_ENTRY, FAT_MAX_FILE_BYTES, FatVolume,
    check_file_size,
};
