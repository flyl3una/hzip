//! 恶意样本回放（02 §9.6；TC-SEC-001~008）。
//!
//! tests-assets/malicious 的 `manifest.json`（T02 产线约定）尚未产出——按任务卡
//! §6.3 以**内嵌等价字面量表**驱动 sanitize/quota（条目名与构造参数与样本族
//! 语义一致：slip-dotdot/slip-abs/slip-drive/slip-unc/slip-nul/slip-backslash、
//! ratio_bomb、total_bomb、count-100k/2m）。真实档案解析归 T32/T50 接入。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use hzip_core::security::quota::QuotaGuard;
use hzip_core::security::sanitize::SanitizedEntry;
use hzip_core::security::sanitize_entry_path;
use hzip_core::security::symlink::SymlinkGuard;
use hzip_error::HzipError;
use hzip_format::{EntryName, QuotaLimits};

/// 回放用例：条目名 + 期望（拒绝码或放行）。
struct Case {
    name: &'static str,
    expect: Expect,
}

enum Expect {
    /// 拒绝码（4001/4003/4004）
    Reject(u32),
    /// 放行且 rel 非空
    Ok,
}

const MALICIOUS_NAMES: &[Case] = &[
    // slip-dotdot.zip：../../etc/evil 族
    Case {
        name: "../../etc/evil",
        expect: Expect::Reject(4001),
    },
    Case {
        name: "a/../../../b",
        expect: Expect::Reject(4001),
    },
    Case {
        name: "../../../tmp/x",
        expect: Expect::Reject(4001),
    },
    // slip-backslash.zip：..\..\ 族
    Case {
        name: "..\\..\\evil.txt",
        expect: Expect::Reject(4001),
    },
    Case {
        name: "a\\..\\..\\x",
        expect: Expect::Reject(4001),
    },
    // slip-drive.zip：C:\ 盘符族
    Case {
        name: "C:/Windows/evil",
        expect: Expect::Reject(4001),
    },
    Case {
        name: "C:\\Windows\\evil",
        expect: Expect::Reject(4001),
    },
    // slip-unc.zip：\\server\share 族
    Case {
        name: "\\\\server\\share\\x",
        expect: Expect::Reject(4001),
    },
    Case {
        name: "//server/share/x",
        expect: Expect::Reject(4001),
    },
    // 绝对路径族
    Case {
        name: "/etc/passwd",
        expect: Expect::Reject(4001),
    },
    // slip-nul.zip：控制字符族
    Case {
        name: "a\x00b.txt",
        expect: Expect::Reject(4004),
    },
    Case {
        name: "a\x01b",
        expect: Expect::Reject(4004),
    },
    // 保留名族（win 拒绝；unix 放行——按 win 主验证判 4003）
    Case {
        name: "CON",
        expect: Expect::Reject(4003),
    },
    Case {
        name: "con.txt",
        expect: Expect::Reject(4003),
    },
    Case {
        name: "NUL.bin",
        expect: Expect::Reject(4003),
    },
    Case {
        name: "COM1",
        expect: Expect::Reject(4003),
    },
    Case {
        name: "lpt9.log",
        expect: Expect::Reject(4003),
    },
    // 正当条目（对照，不误伤）
    Case {
        name: "docs/readme.txt",
        expect: Expect::Ok,
    },
    Case {
        name: "bin/app.exe",
        expect: Expect::Ok,
    },
    Case {
        name: "CONtroller",
        expect: Expect::Ok,
    },
];

fn run_sanitize(raw: &str) -> Result<SanitizedEntry, HzipError> {
    sanitize_entry_path(
        &EntryName::new(raw.to_string()),
        Path::new("C:\\hzip-dest"),
        &hzip_format::SanitizeConfig::default(),
    )
}

/// TC-SEC-001~004：zip-slip / 绝对路径 / 保留名 / 控制字符 回放。
#[test]
fn replay_slip_and_absolute() {
    // 平台差异：保留名族 win→4003 / unix→放行（TC-SEC-003）。判据统一先收集错误码。
    for c in MALICIOUS_NAMES {
        let got = match run_sanitize(c.name) {
            Ok(s) => {
                assert!(!s.rel.as_os_str().is_empty(), "{}", c.name);
                0 // 放行
            }
            Err(e) => e.code(),
        };
        #[cfg(windows)]
        match c.expect {
            Expect::Reject(code) => assert_eq!(got, code, "{}", c.name),
            Expect::Ok => assert_eq!(got, 0, "{} 应放行", c.name),
        }
        #[cfg(not(windows))]
        match c.expect {
            // unix：保留名族放行；其余拒
            Expect::Reject(code) => {
                if is_reserved_name(c.name) {
                    assert_eq!(got, 0, "{} unix 保留名放行", c.name);
                } else {
                    assert_eq!(got, code, "{}", c.name);
                }
            }
            Expect::Ok => assert_eq!(got, 0, "{} 应放行", c.name),
        }
    }
}

