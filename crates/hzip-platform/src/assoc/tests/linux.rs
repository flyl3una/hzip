use super::*;

/// 环境变量是进程级全局：同一批用例串行执行，跑完复原。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 临时 XDG 根（`XDG_DATA_HOME`/`XDG_CONFIG_HOME`/`HOME` 全指过来）。
struct Sandbox {
    root: PathBuf,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "hzip-assoc-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).expect("建临时数据目录");
        std::fs::create_dir_all(root.join("config")).expect("建临时配置目录");
        let mut saved = Vec::new();
        for (key, value) in [
            ("XDG_DATA_HOME", root.join("data")),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("HOME", root.clone()),
        ] {
            let before = std::env::var_os(key);
            // SAFETY: 用例串行（ENV_LOCK）+ 单线程写环境变量
            unsafe { std::env::set_var(key, &value) };
            saved.push((key, before));
        }
        Self { root, saved }
    }

    fn config_file(&self) -> PathBuf {
        self.root.join("config").join("mimeapps.list")
    }

    fn desktop_file(&self) -> PathBuf {
        self.root.join("data").join("applications").join(DESKTOP_ID)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for (key, before) in self.saved.drain(..) {
            unsafe {
                match before {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn spec(exts: &[&str]) -> AssocSpec {
    AssocSpec {
        extensions: exts.iter().map(|s| (*s).to_string()).collect(),
        prog_id: super::super::PROG_ID,
        exe_path: PathBuf::from("/opt/hzip/hzip-gui"),
    }
}

/// Exec 转义（含空格/引号/美元符号的安装路径）。
#[test]
fn exec_quoting() {
    assert_eq!(quote_exec("/usr/bin/hzip"), "\"/usr/bin/hzip\"");
    assert_eq!(
        quote_exec("/home/a b/h$zip"),
        "\"/home/a b/h\\$zip\"".replace("\\\\", "\\")
    );
}

/// 桌面条目内容：认领的 MIME 全列、Exec 带 `--open %f`。
#[test]
fn desktop_entry_lists_mimes_and_open_command() {
    let text = render_desktop(
        Path::new("/opt/hzip/hzip-gui"),
        &["application/zip", "application/x-7z-compressed"],
    );
    assert!(
        text.contains("MimeType=application/zip;application/x-7z-compressed;"),
        "{text}"
    );
    assert!(
        text.contains("Exec=\"/opt/hzip/hzip-gui\" --open %f"),
        "{text}"
    );
    assert!(text.starts_with("[Desktop Entry]\n"), "{text}");
}

/// 应用 → 回读：认领的格式判「已生效」，并且**真的**写进 mimeapps.list。
#[test]
fn apply_then_state_is_effective() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new("apply");
    apply_association(&spec(&["zip", "tgz"])).expect("apply 应成功");

    for ext in ["zip", "tgz"] {
        let st = ext_state(ext);
        assert!(st.is_effective(), "{ext} 应判生效：{st:?}");
        assert_eq!(st.effective.as_deref(), Some(DESKTOP_ID));
        assert!(!st.pinned, "Linux 上没有「改不动」的默认：{st:?}");
    }
    let text = std::fs::read_to_string(sb.config_file()).expect("mimeapps.list 应存在");
    assert!(text.contains("[Default Applications]"), "{text}");
    assert!(text.contains("application/zip=hzip.desktop;"), "{text}");
    assert!(
        text.contains("application/x-compressed-tar=hzip.desktop;"),
        "{text}"
    );
    assert!(sb.desktop_file().exists(), "桌面条目应已写出");
}

/// 行级手术：用户自己的条目、小节与注释必须原样保留。
#[test]
fn apply_preserves_unrelated_entries() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new("preserve");
    std::fs::write(
        sb.config_file(),
        "# 我的手改\n\
         [Default Applications]\n\
         text/plain=my-editor.desktop;\n\
         application/zip=someone-else.desktop;\n\
         \n\
         [Custom Section]\n\
         keep=me\n",
    )
    .expect("预置 mimeapps.list");

    apply_association(&spec(&["zip"])).expect("apply 应成功");
    let text = std::fs::read_to_string(sb.config_file()).expect("读回");
    assert!(text.contains("# 我的手改"), "注释被吃掉：{text}");
    assert!(text.contains("text/plain=my-editor.desktop;"), "{text}");
    assert!(
        text.contains("application/zip=hzip.desktop;"),
        "认领项应被我们替换：{text}"
    );
    assert!(
        !text.contains("someone-else.desktop"),
        "旧默认应被顶掉：{text}"
    );
    assert!(
        text.contains("[Custom Section]") && text.contains("keep=me"),
        "{text}"
    );
}

/// 解除 → 条目消失（而不是留一个空值）、桌面条目删掉、其它内容不动。
#[test]
fn remove_strips_only_our_entries() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new("remove");
    std::fs::write(
        sb.config_file(),
        "[Default Applications]\n\
         text/plain=my-editor.desktop;\n",
    )
    .expect("预置");
    apply_association(&spec(&["zip", "7z"])).expect("apply 应成功");
    remove_association(&spec(&["zip", "7z"])).expect("remove 应成功");

    let text = std::fs::read_to_string(sb.config_file()).expect("读回");
    assert!(
        !text.contains("application/zip=") && !text.contains("application/x-7z-compressed="),
        "条目应整条删除：{text}"
    );
    assert!(text.contains("text/plain=my-editor.desktop;"), "{text}");
    assert!(!sb.desktop_file().exists(), "桌面条目应已删除");
    assert!(!ext_state("zip").is_effective(), "解除后不应再判生效");
}

/// 别的程序是默认：如实报出占用者（`ours=false` 且**不是**「改不动」）。
#[test]
fn state_reads_foreign_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new("foreign");
    std::fs::write(
        sb.config_file(),
        "[Default Applications]\napplication/zip=org.gnome.FileRoller.desktop;\n",
    )
    .expect("预置");
    let st = ext_state("zip");
    assert_eq!(
        st.effective.as_deref(),
        Some("org.gnome.FileRoller.desktop")
    );
    assert!(!st.is_effective());
    assert!(!st.blocked_by_other(), "Linux 上我们改得动：{st:?}");
    // 未认领的扩展名：无默认
    assert_eq!(ext_state("tar").effective, None);
}

