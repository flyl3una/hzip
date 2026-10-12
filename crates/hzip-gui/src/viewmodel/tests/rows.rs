use super::*;

#[test]
fn rows_are_cloneable_pure_data() {
    let e = EntryRow {
        id: 0,
        name: "a.txt".into(),
        is_dir: false,
        size_raw: 3,
        size_packed: 3,
        mtime: None,
        encrypted: false,
        header_encrypted: false,
        name_encoding: NameEncoding::Utf8,
        is_symlink: false,
        symlink_target: None,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
    };
    let e2 = e.clone();
    assert_eq!(e.name, e2.name);
    // 红线声明：类型不含 gpui/gpui_kit（编译面即断言；grep 验收见 T41 §6.3）
    let _ = e2;
}

mod dir_size_map_tests {
    use super::*;

    fn file_entry(id: usize, name: &str, raw: u64, packed: u64) -> EntryRow {
        EntryRow {
            id,
            name: name.into(),
            is_dir: false,
            size_raw: raw,
            size_packed: packed,
            mtime: None,
            encrypted: false,
            header_encrypted: false,
            name_encoding: NameEncoding::Utf8,
            is_symlink: false,
            symlink_target: None,
            encryption: None,
            compression_level: None,
            crc32: None,
            attrs: Default::default(),
        }
    }

    // 嵌套聚合：目录 = 子孙文件之和（多层传递）；文件无祖先则不入任何目录
    #[test]
    fn aggregates_nested_dirs() {
        let entries = vec![
            file_entry(0, "docs/src/a.txt", 100, 80),
            file_entry(1, "docs/src/b.txt", 50, 50),
            file_entry(2, "docs/readme", 10, 10),
            file_entry(3, "top.txt", 7, 7),
        ];
        let map = dir_size_map(&entries);
        assert_eq!(map.get("docs"), Some(&(160, 140)), "docs = 100+50+10");
        assert_eq!(map.get("docs/src"), Some(&(150, 130)), "docs/src = 100+50");
        assert!(!map.contains_key("top.txt"), "顶层文件不产生目录键");
    }

    // 空目录：无子孙贡献 → 不在表中（调用方保持原值 → 展示层回 NO_VALUE）
    #[test]
    fn empty_dir_absent() {
        let entries = vec![file_entry(0, "a.txt", 5, 5)];
        let map = dir_size_map(&entries);
        assert!(!map.contains_key("empty"), "空目录不在表中");
        assert!(map.is_empty(), "无嵌套文件 → 表为空");
    }

    // 段边界：`a/bc/x` 计入 `a` 与 `a/bc`，不计入 `a/b`
    #[test]
    fn ancestor_enum_is_segment_scoped() {
        let entries = vec![file_entry(0, "a/bc/x", 9, 9)];
        let map = dir_size_map(&entries);
        assert_eq!(map.get("a"), Some(&(9, 9)));
        assert_eq!(map.get("a/bc"), Some(&(9, 9)));
        assert!(!map.contains_key("a/b"), "`a/b` 不是 `a/bc/x` 的祖先");
    }

    // 段边界（非前缀）：`abc/x` 与 `ab` 无包含关系
    #[test]
    fn no_false_prefix() {
        let entries = vec![file_entry(0, "abc/x", 9, 9)];
        let map = dir_size_map(&entries);
        assert!(!map.contains_key("ab"), "`abc/x` 不计入 `ab`（bc ≠ b）");
        assert_eq!(map.get("abc"), Some(&(9, 9)));
    }

    // 目录条目自身大小不计（zip 目录条目 size 恒 0 语义——即便非 0 也不计）
    #[test]
    fn dir_entries_do_not_contribute() {
        let mut dir = file_entry(0, "d", 999, 999);
        dir.is_dir = true;
        let entries = vec![dir, file_entry(1, "d/a.txt", 3, 3)];
        let map = dir_size_map(&entries);
        assert_eq!(map.get("d"), Some(&(3, 3)), "目录条目自身 999 不计入");
    }

    // —— T-OPT 项2：子树表与旧实现（`entries.starts_with("{dir}/")` 全量扫描）
    // 逐项等价；差别只在**目录条目也参与**（本表不依赖「目录 size 恒 0」假设）——
    #[test]
    fn subtree_map_matches_prefix_scan_reference() {
        // 参照实现 = 被替换掉的旧 `dir_subtree_sizes` 内层循环，逐字保留。
        fn reference(entries: &[EntryRow], dir: &str) -> (u64, u64) {
            let prefix = format!("{dir}/");
            let (mut raw, mut packed) = (0, 0);
            for e in entries {
                if e.name.starts_with(&prefix) {
                    raw += e.size_raw;
                    packed += e.size_packed;
                }
            }
            (raw, packed)
        }
        let mut dir_entry = file_entry(2, "d/sub", 999, 998);
        dir_entry.is_dir = true;
        let entries = vec![
            dir_entry,
            file_entry(0, "d/sub/a.txt", 100, 80),
            file_entry(1, "d/b.txt", 50, 50),
            file_entry(3, "top.txt", 7, 7),
        ];
        let map = dir_subtree_size_map(&entries);
        for dir in ["d", "d/sub", "top.txt", "不存在"] {
            let expect = reference(&entries, dir);
            let got = map.get(dir).copied().unwrap_or((0, 0));
            assert_eq!(got, expect, "目录 {dir} 子树合计须与旧前缀扫描一致");
        }
        assert_eq!(map.get("d"), Some(&(1149, 1128)), "d = 999 + 100 + 50");
    }

    // 空目录/无子孙 → 不在表中（调用方取 (0,0)，与旧实现空和一致）
    #[test]
    fn subtree_map_absent_for_leaf_dirs() {
        let map = dir_subtree_size_map(&[file_entry(0, "a.txt", 5, 5)]);
        assert!(map.is_empty(), "无嵌套条目 → 表为空");
    }
}
