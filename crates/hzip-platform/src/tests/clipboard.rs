use super::*;

/// 内部剪贴板：写入/读取/清空。
#[test]
fn internal_clipboard_roundtrip() {
    clear_internal();
    assert!(internal_is_empty());
    set_internal(vec![
        PathBuf::from("/tmp/a.txt"),
        PathBuf::from("/tmp/b.txt"),
    ]);
    assert!(!internal_is_empty());
    assert_eq!(internal_paths().len(), 2);
    // 覆盖语义（新一次复制替换旧内容）
    set_internal(vec![PathBuf::from("/tmp/c.txt")]);
    assert_eq!(internal_paths(), vec![PathBuf::from("/tmp/c.txt")]);
    clear_internal();
    assert!(internal_is_empty());
    assert!(internal_paths().is_empty());
}

/// 物化安全名：末段 + 穿越/控制字符净化 + 目录后缀（TC-SEC-015 同源）。
#[test]
fn safe_materialize_name_sanitizes() {
    assert_eq!(safe_materialize_name("a.txt", false), "a.txt");
    assert_eq!(safe_materialize_name("dir/sub/f.txt", false), "f.txt");
    assert_eq!(safe_materialize_name("sub", true), "sub_dir");
    // 穿越片段与控制字符不构成路径效果
    let evil = safe_materialize_name("../../etc/passwd", false);
    assert!(!evil.contains('/') && !evil.contains(".."), "净化: {evil}");
    let ctrl = safe_materialize_name("a\u{0}b", false);
    assert!(!ctrl.contains('\u{0}'), "控制字符剥离: {ctrl:?}");
    // 空/点段 → 兜底名
    assert_eq!(safe_materialize_name("..", false), "entry");
}

/// 物化目标布局：`{root}/{hash8}/{安全名}`，同一档案两次调用稳定。
#[test]
fn materialize_target_is_stable_per_archive() {
    let a = materialize_target(Path::new("/tmp/root"), Path::new("/data/a.zip"), "x.txt");
    let b = materialize_target(Path::new("/tmp/root"), Path::new("/data/a.zip"), "y.txt");
    assert_eq!(a.parent(), b.parent(), "同一档案 → 同 hash 目录");
    assert_eq!(a.file_name().unwrap(), "x.txt");
    assert_eq!(
        a.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .len(),
        8
    );
    let c = materialize_target(Path::new("/tmp/root"), Path::new("/data/b.zip"), "x.txt");
    assert_ne!(a.parent(), c.parent(), "不同档案 → 不同 hash 目录");
}

/// 系统剪贴板占位（B3 降级线）：恒空 —— 「粘贴无内容 → 灰」语义依据。
#[test]
fn system_file_list_is_empty_on_fallback() {
    assert!(system_file_list().is_empty());
}

/// `materialize_entry` 多段子路径：落地结构 `{session}/{hash8}/{sub}/{f}`、
/// 中间目录自动创建、内容写入正确。
#[test]
fn materialize_entry_multi_segment_layout() {
    let archive = Path::new("/data/multi.zip");
    let landed = materialize_entry(archive, "sub/deep/b.txt", false, b"hello-hzip" as &[u8])
        .expect("多段物化应成功");
    // 结构：{session}/{hash8}/{sub}/{deep}/b.txt —— 逐级上溯核对。
    let deep = landed.parent().expect("有父目录（deep）");
    assert_eq!(deep.file_name().unwrap(), "deep");
    let sub = deep.parent().expect("有父目录（sub）");
    assert_eq!(sub.file_name().unwrap(), "sub");
    let hash8 = sub.parent().expect("有 hash 层");
    assert_eq!(hash8.file_name().unwrap().to_string_lossy().len(), 8);
    // 会话目录的父 = {tmp}/hzip（TEMP_SESSION_DIR 布局）。
    assert_eq!(
        hash8
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("hzip")),
        "copy_area 落在 tmp/hzip/uuid 会话布局"
    );
    // 内容往返。
    let content = std::fs::read(&landed).expect("落地文件可读");
    assert_eq!(content, b"hello-hzip");
}

/// `materialize_entry` 中文段与目录叶子 `_dir` 后缀。
#[test]
fn materialize_entry_cjk_and_dir_leaf() {
    let archive = Path::new("/data/cjk.zip");
    // 中文名文件。
    let f =
        materialize_entry(archive, "目录/文件.txt", false, b"" as &[u8]).expect("中文名物化应成功");
    assert_eq!(f.file_name().unwrap().to_string_lossy(), "文件.txt");
    assert_eq!(
        f.parent().unwrap().file_name().unwrap().to_string_lossy(),
        "目录",
        "中间中文目录段保留"
    );
    // 目录叶子：`_dir` 后缀防碰撞。
    let d = materialize_entry(archive, "目录", true, b"" as &[u8]).expect("目录物化应成功");
    assert_eq!(d.file_name().unwrap().to_string_lossy(), "目录_dir");
    assert!(d.is_dir(), "目录叶子落成目录");
}

/// `materialize_entry` 非法输入：空 rel、纯分隔符 → None；非法段净化落地。
#[test]
fn materialize_entry_rejects_and_sanitizes() {
    let archive = Path::new("/data/reject.zip");
    assert!(materialize_entry(archive, "", false, b"" as &[u8]).is_none());
    assert!(materialize_entry(archive, "///", false, b"" as &[u8]).is_none());
    assert!(materialize_entry(archive, "\\\\", false, b"" as &[u8]).is_none());
    // 非法段（穿越/控制字符）→ 兜底净化，仍落地且无路径效果。
    let evil = materialize_entry(archive, "../../etc/passwd", false, b"x" as &[u8])
        .expect("穿越段净化后应落地");
    let s = evil.to_string_lossy();
    assert!(!s.contains(".."), "无穿越残留: {s}");
    // 单 NUL 段剥成 `_`（同 safe_materialize_name 替换规则）。
    let ctrl = materialize_entry(archive, "a\u{0}b.txt", false, b"x" as &[u8])
        .expect("控制字符段净化后应落地");
    assert!(
        !ctrl
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains('\u{0}'),
        "控制字符剥离: {ctrl:?}"
    );
}