/// 桌面条目的 `Name=` 用于把标识显示成可读名。
#[test]
fn desktop_name_is_read_back() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new("name");
    apply_association(&spec(&["zip"])).expect("apply 应成功");
    assert_eq!(desktop_name(DESKTOP_ID).as_deref(), Some(DESKTOP_NAME));
    assert_eq!(desktop_name("nope.desktop"), None);
    let _ = &sb;
}

/// XDG 查找顺序：用户级 `mimeapps.list` 压过旧位置
/// （`$XDG_DATA_HOME/applications/mimeapps.list`，GNOME 3.28 前的写法）。
#[test]
fn user_config_wins_over_legacy_data_location() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new("order");
    std::fs::create_dir_all(sb.root.join("data/applications")).expect("建 applications 目录");
    std::fs::write(
        sb.root.join("data/applications/mimeapps.list"),
        "[Default Applications]\napplication/zip=legacy.desktop;\napplication/x-7z-compressed=legacy.desktop;\n",
    )
    .expect("预置旧位置");
    std::fs::write(
        sb.config_file(),
        "[Default Applications]\napplication/zip=user.desktop;\n",
    )
    .expect("预置用户级");
    assert_eq!(
        ext_state("zip").effective.as_deref(),
        Some("user.desktop"),
        "用户级优先"
    );
    assert_eq!(
        ext_state("7z").effective.as_deref(),
        Some("legacy.desktop"),
        "用户级未定义 → 回落旧位置"
    );
}

/// 取消勾选后再解除：**所有**指向我们的条目都要摘掉（含本次不在勾选集的）。
#[test]
fn remove_cleans_entries_of_no_longer_ticked_formats() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new("remove-stale");
    apply_association(&spec(&["zip", "7z"])).expect("apply 应成功");
    // 模拟用户取消勾选 7z 后再次应用
    apply_association(&spec(&["zip"])).expect("二次 apply 应成功");
    // 解除时只传剩下的那一个
    remove_association(&spec(&["zip"])).expect("remove 应成功");

    let text = std::fs::read_to_string(sb.config_file()).expect("读回");
    assert!(
        !text.contains(DESKTOP_ID),
        "凡指向我们的条目都应清除（含已取消勾选的格式）：{text}"
    );
    assert!(!sb.desktop_file().exists());
    for ext in ["zip", "7z"] {
        assert!(!ext_state(ext).is_effective(), "{ext} 不该再判生效");
    }
}

/// 未勾选任何可用扩展名 → 明确报错，不写半个文件。
#[test]
fn apply_without_mapped_extensions_errors() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new("empty");
    let err = apply_association(&spec(&["unknown"])).expect_err("应报错");
    assert!(err.contains("MIME"), "{err}");
    assert!(!sb.desktop_file().exists());
}
