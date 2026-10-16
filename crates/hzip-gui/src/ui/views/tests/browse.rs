use super::*;
use crate::ui::binding::browse_row_from_entry_row;
use crate::viewmodel::rows::TreeRow;

// 快照应用 → 行派生：徽标/编码/目录占位（headless 纯数据面断言）
#[test]
fn snapshot_rows_derive_badges_and_placeholders() {
    let row = crate::viewmodel::EntryRow {
        id: 3,
        name: "docs/报告.txt".into(),
        is_dir: false,
        size_raw: 12 * 1024 + 410,
        size_packed: 4 * 1024,
        mtime: None,
        encrypted: true,
        header_encrypted: false,
        name_encoding: hzip_format::NameEncoding::Suspect(hzip_format::Encoding::Gbk),
        is_symlink: false,
        symlink_target: None,
        encryption: Some("ZipCrypto"),
        compression_level: None,
        crc32: Some(0xDEADBEEF),
        attrs: Default::default(),
    };
    let b = browse_row_from_entry_row(&row);
    assert_eq!(b.entry_id, Some(3));
    assert_eq!(b.name, "报告.txt");
    assert!(b.encrypted && b.suspect);
    assert_eq!(b.encoding_text, "GBK");
    assert_eq!(b.size_text, "12.4 KiB");

    let dir = crate::viewmodel::EntryRow {
        is_dir: true,
        ..row
    };
    let bd = browse_row_from_entry_row(&dir);
    assert_eq!(bd.size_text, crate::ui::binding::NO_VALUE);
}

// 新四列绑定派生（2026-09-25 用户口径；备注列 2026-09-26 下线）：级别/加密/CRC/属性
#[test]
fn new_four_columns_derive_texts() {
    use crate::ui::binding::NO_VALUE;
    use crate::ui::binding::{attrs_text, crc_text, encryption_text, level_text};
    use hzip_format::EntryAttrs;

    // 级别：数值/未知
    assert_eq!(level_text(Some(6)), "6");
    assert_eq!(level_text(None), NO_VALUE);

    // 加密：未加密恒 `—`（不显示「无」）；加密带算法；加密无细分（rar4）
    assert_eq!(encryption_text(false, None), NO_VALUE);
    assert_eq!(encryption_text(false, Some("AES-256")), NO_VALUE);
    assert_eq!(encryption_text(true, Some("AES-256")), "AES-256");
    assert_eq!(encryption_text(true, None), NO_VALUE);

    // CRC：8 位大写十六进制；未知占位
    assert_eq!(crc_text(Some(0xDEADBEEF)), "DEADBEEF");
    assert_eq!(crc_text(Some(0x1)), "00000001");
    assert_eq!(crc_text(None), NO_VALUE);

    // 属性：标记按 只读/隐藏/系统/可执行 拼；全无 → `—`；可执行未知不显示
    assert_eq!(attrs_text(EntryAttrs::default()), NO_VALUE);
    assert_eq!(
        attrs_text(EntryAttrs {
            read_only: true,
            hidden: true,
            ..Default::default()
        }),
        "只读 隐藏"
    );
    assert_eq!(
        attrs_text(EntryAttrs {
            executable: Some(true),
            ..Default::default()
        }),
        "可执行"
    );
    assert_eq!(
        attrs_text(EntryAttrs {
            executable: None,
            ..Default::default()
        }),
        NO_VALUE,
        "可执行未知（None）不猜测"
    );

    // 全链路：EntryRow → BrowseRow 四列齐备
    let row = crate::viewmodel::EntryRow {
        id: 0,
        name: "a.txt".into(),
        is_dir: false,
        size_raw: 1,
        size_packed: 1,
        mtime: None,
        encrypted: true,
        header_encrypted: false,
        name_encoding: hzip_format::NameEncoding::Utf8,
        is_symlink: false,
        symlink_target: None,
        encryption: Some("AES-256"),
        compression_level: Some(9),
        crc32: Some(0xDEADBEEF),
        attrs: EntryAttrs {
            read_only: true,
            ..Default::default()
        },
    };
    let b = browse_row_from_entry_row(&row);
    assert_eq!(b.level_text, "9");
    assert_eq!(b.encryption_text, "AES-256");
    assert_eq!(b.crc_text, "DEADBEEF");
    assert_eq!(b.attrs_text, "只读");
}

// 空档案占位态：0 行快照 → 不 panic（计数面）
#[test]
fn empty_snapshot_yields_zero_rows() {
    let snap = AppStateUiSnapshot::default();
    assert!(snap.dir_rows.is_empty());
    assert_eq!(snap.entry_count, 0);
}

