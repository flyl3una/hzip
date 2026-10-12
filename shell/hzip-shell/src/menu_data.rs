//! 菜单配置读取（DLL ↔ GUI 的唯一通道）。
//!
//! # 为什么用文件而不是注册表
//!
//! 勾选项由 GUI 的「应用」按钮写入 `%APPDATA%\hzip\shell-menu.json`。DLL **每次
//! 构建菜单时**读它，于是：
//!
//! - 增删勾选项**不需要**改注册表、**不需要**重注册稀疏包、**不需要**重启 Explorer；
//! - DLL 不硬编码任何文案，标签随 GUI 当前语言落盘（`GetTitle` 直接用）；
//! - 与 `hzip-platform::context_menu::menu_config_path()` 同址（契约测试互锁）。
//!
//! # 容错
//!
//! Explorer 进程内**绝不能**因为配置坏掉而抛异常/panic：任何解析失败都退化为
//! 「空菜单」（`GetState` 返回 `ECS_HIDDEN`，条目整体不出现），并保持静默。
//! 这是刻意的——用户在设置页点「应用」之前，菜单就不该出现。

use std::path::PathBuf;

use serde_json::Value;

/// 单个菜单条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// verb key（落盘态，永不翻译）。
    pub key: String,
    /// 已本地化文案。
    pub label: String,
    /// 命令参数（含 `%SEL` / `%SELS` 占位符）。
    pub args: Vec<String>,
}

/// 按目标类型分组的完整配置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuConfig {
    /// `hzip-gui.exe` 绝对路径。
    pub exe: String,
    /// 已关联的档案扩展名（小写、不带点）。
    pub archive_exts: Vec<String>,
    /// 档案文件上的条目（解压类）。
    pub archive_file: Vec<Item>,
    /// 任意文件 / 文件夹上的条目（压缩类）。
    pub any: Vec<Item>,
}

impl MenuConfig {
    /// 是否可用（有 exe 且至少一组条目）。
    pub fn is_usable(&self) -> bool {
        !self.exe.is_empty() && (!self.archive_file.is_empty() || !self.any.is_empty())
    }

    /// 目标是否属于「已关联档案」（决定解压类条目是否出现）。
    pub fn is_archive_path(&self, path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        self.archive_exts
            .iter()
            .any(|ext| lower.ends_with(&format!(".{}", ext.to_ascii_lowercase())))
    }
}

/// 配置文件路径（`%APPDATA%\hzip\shell-menu.json`）。
///
/// **刻意不经环境变量覆写**：DLL 运行在 Explorer / COM 代理进程内，读不到调用方的
/// 环境，任何 `HZIP_CONFIG` 之类的约定在这里都不成立。
///
/// # 为什么用 `SHGetKnownFolderPath` 而不是 `env::var("APPDATA")`
///
/// 稀疏包路径下本 DLL 由 **COM 代理进程 `dllhost.exe`** 加载（实测确认），
/// 该进程由 shell 基础设施启动，**环境块可能不完整**——`APPDATA` 缺失时
/// [`load`] 返回 `None`，进而 `is_usable()` 为假、整棵菜单被 `ECS_HIDDEN` 隐藏。
/// 症状是「包注册成功、DLL 也被加载了，但右键里什么都没有」，极难从外部判断。
///
/// Win32 已知文件夹 API 不依赖进程环境，因此是唯一可靠的方式；环境变量仅作兜底。
pub fn config_path() -> Option<PathBuf> {
    let base = known_roaming_appdata()?;
    Some(base.join("hzip").join("shell-menu.json"))
}

/// `FOLDERID_RoamingAppData`（即 `%APPDATA%`），不依赖进程环境。
fn known_roaming_appdata() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    };

    // `windows` crate 的安全包装：3 个入参、返回 `Result<PWSTR>`
    // （缓冲仍由 shell 经 CoTaskMem 分配，须显式释放）
    // SAFETY: 无外部指针入参；htoken=None 表示当前用户
    let raw =
        unsafe { SHGetKnownFolderPath(&FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, None) }.ok()?;
    if raw.is_null() {
        return None;
    }
    // SAFETY: shell 返回以 NUL 结尾的宽字符串
    let s = unsafe { raw.to_string() }.ok();
    // SAFETY: 所有权归调用方，必须释放
    unsafe { CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void)) };
    s.map(PathBuf::from)
}

/// 读取配置（任何失败 → `None`，由调用方退化为隐藏菜单）。
pub fn load() -> Option<MenuConfig> {
    let Some(path) = config_path() else {
        crate::trace::trace("config_path=None");
        return None;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        crate::trace::trace(&format!("config_read_failed path={}", path.display()));
        return None;
    };
    let cfg = parse(&text);
    // 同时记录环境变量是否存在——用于判定「旧写法（env::var）在 COM 代理进程里
    // 是否会失败」这一根因，而不必靠推理。
    crate::trace::trace(&format!(
        "config_loaded path={} usable={} appdata_env={}",
        path.display(),
        cfg.as_ref().is_some_and(MenuConfig::is_usable),
        if std::env::var_os("APPDATA").is_some() {
            "present"
        } else {
            "ABSENT"
        }
    ));
    cfg
}

