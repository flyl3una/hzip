//! 符号链接策略表（02 §9.6 三行表 + `Skip` 档）与已建链接台账的
//! 逃逸组合攻击检测。
//!
//! 策略表（默认 deny-escape）：
//!
//! | 条目形态 | DenyEscape（默认） | DenyAll | Flatten | Skip |
//! |---|---|---|---|---|
//! | 链接目标净化后仍在 dest 内 | 恢复为符号链接 | 展平 | 展平 | 恢复为符号链接 |
//! | 目标绝对路径 / dest 外 | 拒绝 4002 | 拒绝 4002 | 展平 | **跳过**（记失败明细） |
//! | 后续条目写入「已被链接路径」之下 | 拒绝 4002（**不可配置**，全策略档一致） | 同左 | 同左 | 同左 |
//!
//! 台账仅登记「实际创建成功」的链接（本模块只做决策；创建由 T32 经
//! `hzip-platform::symlink` 执行后回调登记）。

use std::path::{Component, Path, PathBuf};

use hzip_error::{HzipError, SecurityViolation};
use hzip_format::SymlinkPolicy;

/// 单条链接的决策结果（T32 管道按结果执行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkAction {
    /// 恢复为符号链接（登记台账；目标在 dest 内）
    CreateSymlink,
    /// 跳过（不落盘）
    Skip,
    /// 展平：以目标内容写普通文件（由 T32 按目标内容流写入）
    FlattenStore,
    /// 拒绝（默认 deny-escape 命中逃逸目标时上层直接 Err 4002）
    Deny,
}

/// 符号链接台账（02 §9.6 策略表第三行「不可配置」防线）。
#[derive(Debug)]
pub struct SymlinkGuard {
    policy: SymlinkPolicy,
    /// 已登记（实际创建成功）的链接——dest 相对路径
    ledger: Vec<PathBuf>,
}

impl SymlinkGuard {
    /// 新建（策略来自 B.1 `security.symlink`；`dest` 为解压根）。
    pub fn new(policy: SymlinkPolicy) -> Self {
        Self {
            policy,
            ledger: Vec::new(),
        }
    }

    /// 策略表前两行：目标净化判定 → 动作。
    ///
    /// `target_raw` 已按 02 §7.3 语义是链接目标原始字符串；
    /// `link_rel` 是**已净化**的链接自身相对路径（落盘位置，用于台账）。
    /// 目标逃逸（绝对路径 / dest 外 / 含 `..`）→ 依策略 `Deny`（默认，上层报
    /// 4002）/`Skip`/`FlattenStore`。目标在 dest 内 → 依策略 `CreateSymlink`/
    /// `FlattenStore`。
    ///
    /// 返回动作（不实际创建文件）。`CreateSymlink` 时调用方创建成功后须
    /// [`register_created`] 登记。
    pub fn decide(&self, target_raw: &str) -> LinkAction {
        // 目标自身先做「绝对/`..` 逃逸」判定（02 §9.6 表第二行语义）。
        // 完整 sanitize 链属 T32 落盘前执行；此处只需区分「目标在 dest 内」。
        if target_escapes(target_raw) {
            match self.policy {
                SymlinkPolicy::DenyEscape => LinkAction::Deny,
                SymlinkPolicy::DenyAll => LinkAction::Deny,
                SymlinkPolicy::Flatten => LinkAction::FlattenStore,
                // skip：逃逸目标跳过（跨平台档案释放；记失败明细不中止）
                SymlinkPolicy::Skip => LinkAction::Skip,
                // 契约冻结：新增策略默认走最安全语义（拒绝逃逸）
                _ => LinkAction::Deny,
            }
        } else {
            match self.policy {
                SymlinkPolicy::DenyEscape => LinkAction::CreateSymlink,
                SymlinkPolicy::DenyAll => LinkAction::FlattenStore,
                SymlinkPolicy::Flatten => LinkAction::FlattenStore,
                // skip：dest 内目标照常恢复为链接
                SymlinkPolicy::Skip => LinkAction::CreateSymlink,
                // 契约冻结：未知策略默认保守（不自动创建链接）
                _ => LinkAction::FlattenStore,
            }
        }
    }

    /// 登记一条已实际创建的链接（dest 相对路径；仅创建成功才登记）。
    pub fn register_created(&mut self, link_rel: PathBuf) {
        self.ledger.push(link_rel);
    }

    /// 策略表第三行：`entry_rel` 不得落入任何已建链接之下（逃逸组合攻击）。
    ///
    /// 分量级前缀判定：`link` 为 `link/x` 的真前缀命中；`link2/x` 对 `link`
    /// **不**命中（防字节前缀误判/漏判）。命中 → `Err Security{SymlinkEscape}`。
    pub fn check_under_linked(&self, entry_rel: &Path) -> Result<(), HzipError> {
        let comps: Vec<Component<'_>> = entry_rel.components().collect();
        for link in &self.ledger {
            let link_comps: Vec<Component<'_>> = link.components().collect();
            if link_comps.is_empty() {
                continue;
            }
            // link 必须是 entry 的**真前缀**（分量级）
            if comps.len() > link_comps.len() && comps[..link_comps.len()] == link_comps[..] {
                return Err(HzipError::Security {
                    violation: SecurityViolation::SymlinkEscape,
                });
            }
        }
        Ok(())
    }

    /// 台账条目数（测试断言用）。
    pub fn ledger_len(&self) -> usize {
        self.ledger.len()
    }
}

/// 目标是否逃逸：绝对路径 / UNC / 盘符 / 含 `..` 分量。
fn target_escapes(target_raw: &str) -> bool {
    if target_raw.starts_with('/') || target_raw.starts_with('\\') {
        return true;
    }
    if target_raw.starts_with("\\\\") || target_raw.starts_with("//") {
        return true;
    }
    let bytes = target_raw.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return true;
    }
    Path::new(target_raw)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}