// ---- T-G53：行模型下标（.. 行 + 实体行编址） ----
fn tree_row(path: &str, name: &str, depth: usize, is_dir: bool, expanded: bool) -> TreeRow {
    TreeRow {
        path: path.into(),
        name: name.into(),
        depth,
        is_dir,
        entry_id: Some(1),
        expanded,
        has_children: is_dir,
        is_container: false,
        encrypted: false,
        size_raw: 12,
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

fn delegate(rows_n: usize, show_parent: bool) -> BrowseDelegate {
    BrowseDelegate {
        rows: (0..rows_n)
            .map(|i| {
                browse_row_from_tree_row(&tree_row(
                    &format!("f{i}.txt"),
                    &format!("f{i}.txt"),
                    0,
                    false,
                    false,
                ))
            })
            .collect(),
        current_dir: String::new(),
        show_dotdot: show_parent,
        show_parent,
        nested: false,
        columns: crate::ui::contracts::ColumnsConfig::default(),
        visible: visible_columns_of(&crate::ui::contracts::ColumnsConfig::default()),
        widths: Vec::new(),
        sort: None,
        last_modifiers: Modifiers::default(),
        last_click_count: 0,
        last_hit_row: false,
        press_hit_row: false,
        anchor: None,
        selected_ids: std::collections::HashSet::new(),
        selected_dirs: std::collections::HashSet::new(),
        last_sel_sync: (Vec::new(), Vec::new()),
        can_edit: false,
        right_hit_row: false,
        name_rename: None,
        name_draft: false,
        name_input: None,
        kit_focus: None,
    }
}

#[test]
fn data_row_indexing_with_parent_row() {
    let d = delegate(3, true);
    assert!(matches!(data_row(&d, 0), Some(DataRowRef::Parent)));
    assert!(matches!(data_row(&d, 1), Some(DataRowRef::Entry(_))));
    assert!(matches!(data_row(&d, 3), Some(DataRowRef::Entry(_))));
    assert!(data_row(&d, 4).is_none(), "越界 None（.. + 3 实体行）");

    let d = delegate(3, false);
    assert!(matches!(data_row(&d, 0), Some(DataRowRef::Entry(_))));
    assert!(data_row(&d, 3).is_none());
}

#[test]
fn rows_count_includes_parent_row() {
    assert_eq!(browse_rows_count(&delegate(3, true)), 4);
    assert_eq!(browse_rows_count(&delegate(3, false)), 3);
    assert_eq!(browse_rows_count(&delegate(0, false)), 0);
}

// `..` 目标段 = current_dir 剥尾段（R-GUI 走查 Fix3：曾用 rows.first()
// 推导——空目录时 None→""（误跳根）/`..` 混入时取错目标）
#[test]
fn dotdot_parent_target_derives_from_current_dir() {
    let mut d = delegate(0, true);
    d.current_dir = "docs/src".into();
    assert_eq!(parent_seg_of(&d.current_dir), "docs");
    d.current_dir = "docs".into();
    assert_eq!(parent_seg_of(&d.current_dir), "");
    // 根层不显 `..`（show_parent=false）——current_dir 为空仅防御性返回 ""
    d.current_dir.clear();
    assert_eq!(parent_seg_of(&d.current_dir), "");
}

// T-G53 验收：列序消费 ColumnsConfig::visible()（默认配置全序）
#[test]
fn column_order_consumes_columns_config_visible() {
    let ids = visible_columns_of(&BrowseDelegate::default().columns);
    let expect = crate::ui::contracts::ColumnsConfig::default().visible();
    assert_eq!(ids, expect, "渲染列序 = 契约列序（默认配置）");
    for id in &ids {
        assert!(!id.label().is_empty(), "列头文案非空：{}", id.label());
    }
}

// 列头排序三态（R-GUI 走查）：Ascending/Descending 重排 + Default 恢复
// VM 原序（目录恒前 + 名升序）；重灌重排走同一纯函数
#[test]
fn sort_delegate_rows_cycles_and_restores() {
    fn rows() -> Vec<BrowseRow> {
        vec![
            browse_row_from_tree_row(&tree_row("b.txt", "b.txt", 0, false, false)),
            browse_row_from_tree_row(&tree_row("zdir", "zdir", 0, true, false)),
            browse_row_from_tree_row(&tree_row("a.txt", "a.txt", 0, false, false)),
        ]
    }
    // 名称降序：目录恒前，文件 z→b... 名降序 = b.txt, a.txt（目录 zdir 恒首）
    let mut r = rows();
    sort_delegate_rows(&mut r, BrowseColumnId::Name, ColumnSort::Descending);
    assert_eq!(
        r.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        vec!["zdir", "b.txt", "a.txt"],
        "降序：目录恒前 + 名降序"
    );
    // 名称升序
    let mut r = rows();
    sort_delegate_rows(&mut r, BrowseColumnId::Name, ColumnSort::Ascending);
    assert_eq!(
        r.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        vec!["zdir", "a.txt", "b.txt"]
    );
    // Default（第三击取消）→ 恢复原序（等价排序：目录前 + 名升序）
    let mut r = rows();
    sort_delegate_rows(&mut r, BrowseColumnId::Name, ColumnSort::Descending);
    sort_delegate_rows(&mut r, BrowseColumnId::Name, ColumnSort::Default);
    assert_eq!(
        r.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        vec!["zdir", "a.txt", "b.txt"],
        "Default 恢复 VM 原序"
    );
}

// 列头指示器回填：排序列带方向、其余列 Default（可排）——
// table.refresh 重建 col_groups 后指示器不丢（column() 需 &App 现场）
#[gpui_kit::test]
fn column_builder_carries_sort_state(cx: &mut gpui_kit::TestAppContext) {
    cx.update(|cx: &mut gpui_kit::App| {
        let mut d = delegate(3, false);
        d.sort = Some((BrowseColumnId::Size, ColumnSort::Descending));
        // 可见列序 = [Name, Type, Size, ...]（默认全序）
        let name_col = d.column(0, cx);
        assert_eq!(name_col.sort, Some(ColumnSort::Default), "非排序列 Default");
        let size_col = d.column(2, cx);
        assert_eq!(size_col.sort, Some(ColumnSort::Descending), "排序列带方向");
    });
}

// 类型列排序（SortColumn::Type：type_text 字典序）
#[test]
fn sort_by_type_column_orders_by_type_text() {
    let mut a = browse_row_from_tree_row(&tree_row("a.txt", "a.txt", 0, false, false));
    a.type_text = "文本文件".into();
    let mut b = browse_row_from_tree_row(&tree_row("z", "z", 0, true, false));
    b.type_text = "文件夹".into();
    let mut rows = vec![a, b];
    sort_browse_rows(
        &mut rows,
        crate::ui::contracts::SortColumn::Type,
        crate::ui::contracts::SortDir::Asc,
    );
    assert_eq!(rows[0].name, "z", "type_text 升序：文件夹 < 文本文件");
}

// nav 快照签名：同序同值 → 同签；任一变化 → 异签（set_items 门控）
#[test]
fn nav_snapshot_sig_detects_changes() {
    let a = vec![
        tree_row("docs", "docs", 0, true, true),
        tree_row("docs/src", "src", 1, true, false),
    ];
    let b = a.clone();
    assert_eq!(nav_snapshot_sig(&a), nav_snapshot_sig(&b));
    let toggled = vec![
        tree_row("docs", "docs", 0, true, false),
        tree_row("docs/src", "src", 1, true, false),
    ];
    assert_ne!(nav_snapshot_sig(&a), nav_snapshot_sig(&toggled));
    let reordered = vec![
        tree_row("docs/src", "src", 1, true, false),
        tree_row("docs", "docs", 0, true, true),
    ];
    assert_ne!(nav_snapshot_sig(&a), nav_snapshot_sig(&reordered));
    assert_ne!(nav_snapshot_sig(&a), nav_snapshot_sig(&[]));
}

// T-PERF2 表格重灌门控：dir 签名同序同值同签；行内容（含目录聚合
// 大小回填）/次序/条目数变化异签。漏检展示字段 = 该列改了不刷新。
#[test]
fn dir_snapshot_sig_detects_changes() {
    let a = vec![
        tree_row("docs", "docs", 0, true, true),
        tree_row("a.txt", "a.txt", 0, false, false),
    ];
    assert_eq!(dir_snapshot_sig(&a), dir_snapshot_sig(&a.clone()));

    // 目录聚合大小回填（show_folder_size 开关的核心场景）
    let mut sized = a.clone();
    sized[0].size_raw = 1024;
    assert_ne!(dir_snapshot_sig(&a), dir_snapshot_sig(&sized));

    // 次序变化（排序切换）
    let mut reordered = a.clone();
    reordered.reverse();
    assert_ne!(dir_snapshot_sig(&a), dir_snapshot_sig(&reordered));

    // 条目增删 / mtime 变化
    let removed = vec![a[0].clone()];
    assert_ne!(dir_snapshot_sig(&a), dir_snapshot_sig(&removed));
    let mut dated = a.clone();
    dated[1].mtime = Some("2026-01-01T00:00:00Z".into());
    assert_ne!(dir_snapshot_sig(&a), dir_snapshot_sig(&dated));
    assert_ne!(dir_snapshot_sig(&a), dir_snapshot_sig(&[]));
}

// T-SEL1 多选集合运算：普通/Ctrl/Shift 三分支（资源管理器语义）+
// `..` 清空 + 无效锚点退化 + T-SEL 走查 #1 双通道（隐式目录按路径）
#[test]
fn click_selection_explorer_semantics() {
    use std::collections::HashSet;
    // 可见行 [a(实体), d(隐式目录), c(实体), e(隐式目录)]
    let visible = vec![
        RowSel::Entry(10),
        RowSel::Dir("d".into()),
        RowSel::Entry(30),
        RowSel::Dir("e".into()),
    ];
    let set = |ids: &[u64]| ids.iter().copied().collect::<HashSet<u64>>();
    let dirs = |ds: &[&str]| {
        ds.iter()
            .map(|d| d.to_string())
            .collect::<HashSet<String>>()
    };

    // 普通单击实体行：整组替换 + 锚点移至该行
    let (_ids, ds, anchor) = click_selection(
        &set(&[10]),
        &dirs(&[]),
        &RowSel::Entry(20),
        false,
        false,
        &visible,
        Some(0),
    );
    // 20 不在可见集 → 防御分支普通替换
    assert_eq!((_ids, ds), (vec![20], Vec::<String>::new()));
    assert_eq!(anchor, None);

    // Ctrl 单击实体行：增删单条 + 锚点移至该行（集合升序出参）
    let (ids, ds, _) = click_selection(
        &set(&[10, 30]),
        &dirs(&[]),
        &RowSel::Entry(30),
        true,
        false,
        &visible,
        Some(2),
    );
    assert_eq!((ids, ds), (vec![10], Vec::<String>::new()), "Ctrl 移除");
    let (ids, _, a) = click_selection(
        &set(&[10]),
        &dirs(&[]),
        &RowSel::Entry(30),
        true,
        false,
        &visible,
        None,
    );
    assert_eq!(ids, vec![10, 30], "Ctrl 加入");
    assert_eq!(a, Some(2), "锚点移至点击行");

    // Ctrl 单击隐式目录行：目录集增删
    let (_ids, ds, _) = click_selection(
        &set(&[]),
        &dirs(&["d"]),
        &RowSel::Dir("e".into()),
        true,
        false,
        &visible,
        None,
    );
    assert_eq!(ds, vec!["d".to_string(), "e".to_string()], "Ctrl 目录加入");
    let (_ids, ds, _) = click_selection(
        &set(&[]),
        &dirs(&["d", "e"]),
        &RowSel::Dir("d".into()),
        true,
        false,
        &visible,
        Some(0),
    );
    assert_eq!(ds, vec!["e".to_string()], "Ctrl 目录移除");
    assert!(_ids.is_empty());

    // 普通单击隐式目录行：目录单选
    let (ids, ds, a) = click_selection(
        &set(&[10]),
        &dirs(&[]),
        &RowSel::Dir("d".into()),
        false,
        false,
        &visible,
        Some(0),
    );
    assert_eq!(ids, Vec::<u64>::new());
    assert_eq!(ds, vec!["d".to_string()]);
    assert_eq!(a, Some(1));

    // Shift 单击：锚点区间整组替换（区间内实体行进 id 集、目录行进
    // 目录集），锚点不变
    let (ids, ds, a) = click_selection(
        &set(&[10]),
        &dirs(&[]),
        &RowSel::Entry(30),
        false,
        true,
        &visible,
        Some(0),
    );
    assert_eq!(ids, vec![10, 30]);
    assert_eq!(ds, vec!["d".to_string()]);
    assert_eq!(a, Some(0), "Shift 不移锚点");
    // 无锚点 → 退化为普通替换 + 补锚点
    let (ids, _, a) = click_selection(
        &set(&[10]),
        &dirs(&[]),
        &RowSel::Entry(30),
        false,
        true,
        &visible,
        None,
    );
    assert_eq!(ids, vec![30]);
    assert_eq!(a, Some(2));

    // `..` 行（RowSel::None）→ 双集清空
    let (ids, ds, a) = click_selection(
        &set(&[10]),
        &dirs(&["d"]),
        &RowSel::None,
        false,
        false,
        &visible,
        Some(0),
    );
    assert!(ids.is_empty() && ds.is_empty());
    assert_eq!(a, None);

    // 防御：点击行不在可见集 → 普通替换（id/路径各自兜底）
    let (ids, _ds, _) = click_selection(
        &set(&[10]),
        &dirs(&[]),
        &RowSel::Entry(99),
        true,
        false,
        &visible,
        None,
    );
    assert_eq!(ids, vec![99]);
    let (ids, ds, _) = click_selection(
        &set(&[]),
        &dirs(&[]),
        &RowSel::Dir("zz".into()),
        true,
        false,
        &visible,
        None,
    );
    assert_eq!((ids, ds), (Vec::<u64>::new(), vec!["zz".to_string()]));
}

// T-SEL3 菜单可用性判定（rows/delegate 两口径同语义；双通道合计）
#[test]
fn menu_enable_derives_from_selection_shape() {
    let mk = |id: u64, is_dir: bool| TreeRow {
        path: format!("p{id}"),
        name: format!("p{id}"),
        depth: 0,
        is_dir,
        entry_id: Some(id as usize),
        expanded: false,
        has_children: is_dir,
        is_container: false,
        encrypted: false,
        size_raw: 1,
        size_packed: 0,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,
        container_format: None,
    };
    let rows = vec![mk(0, false), mk(1, true)];
    // 无选择 → 全禁用
    assert_eq!(
        menu_enable_of_rows(&[], &[], &rows, false, false),
        MenuEnable::default()
    );
    // 单选文件 → 打开可用 + 可改名
    assert_eq!(
        menu_enable_of_rows(&[0], &[], &rows, true, false),
        MenuEnable {
            has_selection: true,
            single_file: true,
            single_entry: true,
            can_edit: true,
            blank: false,
            paste_available: false,
        }
    );
    // 单选实体目录 → 打开禁用、可改名
    assert_eq!(
        menu_enable_of_rows(&[1], &[], &rows, true, false),
        MenuEnable {
            has_selection: true,
            single_file: false,
            single_entry: true,
            can_edit: true,
            blank: false,
            paste_available: false,
        }
    );
    // 多选 → 打开/改名都禁用
    assert_eq!(
        menu_enable_of_rows(&[0, 1], &[], &rows, true, false),
        MenuEnable {
            has_selection: true,
            single_file: false,
            single_entry: false,
            can_edit: true,
            blank: false,
            paste_available: false,
        }
    );
    // 隐式目录选择 → 选中、非单文件、**可改名**
    assert_eq!(
        menu_enable_of_rows(&[], &["d".into()], &rows, true, false),
        MenuEnable {
            has_selection: true,
            single_file: false,
            single_entry: true,
            can_edit: true,
            blank: false,
            paste_available: false,
        }
    );
    // 不可编辑档案（caps.edit=false / 嵌套会话）→ 改名与新建都禁用
    assert!(!menu_enable_of_rows(&[0], &[], &rows, false, false).can_edit);
    // delegate 口径（BrowseRow）同语义
    let rows = vec![
        browse_row_from_tree_row(&mk(0, false)),
        browse_row_from_tree_row(&mk(1, true)),
    ];
    let mut sel = std::collections::HashSet::new();
    sel.insert(0u64);
    assert!(
        menu_enable_of_delegate(&sel, &std::collections::HashSet::new(), &rows, true, false)
            .single_file
    );
    assert!(
        menu_enable_of_delegate(&sel, &std::collections::HashSet::new(), &rows, true, false)
            .single_entry,
        "单选文件须可改名"
    );
    sel.insert(1);
    assert!(
        !menu_enable_of_delegate(&sel, &std::collections::HashSet::new(), &rows, true, false)
            .single_file
    );
    assert!(
        !menu_enable_of_delegate(&sel, &std::collections::HashSet::new(), &rows, true, false)
            .single_entry,
        "多选不可改名"
    );
}

// 列重排纯函数（R-GUI4 #1）：前移/后移/越界/同位
#[test]
fn reorder_visible_moves_and_clamps() {
    let mut v = BrowseColumnId::ALL.to_vec();
    let name = v[0];
    reorder_visible(&mut v, 0, 3);
    assert_eq!(v[3], name, "名称 0→3");
    let type_ = v[0];
    reorder_visible(&mut v, 0, 0);
    assert_eq!(v[0], type_, "同位 no-op");
    let moved = v[2];
    reorder_visible(&mut v, 2, 99);
    assert_eq!(*v.last().expect("非空"), moved, "越界夹紧到末位");
    assert_eq!(v.len(), BrowseColumnId::ALL.len(), "无丢列");
    reorder_visible(&mut v, 99, 0);
    assert_eq!(v.len(), BrowseColumnId::ALL.len(), "源越界 no-op");
}

// `..` 行目标段：首实体行 path 剥尾段（docs/src → docs；顶层 → ""）
#[test]
fn parent_seg_strips_last_segment() {
    assert_eq!(parent_seg_of("docs/src/readme.txt"), "docs/src");
    assert_eq!(parent_seg_of("docs/src"), "docs");
    assert_eq!(parent_seg_of("top.txt"), "");
    assert_eq!(parent_seg_of(""), "");
}

// 行动作决策：单击不跳转、双击跳**一级**（`..` 行为回归锚点）。
//
// 修复前：`Parent` 分支在「目标推导」阶段就发跳转 → 单击即导航；且双击
// 会发出两次 `SelectRow`+`DoubleClickedRow`（table.rs 每次点击都
// `set_selected_row`）→ 两次导航叠加（泵在两次点击之间刷新了快照）→ 直达根。
#[test]
fn row_action_separates_click_from_activation() {
    let mut d = delegate(2, true);
    d.current_dir = "docs/src".into();
    let cur = "docs/src".to_string();
    assert!(matches!(data_row(&d, 0), Some(DataRowRef::Parent)));
    assert!(matches!(data_row(&d, 1), Some(DataRowRef::Entry(_))));

    // 单击 `..` → 只清选中，**不**跳转
    assert_eq!(
        row_action(false, data_row(&d, 0), &cur, false),
        Some(RowAction::Select(RowSel::None)),
        "单击 `..` 不得触发跳转"
    );
    // 双击 `..` → 跳上一级（只一层：docs/src → docs，不是根）
    assert_eq!(
        row_action(true, data_row(&d, 0), &cur, false),
        Some(RowAction::Up("docs".into())),
        "双击 `..` 只跳上一级"
    );

    // 顶层目录的 `..` → 根（此时「一级」即根，语义正确）
    assert_eq!(
        row_action(true, data_row(&d, 0), "docs", false),
        Some(RowAction::Up(String::new())),
        "顶层 `..` → 根"
    );

    // 嵌套会话的**档案根** `..` → 退出该层（回外层档案），而不是原地回根
    assert_eq!(
        row_action(true, data_row(&d, 0), "", true),
        Some(RowAction::LeaveContainer),
        "嵌套会话档案根 `..` 应退出该层"
    );
    // 嵌套会话里进到子目录后，`..` 仍然只是上一级（不误退出该层）
    assert_eq!(
        row_action(true, data_row(&d, 0), "docs", true),
        Some(RowAction::Up(String::new())),
        "嵌套会话内 `..` 只上跳一级"
    );

    // 单击实体行 → 上报选中 id；越界行 → 无动作
    let sel = match row_action(false, data_row(&d, 1), &cur, false) {
        Some(RowAction::Select(sel)) => sel,
        other => panic!("单击实体行应为 Select，实得 {other:?}"),
    };
    assert!(matches!(sel, RowSel::Entry(_)), "实体行带 entry_id");
    assert_eq!(row_action(true, data_row(&d, 99), &cur, false), None);
}

// 行动作决策：目录/容器双击 → Enter；文件双击 → Activate（路径原样上行）；
// 单击只上报选中（不激活）
#[test]
fn row_action_routes_dir_and_file_on_double_click() {
    let mut d = delegate(1, false);
    let cur = String::new();
    d.rows[0].is_dir = true;
    d.rows[0].path = "docs".into();
    assert_eq!(
        row_action(true, data_row(&d, 0), &cur, false),
        Some(RowAction::Enter("docs".into())),
        "目录双击 → 进入"
    );
    assert!(matches!(
        row_action(false, data_row(&d, 0), &cur, false),
        Some(RowAction::Select(_))
    ));

    d.rows[0].is_dir = false;
    d.rows[0].is_container = true;
    // 容器（内层压缩包）：双击**打开成下一层会话**——曾与目录同走 Enter，
    // 结果是「进入一个父档案索引里根本不存在的目录」⇒ 右栏一片空白。
    assert_eq!(
        row_action(true, data_row(&d, 0), &cur, false),
        Some(RowAction::EnterContainer("docs".into())),
        "容器双击 → 以它为下一层会话打开"
    );

    d.rows[0].is_container = false;
    d.rows[0].path = "a.txt".into();
    assert_eq!(
        row_action(true, data_row(&d, 0), &cur, false),
        Some(RowAction::Activate("a.txt".into())),
        "文件双击 → 激活（预览/外部打开由 shell 决定）"
    );
}

/// 容器行双击 → **开会话**（`EnterContainer`）：组合档的内层容器层
/// （`init.cpio`）现在是延迟展开后的普通容器行（`is_dir=false`），与档案内的
/// `a.zip` 条目同一条路由——「就地展开延迟层 / 物化嵌套条目」的区分在 shell 侧
/// （用户口径 2026-09-24）。
#[test]
fn row_action_container_row_enters_container() {
    let mut d = delegate(1, false);
    let cur = String::new();
    d.rows[0].is_dir = false;
    d.rows[0].is_container = true;
    d.rows[0].path = "init.cpio".into();
    assert_eq!(
        row_action(true, data_row(&d, 0), &cur, false),
        Some(RowAction::EnterContainer("init.cpio".into())),
        "容器行双击 → 开会话（延迟层由 shell 就地展开）"
    );
    // **目录优先**仍是既有契约（容器标记不得夺目录路由——历史缺陷防回归）
    d.rows[0].is_dir = true;
    assert_eq!(
        row_action(true, data_row(&d, 0), &cur, false),
        Some(RowAction::Enter("init.cpio".into())),
        "同时带目录标记时按目录进"
    );
}

#[gpui_kit::test]
fn browse_page_follows_snapshot_settings(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let data_w = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            crate::ui::contracts::ShellHandlers::default(),
            Some(data_w.clone()),
            None,
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 帧 1：非根目录 + 隐藏「压缩后」列 + 反转列序（模拟用户既有配置）
    let mut snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        current_dir: "docs".into(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", "docs"),
        show_dotdot: true,
        ..Default::default()
    };
    snap.columns = crate::ui::contracts::ColumnsConfig {
        order: BrowseColumnId::ALL.to_vec(),
        hidden: vec![BrowseColumnId::Packed],
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        window.draw(cx).clear(cx);
    });

    let held = vtcx.update(|_window, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        let d = hold.read(cx).table.read(cx).delegate();
        (d.show_parent, d.visible.clone(), d.columns.clone())
    });
    assert!(held.0, "冷启动即采用 show_dotdot（非根 → 显 `..` 行）");
    assert!(
        !held.1.contains(&BrowseColumnId::Packed),
        "冷启动即采用隐藏列：{:?}",
        held.1
    );
    assert_eq!(held.1, held.2.visible(), "可见序 = 配置序");

    // 帧 2：设置变更（关 `..`、显示「压缩后」、列序前移）
    let mut next = snap.clone();
    next.show_dotdot = false;
    next.columns.hidden.clear();
    next.columns.order.rotate_left(2);
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &next));
        window.draw(cx).clear(cx);
    });

    let after = vtcx.update(|_window, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        let d = hold.read(cx).table.read(cx).delegate();
        (
            d.show_parent,
            d.show_dotdot,
            d.visible.clone(),
            d.columns.clone(),
        )
    });
    assert!(!after.0 && !after.1, "关 `..` 开关下一帧即生效");
    assert_eq!(after.2, after.3.visible(), "列序/显隐下一帧即生效");
    assert!(
        after.2.contains(&BrowseColumnId::Packed),
        "恢复显示「压缩后」列：{:?}",
        after.2
    );
    assert_eq!(
        after.2.first(),
        held.2.order.get(2),
        "列序按新配置前移（旧第 3 列升首位）"
    );
}