/// 解析配置文本（纯函数，便于单测）。
pub fn parse(text: &str) -> Option<MenuConfig> {
    let doc: Value = serde_json::from_str(text).ok()?;
    if doc.get("schema").and_then(Value::as_u64) != Some(1) {
        return None; // 未知 schema：宁可不出菜单，也不误读
    }
    let exe = doc.get("exe").and_then(Value::as_str)?.to_string();
    let archive_exts = doc
        .get("archive_exts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim_start_matches('.').to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();

    let items = doc.get("items");
    let group = |name: &str| -> Vec<Item> {
        items
            .and_then(|i| i.get(name))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(parse_item).collect())
            .unwrap_or_default()
    };

    Some(MenuConfig {
        exe,
        archive_exts,
        archive_file: group("archive_file"),
        any: group("any"),
    })
}

/// 解析单个条目（缺 `key`/`label` 即丢弃该条，不整体失败）。
fn parse_item(v: &Value) -> Option<Item> {
    let key = v.get("key").and_then(Value::as_str)?.to_string();
    let label = v.get("label").and_then(Value::as_str)?.to_string();
    let args = v
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Item { key, label, args })
}

/// 把选中项展开成实际的 argv 列表（`%SEL` 逐项一次 / `%SELS` 一次收全部）。
///
/// 与 Linux 侧 `script.rs`、macOS 侧 `_expand` 保持**同一语义**——三平台的
/// 多选行为必须一致，否则同一份勾选在不同系统表现不同。
pub fn expand_args(args: &[String], paths: &[String]) -> Vec<Vec<String>> {
    let per_file = args.iter().any(|a| a == "%SEL");
    if per_file {
        return paths
            .iter()
            .map(|p| {
                args.iter()
                    .map(|a| {
                        if a == "%SEL" || a == "%SELS" {
                            p.clone()
                        } else {
                            a.clone()
                        }
                    })
                    .collect()
            })
            .collect();
    }
    let mut one: Vec<String> = Vec::with_capacity(args.len() + paths.len());
    for a in args {
        if a == "%SELS" {
            one.extend(paths.iter().cloned());
        } else {
            one.push(a.clone());
        }
    }
    if paths.is_empty() {
        Vec::new()
    } else {
        vec![one]
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const SAMPLE: &str = r#"{
      "schema": 1,
      "exe": "C:\\Program Files\\hzip\\hzip-gui.exe",
      "archive_exts": ["zip", "7Z"],
      "items": {
        "archive_file": [
          {"key":"extract_here","label":"解压到当前文件夹","args":["--extract-here","%SEL"]}
        ],
        "any": [
          {"key":"add_zip","label":"添加到 zip","args":["--compress","%SELS","--format","zip"]},
          {"key":"add_7z","label":"添加到 7z","args":["--compress","%SELS","--format","7z"]}
        ]
      }
    }"#;

    #[test]
    fn parses_full_config() {
        let c = parse(SAMPLE).expect("样例须可解析");
        assert_eq!(c.exe, r"C:\Program Files\hzip\hzip-gui.exe");
        // 扩展名归一：去点 + 小写
        assert_eq!(c.archive_exts, vec!["zip", "7z"]);
        assert_eq!(c.archive_file.len(), 1);
        assert_eq!(c.archive_file[0].key, "extract_here");
        assert_eq!(c.archive_file[0].args, vec!["--extract-here", "%SEL"]);
        assert_eq!(c.any.len(), 2);
        assert_eq!(c.any[0].key, "add_zip");
        assert!(c.is_usable());
    }

    #[test]
    fn unknown_schema_is_rejected_so_menu_stays_hidden() {
        let bad = SAMPLE.replace("\"schema\": 1", "\"schema\": 99");
        assert_eq!(parse(&bad), None);
        let missing = r#"{"exe":"x","items":{}}"#;
        assert_eq!(parse(missing), None);
    }

    #[test]
    fn malformed_json_and_missing_exe_yield_none_not_panic() {
        assert_eq!(parse("{ not json"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse(r#"{"schema":1,"items":{}}"#), None, "缺 exe 不可用");
    }

    #[test]
    fn one_bad_item_is_skipped_without_killing_the_group() {
        let text = r#"{"schema":1,"exe":"e","items":{"any":[
            {"label":"no key"},
            {"key":"add_zip","label":"ok","args":["--compress","%SELS"]}
        ]}}"#;
        let c = parse(text).expect("应解析成功");
        assert_eq!(c.any.len(), 1);
        assert_eq!(c.any[0].key, "add_zip");
    }

    #[test]
    fn empty_groups_make_config_unusable() {
        let text = r#"{"schema":1,"exe":"e","items":{"any":[]}}"#;
        let c = parse(text).expect("应解析成功");
        assert!(!c.is_usable(), "无条目时不应出菜单");
    }

    /// 解压组可见性 = 「这个路径是不是档案」，由 `archive_exts` 后缀匹配决定。
    ///
    /// 这是**用户可见后果最重**的一处判定：判错则右键档案时解压项整组消失
    /// （实测缺陷：`archive_exts` 上游被错填成「文件关联勾选」，`assoc_formats=["7z"]`
    /// 的机器上右键 `.zip` 只剩压缩项、解压项凭空消失）。
    #[test]
    fn archive_detection_is_case_insensitive_and_extension_scoped() {
        let c = parse(SAMPLE).unwrap();
        assert!(c.is_archive_path(r"C:\a\b.ZIP"), "大小写不敏感");
        assert!(c.is_archive_path(r"D:\x\y.7z"));
        assert!(
            c.is_archive_path(r"C:\a\backup.tar.7z"),
            "多段扩展名看最后一段"
        );
        assert!(!c.is_archive_path(r"C:\a\b.txt"));
        // 不得把后缀包含误判为扩展名匹配（必须是点分隔的扩展名）
        assert!(!c.is_archive_path(r"C:\a\notzip"));
        assert!(!c.is_archive_path(r"C:\a\b.zip.txt"));
        // 不在能力集里的扩展名：rar 由 core 的 `rar` feature 决定是否入集，
        // 本构建未启用 → 不认（认了会对着无法解压的档案露出解压项）
        assert!(!c.is_archive_path(r"C:\a\b.rar"));
        // 无扩展名 / 空串 / 目录名恰好像档案
        assert!(!c.is_archive_path(r"C:\a\README"));
        assert!(!c.is_archive_path(""));
        assert!(!c.is_archive_path(r"C:\a\b.zip\inner.txt"));
    }

    #[test]
    fn per_file_expansion_launches_once_per_selection() {
        let args = vec!["--extract-here".to_string(), "%SEL".to_string()];
        let paths = vec!["a.zip".to_string(), "b.zip".to_string()];
        let out = expand_args(&args, &paths);
        assert_eq!(
            out,
            vec![
                vec!["--extract-here".to_string(), "a.zip".to_string()],
                vec!["--extract-here".to_string(), "b.zip".to_string()],
            ]
        );
    }

    #[test]
    fn one_shot_expansion_passes_all_paths_as_separate_argv() {
        let args = vec![
            "--compress".to_string(),
            "%SELS".to_string(),
            "--format".to_string(),
            "zip".to_string(),
        ];
        let paths = vec!["a.txt".to_string(), "b c.txt".to_string(), "d".to_string()];
        let out = expand_args(&args, &paths);
        assert_eq!(
            out,
            vec![vec![
                "--compress".to_string(),
                "a.txt".to_string(),
                "b c.txt".to_string(),
                "d".to_string(),
                "--format".to_string(),
                "zip".to_string(),
            ]],
            "空格路径必须作为独立 argv 传递，不得拼接"
        );
    }

    #[test]
    fn empty_selection_yields_no_launch() {
        let args = vec!["--compress".to_string(), "%SELS".to_string()];
        assert!(expand_args(&args, &[]).is_empty());
    }

    /// 路径必须能解析、且形状为 `...\hzip\shell-menu.json`。
    ///
    /// # 这条测的边界（诚实说明）
    ///
    /// 「不依赖 `APPDATA` 环境变量」这一点**无法在单测里证伪**：测试进程必然有该
    /// 变量，而 COM 代理进程（`dllhost.exe`）才可能缺它。故此处只守路径**形状**；
    /// 环境无关性由实现结构保证（走 Win32 已知文件夹 API，见 `known_roaming_appdata`），
    /// 权威证据是**真机**上 `dllhost.exe` 加载本 DLL 后菜单项正常出现。
    /// 这里不做 `env::set_var`（在并行测试进程里改动全局环境是未定义行为，
    /// 会变成 CI flaky 源）。
    #[test]
    fn config_path_has_expected_shape() {
        let p = config_path().expect("已知文件夹 API 应能解析 RoamingAppData");
        assert_eq!(p.file_name().unwrap(), "shell-menu.json");
        assert_eq!(p.parent().unwrap().file_name().unwrap(), "hzip");
        assert!(p.is_absolute(), "必须是绝对路径：{}", p.display());
    }

    #[test]
    fn config_path_is_under_hzip_in_appdata() {
        // 环境无 APPDATA（非 Windows 测试沙箱）时允许 None
        if let Some(p) = config_path() {
            assert_eq!(p.file_name().unwrap(), "shell-menu.json");
            assert_eq!(p.parent().unwrap().file_name().unwrap(), "hzip");
        }
    }
}
