use super::*;
use crate::RarHandler;
use hzip_format::{FormatHandler, OpenOptions};
use unrar::Archive;

/// 条目名分隔符归一：rar 出身 Windows，条目名常用 `\` 作路径分隔符，
/// 而 `EntryName` 规则 N2 拒绝 `\`（内部统一 `/`）——不预转换会让**整个档案**
/// 打不开（实测 256MB 样本：`U4P4用户手册\启明版-….docx` → bad header）。
#[test]
fn entry_name_normalizes_backslash_separators() {
    let n = entry_name_of(std::path::Path::new(r"U4P4用户手册\启明版.docx")).unwrap();
    assert_eq!(n.as_str(), "U4P4用户手册/启明版.docx");
    let n = entry_name_of(std::path::Path::new(r"a\b/c.txt")).unwrap();
    assert_eq!(n.as_str(), "a/b/c.txt", "混合分隔符也应归一");
    let n = entry_name_of(std::path::Path::new("dir/file.bin")).unwrap();
    assert_eq!(n.as_str(), "dir/file.bin", "已是 / 的原样");
    // 归一后仍非法（控制字符）→ 仍拒绝（不因本次修复放宽规则）
    assert!(entry_name_of(std::path::Path::new("a\u{1}b")).is_err());
}

/// 定位 unrar crate 自带测试 rar 样本（cargo registry 源码树 unrar-*/data）。
///
/// 无真实 WinRAR 黄金样本时（tests-assets rar-manual 为空），用这些真实
/// rar4/rar5 微型样本驱动冒烟验证（T26 §6.1 允许；样本随 unrar 依赖编译
/// 就绪，属「真实 rar 档案」而非伪造字节）。找不到 → None（测试 auto-skip）。
fn asset(name: &str) -> Option<PathBuf> {
    let home = std::env::var("CARGO_HOME")
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.cargo")))?;
    let root = PathBuf::from(home).join("registry/src");
    let rd = std::fs::read_dir(&root).ok()?;
    for top in rd.flatten() {
        for c in std::fs::read_dir(top.path()).ok()?.flatten() {
            let n = c.file_name().to_string_lossy().into_owned();
            if n.starts_with("unrar-") {
                let cand = c.path().join("data").join(name);
                if cand.exists() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

fn copy_asset(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    let src = asset(name)?;
    let dst = dir.join(name);
    std::fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn read_all(mut r: Box<dyn Read + Send>) -> Vec<u8> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).expect("read_to_end");
    buf
}

/// 断言 Ok 分支无需 Debug（Read/ArchiveReader trait 对象不实现 Debug）——
/// 手动 match 取错误码。
fn err_code<T>(r: Result<T, HzipError>) -> u32 {
    match r {
        Err(e) => e.code(),
        Ok(_) => panic!("期望 Err"),
    }
}

#[test]
fn reader_entries_and_read_on_crate_rar4_sample() {
    // version.rar（rar4 单卷）：恰一条 VERSION，内容 "unrar-0.4.0"
    let d = tempfile::tempdir().unwrap();
    let Some(p) = copy_asset(d.path(), "version.rar") else {
        return; // 资产缺失 auto-skip
    };
    let r = RarHandler::new()
        .open_reader(Source::File(p), &OpenOptions::default())
        .expect("open");
    let es = r.entries().expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].name.as_str(), "VERSION");
    assert!(!es[0].is_dir);
    let got = read_all(r.read_entry(0, PasswordRef::none()).expect("read_entry"));
    assert_eq!(got, b"unrar-0.4.0");
}

#[test]
fn reader_repeat_read_independent() {
    // 重复/独立读取：每次 read_entry 独立物化（红线 2）
    let d = tempfile::tempdir().unwrap();
    let Some(p) = copy_asset(d.path(), "version.rar") else {
        return;
    };
    let r = RarHandler::new()
        .open_reader(Source::File(p), &OpenOptions::default())
        .expect("open");
    for _ in 0..3 {
        assert_eq!(
            read_all(r.read_entry(0, PasswordRef::none()).expect("read")),
            b"unrar-0.4.0"
        );
    }
}

#[test]
fn encrypted_no_password_is_3101_correct_wrong_password() {
    // crypted.rar（rar4 数据加密，密码 "unrar"，内容 "target\nCargo.lock\n"）
    let d = tempfile::tempdir().unwrap();
    let Some(p) = copy_asset(d.path(), "crypted.rar") else {
        return;
    };
    let r = RarHandler::new()
        .open_reader(Source::File(p.clone()), &OpenOptions::default())
        .expect("open");
    let es = r.entries().expect("entries");
    assert_eq!(es.len(), 1);
    assert!(es[0].encrypted, "条目应标记加密");
    // 无密码 → 3101（红线 3：Password 错误而非乱码）
    assert_eq!(
        err_code(r.read_entry(0, PasswordRef::none())),
        3101,
        "无密码应 Password 3101"
    );
    // 错密码 → 3101
    let bad = zeroize::Zeroizing::new("wrong".to_string());
    assert_eq!(
        err_code(r.read_entry(0, PasswordRef::new(&bad))),
        3101,
        "错密码应 Password 3101"
    );
    // 正确密码 → 内容一致
    let pw = zeroize::Zeroizing::new("unrar".to_string());
    let got = read_all(r.read_entry(0, PasswordRef::new(&pw)).expect("带密码读取"));
    assert_eq!(got, b"target\nCargo.lock\n");
}

#[test]
fn header_encrypted_rar5_open_3102_no_name_leak() {
    // comment-hpw-password.rar（rar5 头加密，密码 "password"）：无密码 open
    // → Password{Header} 3102；错误信息不得泄露条目名（边界 §7-6）
    let d = tempfile::tempdir().unwrap();
    let Some(p) = copy_asset(d.path(), "comment-hpw-password.rar") else {
        return;
    };
    let err = RarHandler::new().open_reader(Source::File(p), &OpenOptions::default());
    assert_eq!(err_code(err), 3102, "头加密无密码应 3102");
    // 复核 3102 的零泄露：用 handler 再取一次错误文本断言不含条目名
    let d2 = tempfile::tempdir().unwrap();
    let Some(p2) = copy_asset(d2.path(), "comment-hpw-password.rar") else {
        return;
    };
    let msg = RarHandler::new()
        .open_reader(Source::File(p2), &OpenOptions::default())
        .map(|_| "ok".to_string())
        .unwrap_err()
        .to_string();
    assert!(
        !msg.contains(".gitignore") && !msg.to_lowercase().contains("name"),
        "3102 错误信息不得泄露条目名: {msg}"
    );
}

#[test]
fn split_part1_truncated_maps_to_2005() {
    // archive.part1.rar（rar4 分卷，仅 part1 存在）：unrar 列头推进到跨卷处
    // EOpen → 本层 2005 附缺失卷名（next_expected_name(part1) = part2）
    let d = tempfile::tempdir().unwrap();
    let Some(p) = copy_asset(d.path(), "archive.part1.rar") else {
        return;
    };
    // 确认该样本确为分卷首卷
    let a = Archive::new(&p);
    let o = a.open_for_listing().expect("unrar open");
    assert_eq!(o.volume_info(), unrar::VolumeInfo::First);

    let r = RarHandler::new().open_reader(Source::File(p), &OpenOptions::default());
    match r {
        Err(e) => {
            assert_eq!(
                e.code(),
                2005,
                "仅 part1 的截断分卷 → MissingVolume 2005（实得 {e:?}）"
            );
            let msg = e.to_string();
            assert!(msg.contains("part2"), "2005 错误信息应含缺失卷名: {msg}");
        }
        Ok(rr) => {
            // 极端情形：unrar 认为内容不需续卷也能列完——条目可达即可
            let es = rr.entries().expect("列头");
            assert!(!es.is_empty());
        }
    }
}
