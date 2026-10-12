use super::*;
use crate::ui::contracts::crumbs_from_current_dir;

// fmt_bytes 边界（K/M/G 舍入；B 原值）
#[test]
fn fmt_bytes_boundaries() {
    assert_eq!(fmt_bytes(0), "0 B");
    assert_eq!(fmt_bytes(1023), "1023 B");
    assert_eq!(fmt_bytes(1024), "1.0 KiB");
    assert_eq!(fmt_bytes(12 * 1024 + 410), "12.4 KiB");
    assert_eq!(fmt_bytes(1024 * 1024), "1.0 MiB");
    assert_eq!(fmt_bytes(2 * 1024 * 1024 * 1024), "2.0 GiB");
}

// 排序：目录恒前 + 名称/大小升降序（不改 VM 顺序的展示层语义）
#[test]
fn sort_browse_rows_dir_first_and_dirs() {
    let mk = |name: &str, is_dir: bool, size: &str| BrowseRow {
        entry_id: None,
        path: name.into(),
        name: name.into(),
        is_dir,
        is_container: false,
        encrypted: false,
        suspect: false,
        size_text: size.into(),
        packed_text: NO_VALUE.into(),
        encoding_text: NO_VALUE.into(),
        mtime_text: NO_VALUE.into(),
        is_symlink: false,
        symlink_target: None,
        type_text: NO_VALUE.into(),
        level_text: NO_VALUE.into(),
        encryption_text: NO_VALUE.into(),
        crc_text: NO_VALUE.into(),
        attrs_text: NO_VALUE.into(),
    };
    let mut rows = vec![
        mk("b.txt", false, "2.0 KiB"),
        mk("a", true, NO_VALUE),
        mk("a.txt", false, "1.0 KiB"),
    ];
    sort_browse_rows(&mut rows, SortColumn::Name, SortDir::Asc);
    assert_eq!(rows[0].name, "a", "目录恒排前");
    assert_eq!(rows[1].name, "a.txt");
    assert_eq!(rows[2].name, "b.txt");
    sort_browse_rows(&mut rows, SortColumn::Name, SortDir::Desc);
    assert_eq!(rows[0].name, "a", "目录仍排前（目录不参与反向）");
    assert_eq!(rows[1].name, "b.txt");
    sort_browse_rows(&mut rows, SortColumn::Size, SortDir::Asc);
    assert_eq!(rows[1].size_text, "1.0 KiB");
    sort_browse_rows(&mut rows, SortColumn::Size, SortDir::Desc);
    assert_eq!(rows[1].size_text, "2.0 KiB");
}

// mtime 解析失败 → 原样返回（不 panic、不吞数据）
#[test]
fn fmt_mtime_invalid_passthrough() {
    assert_eq!(fmt_mtime("not-a-date"), "not-a-date");
}

// ---- R-GUI4 #2：类型列 + 压缩后空占位 ----

// 类型标签：目录/软链接/容器/常见扩展/未知扩展/无扩展
#[test]
fn entry_type_label_matrix() {
    assert_eq!(entry_type_label("docs", true, false, false), "文件夹");
    assert_eq!(entry_type_label("link", false, true, false), "快捷方式");
    assert_eq!(entry_type_label("a.zip", false, false, true), "压缩包");
    assert_eq!(entry_type_label("a.txt", false, false, false), "文本文件");
    assert_eq!(
        entry_type_label("photo.PNG", false, false, false),
        "图片文件"
    );
    assert_eq!(
        entry_type_label("lib.rs", false, false, false),
        "源代码文件"
    );
    assert_eq!(
        entry_type_label("data.xyz", false, false, false),
        "XYZ 文件"
    );
    assert_eq!(entry_type_label("Makefile", false, false, false), "文件");
}

// 压缩后未知（size_packed=0）→ 空占位（不显示误导性 0B；R-GUI4 #2）
#[test]
fn packed_unknown_renders_placeholder_not_zero() {
    let row = crate::viewmodel::rows::TreeRow {
        path: "a.txt".into(),
        name: "a.txt".into(),
        depth: 0,
        is_dir: false,
        entry_id: None,
        expanded: false,
        has_children: false,
        is_container: false,
        encrypted: false,
        size_raw: 100,
        size_packed: 0, // 未计算（tar 等格式 browse 层不给 packed）
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,

        container_format: None,
    };
    let b = browse_row_from_tree_row(&row);
    assert_eq!(b.packed_text, NO_VALUE);
    assert_eq!(b.size_text, "100 B");
    assert_eq!(b.type_text, "文本文件");
}
// ---- T-G53：tree_items_from_nav 派生 ----

