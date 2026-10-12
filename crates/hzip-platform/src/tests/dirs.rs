use super::*;

#[test]
fn all_nonempty() {
    let cfg = config_dir();
    let data = data_dir();
    let cache = cache_dir();
    let log = log_dir();
    let tmp = temp_root();
    for (name, p) in [
        ("config", &cfg),
        ("data", &data),
        ("cache", &cache),
        ("log", &log),
        ("temp", &tmp),
    ] {
        assert!(!p.as_os_str().is_empty(), "{name} 目录非空");
        assert!(p.is_absolute(), "{name} 目录绝对: {p:?}");
    }
    // 互不混淆。macOS 上 config 与 data 同为 `~/Library/Application Support/hzip`
    // （模块文档两行同址；`dirs::data_dir()` 即 App Support），是**平台约定**
    // 而非缺陷——故该平台允许这一对相同，其余两两仍须互异。
    let mut v = vec![cfg, data, cache, log, tmp];
    v.sort();
    v.dedup();
    #[cfg(target_os = "macos")]
    assert_eq!(v.len(), 4, "macOS：config/data 同址，其余应互异");
    #[cfg(not(target_os = "macos"))]
    assert_eq!(v.len(), 5, "五类目录应互异");
}

#[test]
#[cfg(windows)]
fn win_env_override() {
    // TEMP 覆盖生效（GetTempPath 读取 TEMP/TMP）
    // 注：设置环境变量影响子进程；此处仅验证 temp_root 与 TEMP 一致
    let t = std::env::temp_dir();
    assert!(temp_root().starts_with(t), "temp_root 基于 GetTempPath");
}

#[test]
fn desktop_dir_is_not_hzip_subdir() {
    // 契约：桌面是用户自有目录，`desktop_dir` **不得**拼 `hzip` 子目录
    // （与 config_dir 等 hzip 自有目录不同）。取不到桌面时无路径可验，跳过。
    let Some(desktop) = desktop_dir() else {
        return;
    };
    assert_ne!(
        desktop.file_name(),
        Some(std::ffi::OsStr::new("hzip")),
        "desktop_dir 末级组件不应为 hzip：{desktop:?}"
    );
}

#[test]
#[cfg(windows)]
fn desktop_dir_matches_userprofile_desktop() {
    // Windows 上桌面由 Known Folder API 解析，通常即 `%USERPROFILE%\Desktop`。
    // 前置条件任一不满足（无 USERPROFILE / 目录不存在 / 取不到桌面）即跳过：
    // 桌面可被重定向（注册表 Shell Folders），那不是缺陷，不该 fail。
    let Ok(profile) = std::env::var("USERPROFILE") else {
        return;
    };
    let expected = PathBuf::from(profile).join("Desktop");
    if !expected.is_dir() {
        return;
    }
    let Some(desktop) = desktop_dir() else {
        return;
    };
    assert_eq!(
        desktop, expected,
        "Windows 桌面应等于 %USERPROFILE%\\Desktop"
    );
}
