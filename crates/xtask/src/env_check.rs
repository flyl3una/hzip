//! env-check：开发环境自检（04 §10），8 项检查 + 汇总退出码。
//! 级别：FAIL（任一 → 非零退出码）/ WARN（提示不阻塞）/ SKIP（本平台不适用）。

use std::process::{Command, ExitCode};

const MSRV: (u32, u32) = (1, 85);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Pass => "PASS",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
            Level::Skip => "SKIP",
        }
    }
}

struct Row {
    no: u32,
    item: &'static str,
    level: Level,
    detail: String,
}

impl Row {
    fn new(no: u32, item: &'static str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            no,
            item,
            level,
            detail: detail.into(),
        }
    }
}

/// 运行命令并取 stdout 首个非空行；命令不存在或退出码非零 → None。
fn probe(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string(),
    )
}

/// 从 `rustc 1.88.0 (hash date)` 解析 semver 并与 MSRV 比较。
fn rustc_at_least(text: &str) -> bool {
    let num = text
        .split_whitespace()
        .find(|t| t.contains('.'))
        .unwrap_or("0.0");
    let mut parts = num.split('.');
    let major: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let minor: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    (major, minor) >= MSRV
}

// —— 1. rustc ≥ MSRV ——
fn check_rustc() -> Row {
    const ITEM: &str = "rustc ≥ MSRV 1.85";
    match probe("rustc", &["--version"]) {
        Some(v) if rustc_at_least(&v) => Row::new(1, ITEM, Level::Pass, v),
        Some(v) => Row::new(1, ITEM, Level::Fail, format!("{v}（低于 MSRV）")),
        None => Row::new(1, ITEM, Level::Fail, "PATH 中未找到 rustc"),
    }
}

// —— 2. cargo 可用 ——
fn check_cargo() -> Row {
    const ITEM: &str = "cargo 可用";
    match probe("cargo", &["--version"]) {
        Some(v) => Row::new(2, ITEM, Level::Pass, v),
        None => Row::new(2, ITEM, Level::Fail, "PATH 中未找到 cargo"),
    }
}

// —— 3. rustfmt / clippy 组件 ——
fn check_components() -> Row {
    const ITEM: &str = "rustfmt / clippy 组件";
    let fmt = probe("cargo", &["fmt", "--version"]);
    let clippy = probe("cargo", &["clippy", "--version"]);
    let detail = format!(
        "{}; {}",
        fmt.clone().unwrap_or_else(|| "rustfmt 缺失".to_string()),
        clippy.clone().unwrap_or_else(|| "clippy 缺失".to_string()),
    );
    let level = if fmt.is_some() && clippy.is_some() {
        Level::Pass
    } else {
        Level::Fail
    };
    Row::new(3, ITEM, level, detail)
}

