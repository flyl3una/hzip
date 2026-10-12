//! 单元 + 平台差异对照测试（02 §9.6；TC-SEC-001~009）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use hzip_error::{HzipError, SecurityViolation};
use hzip_format::{EntryName, QuotaLimits, SymlinkPolicy};

use super::quota::{QuotaGuard, QuotaState};
use super::sanitize::sanitize_entry_path;
use super::symlink::{LinkAction, SymlinkGuard};

fn dest() -> PathBuf {
    std::env::temp_dir().join("hzip-core-t31").join("dest")
}

fn sanitize(raw: &str) -> Result<super::sanitize::SanitizedEntry, HzipError> {
    sanitize_entry_path(
        &EntryName::new(raw.to_string()),
        &dest(),
        &hzip_format::SanitizeConfig::default(),
    )
}

fn code(e: &HzipError) -> u32 {
    e.code()
}

// ============ TC-SEC-001 路径穿越 ============

#[test]
fn sanitize_traversal_rejected() {
    for raw in [
        "../../etc/evil",
        "a/../../../b",
        "a/../b", // 前有合法段也不抵消
        "..\\..\\x",
        "a\\..\\..\\x",
        "../up",
    ] {
        let e = sanitize(raw).expect_err("穿越必须拒绝");
        assert_eq!(code(&e), 4001, "{raw}");
        assert!(matches!(
            e,
            HzipError::Security {
                violation: SecurityViolation::PathTraversal(_)
            }
        ));
    }
}

#[test]
fn sanitize_dotdot_never_traverses_up() {
    // 属性化主断言由 proptest 承担；此处抽查分量语义
    for raw in ["a/../b/../c", "./../x"] {
        assert_eq!(sanitize(raw).expect_err(".. 拒绝").code(), 4001, "{raw}");
    }
}

// ============ TC-SEC-002 绝对路径 ============

#[test]
fn sanitize_absolute_rejected() {
    for raw in [
        "/etc/passwd",
        "/abs",
        "\\abs",
        "C:/Windows/evil",
        "C:\\Windows\\evil",
        "c:x",
        "\\\\server\\share\\x",
        "//server/share",
    ] {
        let e = sanitize(raw).expect_err("绝对路径必须拒绝");
        assert_eq!(code(&e), 4001, "{raw:?}");
    }
}

// ============ TC-SEC-003 保留名 ============

#[test]
fn sanitize_reserved_names_win_rejects_unix_passes() {
    for raw in ["CON", "con.txt", "NUL.bin", "COM1", "lpt9.log", "CON.x.txt"] {
        #[cfg(windows)]
        {
            let e = sanitize(raw).expect_err("win 保留名拒绝");
            assert_eq!(code(&e), 4003, "{raw}");
            assert!(matches!(
                e,
                HzipError::Security {
                    violation: SecurityViolation::ReservedDeviceName
                }
            ));
        }
        #[cfg(not(windows))]
        {
            let s = sanitize(raw).expect("unix 保留名放行");
            assert!(!s.rel.as_os_str().is_empty(), "{raw}");
        }
    }
    // 非保留：两平台均放行
    for raw in ["CONtroller", "CONX.txt", "COM10", "LPT0"] {
        let s = sanitize(raw).expect("非保留名放行");
        assert!(!s.rel.as_os_str().is_empty(), "{raw}");
    }
}

// ============ TC-SEC-004 控制字符 ============

#[test]
fn sanitize_control_chars_rejected() {
    for raw in ["a\x00b", "a\x01b", "a\x1fb", "a\x7fb", "a\u{9f}b", "nl\n"] {
        let e = sanitize(raw).expect_err("控制字符拒绝");
        assert_eq!(code(&e), 4004, "{raw:?}");
        assert!(matches!(
            e,
            HzipError::Security {
                violation: SecurityViolation::ControlChars
            }
        ));
    }
}

// ============ 点段/空段 ============

#[test]
fn sanitize_dot_segments_dropped() {
    // "./a" 语法层 N4 已剥；此处验证全丢语义
    let s = sanitize(".").expect("'.' 不报错");
    assert!(s.rel.as_os_str().is_empty(), "全丢 → dest 本身");
    assert!(!s.warnings.is_empty());
    let s2 = sanitize("a/./b").expect("中间点段丢弃");
    assert_eq!(s2.rel.to_string_lossy(), "a/b");
}

// ============ 段尾截断（win）/ 保留（unix） ============

