use super::*;

fn gz_fmt() -> FormatId {
    FormatId::from("gz")
}

fn file(p: &str) -> NameShape {
    NameShape::File(std::path::PathBuf::from(p))
}

#[test]
fn strip_one_extension_rules() {
    let exts = ["gz"];
    assert_eq!(strip_one_ext("a.txt.gz", &exts).as_deref(), Some("a.txt"));
    assert_eq!(strip_one_ext("a.tar.gz", &exts).as_deref(), Some("a.tar"));
    // 只剥一层
    assert_eq!(strip_one_ext("a.gz.gz", &exts).as_deref(), Some("a.gz"));
    // 非目标扩展不剥
    assert_eq!(strip_one_ext("data.zip", &exts), None);
    assert_eq!(strip_one_ext("a.txt.gz", &["bz2"]), None);
    // 无扩展不剥
    assert_eq!(strip_one_ext("data", &exts), None);
    // 点文件不切
    assert_eq!(strip_one_ext(".gz", &exts), None);
    // 大小写不敏感
    assert_eq!(strip_one_ext("a.TXT.GZ", &exts).as_deref(), Some("a.TXT"));
}

#[test]
fn derive_name_precedence() {
    let fmt = gz_fmt();
    // 1. FNAME 优先
    let n = derive_entry_name(&file("x.gz"), &fmt, &["gz"], Some("fname.txt"), None);
    assert_eq!(n.as_str(), "fname.txt");
    // FNAME 非法（含 \）→ 降级
    let n = derive_entry_name(
        &file("x.gz"),
        &fmt,
        &["gz"],
        Some("bad\\name"),
        Some(&EntryName::new("ovr.txt")),
    );
    assert_eq!(n.as_str(), "ovr.txt");
    // 2. override 优先于文件名
    let n = derive_entry_name(
        &file("a.txt.gz"),
        &fmt,
        &["gz"],
        None,
        Some(&EntryName::new("cli-name.txt")),
    );
    assert_eq!(n.as_str(), "cli-name.txt");
    // 3. 剥一层格式后缀
    let n = derive_entry_name(&file("a.txt.gz"), &fmt, &["gz"], None, None);
    assert_eq!(n.as_str(), "a.txt");
    let n = derive_entry_name(&file("archive.tar.gz"), &fmt, &["gz"], None, None);
    assert_eq!(n.as_str(), "archive.tar");
    // 无后缀 → 原名
    let n = derive_entry_name(&file("data"), &fmt, &["gz"], None, None);
    assert_eq!(n.as_str(), "data");
    // 4. stdin 兜底
    let n = derive_entry_name(&NameShape::Stdin, &fmt, &["gz"], None, None);
    assert_eq!(n.as_str(), "stdin");
    // 4. Stream 兜底格式 id
    let n = derive_entry_name(&NameShape::Stream, &fmt, &["gz"], None, None);
    assert_eq!(n.as_str(), "gz");
}
