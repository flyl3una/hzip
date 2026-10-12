//! 能力过滤与用例 ID（03 §2.1 / 02 §8.6；TC-CONF-001..010）。

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use hzip_format::FormatCaps;

/// 一致性用例 ID（03 §2.1）。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfCaseId {
    /// TC-CONF-001 元数据完整性
    Conf001,
    /// TC-CONF-002 全量 roundtrip
    Conf002,
    /// TC-CONF-003 选择性解压
    Conf003,
    /// TC-CONF-004 乱序与重复读取
    Conf004,
    /// TC-CONF-005 追加条目
    Conf005,
    /// TC-CONF-006 中途取消
    Conf006,
    /// TC-CONF-007 错误密码
    Conf007,
    /// TC-CONF-008 畸形输入
    Conf008,
    /// TC-CONF-009 编辑原子性
    Conf009,
    /// TC-CONF-010 列表零 IO
    Conf010,
}

impl ConfCaseId {
    /// 全部用例（按序）。
    pub const ALL: [ConfCaseId; 10] = [
        ConfCaseId::Conf001,
        ConfCaseId::Conf002,
        ConfCaseId::Conf003,
        ConfCaseId::Conf004,
        ConfCaseId::Conf005,
        ConfCaseId::Conf006,
        ConfCaseId::Conf007,
        ConfCaseId::Conf008,
        ConfCaseId::Conf009,
        ConfCaseId::Conf010,
    ];
}

impl fmt::Display for ConfCaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = match self {
            Self::Conf001 => "001",
            Self::Conf002 => "002",
            Self::Conf003 => "003",
            Self::Conf004 => "004",
            Self::Conf005 => "005",
            Self::Conf006 => "006",
            Self::Conf007 => "007",
            Self::Conf008 => "008",
            Self::Conf009 => "009",
            Self::Conf010 => "010",
        };
        write!(f, "TC-CONF-{n}")
    }
}

/// 档案来源（用例自产档案或引用黄金资产）。
#[derive(Debug, Clone)]
pub enum ArchiveSource {
    /// 套件现场自产（`caps.write` 成立）
    SelfProduced,
    /// 引用 `tests-assets/golden/{fmt}/...` 只读档案
    Asset { path: PathBuf },
}

/// 能力过滤决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunDecision {
    /// 执行
    Run,
    /// 跳过（附原因）
    Skip(&'static str),
}

/// 能力过滤器（03 §1.1 分层 + §2.1 映射表）。
#[derive(Debug, Clone, Default)]
pub struct CapsFilter {
    /// 显式跳过（附原因）
    pub skip: BTreeSet<ConfCaseId>,
    /// 调试用：跳过能力检查强制执行
    pub force: BTreeSet<ConfCaseId>,
}

impl CapsFilter {
    /// 默认：按能力自动过滤。
    pub fn auto() -> Self {
        Self::default()
    }

    /// 显式跳过某组。
    pub fn skip(mut self, id: ConfCaseId) -> Self {
        self.skip.insert(id);
        self
    }

    /// 是否被显式跳过。
    pub fn is_skipped(&self, id: ConfCaseId) -> bool {
        self.skip.contains(&id)
    }

    /// 决策：显式跳过 > 强制 > 能力表。
    pub fn decide(&self, caps: &FormatCaps, id: ConfCaseId) -> RunDecision {
        if self.skip.contains(&id) {
            return RunDecision::Skip("显式跳过");
        }
        if self.force.contains(&id) {
            return RunDecision::Run;
        }
        match id {
            ConfCaseId::Conf001
            | ConfCaseId::Conf003
            | ConfCaseId::Conf004
            | ConfCaseId::Conf008
            | ConfCaseId::Conf010 => {
                if caps.read {
                    RunDecision::Run
                } else {
                    RunDecision::Skip("no read")
                }
            }
            ConfCaseId::Conf002 => {
                if caps.write {
                    RunDecision::Run
                } else if caps.read {
                    // 只读格式：黄金档案降级只读 roundtrip 语义由用例实现决定
                    RunDecision::Run
                } else {
                    RunDecision::Skip("no read/write")
                }
            }
            ConfCaseId::Conf005 => {
                // append=false 不跳过：负断言（create_writer(append) 必须 Err Unsupported）
                RunDecision::Run
            }
            ConfCaseId::Conf006 => {
                if caps.write || caps.read {
                    RunDecision::Run
                } else {
                    RunDecision::Skip("no archive source")
                }
            }
            ConfCaseId::Conf007 => {
                if caps.encryption != hzip_format::EncryptionCaps::None {
                    RunDecision::Run
                } else {
                    RunDecision::Skip("no encryption")
                }
            }
            ConfCaseId::Conf009 => {
                if caps.edit {
                    RunDecision::Run
                } else {
                    RunDecision::Skip("no edit")
                }
            }
        }
    }
}
