//! 套件骨架（02 §8.6：KitCtx / run_conformance / DynTestList）。

use std::fmt;
use std::path::Path;

use hzip_format::FormatHandler;
use tempfile::TempDir;

use crate::cases;
use crate::filter::{CapsFilter, ConfCaseId};

/// 套件上下文（`assets` 指向仓库 tests-assets/；`tmp` 每次调用独立）。
pub struct KitCtx {
    /// 仓库 `tests-assets/` 路径（黄金/生成档案根）
    pub assets: &'static Path,
    /// 独立临时目录（用例现场自产档案/产物）
    pub tmp: TempDir,
    /// 能力过滤
    pub caps_filter: CapsFilter,
}

impl KitCtx {
    /// 新建：`assets` 指向仓库 tests-assets/；tmp 自动创建。
    pub fn new(assets: &Path) -> std::io::Result<Self> {
        // assets 以 'static 承载：调用方传 CARGO_MANIFEST_DIR 拼接的路径字面量
        let assets: &'static Path = Box::leak(assets.to_path_buf().into_boxed_path());
        Ok(Self {
            assets,
            tmp: TempDir::new().map_err(|e| std::io::Error::other(format!("TempDir: {e}")))?,
            caps_filter: CapsFilter::auto(),
        })
    }

    /// 覆盖能力过滤。
    pub fn with_caps_filter(mut self, f: CapsFilter) -> Self {
        self.caps_filter = f;
        self
    }

    /// 临时目录路径（用例产物根）。
    pub fn tmp_path(&self) -> &Path {
        self.tmp.path()
    }
}

/// 单条可命名测试结果（02 §8.6「DynTestList」）。
#[derive(Debug, Clone)]
pub struct DynTest {
    /// 用例 ID
    pub id: ConfCaseId,
    /// 展开名（`{id}-{handler.id()}`）
    pub name: String,
    /// 结果
    pub outcome: DynOutcome,
}

/// 动态测试结果。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum DynOutcome {
    /// 通过
    Pass,
    /// 跳过（附原因）
    Skip(String),
    /// 失败（附消息）
    Fail { message: String },
}

impl fmt::Display for DynOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => write!(f, "PASS"),
            Self::Skip(reason) => write!(f, "SKIP ({reason})"),
            Self::Fail { message } => write!(f, "FAIL ({message})"),
        }
    }
}

/// 一致性汇总。
#[derive(Debug, Clone, Default)]
pub struct ConformanceSummary {
    /// 总数
    pub total: usize,
    /// 通过
    pub passed: usize,
    /// 跳过
    pub skipped: usize,
    /// 失败
    pub failed: usize,
    /// panic 计数（TC-CONF-008 红线 4）
    pub panic_count: usize,
}

/// 可命名测试集合（02 §8.6）。
#[derive(Debug, Clone)]
pub struct DynTestList {
    tests: Vec<DynTest>,
}

impl DynTestList {
    /// 空集合。
    pub fn new() -> Self {
        Self { tests: Vec::new() }
    }

    /// 追加一条。
    pub fn push(&mut self, t: DynTest) {
        self.tests.push(t);
    }

    /// 测试列表。
    pub fn tests(&self) -> &[DynTest] {
        &self.tests
    }

    /// 汇总统计。
    pub fn summary(&self) -> ConformanceSummary {
        let mut s = ConformanceSummary {
            total: self.tests.len(),
            ..Default::default()
        };
        for t in &self.tests {
            match &t.outcome {
                DynOutcome::Pass => s.passed += 1,
                DynOutcome::Skip(_) => s.skipped += 1,
                DynOutcome::Fail { .. } => s.failed += 1,
            }
        }
        s
    }

    /// 任一 Fail 即 panic（供 `#[test]` 直接调用）。
    pub fn assert_all_pass(&self) {
        let failed: Vec<String> = self
            .tests
            .iter()
            .filter(|t| matches!(t.outcome, DynOutcome::Fail { .. }))
            .map(|t| format!("[{}] {}", t.name, t.outcome))
            .collect();
        assert!(
            failed.is_empty(),
            "一致性套件存在失败用例:\n{}",
            failed.join("\n")
        );
    }
}

impl Default for DynTestList {
    fn default() -> Self {
        Self::new()
    }
}

/// 展开名辅助：`{id}-{handler_id}`（03 §2.1 命名约定）。
pub(crate) fn expanded_name<H: FormatHandler + ?Sized>(h: &H, id: ConfCaseId) -> String {
    format!("{id}-{}", h.id())
}

/// 对 handler 展开全部 10 组（02 §8.6 原文）。
pub fn run_conformance<H: FormatHandler>(h: &H, ctx: &KitCtx) -> DynTestList {
    run_cases(h, ctx, &ConfCaseId::ALL)
}

/// 对 handler 展开指定用例组（PR 抽样模式：03 §1.1 每次提交跑抽样、nightly 全量）。
pub fn run_cases<H: FormatHandler>(h: &H, ctx: &KitCtx, ids: &[ConfCaseId]) -> DynTestList {
    let mut list = DynTestList::new();
    for id in ids {
        list.push(cases::run_case(h, ctx, *id));
    }
    list
}