#[test]
fn sanitize_trailing_trim_platform() {
    #[cfg(windows)]
    {
        // 段尾空格/句点截断 + warning（02 §9.6 L→M）
        let s = sanitize("a.txt ").expect("win 截断不报错");
        assert_eq!(s.rel.to_string_lossy(), "a.txt");
        assert_eq!(s.warnings.len(), 1);
        assert_eq!(s.warnings[0].original, "a.txt ");
        let s_b = sanitize("dir/b.").expect("段尾句点截断");
        assert_eq!(s_b.rel.to_string_lossy(), "dir/b");
        // 双检闭合：CON. → 截断 → CON → 4003
        let e = sanitize("CON. ").expect_err("CON. 截断后复检拒绝");
        assert_eq!(code(&e), 4003);
        // 截空丢弃：目录名全空格+点 → 丢弃该段
        let s2 = sanitize("dir/   ").expect("截空段丢弃不报错");
        assert_eq!(s2.rel.to_string_lossy(), "dir");
    }
    #[cfg(not(windows))]
    {
        let s = sanitize("a .txt").expect("unix 保留");
        assert_eq!(s.rel.to_string_lossy(), "a .txt");
        assert!(s.warnings.is_empty());
    }
}

// ============ 符号链接策略表 ============

#[test]
fn symlink_table_rows() {
    // 行1：目标在 dest 内（相对、无逃逸）→ 默认 CreateSymlink
    let g = SymlinkGuard::new(SymlinkPolicy::DenyEscape);
    assert_eq!(g.decide("inside.txt"), LinkAction::CreateSymlink);
    assert_eq!(g.decide("sub/x"), LinkAction::CreateSymlink);
    // 行2：绝对/逃逸 → 默认 Deny；flatten/skip 分支
    assert_eq!(g.decide("/abs"), LinkAction::Deny);
    assert_eq!(g.decide("\\\\srv\\share"), LinkAction::Deny);
    assert_eq!(g.decide("C:/win"), LinkAction::Deny);
    assert_eq!(g.decide("../up"), LinkAction::Deny);
    let gf = SymlinkGuard::new(SymlinkPolicy::Flatten);
    assert_eq!(gf.decide("/abs"), LinkAction::FlattenStore);
    assert_eq!(gf.decide("inside"), LinkAction::FlattenStore);
    let ga = SymlinkGuard::new(SymlinkPolicy::DenyAll);
    assert_eq!(ga.decide("inside"), LinkAction::FlattenStore);
    assert_eq!(ga.decide("/abs"), LinkAction::Deny);
}

// Skip 档（跨平台档案释放）：dest 内目标建链；逃逸目标跳过（记失败不中止）
#[test]
fn symlink_skip_policy_rows() {
    let g = SymlinkGuard::new(SymlinkPolicy::Skip);
    // 行1：dest 内相对目标 → 照常恢复链接
    assert_eq!(g.decide("inside.txt"), LinkAction::CreateSymlink);
    assert_eq!(g.decide("sub/x"), LinkAction::CreateSymlink);
    // 行2：绝对/逃逸目标 → Skip（不中止，extract_selection 记失败明细）
    assert_eq!(g.decide("/usr/lib/nginx/modules"), LinkAction::Skip);
    assert_eq!(g.decide("../up"), LinkAction::Skip);
    assert_eq!(g.decide("C:/win"), LinkAction::Skip);
}

// ============ TC-SEC-009 组合攻击 ============

#[test]
fn symlink_combo_attack() {
    let mut g = SymlinkGuard::new(SymlinkPolicy::DenyEscape);
    g.register_created(PathBuf::from("lnk"));
    // 落入链接之下 → 4002
    let e = g
        .check_under_linked(Path::new("lnk/etc/evil"))
        .expect_err("落入链接之下必须拒绝");
    assert_eq!(code(&e), 4002);
    // 分量级前缀：link2/x 对 lnk 不误报
    assert!(g.check_under_linked(Path::new("lnk2/x")).is_ok());
    // 非链接路径正常
    assert!(g.check_under_linked(Path::new("other/file")).is_ok());
    // 多层
    g.register_created(PathBuf::from("deep/nest"));
    assert!(g.check_under_linked(Path::new("deep/nest2")).is_ok());
    let e2 = g
        .check_under_linked(Path::new("deep/nest/evil"))
        .expect_err("");
    assert_eq!(code(&e2), 4002);
}

// ============ 配额 ============

/// 小间隔便于测试：4 MiB 门控在 3 MiB 不触发、5 MiB 触发。
#[test]
fn quota_gate_and_kinds() {
    let limits = QuotaLimits {
        max_file_bytes: 100,
        max_total_bytes: 500,
        max_ratio: 200,
        max_entries: 10,
    };
    let mut q = QuotaGuard::new(limits);
    q.entry_start().unwrap();
    // 4 MiB 粒度：一次只加 3 MiB 不检
    let interval = crate::consts::QUOTA_CHECK_INTERVAL; // 4*1024*1024
    q.checkpoint(interval - 1, interval / 200).unwrap();
    assert_eq!(q.state().accrual, interval - 1);
    // 累积到 4 MiB 才检：此 chunk raw 巨大 → 4501
    let e = q.checkpoint(1, 0).unwrap_err();
    assert_eq!(code(&e), 4501);
}