/// 单元格内容盒：**行框与密度解耦**（用户逐档实测的根因锚点）。
///
/// 用户口径：紧凑/标准/宽松**只调行内上下 padding**，不动字体行高与对齐。
/// 此前的实测（本锚点量的就是这几个数）：
///
/// | 档 | 行盒 | 内容盒 | 单元格字号 | 行框 |
/// |---|---|---|---|---|
/// | 紧凑 | 21 | 21 | **14**（kit `Size::Small` ⇒ `text_sm`） | 21 |
/// | 标准 | 26 | 24 | 16 | 24 |
/// | 宽松 | 28 | 24 | 16 | 24 |
///
/// 紧凑档那行就是「`_` 直接显示不出来」：字号被密度带小 ⇒ 行框缩到 21px，
/// 墨迹盒（14px 下 ≈ 19.1px）几乎顶满，`_` 与行框边缘重合。
///
/// 现在钉住三条：
/// ① 内容盒高度（= 行框）**三档完全相同**，且 ≥ 基准字号墨迹盒 + 上下各 2px；
/// ② 行盒 = 内容盒 + 上下各 `row_gap()`（密度**只**作用在这两个数上）；
/// ③ 内容盒在行盒里**垂直居中**（上下留白相等）——此前内容顶在内容盒上沿，
///    多出来的留白全落在下方，才有「`_` 太贴行底」。
#[gpui_kit::test]
fn file_list_cell_box_is_density_independent(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;
    use crate::viewmodel::density::Density;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            crate::ui::contracts::ShellHandlers::default(),
            Some(data.clone()),
            None,
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        dir_rows: std::sync::Arc::new(vec![
            row("gjpqy_under_.txt", false, 1),
            row("HHHHHHHH.txt", false, 2),
            row("中文名称.txt", false, 3),
        ]),
        ..Default::default()
    };

    // 墨迹盒下限（基准字号）：墨迹盒 + 上下各 `TEXT_SLACK_PX` 常量余量
    let ink_need = vtcx.update(|window, cx| {
        let ts = cx.text_system();
        let id = ts.resolve_font(&window.text_style().font());
        let font = window.rem_size();
        let a = f32::from(ts.ascent(id, font));
        let d = f32::from(-ts.descent(id, font));
        let bb = ts.bounding_box(id, font);
        let up = f32::from(bb.origin.y + bb.size.height);
        let down = f32::from(-bb.origin.y);
        (a + d) + 2. * (up - a).max(down - d).max(0.) + 2. * crate::ui::constant::TEXT_SLACK_PX
    });
    let mut cell_hs: Vec<f32> = Vec::new();
    let mut row_hs: Vec<f32> = Vec::new();
    for density in [Density::Compact, Density::Standard, Density::Loose] {
        crate::ui::spacing::set_density(density);
        vtcx.update(|window, cx| {
            shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
            let _ = window.draw(cx);
        });
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let row0 = vtcx.debug_bounds("list-row-0").expect("行锚点");
        let cell0 = vtcx.debug_bounds("td-cell-0-0").expect("首列内容盒锚点");
        let gap = vtcx.update(|_, _| f32::from(crate::ui::spacing::row_gap()));
        let (row_h, cell_h) = (f32::from(row0.size.height), f32::from(cell0.size.height));
        let above = f32::from(cell0.origin.y - row0.origin.y);
        let below = f32::from(row0.origin.y + row0.size.height)
            - f32::from(cell0.origin.y + cell0.size.height);
        cell_hs.push(cell_h);
        row_hs.push(row_h);
        // 行盒 = 内容盒 + kit 的 `border_b_1`（border-box）⇒ 内容盒随 density 的
        // 上下 padding 一起长，行框（= 文字行框，见 `list_text_line_height`）
        // 恒定居中其中。
        assert!(
            row_h - cell_h >= -0.51 && row_h - cell_h <= 1.51 + 2. * gap,
            "{density:?}：行盒 {row_h} 与内容盒 {cell_h} 的关系不符（差 {}，row_gap {gap}）",
            row_h - cell_h
        );
        assert!(
            (above - below).abs() <= 1.51,
            "{density:?}：内容盒应在行盒内垂直居中（上 {above} vs 下 {below}，差 ≤1px 为边框）\
             ——内容顶到上沿时下方会多出一整块留白"
        );
    }
    crate::ui::spacing::set_density(Density::Standard);
    // 行框本身（文字行高）必须与密度无关：用公开口径复算三档，同一个值
    let floor_hs = vtcx.update(|window, _| {
        [
            crate::viewmodel::density::Density::Compact,
            crate::viewmodel::density::Density::Standard,
            crate::viewmodel::density::Density::Loose,
        ]
        .map(|d| {
            crate::ui::spacing::set_density(d);
            f32::from(crate::ui::spacing::text_line_floor(
                window,
                window.rem_size(),
            ))
        })
    });
    crate::ui::spacing::set_density(crate::viewmodel::density::Density::Standard);
    assert!(
        (floor_hs[0] - floor_hs[1]).abs() < 0.01 && (floor_hs[1] - floor_hs[2]).abs() < 0.01,
        "文字行框必须与密度无关：紧凑 {} / 标准 {} / 宽松 {}",
        floor_hs[0],
        floor_hs[1],
        floor_hs[2]
    );
    // 密度的**唯一**作用面 = 行内上下 padding：逐档 +2px（row_gap 0/1/2）
    for (a, b, step) in [(0, 1, "紧凑→标准"), (1, 2, "标准→宽松")] {
        assert!(
            (row_hs[b] - row_hs[a] - 2.).abs() < 0.51,
            "{step} 行盒应只多 2px（上下各 1px padding）：{:?}",
            row_hs
        );
    }
    assert!(
        cell_hs.iter().all(|h| *h >= ink_need.ceil() - 0.01),
        "内容盒（= 行框）必须装得下墨迹盒 + 上下各 {}px 余量：pads={cell_hs:?} 下限={:.2}",
        crate::ui::constant::TEXT_SLACK_PX,
        ink_need
    );
    // 行框 ≥ 基准字号墨迹盒 + 上下各 TEXT_SLACK_PX（「`_` 不贴行底」的常量余量）
}

