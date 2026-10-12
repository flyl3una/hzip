use super::*;
use hzip_error::FormatErrorKind;

fn missing_name(r: Result<Vec<PathBuf>, HzipError>) -> Option<String> {
    match r {
        Err(HzipError::Format {
            kind: FormatErrorKind::MissingVolume(name),
            ..
        }) => Some(name),
        _ => None,
    }
}

fn file_names(v: &[PathBuf]) -> Vec<String> {
    v.iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn write(dir: &Path, name: &str) {
    // 占位内容：本层纯 FS 逻辑不打开卷内容（unrar 层才读）
    std::fs::write(dir.join(name), b"placeholder").expect("write placeholder");
}

#[test]
fn new_style_name_parse() {
    assert_eq!(
        new_style_n(std::path::Path::new("dir/arc.part1.rar")),
        Some(("arc".to_string(), 1))
    );
    assert_eq!(
        new_style_n(std::path::Path::new("arc.part0010.rar")),
        Some(("arc".to_string(), 10))
    );
    assert_eq!(
        new_style_n(std::path::Path::new("arc.rar")),
        None,
        "无 .partN → 旧命名/单卷"
    );
    assert_eq!(new_style_n(std::path::Path::new("arc.partX.rar")), None);
    assert_eq!(new_style_n(std::path::Path::new("arc.r00")), None);
    assert!(is_new_style_name(std::path::Path::new("arc.part2.rar")));
    assert!(!is_new_style_name(std::path::Path::new("arc.rar")));
}

#[test]
fn old_extension_parse() {
    assert_eq!(old_continuation_number("r00"), Some(0));
    assert_eq!(old_continuation_number("r01"), Some(1));
    assert_eq!(old_continuation_number("r12"), Some(12));
    assert_eq!(old_continuation_number("r9"), Some(9));
    assert_eq!(old_continuation_number("rar"), None);
    assert_eq!(old_continuation_number("zip"), None);
    assert_eq!(old_continuation_number(""), None);
}

#[test]
fn new_naming_numeric_order_and_complete() {
    let d = tempfile::tempdir().unwrap();
    // part1..part12 齐全：验证数值序（part10/11/12 依序排在 part9 后）
    for n in 1..=12u32 {
        write(d.path(), &format!("arc.part{n}.rar"));
    }
    let vols = discover_multipart(&d.path().join("arc.part1.rar")).expect("连续齐全");
    let expect: Vec<String> = (1..=12u32).map(|n| format!("arc.part{n}.rar")).collect();
    assert_eq!(file_names(&vols), expect, "数值序而非字典序");
}

#[test]
fn new_naming_interior_gap_reported() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "arc.part1.rar");
    write(d.path(), "arc.part3.rar");
    let missing = missing_name(discover_multipart(&d.path().join("arc.part1.rar")))
        .expect("中间缺口应报 2005");
    assert_eq!(missing, "arc.part2.rar", "缺口 2 报 base.part2.rar");
}

#[test]
fn new_naming_only_part1_no_gap() {
    // 仅 part1 存在（内容可能已足）：不误报 part2；未来卷缺失由 unrar 处理期报
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "arc.part1.rar");
    let vols = discover_multipart(&d.path().join("arc.part1.rar")).expect("仅 part1 放行");
    assert_eq!(file_names(&vols), ["arc.part1.rar"]);
}

#[test]
fn subsequent_part_normalizes_to_part1() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "arc.part1.rar");
    write(d.path(), "arc.part2.rar");
    write(d.path(), "arc.part3.rar");
    let vols = discover_multipart(&d.path().join("arc.part2.rar")).expect("非首卷归一成功");
    assert_eq!(
        file_names(&vols),
        ["arc.part1.rar", "arc.part2.rar", "arc.part3.rar"]
    );

    // part1 缺失 → 报 part1
    let d2 = tempfile::tempdir().unwrap();
    write(d2.path(), "arc.part2.rar");
    let missing =
        missing_name(discover_multipart(&d2.path().join("arc.part2.rar"))).expect("报 part1");
    assert_eq!(missing, "arc.part1.rar");
}

#[test]
fn old_naming_sequence() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "old.rar");
    write(d.path(), "old.r00");
    write(d.path(), "old.r01");
    write(d.path(), "old.r02");
    let vols = discover_multipart(&d.path().join("old.rar")).expect("连续");
    assert_eq!(
        file_names(&vols),
        ["old.rar", "old.r00", "old.r01", "old.r02"]
    );
}

#[test]
fn old_naming_single_degrade_no_false_positive() {
    // 首卷 .rar 无任何续卷 → 单卷退化（不报 r00；unrar 处理期才知是否真需要）
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "old.rar");
    let vols = discover_multipart(&d.path().join("old.rar")).expect("单卷退化");
    assert_eq!(file_names(&vols), ["old.rar"]);
}

#[test]
fn old_naming_continuation_given_and_first_missing() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "old.r01");
    let missing =
        missing_name(discover_multipart(&d.path().join("old.r01"))).expect("首卷缺失应报");
    assert_eq!(missing, "old.rar");
}

#[test]
fn old_naming_truncates_at_first_gap() {
    // r00 在、r01 缺、r02 在 → 截断到 r00（首个缺失即止）
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "old.rar");
    write(d.path(), "old.r00");
    write(d.path(), "old.r02");
    let vols = discover_multipart(&d.path().join("old.rar")).expect("首个缺失即截断");
    assert_eq!(file_names(&vols), ["old.rar", "old.r00"]);
}

#[test]
fn next_expected_naming() {
    assert_eq!(
        next_expected_name(std::path::Path::new("d/arc.part2.rar")),
        "arc.part3.rar"
    );
    assert_eq!(
        next_expected_name(std::path::Path::new("arc.rar")),
        "arc.r00"
    );
    assert_eq!(
        next_expected_name(std::path::Path::new("arc.r01")),
        "arc.r02"
    );
    assert_eq!(
        next_expected_name(std::path::Path::new("arc.r9")),
        "arc.r10",
        "宽度跟随当前名（r9 → r10）"
    );
}