fn tree_row(path: &str, name: &str, depth: usize, is_dir: bool, expanded: bool) -> TreeRow {
    TreeRow {
        path: path.into(),
        name: name.into(),
        depth,
        is_dir,
        entry_id: None,
        expanded,
        has_children: is_dir,
        is_container: false,
        encrypted: false,
        size_raw: 0,
        size_packed: 0,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,

        container_format: None,
    }
}

// 嵌套派生：目录进树、文件跳过 → 根 1 项（docs）
#[test]
fn tree_items_derive_dirs_only() {
    let rows = vec![
        tree_row("docs", "docs", 0, true, true),
        tree_row("docs/src", "src", 1, true, true),
        tree_row("docs/src/a.txt", "a.txt", 2, false, false),
        tree_row("README.md", "README.md", 0, false, false),
    ];
    let (items, meta) = tree_items_from_nav(&rows);
    assert_eq!(items.len(), 1, "文件不进树：根层只剩 docs");
    // id = `"{level}:{path}"`（跨嵌套层唯一）；解回来应还原层号与路径
    assert_eq!(split_tree_id(&items[0].id), Some((0, "docs")));
    assert_eq!(items[0].children.len(), 1, "docs 下挂 src");
    assert!(
        items[0].children[0].children.is_empty(),
        "文件行不进树：src 无子"
    );
    // meta：两个目录均在表内（is_dir=true、path 不带层号前缀），文件不在
    assert!(
        meta.get("0:docs")
            .is_some_and(|m| m.is_dir && m.path == "docs")
    );
    assert!(
        meta.get("0:docs/src")
            .is_some_and(|m| m.is_dir && m.path == "docs/src")
    );
    assert!(!meta.contains_key("0:README.md"));
    assert!(!meta.contains_key("0:docs/src/a.txt"));
}

// 懒展开语义：expanded=false 的目录行无子行进快照 → 派生为叶子
#[test]
fn tree_items_lazy_expansion_collapsed_dir_has_no_children() {
    let rows = vec![
        tree_row("docs", "docs", 0, true, false),
        tree_row("src", "src", 0, true, true),
        tree_row("src/lib", "lib", 1, true, true),
        tree_row("src/lib.rs", "lib.rs", 1, false, false),
    ];
    let (items, _) = tree_items_from_nav(&rows);
    assert_eq!(items.len(), 2);
    assert!(
        items[0].children.is_empty(),
        "未展开目录（VM 未铺子行）→ 叶子态"
    );
    assert_eq!(items[1].children.len(), 1, "展开目录子目录折叠为 child");
}

// 深链同层兄弟折叠：深度回退正确闭合多层
#[test]
fn tree_items_depth_regression_closes_stack() {
    let rows = vec![
        tree_row("a", "a", 0, true, true),
        tree_row("a/b", "b", 1, true, true),
        tree_row("a/b/c", "c", 2, true, true),
        tree_row("a/b/c/d", "d", 3, true, true),
        tree_row("top", "top", 0, true, false),
    ];
    let (items, _) = tree_items_from_nav(&rows);
    assert_eq!(items.len(), 2);
    let b = &items[0].children[0];
    let c = &b.children[0];
    assert_eq!(split_tree_id(&c.children[0].id), Some((0, "a/b/c/d")));
    assert!(items[1].children.is_empty());
}