/// 文件列表行高随密度（用户逐档口径：行上下留白 紧凑 0 / 标准 1 / 宽松 2px）。
///
/// 行高在 `render_tr` 里覆盖：kit 先 `h(Size 档定值 30/32px)`，再
/// `refine_style(委托样式)`——refine 语义是**传入者覆盖**（gpui-base 0.7.1
/// `StyledExt::refine_style`（styled.rs:79）直接转调 `Style::refine`；derive 生成体是
/// `if let Some(v) = refinement.f { self.f = v }`
/// ——gpui-pre-derive-refineable 0.3.8 `derive_refineable.rs:144`，语义 0.3.4 →
/// 0.3.8 未变）⇒ 我们的 `h()` 胜出。
/// 断言面 = **行距**（`list-row-0/1` 的 y 差）：kit 的 `uniform_list` 每帧
/// 按首行量得的整行高平铺，故行距 = 整行高（含上下留白）。
#[gpui_kit::test]
fn file_list_row_height_follows_density(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;
    use crate::viewmodel::density::Density;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            crate::ui::contracts::ShellHandlers::default(),
            Some(data.clone()),
            None,
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        dir_rows: std::sync::Arc::new(vec![
            row("a.txt", false, 1),
            row("b.txt", false, 2),
            row("c.txt", false, 3),
        ]),
        ..Default::default()
    };

    let pitch = |vtcx: &mut gpui_kit::VisualTestContext, density: Density| -> f32 {
        crate::ui::spacing::set_density(density);
        vtcx.update(|window, cx| {
            shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
            let _ = window.draw(cx);
        });
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let r0 = vtcx.debug_bounds("list-row-0").expect("首行锚点");
        let r1 = vtcx.debug_bounds("list-row-1").expect("次行锚点");
        f32::from(r1.origin.y - r0.origin.y)
    };

    let cmp_p = pitch(vtcx, Density::Compact);
    let std_p = pitch(vtcx, Density::Standard);
    let los_p = pitch(vtcx, Density::Loose);
    crate::ui::spacing::set_density(Density::Standard);

    assert!(
        (los_p - std_p - 2.).abs() < 0.51,
        "宽松档每行应比标准高 2px（上下各 2px）：{los_p} vs {std_p}"
    );
    assert!(
        cmp_p < std_p,
        "紧凑档行应更矮（留白 0 + 字号随 Size 档收一档）：{cmp_p} vs {std_p}"
    );

    // 行框随 `[ui].line_height` 走（用户口径：文件列表/目录树的行高要能配）。
    // 断言面 = **相邻两行的 y 间距**（渲染后的真实几何）。
    let with_line_height = |vtcx: &mut gpui_kit::VisualTestContext, em: f32| -> f32 {
        crate::ui::spacing::set_line_height(em);
        vtcx.update(|window, cx| {
            shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
            let _ = window.draw(cx);
        });
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let r0 = vtcx.debug_bounds("list-row-0").expect("首行锚点");
        let r1 = vtcx.debug_bounds("list-row-1").expect("次行锚点");
        f32::from(r1.origin.y - r0.origin.y)
    };
    let dense = with_line_height(vtcx, 1.2); // = 夹到墨迹盒下限
    let mid = with_line_height(vtcx, 2.0);
    let airy = with_line_height(vtcx, 2.6);
    // 夹下限：1.0（低于字形盒）与 1.2 必须**逐像素相同**——再紧就是裁降部
    //（用户明确不要「文字显示不全」，见 `ui::constant::GLYPH_BOX_EM`）。
    let floor_a = with_line_height(vtcx, 1.0);
    let floor_b = with_line_height(vtcx, 1.2);
    crate::ui::spacing::set_line_height(hzip_config::LINE_HEIGHT_DEFAULT);
    assert!(
        mid > dense + 0.5 && airy > mid + 0.5,
        "列表行高应随 `[ui].line_height` 变：1.2⇒{dense} / 2.0⇒{mid} / 2.6⇒{airy}"
    );
    assert!(
        (floor_a - floor_b).abs() < 0.01,
        "低于字形盒的配置必须被夹到同一下限：1.0⇒{floor_a} vs 1.2⇒{floor_b}"
    );

    // **裁字回归**（用户实测：行高改小后「文件列表每行文字底下丢了一部分」）：
    // 行盒必须装得下**字形盒**（单元格文字的行框与行盒同源，见
    // `list_text_line_height`）。这里按「上一版实现」的公式（吃 gpui 默认行高
    // φ）与现值的差反推：现值 ≥ 字形盒 + 上下各 row_gap，且严格小于 φ 版。
    let (glyph, ink_need, cell_font, gap) = vtcx.update(|window, cx| {
        let ts = cx.text_system();
        let id = ts.resolve_font(&window.text_style().font());
        use gpui_kit::component::Size;
        let scale = match crate::ui::spacing::metrics().component_size() {
            Size::XSmall | Size::Small => 0.875,
            _ => 1.,
        };
        let font = window.rem_size() * scale;
        let a = f32::from(ts.ascent(id, font));
        let d = f32::from(-ts.descent(id, font));
        let bb = ts.bounding_box(id, font);
        let ink_up = f32::from(bb.origin.y + bb.size.height);
        let ink_down = f32::from(-bb.origin.y);
        (
            a - d,
            (a + d) + 2. * (ink_up - a).max(ink_down - d).max(0.),
            f32::from(font),
            f32::from(crate::ui::spacing::row_gap()),
        )
    });
    // 下限取**字体墨迹盒**（`bounding_box`）而不是字形盒：字形盒装不下 `_`
    // 与全部降部（用户实测：紧凑档 `_` 看不见、标准档「在最底部马上就要溢出」）。
    let min_h = ink_need.ceil() + 2. * gap;
    assert!(
        dense + 0.01 >= min_h,
        "出厂行高下列表行 {dense} < 墨迹盒底线 {min_h:.2}（墨迹盒下限 {ink_need:.2} / \
         字形盒 {glyph:.2} @ {cell_font:.2}px）—— 文件名里的 `_`/降部会被裁"
    );
    assert!(
        dense < std_p + 0.01,
        "出厂行高下列表行 {dense} 不比 gpui 默认行高（φ）版 {std_p} 紧"
    );
    // 出厂配置下的两条硬边界：
    // ① 不裁字——行框不低于字体字形盒（+ 上下各 row_gap）；
    // ② 不倒退——不比改造前（吃 gpui `Window::line_height()`）更松。
    let (glyph, gap) = vtcx.update(|window, cx| {
        let ts = cx.text_system();
        let id = ts.resolve_font(&window.text_style().font());
        let size = window.rem_size();
        (
            f32::from(ts.ascent(id, size) - ts.descent(id, size)),
            f32::from(crate::ui::spacing::row_gap()),
        )
    });
    assert!(
        dense + 0.51 >= glyph + 2. * gap,
        "出厂行高下列表行 {dense} 低于字形盒底线 {:.2}（字形盒 {glyph:.2} + 2×row_gap {gap:.2}）\
         —— 降部会被裁（用户报告过「g 看起来像 a」）",
        glyph + 2. * gap
    );
    assert!(
        dense <= std_p + 0.01,
        "出厂行高下列表行 {dense} 比改造前（gpui 默认行高）{std_p} 更松了"
    );
}