#[test]
fn quota_order_4501_over_4502() {
    let limits = QuotaLimits {
        max_file_bytes: 50,
        max_total_bytes: 1000,
        max_ratio: 200,
        max_entries: 10,
    };
    let mut q = QuotaGuard::new(limits);
    q.entry_start().unwrap();
    // 条目内累计：raw 60 > 50 → 4501（同时未超 total → 验证顺序优先 4501）
    q.checkpoint(30, 15).unwrap();
    q.checkpoint(30, 15).unwrap();
    let e = q.check_now().unwrap_err();
    assert_eq!(code(&e), 4501, "4501 先于 4502");
}

#[test]
fn quota_kind_4502_total() {
    let limits = QuotaLimits {
        max_file_bytes: 10_000,
        max_total_bytes: 100,
        max_ratio: 200,
        max_entries: 10,
    };
    let mut q = QuotaGuard::new(limits);
    q.entry_start().unwrap();
    q.checkpoint(60, 30).unwrap();
    q.checkpoint(60, 30).unwrap(); // total 120 > 100，raw 120<10000 → 4502
    let e = q.check_now().unwrap_err();
    assert_eq!(code(&e), 4502);
}

#[test]
fn quota_ratio_zero_packed() {
    let limits = QuotaLimits {
        max_file_bytes: 10_000,
        max_total_bytes: 100_000,
        max_ratio: 200,
        max_entries: 10,
    };
    let mut q = QuotaGuard::new(limits);
    q.entry_start().unwrap();
    // packed=0 且 raw>0 → 视为 ∞ → 4503（一次触发不依赖 4MiB，经 check_now）
    q.checkpoint(5, 0).unwrap();
    let e = q.check_now().unwrap_err();
    assert_eq!(code(&e), 4503);
}

#[test]
fn quota_ratio_threshold() {
    let limits = QuotaLimits {
        max_file_bytes: 10_000_000,
        max_total_bytes: 100_000_000,
        max_ratio: 200,
        max_entries: 10,
    };
    let mut q = QuotaGuard::new(limits);
    q.entry_start().unwrap();
    // ratio = 1000/5 = 200 → 恰好等于上限不触发（> 语义）
    q.checkpoint(1000, 5).unwrap();
    assert!(q.check_now().is_ok(), "ratio == 200 不超");
    // ratio 201 触发
    let mut q2 = QuotaGuard::new(limits);
    q2.entry_start().unwrap();
    q2.checkpoint(1005, 5).unwrap();
    let e = q2.check_now().unwrap_err();
    assert_eq!(code(&e), 4503);
}

#[test]
fn quota_entries_bomb_4504() {
    let limits = QuotaLimits {
        max_file_bytes: u64::MAX,
        max_total_bytes: u64::MAX,
        max_ratio: 1000,
        max_entries: 5,
    };
    let mut q = QuotaGuard::new(limits);
    for _ in 0..5 {
        q.entry_start().unwrap();
    }
    let e = q.entry_start().unwrap_err(); // 第 6 条 → 4504
    assert_eq!(code(&e), 4504);
    assert_eq!(q.state().entries_seen, 6);
}

#[test]
fn quota_entry_scale_reset() {
    let limits = QuotaLimits {
        max_file_bytes: 100,
        max_total_bytes: 1_000_000,
        max_ratio: 200,
        max_entries: 10,
    };
    let mut q = QuotaGuard::new(limits);
    // 条目 A 写 80
    q.entry_start().unwrap();
    q.checkpoint(80, 40).unwrap();
    assert!(q.check_now().is_ok());
    // 条目 B 重新计数 → 不因 A 的 raw 超限误伤
    q.entry_start().unwrap();
    q.checkpoint(80, 40).unwrap();
    assert!(q.check_now().is_ok(), "条目尺度重置");
    // 但 total 累计：再大量写满 total
    assert!(q.state().total_done >= 160);
}

#[test]
fn quota_guard_state_access() {
    let mut g = QuotaGuard::new(QuotaLimits::default());
    g.entry_start().unwrap();
    g.checkpoint(4 * 1024 * 1024, 2 * 1024 * 1024).unwrap();
    assert_eq!(g.state().raw_done, 4 * 1024 * 1024);
    assert_eq!(g.state().total_done, 4 * 1024 * 1024);
    let _: &mut QuotaState = g.state_mut();
}
