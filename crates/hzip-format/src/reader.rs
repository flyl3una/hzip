//! 读取契约（02 §7.3；契约冻结）。
//!
//! 四条契约红线（testkit TC-CONF-001..010 逐条验证）：
//!
//! 1. `entries()` 除头部加密外**不读数据区**——大档案列表秒开；
//! 2. `read_entry` 任意 id **独立可用、可乱序、可重复**；
//! 3. 错误密码必须返回 `Password` 错误，**禁止静默产出乱码流**；
//! 4. **任何输入不 panic**：畸形数据 → `Format` 错误。

use std::io::Read;
use std::path::Path;

use hzip_error::HzipError;
use hzip_progress::{CancellationToken, ProgressBus};

use crate::caps::FormatCaps;
use crate::password::PasswordRef;
use crate::password::PasswordSource;
use crate::types::{Encoding, EntryId, EntryInfo, EntryName};

/// 选择范围（TC-CONF-003 按 id；CLI `--select` 按名）。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Selection {
    /// 全部
    All,
    /// 按 ID 列表
    Ids(Vec<EntryId>),
    /// 按名字列表
    Names(Vec<EntryName>),
}

/// 解压/测试报告（02 §7.3 返回值；core::report 聚合为 JobStats，02 §9.1）。
/// `failures`：best_effort 累积；fail-fast 为空。数据类（`Default` + 公开字段）。
#[derive(Debug, Default)]
pub struct ExtractReport {
    /// 成功条目数
    pub entries_done: usize,
    /// 写出字节数
    pub bytes_written: u64,
    /// 逐条目失败明细（best_effort 模式填充）
    pub failures: Vec<(EntryId, Box<HzipError>)>,
}

/// 冲突策略（02 §9.8 / 附录 B extract.conflict；执行逻辑归 core）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// 询问用户（core 请求-应答）
    Ask,
    /// 覆盖
    Overwrite,
    /// 跳过
    Skip,
    /// 自动改名（幂等）
    RenameAuto,
    /// 仅当源更新
    NewerOnly,
}

/// 净化策略配置（02 §9.6；算法在 core::security，本 crate 仅承载配置）。数据类。
#[derive(Debug, Clone, Copy)]
pub struct SanitizeConfig {
    /// 符号链接策略
    pub symlink: SymlinkPolicy,
}

impl Default for SanitizeConfig {
    fn default() -> Self {
        Self {
            symlink: SymlinkPolicy::DenyEscape,
        }
    }
}

/// 符号链接策略（附录 B security.symlink）。
///
/// `#[non_exhaustive]` 契约：新增档位走最安全语义（见 core 策略表兜底）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkPolicy {
    /// 默认：目标净化后仍在 dest 内 → 恢复链接；逃逸 → 拒绝
    DenyEscape,
    /// 一律拒绝链接（跳过/报错由消费方决定）
    DenyAll,
    /// 展平：链接目标内容以普通文件写入
    Flatten,
    /// 跳过：dest 内目标 → 恢复链接；逃逸/绝对路径目标 → 跳过该链接
    /// （记入失败明细，其余条目照常释放——跨平台 Linux 档案释放场景；
    /// 组合攻击防线不受影响，core 策略表第三行仍 fail-fast）
    Skip,
}

/// 配额限制（02 §9.6 四类 Quota；默认值见附录 B.1：32 GiB/128 GiB/200/1e6
/// → HZ-4501..4504）。数据类。
#[derive(Debug, Clone, Copy)]
pub struct QuotaLimits {
    /// 单文件解压上限（HZ-4501）
    pub max_file_bytes: u64,
    /// 总解压上限（HZ-4502）
    pub max_total_bytes: u64,
    /// 压缩比率上限（HZ-4503）
    pub max_ratio: u32,
    /// 条目数上限（HZ-4504）
    pub max_entries: u64,
}

impl Default for QuotaLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 32 * 1024 * 1024 * 1024,   // 32 GiB
            max_total_bytes: 128 * 1024 * 1024 * 1024, // 128 GiB
            max_ratio: 200,
            max_entries: 1_000_000,
        }
    }
}

/// 解压上下文（02 §7.3）：格式层只消费不构造；密码/冲突经请求-应答回调到 core。
/// `codepage`：手动指定优先于自动检测。
pub struct ExtractCtx<'a> {
    /// 取消令牌（检查点）
    pub cancel: &'a CancellationToken,
    /// 进度总线
    pub progress: &'a ProgressBus,
    /// 密码来源
    pub passwords: &'a dyn PasswordSource,
    /// 冲突策略
    pub conflicts: ConflictPolicy,
    /// 净化配置
    pub sanitize: &'a SanitizeConfig,
    /// 配额限制
    pub quota: &'a QuotaLimits,
    /// best-effort 模式（逐条目失败累积进报告而非 fail-fast）
    pub best_effort: bool,
    /// 手动指定编码
    pub codepage: Option<Encoding>,
}

/// 档案读取器契约（02 §7.3 冻结签名）。
pub trait ArchiveReader: Send {
    /// 能力矩阵
    fn caps(&self) -> FormatCaps;
    /// 条目列表（红线 1：不读数据区；头部加密 → `Password{Header}`）
    fn entries(&self) -> Result<Vec<EntryInfo>, HzipError>;
    /// 读取单条目数据（红线 2：独立/乱序/重复可用）
    fn read_entry(
        &self,
        id: EntryId,
        pw: PasswordRef<'_>,
    ) -> Result<Box<dyn Read + Send>, HzipError>;
    /// 解压（净化/冲突/配额执行逻辑归 core，本方法在 core 的驱动下逐条目落盘；
    /// fmt 实现直接调用的形态以各任务卡为准）
    fn extract(
        &self,
        sel: &Selection,
        ctx: &ExtractCtx<'_>,
        dest: &Path,
    ) -> Result<ExtractReport, HzipError>;
    /// 完整性测试（读全部数据区校验 CRC）
    fn test(&self, ctx: &ExtractCtx<'_>) -> Result<ExtractReport, HzipError>;
}