// 行上挂的 `line_height(relative(...))` 必须**真的落到文字上**——「行盒与文字
// 行框同源」这条修法的前提（不同源就是用户实测的「文字底下丢了一部分」）。
//
// gpui 的文本样式沿元素树继承：kit 的表格单元格不设行高（`table/*.rs` 无
// `line_height`），故行上设的值就是单元格文字的行框。这条用最小探针把该
// 继承关系钉在仓库里（不依赖读 kit 源码）。
#[gpui_kit::test]
fn row_line_height_style_reaches_text(cx: &mut gpui_kit::TestAppContext) {
    struct Probe;
    impl gpui_kit::Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            gpui_kit::div()
                .line_height(gpui_kit::relative(crate::ui::spacing::line_height_em()))
                .child(
                    gpui_kit::div()
                        .debug_selector(|| "probe-text".into())
                        .child("gjpqy"),
                )
        }
    }
    let (_root, vtcx) = cx.add_window_view(|window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let view: gpui_kit::AnyView = cx.new(|_| Probe).into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let want = vtcx
        .update(|window, _| f32::from(window.rem_size()) * crate::ui::spacing::line_height_em());
    let got = f32::from(
        vtcx.debug_bounds("probe-text")
            .expect("探针文字锚点")
            .size
            .height,
    );
    assert!(
        (got - want).abs() <= 0.51,
        "行上设的行高没落到文字上：文字行框 {got:.2}，期望 {want:.2}（= 字号 × line_height_em）；\
         若 gpui 的继承语义变了，文件列表的「行盒与文字行框同源」也要重做"
    );
}

/// 焦点行（`..` 占位行 / 面包屑跳转后悬空的聚焦行）也走自绘浅色行高亮。
///
/// 用户实测两处不一致：① `..` 行的选中底色与其它行不同；② 面包屑跳转后
/// 「聚焦的那行」底色不对。根因同一个——应用选中集（`selected_ids` /
/// `selected_dirs`）表达不了这些行，露出的就是 kit 自绘的**全量 accent**
/// （0.7.1 `table/state.rs:2242-2250` 的 `.when(is_selected, …)`，
/// `is_selected` 判定见 `:2012`；`list.active_highlight=false` 分支）。
/// 修法：`kit_focus`（每帧镜像 `TableState::selected_row`）并入行高亮层。
#[gpui_kit::test]
fn kit_focus_row_uses_same_highlight_layer(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            crate::ui::contracts::ShellHandlers::default(),
            Some(data.clone()),
            None,
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");

    // 非根目录 + 开 `..` 行：行 0 = `..`，行 1..3 = 普通条目
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        current_dir: "docs".into(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", "docs"),
        show_dotdot: true,
        dir_rows: std::sync::Arc::new(vec![
            row("a.txt", false, 1),
            row("b.txt", false, 2),
            row("c.txt", false, 3),
        ]),
        ..Default::default()
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        let _ = window.draw(cx);
    });

    // 应用选中集为空 ⇒ 没有任何行自带高亮层
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("list-selected-layer-0").is_none(),
        "无选中/无焦点时行 0 不应有高亮层"
    );

    // kit 焦点行 = `..` 行（等价于「单击 `..` 行」/「面包屑跳转后悬空聚焦」）
    let table = vtcx.update(|_window, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        hold.read(cx).table.clone()
    });
    vtcx.update(|_window, cx| {
        table.update(cx, |t, cx| t.set_selected_row(0, cx));
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("list-selected-layer-0").is_some(),
        "焦点行（`..`）必须走自绘浅色高亮层（否则露 kit 全量 accent）"
    );
    assert!(
        vtcx.debug_bounds("list-selected-layer-2").is_none(),
        "非焦点、非选中的行不得有高亮层"
    );
}

/// 用户口径 2026-10-12（Main 确认）：**点列头排序不得丢焦点行的自绘高亮**。
///
/// 0.7.1 的语义变化：`TableState::selected_row()` 按 `SelectionMode` 过滤
/// （`table/state.rs:496-498`）。点列头走 `on_col_head_click` → `set_selected_col`
/// （`state.rs:822-836` / `:550-558`）把模式切成 `Column` ⇒ 该帧
/// `selected_row()` 返回 `None`；镜像若照抄它（0.6 写法），刚聚焦的 `..` 行的
/// 自绘高亮层会消失。我们的口径 = 保持 0.6 行为：镜像改由
/// `TableState::selection()` 派生，`Row` 才接受新值、`Column`/`Cell` 沿用上一帧
/// （见 `browse_root` 里那段注释）。
///
/// **走真实交互路径**（不是直接调 `set_selected_col`）：`vtcx.simulate_click`
/// 点表头带，y 在「行 0 上沿往上 2..24px」里扫掠（表头紧贴行区上方，而行 0
/// 之上没有别的行，故这段窗口内只有表头或空隙），并断言 kit 的选择模式**确实
/// 切成了 `Column`**——否则说明点击没落到表头，用例必须失败而不是假绿。
#[gpui_kit::test]
fn header_click_keeps_kit_focus_row_highlight(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;
    use gpui_kit::component::table::TableSelection;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            crate::ui::contracts::ShellHandlers::default(),
            Some(data.clone()),
            None,
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = slot.borrow().clone().expect("shell 实体");

    // 非根目录 + 开 `..` 行：行 0 = `..`（应用选中集表达不了的行——高亮只能来自
    // kit 焦点镜像，正好用来钉这条口径），行 1..3 = 普通条目
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        current_dir: "docs".into(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", "docs"),
        show_dotdot: true,
        dir_rows: std::sync::Arc::new(vec![
            row("a.txt", false, 1),
            row("b.txt", false, 2),
            row("c.txt", false, 3),
        ]),
        ..Default::default()
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        let _ = window.draw(cx);
    });

    let table = vtcx.update(|_window, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        hold.read(cx).table.clone()
    });
    // 前置：行模式单选焦点 = 行 0（等价「单击 `..` 行」/「面包屑跳转后悬空聚焦」）
    vtcx.update(|_window, cx| {
        table.update(cx, |t, cx| t.set_selected_row(0, cx));
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("list-selected-layer-0").is_some(),
        "前置：焦点行（`..`）的自绘高亮层必须先出现，否则用例空跑"
    );

    // 真实鼠标路径点表头带：x 取行左沿 +20px（表头贴边 4px 是 kit 的列宽拖拽
    // 手柄，避开它；右端才是排序图标——点图标会同时触发 `perform_sort`，虽然
    // 模式同样切到 `Column`，但行序会变浑）
    let b = vtcx.debug_bounds("list-row-0").expect("第 0 行必须已渲染");
    let x = f32::from(b.origin.x) + 20.;
    let row_top = f32::from(b.origin.y);
    let mut hit_header = false;
    for dy in (2..=24).step_by(2) {
        vtcx.simulate_click(
            gpui_kit::Point {
                x: gpui_kit::px(x),
                y: gpui_kit::px(row_top - dy as f32),
            },
            gpui_kit::Modifiers::default(),
        );
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        if vtcx
            .update(|_window, cx| matches!(table.read(cx).selection(), TableSelection::Column(_)))
        {
            hit_header = true;
            // 命中列头那次点击**自身**（重绘前）就必须保住镜像——如果清空发生在
            // 点击处理器里（例如 down 阶段把列头误判成点空白），会先在这里露出来，
            // 便于把「点击期清了」与「重绘期镜像清了」分开定位。
            let kf_now = vtcx.update(|_window, cx| table.read(cx).delegate().kit_focus);
            assert_eq!(
                kf_now,
                Some(0),
                "命中列头的那次点击（重绘前）后 kit_focus 应仍为 Some(0)"
            );
            break;
        }
    }
    assert!(
        hit_header,
        "没点到列头（行 0 上沿往上 24px 内未把选择模式打到 `Column`）——\
         用例失效，不许当成通过"
    );

    // 口径面：列头点击（模式 → `Column` ⇒ `selected_row()` 变 `None`）之后，
    // 焦点行的自绘高亮层**必须还在**（0.6 行为；照抄 `selected_row()` 的写法会丢）
    assert!(
        vtcx.debug_bounds("list-selected-layer-0").is_some(),
        "点列头排序后焦点行高亮层不得消失（0.7.1 `selected_row()` 按 SelectionMode 过滤的坑）"
    );
}

