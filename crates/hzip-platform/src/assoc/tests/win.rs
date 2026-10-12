use super::*;
use std::path::PathBuf;

/// 测试用子键/扩展名（**不碰真 ProgId/真扩展名**，跑完清理干净）。
///
/// 扩展名也用假名（`hztest1/hztest2`）：apply 会写 `.<ext>\(Default)`
/// （双击落点），拿真扩展名跑用例等于临时改用户的默认打开程序——用例中途
/// panic 就留下烂摊子。假扩展名没有真实文件类型，怎么写都伤不到用户。
const TEST_PROG_ID: &str = "hzip.ArchiveTest";
const TEST_EXTS: [&str; 2] = ["hztest1", "hztest2"];
static REG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn test_spec() -> AssocSpec {
    AssocSpec {
        extensions: TEST_EXTS.iter().map(|s| (*s).to_string()).collect(),
        prog_id: TEST_PROG_ID,
        exe_path: PathBuf::from(r"C:\Program Files\hzip\hzip-gui.exe"),
    }
}

fn hkcu() -> RegKey {
    RegKey::predef(HKEY_CURRENT_USER)
}

/// 应用关联 = 经典模式（用户口径 2026-09-29）：清掉系统的「用户显式选择」层，
/// 使 `.<ext>\(Default)` 当场成为双击落点——不再需要用户去系统设置里点。
///
/// 现场用假扩展名手写一份 `UserChoice`（真件的 Hash 由系统内部算，回读只认
/// `ProgId`，故手写足够）。**注意**：手写的键没有系统那层 `Deny SetValue`
/// 保护，所以这里验证的是「删键 → 回落我们的默认值」这条链路；而「带 Deny 的
/// 真键为何**不需要管理员**也能删」由本机手工实测钉在 `delete_user_choice`
/// 文档里（`KEY_READ` 可开 / `KEY_READ|KEY_WRITE` 拒绝访问 / `delete_subkey_all` 通过）。
#[test]
fn apply_clears_user_choice_and_takes_over() {
    let _guard = REG_TEST_LOCK.lock().expect("锁被毒化");
    let uc = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.hztest1\UserChoice";
    let (key, _) = hkcu().create_subkey(uc).expect("预置 UserChoice");
    key.set_value("ProgId", &"Bandizip.hztest1")
        .expect("写 ProgId");
    key.set_value("Hash", &"fake").expect("写 Hash");
    drop(key);
    assert!(
        super::super::ext_state("hztest1").pinned,
        "预置现场应判「改不动」"
    );

    let s = test_spec();
    apply_association(&s).expect("apply 应成功");

    assert!(hkcu().open_subkey(uc).is_err(), "应用关联应清掉 UserChoice");
    let st = super::super::ext_state("hztest1");
    assert_eq!(st.effective.as_deref(), Some(TEST_PROG_ID));
    assert!(
        !st.pinned && !st.blocked_by_other(),
        "净化后不再是「改不动」"
    );

    remove_association(&s).expect("remove 应成功");
    hkcu()
        .delete_subkey_all(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.hztest1")
        .expect("清理");
}

#[test]
fn apply_query_remove_roundtrip() {
    let _guard = REG_TEST_LOCK.lock().expect("锁被毒化");
    let s = test_spec();
    apply_association(&s).expect("apply 应成功");

    // 双击落点：`.<ext>\(Default)` = 本 ProgId，实况回读应判「生效」
    for ext in TEST_EXTS {
        let st = super::super::ext_state(ext);
        assert_eq!(
            st.effective.as_deref(),
            Some(TEST_PROG_ID),
            "{ext} 默认值应已写入"
        );
        assert!(!st.pinned, "{ext} 不该被判「改不动」");
        // 「打开方式」列表里也应有我们
        let ow = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey(format!(r"Software\Classes\.{ext}\OpenWithProgids"))
            .expect("OpenWithProgids 键应存在");
        assert!(ow.get_value::<String, _>(TEST_PROG_ID).is_ok());
    }

    // 顶替备份：旧值本就不存在 → 记空串（解除时据此删除，不写死默认）
    let backup = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(format!(r"Software\Classes\{TEST_PROG_ID}\PrevDefault"))
        .expect("PrevDefault 键应存在");
    assert_eq!(
        backup.get_value::<String, _>("hztest1").expect("应记备份"),
        "",
        "旧值不存在应记空串"
    );

    // 双击命令串：shell\open\command 指向 --open "%1"
    let cmd: String = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(format!(
            r"Software\Classes\{TEST_PROG_ID}\shell\open\command"
        ))
        .expect("open 命令键应存在")
        .get_value("")
        .expect("命令串应存在");
    assert!(
        cmd.contains("--open") && cmd.contains("%1"),
        "命令串异常: {cmd}"
    );

    // 文件图标（Explorer 画图标用）：必须在——旧实现用「只打开不创建」的
    // `open_subkey_with_flags`，DefaultIcon 永远写不出来，文件图标退化成未知
    // 类型（用户报障「zip 关联上了、7z/tgz 没有」的表征之一）。
    let icon: String = hkcu()
        .open_subkey(format!(r"Software\Classes\{TEST_PROG_ID}\DefaultIcon"))
        .expect("DefaultIcon 键应存在")
        .get_value("")
        .expect("DefaultIcon 值应存在");
    assert!(icon.ends_with(",0"), "DefaultIcon 应指向本程序: {icon}");

    // 「默认应用」页可见性：RegisteredApplications → Capabilities
    let ra: String = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\RegisteredApplications")
        .expect("RegisteredApplications 应存在")
        .get_value("hzip")
        .expect("hzip 应已注册");
    assert_eq!(ra, r"Software\hzip\Capabilities");
    let fa: String = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\hzip\Capabilities\FileAssociations")
        .expect("FileAssociations 应存在")
        .get_value(".hztest1")
        .expect(".hztest1 应登记");
    assert_eq!(fa, TEST_PROG_ID);

    remove_association(&s).expect("remove 应成功");
    for ext in TEST_EXTS {
        assert_eq!(
            super::super::ext_state(ext).effective,
            None,
            "{ext} 默认值应已还原为「不存在」"
        );
        assert!(
            winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
                .open_subkey(format!(r"Software\Classes\.{ext}\OpenWithProgids"))
                .and_then(|k| k.get_value::<String, _>(TEST_PROG_ID))
                .is_err(),
            "{ext} 的 OpenWithProgids 值应已清除"
        );
    }
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    assert!(
        hkcu.open_subkey(format!(r"Software\Classes\{TEST_PROG_ID}"))
            .is_err(),
        "ProgId 测试子键应已删除"
    );
    assert!(
        hkcu.open_subkey(r"Software\RegisteredApplications")
            .and_then(|k| k.get_value::<String, _>("hzip"))
            .is_err(),
        "RegisteredApplications 里的 hzip 应已删除"
    );
    assert!(
        hkcu.open_subkey(r"Software\hzip\Capabilities").is_err(),
        "Capabilities 应已删除"
    );
}

/// 顶替 → 还原：原本指向别人的默认程序，解除关联后必须**还给别人**。
///
/// 守的是「用完就走、不留尸体」：不还原会把用户的 `.tar` 默认程序留成
/// 已删除的 ProgId（双击弹「打开方式」，且用户自己也不知道原来是谁）。
#[test]
fn remove_restores_previous_default() {
    let _guard = REG_TEST_LOCK.lock().expect("锁被毒化");
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(r"Software\Classes\.hztest1")
        .expect("预置键应可建");
    key.set_value("", &"Other.Prog").expect("预置默认值");
    drop(key);

    let s = test_spec();
    apply_association(&s).expect("apply 应成功");
    assert_eq!(
        super::super::ext_state("hztest1").effective.as_deref(),
        Some(TEST_PROG_ID),
        "顶替后双击落点应是本 ProgId"
    );
    let backup: String = hkcu
        .open_subkey(format!(r"Software\Classes\{TEST_PROG_ID}\PrevDefault"))
        .and_then(|k| k.get_value("hztest1"))
        .expect("应备份旧默认程序");
    assert_eq!(backup, "Other.Prog");

    remove_association(&s).expect("remove 应成功");
    assert_eq!(
        super::super::ext_state("hztest1").effective.as_deref(),
        Some("Other.Prog"),
        "解除关联应还原原默认程序"
    );
    hkcu.delete_subkey_all(r"Software\Classes\.hztest1")
        .expect("清理预置键");
}

/// Windows 解析默认程序时会看**每用户**「打开方式」列表
/// （`FileExts\.<ext>\OpenWithProgids`）：里面若有别的程序，就当默认——这条
/// 实测（2026-09-29）压过 `Classes\.<ext>` 的默认值，是用户报障
/// 「状态行说已生效、资源管理器却仍归 Bandizip」的真因。
///
/// 故经典模式必须把该列表清成只剩我们，并在解除关联时**原样还原**（不吞别人条目）。
#[test]
fn apply_prunes_per_user_open_with_and_remove_restores() {
    let _guard = REG_TEST_LOCK.lock().expect("锁被毒化");
    let path =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.hztest1\OpenWithProgids";
    let (k, _) = hkcu().create_subkey(path).expect("预置 OpenWithProgids");
    k.set_value("Other.Prog", &"").expect("预置竞争者");
    k.set_value("hzip.ArchiveTest", &"")
        .expect("我们也在列表里");
    drop(k);

    let s = test_spec();
    apply_association(&s).expect("apply 应成功");
    let names: Vec<String> = hkcu()
        .open_subkey(path)
        .expect("列表应存在")
        .enum_values()
        .filter_map(|v| v.ok())
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        names,
        vec![TEST_PROG_ID.to_string()],
        "列表应只剩我们：{names:?}"
    );
    let backup: String = hkcu()
        .open_subkey(format!(r"Software\Classes\{TEST_PROG_ID}\PrevOpenWith"))
        .expect("PrevOpenWith 应存在")
        .get_value("hztest1")
        .expect("应备份被摘掉的 ProgId");
    assert!(backup.contains("Other.Prog"), "{backup}");

    remove_association(&s).expect("remove 应成功");
    let names: Vec<String> = hkcu()
        .open_subkey(path)
        .expect("列表应存在")
        .enum_values()
        .filter_map(|v| v.ok())
        .map(|(n, _)| n)
        .collect();
    assert!(
        names.contains(&"Other.Prog".to_string()),
        "竞争者应还原：{names:?}"
    );
    assert!(
        !names.contains(&TEST_PROG_ID.to_string()),
        "我们应撤出：{names:?}"
    );
    hkcu()
        .delete_subkey_all(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.hztest1")
        .expect("清理");
}

/// 用户中途自己改过默认程序 → 解除关联**不得**把人家改回去。
#[test]
fn remove_respects_user_change() {
    let _guard = REG_TEST_LOCK.lock().expect("锁被毒化");
    let s = test_spec();
    apply_association(&s).expect("apply 应成功");
    let key = hkcu()
        .open_subkey_with_flags(r"Software\Classes\.hztest1", winreg::enums::KEY_WRITE)
        .expect("键应存在");
    key.set_value("", &"Someone.Else").expect("改默认值");
    drop(key);

    remove_association(&s).expect("remove 应成功");
    assert_eq!(
        super::super::ext_state("hztest1").effective.as_deref(),
        Some("Someone.Else"),
        "当前值不是我们写的就不该动"
    );
    hkcu()
        .delete_subkey_all(r"Software\Classes\.hztest1")
        .expect("清理");
}

/// 幂等：二次 apply 不得把「我们自己」记成旧值（否则解除后留一个死默认）。
#[test]
fn apply_is_idempotent_no_duplicate_shell_keys() {
    let _guard = REG_TEST_LOCK.lock().expect("锁被毒化");
    let s = test_spec();
    apply_association(&s).expect("首次 apply 应成功");
    apply_association(&s).expect("二次 apply 应成功（幂等覆盖）");
    let backup: String = hkcu()
        .open_subkey(format!(r"Software\Classes\{TEST_PROG_ID}\PrevDefault"))
        .and_then(|k| k.get_value("hztest1"))
        .expect("应有备份");
    assert_eq!(backup, "", "二次 apply 后备份仍应是「旧值不存在」");

    let shell = hkcu()
        .open_subkey(format!(r"Software\Classes\{TEST_PROG_ID}\shell"))
        .expect("shell 键应存在");
    let mut verbs: Vec<String> = shell.enum_keys().filter_map(|k| k.ok()).collect();
    verbs.sort();
    // T-CM 后 ProgId 下只剩 open（双击打开）；解压/压缩 verb 归 context_menu
    assert_eq!(verbs, vec!["open"], "ProgId 下不应再有右键 verb: {verbs:?}");
    remove_association(&s).expect("remove 应成功");
}

/// `UserChoice` 存在 ⇒ 判「改不动」（`pinned`）——界面据此改走「打开方式」。
///
/// 用假扩展名造一份 `UserChoice`（真 UserChoice 的 Hash 是系统内部算的，我们
/// 只读 `ProgId`，故这里手写一个就够了），验证回读的判定分支。
#[test]
fn user_choice_marks_state_pinned() {
    let _guard = REG_TEST_LOCK.lock().expect("锁被毒化");
    let (key, _) = hkcu()
        .create_subkey(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.hztest3\UserChoice",
        )
        .expect("预置 UserChoice 键");
    key.set_value("ProgId", &"Bandizip.hztest3")
        .expect("写 ProgId");
    drop(key);

    let st = super::super::ext_state("hztest3");
    assert_eq!(st.effective.as_deref(), Some("Bandizip.hztest3"));
    assert!(st.pinned, "UserChoice 在 = 程序改不动");
    assert!(st.blocked_by_other());
    assert!(!st.is_effective());

    hkcu()
        .delete_subkey_all(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.hztest3")
        .expect("清理");
}