#[cfg(not(windows))]
fn is_reserved_name(name: &str) -> bool {
    matches!(name, "CON" | "con.txt" | "NUL.bin" | "COM1" | "lpt9.log")
}

/// TC-SEC-005：高比率炸弹（ratio 500:1 > 200 → 4503）。
#[test]
fn replay_ratio_bomb() {
    let mut g = QuotaGuard::new(QuotaLimits::default());
    g.entry_start().unwrap();
    // 模拟流：解压 500 字节 / 读入 1 字节压缩（ratio 500 > 200）
    g.checkpoint(500, 1).unwrap();
    let e = g.check_now().unwrap_err();
    assert_eq!(e.code(), 4503);
}

/// TC-SEC-006：42 KB → 4 PB 经典炸弹（4502 总量超 128 GiB）。
#[test]
fn replay_total_bomb() {
    let mut g = QuotaGuard::new(QuotaLimits::default());
    g.entry_start().unwrap();
    // packed 42 KB，raw 4 PB：ratio 远超 200，但先按 total 判定语义——
    // 总量一旦超过 128 GiB 即 4502（条目尺度 raw 计数模拟）。
    // 4 PB > 128 GiB：单条目 raw 超 max_file(32 GiB) → 4501 更先。
    let four_pb = 4u64 * 1024 * 1024 * 1024 * 1024 * 1024;
    // 单次超大增量直接越 4 MiB 门控并触发 4501
    let e = g.checkpoint(four_pb, 42 * 1024).unwrap_err();
    assert_eq!(e.code(), 4501, "单条目 raw 4PB 先触发 4501");
    // 条目数尺度：模拟多条目各 <32GiB 累计超 total → 4502
    let mut g2 = QuotaGuard::new(QuotaLimits::default());
    let mut first: Option<u32> = None;
    for _ in 0..5 {
        g2.entry_start().unwrap();
        // 30 GiB 增量越 4 MiB 门控；ratio = 30GiB/30MiB ≈ 1024 > 200 → 4503 会先于 4502
        // 此处验证语义顺序：把 ratio 控在 <200 内以命中总量判定
        match g2.checkpoint(30 * 1024 * 1024 * 1024u64, 200 * 1024 * 1024u64) {
            Ok(()) => {}
            Err(e) => {
                first.get_or_insert(e.code());
                break;
            }
        }
    }
    // 30GiB/200MiB = ratio 150 < 200 → 总量累计 5×30GiB=150GiB > 128GiB → 4502
    assert_eq!(first, Some(4502), "总量累计超限报 4502");
}

/// TC-SEC-007：正当高压缩（比率 < 200）不误伤。
#[test]
fn replay_legit_high_compression() {
    let mut g = QuotaGuard::new(QuotaLimits::default());
    g.entry_start().unwrap();
    // 1 GiB 全零 deflate 实际 <1 MiB（ratio ≈ 1024）会误伤——按任务卡 §7.2
    // 决策用比率 < 200 的正当内容样本 + max_file/max_total 不误伤断言。
    g.checkpoint(190 * 1024 * 1024, 1024 * 1024).unwrap(); // ratio 190 < 200
    assert!(g.check_now().is_ok(), "ratio<200 正当高压缩不误伤");
    g.checkpoint(1024 * 1024, 1024 * 1024).unwrap();
    assert!(g.check_now().is_ok());
}

/// TC-SEC-008：条目数炸弹（10 万空条目全过；200 万样本第 100 万+1 → 4504）。
#[test]
fn replay_entry_count_bomb() {
    let limits = QuotaLimits {
        max_entries: 1_000_000,
        ..Default::default()
    };
    // 10 万空条目：全过（模拟抽样）
    let mut g = QuotaGuard::new(limits);
    for _ in 0..100_000 {
        g.entry_start().unwrap();
    }
    assert_eq!(g.state().entries_seen, 100_000);
    // 到 100 万+1 → 4504
    let mut g2 = QuotaGuard::new(limits);
    for _ in 0..1_000_000 {
        g2.entry_start().unwrap();
    }
    let e = g2.entry_start().unwrap_err();
    assert_eq!(e.code(), 4504);
}

/// TC-SEC-009（core 半场）：链接台账 + 后续条目落入链接之下组合攻击回放。
#[test]
fn replay_symlink_combo() {
    // tar: ln -s / pwn（绝对目标 → 默认拒绝 4002，链接未建）
    let g = SymlinkGuard::new(hzip_format::SymlinkPolicy::DenyEscape);
    assert_eq!(
        hzip_core::security::symlink::LinkAction::Deny,
        g.decide("/pwn")
    );
    // 即便内链创建成功，后续 pwn/etc/evil 落入其下 → 4002
    let mut g2 = SymlinkGuard::new(hzip_format::SymlinkPolicy::DenyEscape);
    g2.register_created(std::path::PathBuf::from("pwn"));
    let e = g2
        .check_under_linked(std::path::Path::new("pwn/etc/evil"))
        .expect_err("组合攻击必须拒绝");
    assert_eq!(e.code(), 4002);
}
