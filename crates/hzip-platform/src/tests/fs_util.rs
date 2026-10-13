use super::*;

#[test]
fn available_bytes_positive() {
    let dir = crate::testutil::temp_dir("fs-util");
    assert!(available_bytes(dir.path()).unwrap() > 0);
}

#[test]
fn case_sensitive_probe() {
    let dir = crate::testutil::temp_dir("fs-util-case");
    let a = is_case_sensitive(dir.path());
    let b = is_case_sensitive(dir.path());
    assert_eq!(a, b, "缓存命中应返回相同结果");
    assert!(
        a == cfg!(not(windows)) || !a,
        "win 默认不敏感、unix 默认敏感（可配置卷除外）"
    );
}

#[test]
fn atomic_replace_same_vol() {
    let dir = crate::testutil::temp_dir("fs-util-replace");
    let src = dir.join("src.bin");
    let dst = dir.join("dst.bin");
    std::fs::write(&src, b"new-content").unwrap();
    std::fs::write(&dst, b"old-content").unwrap();
    atomic_replace(&src, &dst).unwrap();
    assert_eq!(std::fs::read(&dst).unwrap(), b"new-content");
    // 无 .hzip-tmp-* 残留
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".hzip-tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "残留: {leftovers:?}");
}

#[test]
fn atomic_replace_cross_vol_sim() {
    // 模拟跨卷分支（同卷两目录直接调用内部跨卷函数验证 copy+fsync+rename 路径）
    let dir = crate::testutil::temp_dir("fs-util-xvol");
    let src = dir.join("src.bin");
    let dst = dir.join("sub").join("dst.bin");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::write(&src, b"cross").unwrap();
    std::fs::write(&dst, b"old").unwrap();
    replace_cross_volume(&src, &dst).unwrap();
    assert_eq!(std::fs::read(&dst).unwrap(), b"cross");
    // 中途失败（src 不存在）：dst 字节不变
    let src2 = dir.join("missing.bin");
    let dst2 = dir.join("dst2.bin");
    std::fs::write(&dst2, b"keep").unwrap();
    assert!(replace_cross_volume(&src2, &dst2).is_err());
    assert_eq!(std::fs::read(&dst2).unwrap(), b"keep");
}

#[test]
fn remove_no_follow() {
    let dir = crate::testutil::temp_dir("fs-util-rmnf");
    let root = dir.join("root");
    let outside = dir.join("outside");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("sub").join("f.txt"), b"x").unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();

    // 符号链接指向外部目录（unix 原生；win 无特权时跳过符号链接部分）
    let link = root.join("esc");
    let created = crate::symlink::create_symlink(
        &outside,
        &link,
        &crate::symlink::SymlinkPolicy {
            fallback: crate::symlink::Fallback::Reject,
        },
    );
    if created.is_ok() {
        remove_all_no_follow(&root).unwrap();
        assert!(!root.exists());
        // 外部目录原封不动
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
    } else {
        // win 无特权：符号链接创建被拒（Unsupported），跳过本分支
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `remove_all_no_follow` 删**目录链接**（Windows 目录符号链接/联接点、unix 符号链接）
/// 必须成功，且不碰链接目标。
///
/// 为什么单列一条（2026-10-13 CI 实测失败）：目录链接在 Windows 是「**目录**属性 +
/// reparse 点」的对象，用 `fs::remove_file`（DeleteFileW）删它 ⇒ **ERROR_ACCESS_DENIED(5)**，
/// 必须走 `fs::remove_dir`（RemoveDirectoryW）。上一条 `remove_no_follow` 在**没有符号链接
/// 特权**的机器上会走「创建被拒 → 跳过」分支，于是这条路径在开发机（未开开发者模式、
/// 非管理员）从未被执行，只在**管理员身份的 CI runner** 上暴露；本用例改用 **目录联接点**
/// （`mklink /J`，普通用户即可创建，且同为 DIRECTORY+REPARSE_POINT）⇒ 任何 Windows
/// 开发机都能挡住这个回归。unix 侧走 `symlink`（链接恒为链接对象，`remove_file` 正确）。
#[test]
fn remove_all_no_follow_removes_directory_link() {
    let dir = crate::testutil::temp_dir("fs-util-rmnf-dirlink");
    let root = dir.join("root");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();

    let link = root.join("dirlink");
    #[cfg(windows)]
    {
        // 分开传参（不要自己拼引号：cmd 会把整串再解析一遍，路径带引号反而报语法错）
        let out = std::process::Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&outside)
            .output()
            .expect("cmd 应可执行");
        assert!(
            out.status.success() && link.exists(),
            "建立目录联接点失败（普通用户即可建 /J）：{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    // 修前这里就是 CI 上的 `Access is denied. (os error 5)`
    remove_all_no_follow(&root).unwrap();
    assert!(!root.exists(), "整棵 root 应被删除（含目录链接本身）");
    assert_eq!(
        std::fs::read(outside.join("keep.txt")).unwrap(),
        b"keep",
        "链接目标（外部目录）不得被删"
    );
}