/// 行快照（密度用例的表格输入；字段与目录树行同源）
fn row(name: &str, is_dir: bool, id: usize) -> crate::viewmodel::rows::TreeRow {
    crate::viewmodel::rows::TreeRow {
        path: name.into(),
        name: name.into(),
        depth: 0,
        is_dir,
        entry_id: Some(id),
        expanded: false,
        has_children: is_dir,
        is_container: false,
        encrypted: false,
        size_raw: 10,
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
/// 用户口径 2026-09-28：文件列表**空白区左键**取消选中（与资源管理器一致）。
///
/// 覆盖两件事：① 行内左键**不得**被空白区处理器误判（事件由内向外冒泡，
/// 行处理器先置 `last_hit_row`）；② 空白处左键清空 VM 双通道选中集。
#[gpui_kit::test]
fn click_empty_area_clears_selection(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    // 记录上行（与 ctrl_click 测试同法）：空白区清理也走同一上行 → 断言其清空
    type Uplinks = std::rc::Rc<std::cell::RefCell<Vec<(Vec<u64>, Vec<String>)>>>;
    let rec: Uplinks = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let rec_sel = rec.clone();
    let handlers = crate::ui::contracts::ShellHandlers {
        on_selection_changed: Box::new(move |up: crate::ui::contracts::SelectionUpdate| {
            rec_sel.borrow_mut().push((up.ids, up.dirs));
        }),
        ..Default::default()
    };
    let data_w = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(cx, handlers, Some(data_w.clone()), None);
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    let mk_row = |name: &str, id: u64| TreeRow {
        path: name.into(),
        name: name.into(),
        depth: 0,
        is_dir: false,
        entry_id: Some(id as usize),
        expanded: false,
        has_children: false,
        is_container: false,
        encrypted: false,
        size_raw: 1,
        size_packed: 1,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,
        container_format: None,
    };
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        current_dir: String::new(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", ""),
        dir_rows: std::sync::Arc::new(vec![mk_row("a.txt", 0), mk_row("b.txt", 1)]),
        ..Default::default()
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        window.draw(cx).clear(cx);
    });

    // 扫掠找一行（x=400 落右栏表格）
    let mut row_y: Option<f32> = None;
    for y in (40..560).step_by(6) {
        vtcx.simulate_click(
            gpui_kit::Point {
                x: gpui_kit::px(400.),
                y: gpui_kit::px(y as f32),
            },
            gpui_kit::Modifiers::default(),
        );
        if rec.borrow().last().is_some_and(|(ids, _)| ids.len() == 1) {
            row_y = Some(y as f32);
            break;
        }
    }
    let row_y = row_y.expect("扫掠须命中一行");
    assert_eq!(
        rec.borrow().last().map(|(ids, _)| ids.len()),
        Some(1),
        "行内单击应上行单行选中"
    );

    // 空白处：表格底部（远低于行区）
    rec.borrow_mut().clear();
    vtcx.simulate_click(
        gpui_kit::Point {
            x: gpui_kit::px(400.),
            y: gpui_kit::px(row_y + 260.),
        },
        gpui_kit::Modifiers::default(),
    );
    let last = rec.borrow().last().cloned();
    assert_eq!(
        last,
        Some((Vec::new(), Vec::new())),
        "空白区左键应上行「清空选中」（实得 {last:?}）"
    );
}

/// 用户口径 2026-09-28 #1：空白处左键不仅要清**选中**，还要清**聚焦**
/// （kit 的 `selected_row`）。
///
/// 只清选中时的症状：点空白后行高亮层还在（`kit_focus` 驱动的自绘底色），
/// 用户看到「点了空白，那一行还亮着」。
#[gpui_kit::test]
fn blank_click_clears_kit_focus_not_just_selection(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let data_w = data.clone();
    let (rec_tx, rec_rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            crate::ui::contracts::ShellHandlers::default(),
            Some(data_w.clone()),
            Some(std::sync::Arc::new(rec_tx)),
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let _ = rec_rx;
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    let row = |name: &str, id: u64| TreeRow {
        path: name.into(),
        name: name.into(),
        depth: 0,
        is_dir: false,
        entry_id: Some(id as usize),
        expanded: false,
        has_children: false,
        is_container: false,
        encrypted: false,
        size_raw: 1,
        size_packed: 1,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,
        container_format: None,
    };
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        current_dir: String::new(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", ""),
        dir_rows: std::sync::Arc::new(vec![row("a.txt", 0), row("b.txt", 1)]),
        // 已选中第 0 行（选中集口径——真实链路里由泵快照回流）
        selection: vec![0],
        ..Default::default()
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        let _ = window.draw(cx);
    });
    // 单击一行（kit 自己会置 `selected_row`）→ 该行出现自绘高亮层
    let mut clicked = false;
    for y in (40..560).step_by(6) {
        vtcx.simulate_click(
            gpui_kit::Point {
                x: gpui_kit::px(400.),
                y: gpui_kit::px(y as f32),
            },
            gpui_kit::Modifiers::default(),
        );
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        if vtcx.debug_bounds("list-selected-layer-0").is_some() {
            clicked = true;
            break;
        }
    }
    assert!(clicked, "扫掠须命中第 0 行并出现高亮层");

    // 空白区左键 → 清选中（上行）+ 清聚焦（kit）；快照回流（清空选中集）
    let cleared = snap.clone();
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.browse_clear_selection(cx);
        });
        shell.update(cx, |s, cx| {
            s.update_from_state(
                cx,
                &AppStateUiSnapshot {
                    selection: Vec::new(),
                    ..cleared.clone()
                },
            );
        });
        let _ = window.draw(cx);
    });
    let focus = vtcx.update(|_window, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        hold.read(cx).table.read(cx).selected_row()
    });
    assert_eq!(focus, None, "空白区左键必须清掉 kit 聚焦行");
    vtcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("list-selected-layer-0").is_none(),
        "聚焦清掉后该行高亮层必须消失（用户口径：点空白不应还亮着一行）"
    );
}

/// 用户口径 2026-09-28 #2/#3：右键「重命名」「新建文件夹」进入**行内编辑态**，
/// 回车即提交（后台重写档案）。
#[gpui_kit::test]
fn rename_and_new_folder_commands_enter_inline_edit(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;

    let dir = tempfile::TempDir::new().unwrap();
    let zip = dir.path().join("t.zip");
    crate::viewmodel::testutil::write_zip(&zip, &[("a.txt", b"a"), ("b.txt", b"b")]);
    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    // 轮询改写结果要复用同一个 registry（下方 `handler("zip")`）；AppData
    // 按值收 engine，故先留一份 Arc（refcount，不新建 registry）。
    let engine_probe = std::sync::Arc::clone(&engine);
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new({
        let mut d = crate::app::state::AppData::new(engine);
        d.main.open(&d.engine, &zip).expect("open zip");
        d.archive = Some(zip.clone());
        d.main.tree();
        d.dir_rows = std::sync::Arc::new(d.main.dir_children());
        d.main.select_rows(&[0], &[]);
        d
    }));
    let (tx, _rx): (
        crate::ui::contracts::SignalTx,
        crate::ui::contracts::SignalRx,
    ) = smol::channel::bounded(64);
    let data_w = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            crate::ui::contracts::ShellHandlers::default(),
            Some(data_w.clone()),
            Some(std::sync::Arc::new(tx)),
        );
        let shell = cx.new(|_| shell);
        shell.update(cx, |s, cx| s.observe_state(cx));
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    let snap = AppStateUiSnapshot {
        archive_name: "t.zip".into(),
        current_dir: String::new(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("t.zip", ""),
        dir_rows: std::sync::Arc::new({
            let d = data.lock().unwrap();
            d.dir_rows.as_ref().clone()
        }),
        caps_edit: true,
        ..Default::default()
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        let _ = window.draw(cx);
    });

    // ① 重命名：选中行名进编辑态
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.browse_command(crate::ui::views::browse::BrowseCommand::Rename, window, cx);
        });
        let _ = window.draw(cx);
    });
    {
        let d = data.lock().unwrap();
        let edit = d.name_edit.clone().expect("应进入行内编辑态");
        assert_eq!(edit.dir, "");
        assert_eq!(edit.from.as_deref(), Some("a.txt"), "改名目标 = 选中行名");
        assert_eq!(edit.text, "a.txt", "初值 = 原名");
    }
    // 生产链路：编辑态变化经信号 → 泵导出快照 → `update_from_state`
    vtcx.update(|window, cx| {
        let fresh = data.lock().unwrap().export_snapshot();
        shell.update(cx, |s, cx| s.update_from_state(cx, &fresh));
        let _ = window.draw(cx);
    });
    let mirrored = vtcx.update(|_window, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        let d = hold.read(cx).table.read(cx).delegate();
        (d.name_rename.clone(), d.name_draft)
    });
    assert_eq!(
        mirrored.0.as_deref(),
        Some("a.txt"),
        "委托须把该行标记为改名中（名称列渲染输入框）"
    );
    assert!(!mirrored.1);

    // ② 直接**敲键盘**改名（聚焦时全选 ⇒ 新输入替换原名）→ 回车提交
    vtcx.simulate_keystrokes("c.txt");
    let typed = vtcx.update(|_window, cx| {
        shell
            .read(cx)
            .name_edit_input
            .clone()
            .expect("输入框实体")
            .read(cx)
            .value()
            .to_string()
    });
    assert_eq!(typed, "c.txt", "键入须替换全选的原名（F2 改名惯例）");
    // ⚠️ 改写**前的**档案 stat 必须在这里取：提交后任务可能**毫秒级**就完成落定，
    // 等到下面才开始记基线就会「基线已是新档」⇒ 变化永远等不到，白等满整个截止
    //（本机实测过一次 90 s 全耗在这上面，随后内容断言其实是过的）。
    let stat_before = std::fs::metadata(&zip)
        .ok()
        .map(|m| (m.modified().ok(), m.len()));
    vtcx.simulate_keystrokes("enter");
    let (edit_gone, job_kind) = {
        let d = data.lock().unwrap();
        (
            d.name_edit.is_none(),
            d.jobs
                .lock()
                .unwrap()
                .rows()
                .iter()
                .map(|r| r.kind)
                .collect::<Vec<_>>(),
        )
    };
    assert!(edit_gone, "回车提交后编辑态必须收尾");
    assert!(
        job_kind.contains(&"edit"),
        "回车提交须投递编辑任务（实得 {job_kind:?}）"
    );
    // 任务在后台真跑：轮询到档案里出现 `c.txt` 且 `a.txt` 消失（端到端）。
    //
    // T-OPT 尾项（负载敏感 flake 硬化）：本用例在全量并行跑时曾间歇失败
    // （隔离跑 0.13 s 即过）——数百个测试并发时后台编辑任务可能被饿到原
    // 10 s 截止之外。硬化三点，**断言未改**：①处理器建一次（旧实现每轮重建
    // 注册表）；②改写中途档案可能瞬时不可开（Windows 原子替换窗口）→ 轮询
    // 容忍该瞬时错误而不是直接 expect 炸；③截止 20 s。
    //
    // 2026-10-16 复测：20 s 仍被击穿一次（531 个测试并行 + 同机 clippy/workspace
    // 构建时，本轮实测失败即停在第 20 s 边界；常态仍 0.13 s）。截止放宽到 60 s
    // ——循环命中即退出，**正常路径零代价**，只有真回归才会等满（失败信息带最后
    // 观察到的错误与名字集，足以定位）。
    let handler = engine_probe.registry().handler("zip").expect("zip");
    // ②′ 等**后台改写落定**，再 open 一次读内容做断言。
    //
    // ⚠️ 不能「边轮询边 open」（旧实现每 50 ms 开一次源档案）：本仓库既有实测
    // （`AppData::browse_session_reopen_request` 的注释，2026-09-29）——**目标被打开
    // 时 Windows 的原子替换会失败（os error 5）**，编辑任务不重试 ⇒ 我们的轮询等于
    // 持续把改写挡在门外。2026-10-16 实测复现：跑满 60 s 截止、档案仍是原结构
    // （`["a.txt","b.txt"]`）、`last_err None`——是「任务被挡」而不是「读失败」。
    // 也不能等「任务行终局」：本用例不跑泵，`JobsVm` 的行状态不会被事件推进
    // （实测：等满整个截止后内容其实早已改好 ⇒ 白等 60 s 且掩盖了真正的信号）。
    // 故改为 **stat 轮询**（`metadata` 只取属性、不持会挡替换的句柄）：mtime/长度一变
    // 即说明替换已落定，随后单次 open 断言内容。
    // 基线在上文 `stat_before`（提交**之前**取，见那里的说明）
    let before = stat_before;
    // 90 s：常态 0.1 s（命中即退出），但**首次执行刚链接出来的测试二进制**时 Windows
    // Defender 会扫一遍新 exe，后台任务可能被拖到几十秒。放宽只影响真回归的等待时长。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    let mut rewrote = false;
    while std::time::Instant::now() < deadline {
        let now = std::fs::metadata(&zip)
            .ok()
            .map(|m| (m.modified().ok(), m.len()));
        if now != before {
            rewrote = true;
            break;
        }
        // 顺手驱动测试执行器：任务提交/落账若还有 gpui 侧的一段调度，别靠 sleep 干等
        vtcx.update(|_window, cx| cx.background_executor().run_until_parked());
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // 落定后单次读内容（仍给几次重试，容忍替换瞬间的瞬时错误）
    let mut names: Vec<String> = Vec::new();
    let mut last_err: Option<String> = None;
    for _ in 0..20 {
        match handler.open_reader(
            hzip_format::Source::File(zip.clone()),
            &hzip_format::OpenOptions::default(),
        ) {
            Ok(r) => match r.entries() {
                Ok(entries) => {
                    names = entries
                        .iter()
                        .map(|e| e.name.as_str().to_string())
                        .collect();
                    last_err = None;
                }
                Err(e) => last_err = Some(format!("entries: {e}")),
            },
            Err(e) => last_err = Some(format!("open: {e}")),
        }
        if names.iter().any(|n| n == "c.txt") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        names.iter().any(|n| n == "c.txt") && !names.iter().any(|n| n == "a.txt"),
        "档案须已在后台重写为改名后的结构（实得 {names:?}；档案 stat 是否变化 {rewrote}；末次读取错误 {last_err:?}）"
    );

    // ③ 新建文件夹：草稿行 + 行内编辑
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.browse_command(
                crate::ui::views::browse::BrowseCommand::NewFolder,
                window,
                cx,
            );
        });
        let _ = window.draw(cx);
    });
    {
        let d = data.lock().unwrap();
        let edit = d.name_edit.clone().expect("应进入新建草稿态");
        assert!(edit.from.is_none(), "新建 = 无源条目");
        assert_eq!(edit.dir, "");
        assert_eq!(edit.text, crate::tr("browse.new_folder.default"));
    }
    vtcx.update(|window, cx| {
        let fresh = data.lock().unwrap().export_snapshot();
        shell.update(cx, |s, cx| s.update_from_state(cx, &fresh));
        let _ = window.draw(cx);
    });
    assert!(
        vtcx.debug_bounds("list-row-draft").is_some(),
        "草稿行必须渲染在列表里（名称列处于编辑态）"
    );
    let draft_flag = vtcx.update(|_window, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        let d = hold.read(cx).table.read(cx).delegate();
        (d.name_draft, d.name_input.is_some())
    });
    assert!(draft_flag.0, "委托须进入草稿行态");
    assert!(draft_flag.1, "委托须持有输入框实体");
}

