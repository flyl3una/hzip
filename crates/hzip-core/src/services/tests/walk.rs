use super::*;
use tempfile::TempDir;

fn write(p: &std::path::Path, data: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

/// glob 排除（`*.tmp` 段命中、`sub/*` 路径前缀命中）、目录剪枝。
///
/// v2 rel 语义：目录输入收编自身条目（rel = 目录名）+ 子条目带 `{目录}/`
/// 前缀（保目录名，7-Zip 同）。断言用 ends_with/段匹配规避临时目录名变化。
#[test]
fn glob_exclude_and_prune() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join("a.txt"), b"a");
    write(&dir.path().join("x.tmp"), b"t");
    write(&dir.path().join("sub/b.txt"), b"b");
    write(&dir.path().join("sub/c.tmp"), b"c");
    let f = InputFilters {
        exclude_globs: vec!["*.tmp".into(), "sub/*".into()],
        ..Default::default()
    };
    let got = collect(&[dir.path().to_path_buf()], &f).unwrap();
    let rels: Vec<String> = got.iter().map(|x| x.rel.clone()).collect();
    // 根目录自身条目收编（rel = 临时目录名）
    let dname = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(rels.iter().any(|r| r == &dname), "根目录条目收编: {rels:?}");
    // a.txt 以 {dname}/a.txt 收编（保目录名）
    assert!(
        rels.iter().any(|r| r.ends_with("a.txt")),
        "a.txt 保留: {rels:?}"
    );
    assert!(!rels.iter().any(|r| r.ends_with(".tmp")), "tmp 剔除");
    // 子目录条目（{dname}/sub）保留；其内容（sub/b.txt、sub/c.tmp）剪枝
    assert!(
        rels.iter().any(|r| r.ends_with("/sub")),
        "sub 目录条目保留: {rels:?}"
    );
    assert!(
        !rels
            .iter()
            .any(|r| r.ends_with("sub/b.txt") || r.ends_with("sub/c.tmp")),
        "sub 内容剪枝: {rels:?}"
    );
}