// 容器行（内层压缩包）进树且可展开；跨层 id 唯一（同名目录内外各一份）
//
// 守的是本次改动：旧实现 `!row.is_dir → continue` 把容器挡在树外，
// 于是「内层压缩包当子目录展示」无从谈起。
#[test]
fn tree_items_keep_container_rows_and_namespace_ids_by_level() {
    let mut child = tree_row("sub/child.7z", "child.7z", 1, false, true);
    child.is_container = true;
    child.level = 1; // 外层会话里的容器节点
    child.has_children = true;
    let inner = tree_row("deep", "deep", 2, true, false); // 内层会话的目录
    let mut outer_deep = tree_row("deep", "deep", 0, true, false); // 外层同名目录
    outer_deep.level = 1;
    let rows = vec![
        tree_row("sub", "sub", 0, true, true),
        child,
        inner,
        outer_deep,
    ];
    let (items, meta) = tree_items_from_nav(&rows);
    let sub = &items[0];
    let container = &sub.children[0];
    assert_eq!(
        split_tree_id(&container.id),
        Some((1, "sub/child.7z")),
        "容器节点保留在树里，id 带层号"
    );
    assert!(
        meta.get("1:sub/child.7z").is_some_and(|m| m.is_container),
        "容器标记进 meta（图标与点击路由都靠它）"
    );
    assert_eq!(
        split_tree_id(&container.children[0].id),
        Some((0, "deep")),
        "内层会话的行挂在容器下"
    );
    // 同名目录不撞车：外层 `deep`（层 1）与内层 `deep`（层 0）id 不同
    assert_eq!(split_tree_id(&items[1].id), Some((1, "deep")));
    assert_ne!(
        container.children[0].id, items[1].id,
        "同名目录分处两层，id 必须可区分"
    );
    assert!(meta.get("1:deep").is_some_and(|m| m.is_dir));
}

// 空快照 → 空根
#[test]
fn tree_items_empty_snapshot_yields_empty() {
    let (items, meta) = tree_items_from_nav(&[]);
    assert!(items.is_empty());
    assert!(meta.is_empty());
}

// expanded 位从 TreeRow 传播到 TreeItem（VM 事实源同步前提）
#[test]
fn tree_items_propagate_expanded_bit() {
    let rows = vec![
        tree_row("docs", "docs", 0, true, true),
        tree_row("docs/src", "src", 1, true, false),
        tree_row("target", "target", 0, true, false),
    ];
    let (items, _) = tree_items_from_nav(&rows);
    assert!(items[0].is_expanded(), "docs expanded=true 传播");
    assert!(
        !items[0].children[0].is_expanded(),
        "src expanded=false 传播"
    );
    assert!(!items[1].is_expanded(), "target expanded=false 传播");
}

// ---- T-G53：.. 行可见性 ----

#[test]
fn parent_row_visibility_follows_crumb_depth() {
    let root = crumbs_from_current_dir("demo.zip", "");
    assert!(!show_parent_row(&root), "档案根不给 .. 行");
    let deep = crumbs_from_current_dir("demo.zip", "docs/src");
    assert!(show_parent_row(&deep), "非根目录给 .. 行");
}

mod jobs_row_ui_tests {
    use super::*;
    use crate::viewmodel::jobs_vm::JobsViewModel;

    /// 失败原因落进任务中心的失败行：逐条目明细优先，缺失时用终局摘要（打开记录
    /// 与「快速失败」的任务都只有终局摘要——用户口径 2026-09-24 要求任务中心
    /// 必须显示明确原因）。
    #[test]
    fn failure_reason_from_final_line_when_no_entry_failures() {
        let mut vm = JobsViewModel::new();
        let err: Result<(), String> = Err("HZ-1002: 磁盘空间不足 (C:/Temp)".into());
        let id = vm.record_open("x.cpio.gz", "C:/tmp/x.cpio.gz", &err);
        let ui = jobs_row_ui(vm.row(id).unwrap());
        assert_eq!(ui.failures.len(), 1, "失败行必须出现在任务中心");
        assert!(
            ui.failures[0].0.is_empty() && ui.failures[0].1.contains("磁盘空间不足"),
            "整行原因：{:?}",
            ui.failures[0]
        );
        assert_eq!(ui.action_text, crate::tr("job.kind_open"));
        assert_eq!(ui.state_label, crate::tr("job.state_failed"));
        assert!(!ui.is_active, "打开记录恒终态（无双击重开/取消）");

        // 成功记录：无失败行
        let ok: Result<(), String> = Ok(());
        let id_ok = vm.record_open("y.zip", "C:/tmp/y.zip", &ok);
        let ui_ok = jobs_row_ui(vm.row(id_ok).unwrap());
        assert!(ui_ok.failures.is_empty());
        assert_eq!(ui_ok.state_label, crate::tr("job.state_done"));
    }
}