/// 用户口径 2026-09-29 #2 的接线面：右键落点判定——
/// 行上右键 = 命中（行菜单出，容器空白菜单抑制）；空白处右键 = 不命中
/// （容器空白菜单出，含新建文件夹）。
///
/// 判据同时核对 kit 的 `right_clicked_row`：行上右键它必须有值、空白处必须为
/// `None`——这正是容器菜单「行上不出」的依据（kit 的 table-level 菜单只在
/// `right_clicked_row` 有值时装配行菜单）。
#[gpui_kit::test]
fn right_click_target_splits_row_menu_and_blank_menu(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let data_w = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(
            cx,
            crate::ui::contracts::ShellHandlers::default(),
            Some(data_w.clone()),
            None,
        );
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    let row = |name: &str, id: u64| TreeRow {
        path: name.into(),
        name: name.into(),
        depth: 0,
        is_dir: false,
        entry_id: Some(id as usize),
        expanded: false,
        has_children: false,
        is_container: false,
        encrypted: false,
        size_raw: 1,
        size_packed: 1,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,
        container_format: None,
    };
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        current_dir: String::new(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", ""),
        dir_rows: std::sync::Arc::new(vec![row("a.txt", 0), row("b.txt", 1)]),
        caps_edit: true,
        ..Default::default()
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        let _ = window.draw(cx);
    });

    // 用行元素的实测边界定点右键（扫掠会被弹出的菜单挡住）
    let bounds = vtcx.debug_bounds("list-row-0").expect("第 0 行必须已渲染");
    let row_x = f32::from(bounds.origin.x) + f32::from(bounds.size.width) / 2.;
    let row_y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.;
    vtcx.simulate_mouse_down(
        gpui_kit::Point {
            x: gpui_kit::px(row_x),
            y: gpui_kit::px(row_y),
        },
        gpui_kit::MouseButton::Right,
        gpui_kit::Modifiers::default(),
    );
    // ① 命中标记的读-清语义（确定性）：置位 → 读到 true → 再读 false
    //    （容器菜单是消费式读取，第二次空白右键不能被旧值污染）
    vtcx.update(|_w, cx| note_right_hit_row(cx));
    assert!(
        vtcx.update(|_w, cx| take_right_hit_row(cx)),
        "置位后必须读到命中"
    );
    assert!(!vtcx.update(|_w, cx| take_right_hit_row(cx)), "消费后归零");
    // ② 行上右键：kit 记成行命中（行菜单据此装配；容器菜单在此时读到命中）
    assert!(
        vtcx.update(|_w, cx| {
            let hold = cx.global::<BrowseGlobal>().0.clone();
            hold.read(cx).table.read(cx).right_clicked_row().is_some()
        }),
        "行上右键 kit 须记成行命中（行菜单据此装配）"
    );
    // 空白处右键：不命中 + kit 的 right_clicked_row 为 None
    vtcx.simulate_mouse_down(
        gpui_kit::Point {
            x: gpui_kit::px(row_x),
            y: gpui_kit::px(row_y + 400.),
        },
        gpui_kit::MouseButton::Right,
        gpui_kit::Modifiers::default(),
    );
    let kit_row = vtcx.update(|_w, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        hold.read(cx).table.read(cx).right_clicked_row().is_some()
    });
    assert!(!kit_row, "空白处右键 kit 不得记成行命中");
}

/// 用户口径 2026-09-29 #2：右键菜单按**目标**分流——
/// 空白区 = 目录/整档级项（含新建文件夹，不含重命名/删除/打开…）；
/// 文件/文件夹行 = 条目级项（**不含新建文件夹**）。
#[test]
fn menu_items_are_scoped_by_right_click_target() {
    use BrowseMenuItem::*;
    // 空白区
    let blank = menu_items(&blank_menu_enable(true, false));
    assert!(blank.contains(&NewFolder), "空白区必须有新建文件夹");
    for it in [Open, OpenWith, Edit, Delete, ExtractSel, Rename] {
        assert!(!blank.contains(&it), "空白区不该有 {it:?}");
    }
    // 不可编辑档案：新建文件夹灰
    assert!(item_disabled(NewFolder, &blank_menu_enable(false, false)));
    assert!(!item_disabled(NewFolder, &blank_menu_enable(true, false)));

    // 文件/文件夹行：没有新建文件夹；有重命名/打开等
    let rows = menu_items(&MenuEnable {
        has_selection: true,
        single_file: true,
        single_entry: true,
        can_edit: true,
        blank: false,
        paste_available: false,
    });
    assert!(!rows.contains(&NewFolder), "条目行不该有新建文件夹");
    assert!(rows.contains(&Rename) && rows.contains(&Open) && rows.contains(&Delete));

    // 文件 vs 文件夹：打开/打开方式/编辑只对**文件**可用（文件夹仍显示但灰）
    let file = MenuEnable {
        has_selection: true,
        single_file: true,
        single_entry: true,
        can_edit: true,
        blank: false,
        paste_available: false,
    };
    let folder = MenuEnable {
        single_file: false,
        ..file
    };
    for it in [Open, OpenWith, Edit] {
        assert!(!item_disabled(it, &file), "文件行 {it:?} 应可用");
        assert!(item_disabled(it, &folder), "文件夹行 {it:?} 应禁用");
    }
    // 文件夹仍可改名/删除/解压选定
    for it in [Rename, Delete, ExtractSel] {
        assert!(!item_disabled(it, &folder), "文件夹行 {it:?} 应可用");
    }
    // 多选：重命名灰（其余条目级项照旧）
    let multi = MenuEnable {
        single_file: false,
        single_entry: false,
        ..file
    };
    assert!(item_disabled(Rename, &multi));
    assert!(!item_disabled(Delete, &multi));
    // 不可编辑（caps.edit=false / 嵌套会话）：改名灰
    let no_edit = MenuEnable {
        can_edit: false,
        ..file
    };
    assert!(item_disabled(Rename, &no_edit));
    // 解压全部/全部选择恒可用
    assert!(!item_disabled(ExtractAll, &MenuEnable::default()));
    assert!(!item_disabled(SelectAll, &MenuEnable::default()));
    // 空白菜单的项序列（顺序即渲染顺序）
    assert_eq!(
        menu_items(&blank_menu_enable(true, false))[0],
        ExtractAll,
        "空白菜单首项 = 解压全部"
    );
}

