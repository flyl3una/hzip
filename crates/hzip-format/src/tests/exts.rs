use super::*;

/// 表自身的一致性：格式 id 与扩展名不重复；复合族的 container 与其
/// **内层容器**一致（`tar.*` → tar、`cpio.*` → cpio）。
///
/// 复合族不止 tar 一族之后，判据从「都是 tar」放宽为「container 恒等于格式
/// id 的第一层」——这既覆盖 tar 族，也覆盖 cpio 族，且仍能抓住
/// 「`cpio.gz` 误写成下钻 tar」这类错配。
#[test]
fn table_is_self_consistent() {
    let mut seen = Vec::new();
    for e in FORMAT_EXTS {
        for ext in e.exts {
            assert!(
                ext.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.'),
                "扩展名须为小写/数字/点：{ext}"
            );
            assert!(!ext.starts_with('.'), "扩展名不含前导点：{ext}");
            assert!(!seen.contains(ext), "扩展名重复：{ext}");
            seen.push(*ext);
        }
        assert!(!e.exts.is_empty(), "格式 {} 无扩展名", e.format);
        if e.kind == ExtKind::TarComposite {
            let inner = components(e.format).next().expect("复合格式 id 非空");
            assert_eq!(
                e.container,
                Some(inner),
                "{} 应下钻到内层容器 {inner}",
                e.format
            );
            assert!(
                components(e.format).nth(1).is_some(),
                "{} 复合 id 必须含压缩层（`容器.codec`）",
                e.format
            );
        }
    }
}

/// 别名/复合写法必须被认出（用户报的缺陷：`tgz` 不被当档案）。
#[test]
fn aliases_are_recognized() {
    for name in [
        "a.zip",
        "a.7z",
        "a.rar",
        "a.tar",
        "a.tgz",
        "a.tar.gz",
        "a.tbz",
        "a.tbz2",
        "a.tar.bz2",
        "a.txz",
        "a.tar.xz",
        "a.tzst",
        "a.tar.zst",
        "a.gz",
        "a.bz2",
        "a.xz",
        "a.zst",
        "A.TGZ",
    ] {
        assert!(is_archive_ext(name), "{name} 应被认作档案");
    }
    for name in ["a.txt", "notzip", "a.zip.txt", "a.tgzx", "tgz", "", "."] {
        assert!(!is_archive_ext(name), "{name} 不应被认作档案");
    }
}

/// 组合档内层容器层名（浏览视图两层观感的命名规则）。
#[test]
fn composite_inner_name_matrix() {
    let cases = [
        ("init.cpio.gz", Some("init.cpio")),
        ("initramfs.cpio.xz", Some("initramfs.cpio")),
        ("data.tar.gz", Some("data.tar")),
        ("a.tgz", Some("a.tar")), // 别名归一成标准名
        ("a.tbz2", Some("a.tar")),
        ("我的档案.cpio.bz2", Some("我的档案.cpio")),
        ("DATA.CPIO.GZ", Some("DATA.cpio")), // 大小写不敏感（词干保原样）
    ];
    for (name, want) in cases {
        assert_eq!(
            composite_inner_name(name).as_deref(),
            want,
            "{name} 的内层容器层名"
        );
    }
    // 无压缩层 / 非档案 / 单流：不加层
    for name in ["a.zip", "a.tar", "a.cpio", "a.txt.gz", "a.txt", "plain", ""] {
        assert_eq!(composite_inner_name(name), None, "{name} 不应加层");
    }
}

/// 最长后缀优先：复合写法不得被单流族抢走（决定下钻 handler）。
#[test]
fn longest_suffix_wins() {
    assert_eq!(find("a.tar.gz").map(|e| e.format), Some("tar.gz"));
    assert_eq!(container_handler("a.tar.gz"), Some("tar"));
    assert_eq!(container_handler("a.tgz"), Some("tar"));
    assert_eq!(container_handler("a.tbz2"), Some("tar"));
    assert_eq!(container_handler("a.tar.zst"), Some("tar"));
    assert_eq!(container_handler("a.zip"), Some("zip"));
    assert_eq!(container_handler("a.7z"), Some("7z"));
    // 单流：认出但不作容器（解出的是单文件）
    assert_eq!(container_handler("a.gz"), None);
    assert_eq!(find("a.gz").map(|e| e.format), Some("gz"));
    assert_eq!(container_handler("a.rar"), None);
    // 未知
    assert!(find("a.txt").is_none());
}

/// 下钻时「容器 + 待解压缩层」：复合条目必须先解一层再交容器 handler
/// （`Browser::enter` 的接线依据）。
#[test]
fn container_and_codec_pairs() {
    assert_eq!(container_and_codec("a.zip"), Some(("zip", None)));
    assert_eq!(container_and_codec("a.7z"), Some(("7z", None)));
    assert_eq!(container_and_codec("a.tar"), Some(("tar", None)));
    assert_eq!(container_and_codec("a.tgz"), Some(("tar", Some("gz"))));
    assert_eq!(container_and_codec("a.tar.gz"), Some(("tar", Some("gz"))));
    assert_eq!(container_and_codec("a.tbz2"), Some(("tar", Some("bz2"))));
    assert_eq!(container_and_codec("a.txz"), Some(("tar", Some("xz"))));
    assert_eq!(container_and_codec("a.tzst"), Some(("tar", Some("zst"))));
    // 不可下钻：单流压缩、rar、未知
    assert_eq!(container_and_codec("a.gz"), None);
    assert_eq!(container_and_codec("a.rar"), None);
    assert_eq!(container_and_codec("a.txt"), None);
}

/// 关联候选只含单点扩展名，且覆盖全部别名（Windows 以最后一个点为准）。
#[test]
fn assoc_candidates_are_single_dot() {
    let assoc: Vec<_> = assoc_exts().collect();
    assert!(assoc.iter().all(|e| !e.contains('.')), "{assoc:?}");
    assert!(assoc.contains(&"tgz"), "缺 tgz：{assoc:?}");
    assert!(assoc.contains(&"tbz2") && assoc.contains(&"txz") && assoc.contains(&"tzst"));
    assert!(assoc.contains(&"tar") && assoc.contains(&"gz"));
    assert!(
        !assoc.contains(&"tar.gz"),
        "复合写法不做关联候选（Windows 走最后一个点）"
    );
    // 表序 ≠ 字典序：展示顺序由表决定
    assert_eq!(assoc.first(), Some(&"zip"));
}

/// 格式 id 组成层：形态匹配（下钻 tar.gz 得到 tar）——正是组合嗅探的装配依据。
#[test]
fn components_shape() {
    assert_eq!(components("zip").collect::<Vec<_>>(), ["zip"]);
    assert_eq!(components("tar.gz").collect::<Vec<_>>(), ["tar", "gz"]);
    assert_eq!(components("tar.bz2").collect::<Vec<_>>(), ["tar", "bz2"]);
}
