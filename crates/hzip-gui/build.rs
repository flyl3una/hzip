//! 构建脚本：把 `locales/` 纳入 cargo 的**重编译依赖**。
//!
//! # 为什么需要
//!
//! `rust_i18n::i18n!("locales", …)`（见 `src/lib.rs`）在**编译期**把 TOML 文案
//! 展开成字面量表，但**不向 cargo 声明对源 TOML 的依赖**。后果：改完文案直接
//! `cargo build` 会命中旧缓存，**旧文案静默上线**——实测踩到：改了
//! `ctxmenu.add_archive` 后菜单里仍是旧文案，必须先 `touch` 一个 `.rs` 才更新。
//!
//! 这类「改了没生效」的静默错误极难查（源码明明是对的），故在此显式声明依赖。
//! 任何新增的 locale 文件都自动覆盖（遍历目录，不写死名单）。
//!
//! 注：这是**构建正确性**守卫，不是功能代码——`cargo:rerun-if-changed` 只影响
//! 何时重编译，不改变产物内容。

fn main() {
    derive_license();
    stage_shell_ext();
    embed_windows_icon();
    println!("cargo:rerun-if-changed=locales");
    println!("cargo:rerun-if-changed=build.rs");
    // 目录缺失由 `i18n!` 宏自身报错（更早、更明确），此处不重复报错
    if let Ok(entries) = std::fs::read_dir("locales") {
        for entry in entries.flatten() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
}

/// 把 `hzip.rc` 编译进 exe 资源段（程序图标）。
///
/// # 为什么资源 ID 必须是 1
///
/// 三处取用**都**指向「exe 里 ID 最低的图标组」：
///
/// 1. **窗口/任务栏/Alt-Tab**：gpui 的 Windows 后端用
///    `LoadImageW(GetModuleHandleW(NULL), MAKEINTRESOURCE(1), IMAGE_ICON, …)`
///    取窗口类图标（gpui-pre-windows `platform.rs::load_icon`，注册 WNDCLASS 时
///    作为 `hIcon`）。没有这条资源时拿到的是空 HICON → 标题栏、任务栏、Alt-Tab
///    全是系统默认图标——**这正是加图标前的现状**。
/// 2. **资源管理器里的 exe 图标**：shell 取 exe 的最低 ID 图标组。
/// 3. **右键菜单项图标**：`hzip-platform::context_menu` 经典注册写
///    `Icon = "{exe},0"`；`hzip-shell` 的 `IExplorerCommand::GetIcon` 同样返回
///    `"{exe},0"`——都取该 exe 的图标索引 0（= 最低 ID 组）。
///
/// # 为什么用 embed-resource 而不是自带 rc.exe 调用
///
/// 它已在依赖树里（`gpui-pre` 的 build-dependency，同版本复用），且自带
/// rc.exe 定位（`RC` / `RC_<target>` 环境变量 → Windows Kits 注册表 → PATH）。
/// 它把编译命令的工作目录设为 `.rc` 所在目录，故 `hzip.rc` 里的
/// `"assets/logo/hzip.ico"` 相对 crate 根解析。
///
/// 非 Windows 目标上 `compile` 是 no-op（`CompilationResult::NotWindows`），
/// 交叉编译到 Windows 时走 windres/llvm-rc。
fn embed_windows_icon() {
    const RC: &str = "hzip.rc";
    const ICO: &str = "assets/logo/hzip.ico";
    // 图标换图后必须重新内嵌（否则 exe 里还是旧图标）
    println!("cargo:rerun-if-changed={RC}");
    println!("cargo:rerun-if-changed={ICO}");
    let _ = embed_resource::compile(RC, embed_resource::NONE);
}

/// 把 shell 扩展产物（`hzip-shell.dll` + `hzip-shellmenu.msix`）落到 **exe 同级**。
///
/// # 为什么必须落位
///
/// 「应用到系统」时 `context_menu` 的 `validate_spec` 要求 DLL 与 exe 同目录
/// （经典注册写的是 `<exe 目录>\hzip-shell.dll` 的 COM 服务器路径；稀疏包的
/// manifest 同源）。而 shell 扩展是**独立 crate**（`shell/hzip-shell`，自带
/// target 目录），由 `build/shell-ext/build-shell-ext.ps1` 产出到
/// `shell/hzip-shell/target/shell-ext/`——直接 `cargo build` 出来的
/// `target/<profile>/hzip-gui.exe` 旁边**没有**这两个文件，于是设置页点「应用
/// 到系统」必然失败：
///
/// ```text
/// 右键菜单安装失败: Spec: Shell 扩展未随程序安装：<…>\hzip-shell.dll
/// ```
///
/// 用户看到的就是「点应用没反应/系统里没有 hzip 项」。发行包由打包脚本拷到
/// 安装目录（原本正常），这条只管**开发运行**：产物存在就复制，不存在就跳过
/// （shell 扩展从未构建过的机器仍可正常编译 GUI，安装时才给出可操作的报错）。
///
/// 落位目录从 `OUT_DIR` 反推（`target/<profile>/build/<pkg>-<hash>/out` →
/// `target/<profile>`），因此 `CARGO_TARGET_DIR` 重定位也正确。
fn stage_shell_ext() {
    const ARTIFACTS: [&str; 2] = ["hzip-shell.dll", "hzip-shellmenu.msix"];
    let manifest =
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let src_dir = manifest
        .join("../../shell/hzip-shell/target/shell-ext")
        .canonicalize()
        .unwrap_or_else(|_| manifest.join("../../shell/hzip-shell/target/shell-ext"));
    let out_dir = match std::env::var("OUT_DIR") {
        Ok(v) => std::path::PathBuf::from(v),
        Err(_) => return,
    };
    // out → <pkg>-<hash> → build → <profile>
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    for name in ARTIFACTS {
        let src = src_dir.join(name);
        // 产物变化要触发重编译：否则复制的是上一版 DLL（右键菜单加载旧扩展）
        println!("cargo:rerun-if-changed={}", src.display());
        if src.is_file() {
            let _ = std::fs::copy(&src, profile_dir.join(name));
        }
    }
}

/// 从仓库根 `LICENSE` 派生许可信息，写成 `OUT_DIR/license.rs` 供 `ui::dialogs::about`
/// `include!`。**「关于」页的许可文字不再是硬编码字符串**。
///
/// # 为什么在编译期派生而不是写死
///
/// 此前「关于」页写死 `"MIT OR Apache-2.0"`——换许可时改一处、忘另一处就会**静默
/// 漂移**（界面显示旧许可，编译却照过）。改为从**正文**派生，并在编译期把它与
/// `CARGO_PKG_LICENSE`（`Cargo.toml` 的 `license` 字段）**交叉校验**：任一方变化
/// 不匹配即**构建失败**，漂移不可能落进产物。
///
/// # 派生规则（与 GPL-3.0 正文逐字对齐）
///
/// 正文（去行首缩进、去空行后）首行 = `GNU GENERAL PUBLIC LICENSE`，次行 =
/// `Version 3, 29 June 2007`。由此取 `NAME`/`VERSION`(`3`)/`DATED`(`29 June 2007`)；
/// `SPDX` 取自清单；`DISPLAY` 由 `NAME` 词首字母大写 + ` v{version}` 拼出（`GNU`
/// 等 ≤3 字母的全大写缩写保留原样 → `GNU General Public License v3`）。
fn derive_license() {
    const LICENSE: &str = "../../LICENSE";
    // 改正文即重编（否则命中缓存，产物里仍是旧派生值）——与 locales 同款守卫
    println!("cargo:rerun-if-changed={LICENSE}");
    // 清单侧也必须声明：`CARGO_PKG_LICENSE` 变而正文不变时，若本脚本不重跑，
    // 下面的交叉校验会被 cargo 缓存整段跳过（实测抓到：把清单改成 GPL-2.0-only 后
    // `cargo check` 仍然通过 ⇒ 门禁形同虚设）。
    println!("cargo:rerun-if-changed=../../Cargo.toml");

    let text = std::fs::read_to_string(LICENSE)
        .unwrap_or_else(|e| panic!("仓库根 LICENSE 缺失或不可读（{LICENSE}）：{e}"));

    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let name = lines.next().unwrap_or_default();
    if name != "GNU GENERAL PUBLIC LICENSE" {
        panic!(
            "LICENSE 不是 GPL-3.0 正文字样：首行应为 `GNU GENERAL PUBLIC LICENSE`，实际 `{name}`"
        );
    }
    let version_line = lines.next().unwrap_or_default();
    let rest = version_line.strip_prefix("Version ").unwrap_or_else(|| {
        panic!("LICENSE 不是 GPL-3.0 正文字样：次行应为 `Version 3, 29 June 2007`，实际 `{version_line}`")
    });
    let (version, dated) = rest
        .split_once(',')
        .unwrap_or_else(|| panic!("LICENSE 次行缺少 `,` 分隔的发布日期：`{version_line}`"));
    let version = version.trim();
    let dated = dated.trim();

    // 清单与正文交叉校验：防「Cargo.toml 写 GPL-3.0 而正文其实是别的版本」的漂移
    let spdx = std::env::var("CARGO_PKG_LICENSE").unwrap_or_default();
    let base = spdx
        .strip_suffix("-only")
        .or_else(|| spdx.strip_suffix("-or-later"))
        .unwrap_or(spdx.as_str());
    if base != "GPL-3.0" {
        panic!(
            "清单与 LICENSE 正文漂移：Cargo.toml license=`{spdx}`（主体 `{base}`）与 GPL-3.0 正文不符"
        );
    }

    let display = display_name(name, version);
    // 仓库策略 deny `expect_used`（Cargo.toml [workspace.lints.clippy]）⇒ 用 let-else 表达。
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        panic!("build 脚本应在 OUT_DIR 下运行");
    };
    let out = std::path::PathBuf::from(out_dir).join("license.rs");
    // 用 `{:?}` 生成合法的 Rust 字符串字面量（自动转义）
    let rs = format!(
        "// 由 build.rs::derive_license 从仓库根 LICENSE 派生——勿手改（改 LICENSE 自动重生成）。\n\
         pub const SPDX: &str = {spdx:?};\n\
         pub const NAME: &str = {name:?};\n\
         pub const VERSION: &str = {version:?};\n\
         pub const DATED: &str = {dated:?};\n\
         pub const DISPLAY: &str = {display:?};\n"
    );
    std::fs::write(&out, rs).unwrap_or_else(|e| panic!("写 {} 失败：{e}", out.display()));
}

/// `GNU GENERAL PUBLIC LICENSE` + `3` → `GNU General Public License v3`。
///
/// ≤3 字母的全大写词（`GNU`）视为缩写原样保留；其余词首字母大写、其余小写。
fn display_name(name: &str, version: &str) -> String {
    let mut out = String::new();
    for word in name.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        if word.len() <= 3 {
            out.push_str(word);
        } else {
            let lower = word.to_lowercase();
            let mut chars = lower.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    format!("{out} v{version}")
}
