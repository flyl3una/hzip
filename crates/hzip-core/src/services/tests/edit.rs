use super::*;

fn info(name: &str, is_dir: bool) -> EntryInfo {
    EntryInfo {
        id: 0,
        name: name.into(),
        is_dir,
        is_symlink: false,
        symlink_target: None,
        size_raw: 10,
        size_packed: 10,
        mtime: None,
        mode: None,
        attributes: Default::default(),
        encrypted: false,
        header_encrypted: false,
        crc32: None,
        codec: None,
        name_encoding: hzip_format::NameEncoding::Utf8,
        encryption: None,
        compression_level: None,
    }
}

/// 物化安全名（TC-SEC-015 edit 半场）：多段条目取末段；目录条目补后缀。
#[test]
fn materialize_safe_name_segments() {
    assert_eq!(entry_safe_name(&info("a.txt", false)), "a.txt");
    // 恶意/多段名 → 末段（无路径穿越材料）
    assert_eq!(entry_safe_name(&info("../../evil", false)), "evil");
    assert_eq!(entry_safe_name(&info("d/f.txt", false)), "f.txt");
    // 目录条目补 _dir（与同名文件区分）
    assert_eq!(entry_safe_name(&info("sub", true)), "sub_dir");
}

/// 临时档案命名（同目录同卷）：`{名}.hzip-tmp-{uuid}`、父目录保留。
#[test]
fn tmp_archive_same_dir_naming() {
    let p = Path::new("C:/arc/doc.zip");
    let t = tmp_archive_path(p);
    assert_eq!(t.parent().unwrap(), Path::new("C:/arc"));
    let name = t.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("doc.zip.hzip-tmp-"), "命名: {name}");
    assert!(name.len() > "doc.zip.hzip-tmp-".len(), "含 uuid");
}