// —— 4. C/C++ 编译器（cc crate 编译 xz2/bzip2/zstd/unrar C 源，04 §1）——
#[cfg(windows)]
fn detect_c_compiler() -> Option<String> {
    if let Some(path) = probe("where.exe", &["cl"]) {
        return Some(format!(
            "cl.exe（PATH）：{}",
            path.lines().next().unwrap_or("")
        ));
    }
    // 与 cc crate 同路径：vswhere 定位 VS Build Tools 内 cl.exe（dev prompt 非必需）。
    let vswhere = std::path::PathBuf::from(
        r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe",
    );
    let out = Command::new(&vswhere)
        .args(["-latest", "-products", "*", "-property", "installationPath"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let install = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .to_string();
    let mut versions: Vec<std::path::PathBuf> =
        std::fs::read_dir(std::path::Path::new(&install).join(r"VC\Tools\MSVC"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
    versions.sort();
    let cl = versions
        .last()
        .map(|dir| dir.join(r"bin\Hostx64\x64\cl.exe"))
        .filter(|cl| cl.exists())?;
    Some(format!("cl.exe（vswhere）：{}", cl.display()))
}

#[cfg(not(windows))]
fn detect_c_compiler() -> Option<String> {
    for prog in ["cc", "gcc", "clang"] {
        if let Some(v) = probe(prog, &["--version"]) {
            return Some(format!("{prog}：{v}"));
        }
    }
    None
}

fn check_c_compiler() -> Row {
    const ITEM: &str = "C/C++ 编译器（cc crate，04 §1）";
    match detect_c_compiler() {
        Some(v) => Row::new(4, ITEM, Level::Pass, v),
        None => Row::new(
            4,
            ITEM,
            Level::Fail,
            "未找到 cl.exe / cc / gcc / clang（Windows 需 VS Build Tools，04 §2.1）",
        ),
    }
}

// —— 5. pkg-config（非 Windows；04 §2.2）——
#[cfg(windows)]
fn check_pkg_config() -> Row {
    Row::new(
        5,
        "pkg-config（非 Windows）",
        Level::Skip,
        "Windows 跳过（04 §10 项 5）",
    )
}

#[cfg(not(windows))]
fn check_pkg_config() -> Row {
    const ITEM: &str = "pkg-config";
    match probe("pkg-config", &["--version"]) {
        Some(v) => Row::new(5, ITEM, Level::Pass, v),
        None => Row::new(5, ITEM, Level::Fail, "PATH 中未找到 pkg-config（04 §2.2）"),
    }
}

// —— 6. 7z CLI（交叉测试基准，04 §10；W1 阶段仅交叉测试需要 → WARN）——
fn check_sevenz() -> Row {
    const ITEM: &str = "7z CLI（交叉测试基准）";
    match probe("7z", &["i"]) {
        Some(v) => Row::new(6, ITEM, Level::Pass, v),
        None => Row::new(
            6,
            ITEM,
            Level::Warn,
            "7z 不在 PATH（仅 W1 交叉测试需要，04 §10）",
        ),
    }
}

// —— 7. python3（tools 脚本宿主；Windows 回退 python 启动器）——
fn check_python() -> Row {
    const ITEM: &str = "python3（tools 脚本宿主）";
    match probe("python3", &["--version"]).or_else(|| probe("python", &["--version"])) {
        Some(v) => Row::new(7, ITEM, Level::Pass, v),
        None => Row::new(
            7,
            ITEM,
            Level::Warn,
            "未找到 python3/python（build/checks/deps-direction.py 需要，02 §2.1）",
        ),
    }
}

// —— 8. git core.longpaths（仅 Windows，04 §2.1：仓库含 260+ 字符路径测试资产）——
#[cfg(windows)]
fn check_longpaths() -> Row {
    const ITEM: &str = "git core.longpaths（Windows）";
    let val = Command::new("git")
        .args(["config", "--get", "core.longpaths"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if val == "true" {
        Row::new(8, ITEM, Level::Pass, "core.longpaths = true")
    } else {
        Row::new(
            8,
            ITEM,
            Level::Warn,
            format!("当前值：'{val}'（建议 git config --global core.longpaths true，04 §2.1）"),
        )
    }
}

#[cfg(not(windows))]
fn check_longpaths() -> Row {
    Row::new(
        8,
        "git core.longpaths（仅 Windows）",
        Level::Skip,
        "非 Windows 跳过（04 §10 项 8）",
    )
}

/// 逐项探测 → 表格输出 → 任一 FAIL 返回 FAILURE。
pub(crate) fn run() -> ExitCode {
    let rows = vec![
        check_rustc(),
        check_cargo(),
        check_components(),
        check_c_compiler(),
        check_pkg_config(),
        check_sevenz(),
        check_python(),
        check_longpaths(),
    ];
    println!("{:<4}{:<46}{:<8}详情", "编号", "检查项", "级别");
    for r in &rows {
        println!(
            "{:<4}{:<46}{:<8}{}",
            r.no,
            r.item,
            r.level.label(),
            r.detail
        );
    }
    let fails = rows.iter().filter(|r| r.level == Level::Fail).count();
    let warns = rows.iter().filter(|r| r.level == Level::Warn).count();
    println!("汇总：{} 项 / FAIL {fails} / WARN {warns}", rows.len());
    if fails > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