/// T-SEL1 走查缺陷 #1 复现（**真实鼠标事件链**，非 cx.emit 旁路）：
/// Ctrl+单击 = 增选。`simulate_event` 派发带修饰键的 MouseDown/Up
/// （平台层 modifiers 由此注入），断言「render_tr 捕获 → SelectRow
/// 订阅集合运算 → 整组上行」全链。失败 = 修饰键捕获链断点。
#[gpui_kit::test]
fn ctrl_click_multi_selects_through_real_mouse(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;

    /// 上行记录（selection uplink 序列——双通道）
    #[derive(Default)]
    struct Rec {
        selections: Vec<Vec<u64>>,
        dir_selections: Vec<Vec<String>>,
    }

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let rec = std::rc::Rc::new(std::cell::RefCell::new(Rec::default()));
    let rec_sel = rec.clone();
    let handlers = crate::ui::contracts::ShellHandlers {
        on_selection_changed: Box::new(move |up: crate::ui::contracts::SelectionUpdate| {
            let mut r = rec_sel.borrow_mut();
            r.selections.push(up.ids);
            r.dir_selections.push(up.dirs);
        }),
        ..Default::default()
    };
    let data_w = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(cx, handlers, Some(data_w.clone()), None);
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 根目录：隐式目录行（entry_id None——T-SEL 走查 #1 场景）+ 2 文件行
    let mk_row = |path: &str, id: Option<u64>| TreeRow {
        path: path.into(),
        name: path.into(),
        depth: 0,
        is_dir: id.is_none(),
        entry_id: id.map(|i| i as usize),
        expanded: false,
        has_children: id.is_none(),
        is_container: false,
        encrypted: false,
        size_raw: 1,
        size_packed: 1,
        mtime: None,
        is_symlink: false,
        encryption: None,
        compression_level: None,
        crc32: None,
        attrs: Default::default(),
        level: 0,

        container_format: None,
    };
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        current_dir: String::new(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", ""),
        dir_rows: std::sync::Arc::new(vec![
            mk_row("a.txt", Some(0)),
            mk_row("d", None),
            mk_row("b.txt", Some(1)),
        ]),
        ..Default::default()
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        window.draw(cx).clear(cx);
    });

    // —— 定位两行命中位置：扫 y 派发普通点击，记录各 entry 的首位置
    // （x=400 落右栏表格；左树 nav_rows 为空无树行可扰）——
    let mut hits: Vec<(u64, f32)> = Vec::new();
    for y in (40..560).step_by(8) {
        vtcx.simulate_click(
            gpui_kit::Point {
                x: gpui_kit::px(400.),
                y: gpui_kit::px(y as f32),
            },
            gpui_kit::Modifiers::default(),
        );
        let last = rec.borrow().selections.last().cloned();
        if let Some(ids) = last
            && ids.len() == 1
            && !hits.iter().any(|(id, _)| *id == ids[0])
        {
            hits.push((ids[0], y as f32));
        }
    }
    assert!(hits.len() >= 2, "扫掠须命中至少两行，实得 {hits:?}");
    let (id_a, y_a) = hits[0];
    let (id_b, y_b) = hits[1];
    let p_a = gpui_kit::Point {
        x: gpui_kit::px(400.),
        y: gpui_kit::px(y_a),
    };
    let p_b = gpui_kit::Point {
        x: gpui_kit::px(400.),
        y: gpui_kit::px(y_b),
    };

    // —— 正式链路：普通单击 A（基线替换）→ Ctrl+单击 B（增选）——
    rec.borrow_mut().selections.clear();
    vtcx.simulate_click(p_a, gpui_kit::Modifiers::default());
    assert_eq!(
        rec.borrow().selections.last().cloned().unwrap_or_default(),
        vec![id_a],
        "普通单击 = 单选基线"
    );
    let ctrl = gpui_kit::Modifiers {
        control: true,
        ..Default::default()
    };
    vtcx.simulate_mouse_down(p_b, MouseButton::Left, ctrl);
    vtcx.simulate_mouse_up(p_b, MouseButton::Left, ctrl);
    let got = rec.borrow().selections.last().cloned().unwrap_or_default();
    let mut expect = vec![id_a, id_b];
    expect.sort_unstable();
    assert_eq!(
        got, expect,
        "Ctrl+单击 = 增选（真实鼠标事件链；失败 = 修饰键捕获断链）"
    );

    // T-SEL 走查 #1：普通单击隐式目录行（a/b 之间的行）→ 目录通道上行
    // （此前该行 entry_id=None → 点击即清空选择，两症状的根源）
    let y_mid = ((y_a + y_b) / 2.0).round() as i32;
    vtcx.simulate_click(
        gpui_kit::Point {
            x: gpui_kit::px(400.),
            y: gpui_kit::px(y_mid as f32),
        },
        gpui_kit::Modifiers::default(),
    );
    assert_eq!(
        rec.borrow()
            .dir_selections
            .last()
            .cloned()
            .unwrap_or_default(),
        vec!["d".to_string()],
        "隐式目录行点击 = 目录通道选择（不再清空）"
    );
    assert!(
        rec.borrow()
            .selections
            .last()
            .cloned()
            .unwrap_or_default()
            .is_empty(),
        "目录选择与实体选择互斥整组替换"
    );
}

/// `..` 行点击链路（真实 shell + 真实窗口 + 真实表格事件）。
///
/// 订阅是**接线面**：`row_action` 决策单测覆盖不到「哪个 TableEvent 进哪条
/// 分支」。此处直接向真实 `TableState` 发事件，断言上行到 shell handlers
/// 的实际调用——即用户报告的「单击就跳 / 跳到根」两个缺陷。
#[gpui_kit::test]
fn dotdot_row_navigates_on_double_click_only(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::shell::AppShell;

    /// 上行记录（on_crumb 的目标段 / 选择变更次数）。
    #[derive(Default)]
    struct Rec {
        crumbs: Vec<String>,
        selections: usize,
    }

    let engine = std::sync::Arc::new(crate::viewmodel::testutil::engine());
    let data: crate::ui::shell::AppDataHandle = std::sync::Arc::new(std::sync::Mutex::new(
        crate::app::state::AppData::new(engine),
    ));
    let rec = std::rc::Rc::new(std::cell::RefCell::new(Rec::default()));
    let (rec_crumb, rec_sel) = (rec.clone(), rec.clone());
    let handlers = crate::ui::contracts::ShellHandlers {
        on_crumb: Box::new(move |c| rec_crumb.borrow_mut().crumbs.push(c.seg)),
        on_selection_changed: Box::new(move |_| rec_sel.borrow_mut().selections += 1),
        ..Default::default()
    };
    let data_w = data.clone();
    let shell_slot: std::rc::Rc<std::cell::RefCell<Option<gpui_kit::Entity<AppShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot2 = shell_slot.clone();
    let (_root, vtcx) = cx.add_window_view(move |window, cx| {
        gpui_kit::component::init(cx);
        crate::ui::theme::init(cx);
        let shell = AppShell::with_data(cx, handlers, Some(data_w.clone()), None);
        let shell = cx.new(|_| shell);
        *slot2.borrow_mut() = Some(shell.clone());
        let view: gpui_kit::AnyView = shell.into();
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shell = shell_slot.borrow().clone().expect("shell 实体");

    // 非根目录 + `..` 开关开 → 表格首行为 `..` 占位行
    let snap = AppStateUiSnapshot {
        archive_name: "demo.zip".into(),
        current_dir: "docs/src".into(),
        crumbs: crate::ui::contracts::crumbs_from_current_dir("demo.zip", "docs/src"),
        show_dotdot: true,
        ..Default::default()
    };
    vtcx.update(|window, cx| {
        shell.update(cx, |s, cx| s.update_from_state(cx, &snap));
        window.draw(cx).clear(cx);
    });
    let table = vtcx.update(|_window, cx| {
        let hold = cx.global::<BrowseGlobal>().0.clone();
        let d = hold.read(cx).table.read(cx).delegate();
        assert!(d.show_parent, "非根 + 开关开 → `..` 行在场");
        hold.read(cx).table.clone()
    });

    // 单击 `..` → 上行清选中，**不**跳转
    table.update(vtcx, |_, cx| cx.emit(TableEvent::SelectRow(0)));
    assert_eq!(
        rec.borrow().crumbs,
        Vec::<String>::new(),
        "单击 `..` 不得跳转（用户报告缺陷 1）"
    );
    assert_eq!(rec.borrow().selections, 1, "单击仍上报选中（清空）");

    // 双击 `..`（表格真实序列：SelectRow 后 DoubleClickedRow）→ 只跳一级
    table.update(vtcx, |_, cx| {
        cx.emit(TableEvent::SelectRow(0));
        cx.emit(TableEvent::DoubleClickedRow(0));
    });
    assert_eq!(
        rec.borrow().crumbs,
        vec!["docs".to_string()],
        "双击 `..` 目标 = 父级 docs（用户报告缺陷 2：不得直达根）"
    );
}

// —— T-CP P0：右键菜单复制/粘贴项与空白粘贴防双开 ——

// MenuEnable 四态口径：复制跟选中、粘贴跟剪贴板（装配层 disabled 输入；
// 字段齐备由结构体字面量强制——漏字段即编译失败）
#[test]
fn menu_enable_paste_field_defaults_and_semantics() {
    let none = MenuEnable::default();
    assert!(!none.has_selection && !none.single_file && !none.paste_available);
    assert!(!none.can_edit && !none.single_entry && !none.blank);
    // Copy 跟选中（无选中 → 灰）
    assert!(item_disabled(BrowseMenuItem::Copy, &MenuEnable::default()));
    // Paste 跟剪贴板（默认不可粘贴）
    assert!(item_disabled(BrowseMenuItem::Paste, &MenuEnable::default()));
    let has_cb = MenuEnable {
        paste_available: true,
        ..Default::default()
    };
    assert!(!item_disabled(BrowseMenuItem::Paste, &has_cb));
    let has_sel = MenuEnable {
        has_selection: true,
        ..Default::default()
    };
    assert!(!item_disabled(BrowseMenuItem::Copy, &has_sel));
}

/// 「类型」列文案优先级（T-ICON 决策 #3 的纯函数面）：系统类型名可用时胜出；
/// `None`（开关关 / 未命中 / 语言不一致）时回退行的自有本地化文案。
#[test]
fn type_cell_text_prefers_system_name() {
    assert_eq!(
        type_cell_text(
            Some(gpui_kit::SharedString::from("hzip 压缩档案")),
            "Zip 压缩包"
        ),
        gpui_kit::SharedString::from("hzip 压缩档案"),
        "系统类型名命中（Some）时必须胜出"
    );
    assert_eq!(
        type_cell_text(None, "Zip 压缩包"),
        gpui_kit::SharedString::from("Zip 压缩包"),
        "系统类型名不可用时回退自有文案"
    );
    // 空回退（极端行）不得 panic / 变形
    assert_eq!(type_cell_text(None, ""), gpui_kit::SharedString::from(""));
}
