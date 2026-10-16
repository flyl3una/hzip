//! 编辑契约（02 §7.3/§9.9；契约冻结）。
//!
//! 编辑保存 = `ChangeSet::Replace`（D11）；`TC-CONF-009` 原子性由消费方
//! （core 编辑服务）以「临时物化 → rewrite → 原子替换」保证。

use std::path::PathBuf;

use hzip_error::HzipError;

use crate::Sink;
use crate::types::EntryId;
use crate::types::EntryName;
use crate::writer::WriteCtx;
use crate::writer::WriteReport;

/// 单条变更（02 §9.9）。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Change {
    /// 替换：以本地文件内容覆盖档案条目（编辑保存主路径）
    Replace {
        /// 目标条目 ID
        entry: EntryId,
        /// 本地新内容
        src: PathBuf,
    },
    /// 删除条目
    Delete(EntryId),
    /// 追加新条目
    Append {
        /// 本地文件
        src: PathBuf,
        /// 档案内条目名
        name: EntryName,
    },
    /// 重命名条目（用户口径 2026-09-28）。
    ///
    /// 按**名**而非 id 引用：目录可以是「隐式目录」（路径派生、档案里没有
    /// 独立目录条目），GUI 的目录行因而拿不到可用 id。`from` 为目录时，
    /// 其下所有以 `from + '/'` 开头的条目一并改前缀（整棵子树）。
    Rename {
        /// 旧条目名（档案内路径，已规范化）
        from: EntryName,
        /// 新条目名（档案内路径，已规范化；不含父目录段）
        to: EntryName,
    },
    /// 新建空目录条目（用户口径 2026-09-28；时间戳取执行时刻）。
    MakeDir {
        /// 新目录名（档案内完整路径）
        name: EntryName,
    },
}

/// 变更集（数据类；`Default` = 空集）。
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    /// 有序变更列表（按序执行）
    pub changes: Vec<Change>,
}

/// 档案编辑器契约（02 §7.3 冻结签名；`caps.edit` 才实现）。
pub trait ArchiveEditor: Send {
    /// 基于现有档案 + 变更集**流式重写**新档案（zip/7z）。
    ///
    /// 原子性由调用方保证：写入 `dst` 为临时目标，成功后原子替换原档案。
    ///
    /// # 不变式（用户口径 2026-09-28）
    ///
    /// 未被变更集命中的条目必须**无损保留**：压缩方法/数据、权限位（mode）、
    /// 修改时间、平台属性、**加密状态与算法**（含逐条目差异）。实现应优先
    /// 原样拷贝既有压缩数据，而非解压重压——既保真（不因重压改变
    /// 压缩级别/字节），也免于为「只改名」去解密整档。
    fn rewrite(
        &self,
        changes: ChangeSet,
        dst: Sink,
        ctx: &WriteCtx<'_>,
    ) -> Result<WriteReport, HzipError>;

    /// **就地写**（T-IMG1 契约扩展）：直接对源对象应用变更集（**不产出新档**）。
    ///
    /// 与 `rewrite` 的分工：`rewrite` 产出 `dst` 后由调用方原子替换；本方法改的是
    /// 源本身，原子性由格式自持（卷格式 = 覆盖层暂存 → 提交回放，见 T-IMG6）。
    /// core 只在 `FormatHandler::supports_in_place_edit()` 为真时走本方法，否则一律回落 `rewrite`。
    ///
    /// # 可重入要求
    ///
    /// core 的密码重试会对**同一 editor** 多次调用 ⇒ 每次调用必须从干净中间态
    /// 开始（丢弃上一次的暂存），不得累积——两次调用等价于一次。
    ///
    /// # 默认实现
    ///
    /// `Unsupported`（HZ-5001）：既有格式未实现就地写，无需改动。
    fn write_in_place(
        &self,
        _changes: ChangeSet,
        _ctx: &WriteCtx<'_>,
    ) -> Result<WriteReport, HzipError> {
        Err(HzipError::Unsupported {
            what: "hz-5001 in-place-edit-unsupported".into(),
        })
    }
}
