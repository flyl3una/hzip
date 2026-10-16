use super::*;
use crate::viewmodel::testutil;

#[test]
fn open_converts_entries_and_counts() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(
        &ar,
        &[
            ("root/a.txt", b"aa"),
            ("root/sub/b.txt", b"bb"),
            ("top.txt", b"t"),
        ],
    );
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    assert!(vm.is_open());
    assert_eq!(vm.entry_count(), 3);
    assert_eq!(vm.entries().len(), 3);
    // 全部 Utf8（ascii 名）→ 无可疑
    assert_eq!(vm.suspect_count(), 0);
    assert!(vm.suspect_entries().is_empty());
}

// `..` 占位行归 UI 委托合成（R-GUI 走查 Fix3）：dir_children 保持纯实体行
// ——曾混入字面 ".." 行，表格出现第二个可双击 `..` 行且 enter_dir("..") 失效
#[test]
fn dir_children_never_contains_dotdot_placeholder() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(&ar, &[("root/a.txt", b"aa"), ("top.txt", b"t")]);
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    for dir in ["", "root"] {
        vm.enter_dir(dir);
        let rows = vm.dir_children();
        assert!(
            rows.iter().all(|r| r.path != ".." && r.name != ".."),
            "dir_children({dir:?}) 不含 `..` 占位行"
        );
    }
}

// 路径跳转事件（R-GUI 走查 #2 统一入口 = enter_dir + expand_to）：双栏
// 跟进——右栏切到该目录子项，左栏展开父链使该目录及其子目录可见
#[test]
fn navigation_event_follows_in_tree_and_list() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(&ar, &[("root/sub/b.txt", b"bb"), ("root/a.txt", b"aa")]);
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    vm.tree(); // 懒建树（生产路径 refresh_browse_rows 同序）
    // 跳转事件：进入 root（表格双击/树单击/面包屑共用语义）
    vm.enter_dir("root");
    vm.expand_to("root");
    // 右栏 = root 直接子项（a.txt 文件 + sub 目录）
    let children = vm.dir_children();
    let names: Vec<&str> = children.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"a.txt") && names.contains(&"sub"),
        "文件列表切换到该目录: {names:?}"
    );
    // 左栏 = 父链展开，root 及其子目录 sub 出现在可见行序列
    let nav_rows = vm.nav_rows();
    let nav: Vec<&str> = nav_rows.iter().map(|r| r.path.as_str()).collect();
    assert!(
        nav.contains(&"root") && nav.contains(&"root/sub"),
        "树展开父链可见当前目录: {nav:?}"
    );
    // `..` 跳转目标段：root 的上一级 = 档案根（current_dir 剥尾段）
    assert_eq!(
        crate::ui::contracts::crumbs_from_current_dir("t.tar", "root").len(),
        2
    );
}

#[test]
fn refresh_reloads() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(&ar, &[("root/a.txt", b"aa"), ("top.txt", b"t")]);
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    assert_eq!(vm.entry_count(), 2);
    // 外部改档案（重写单条目 tar）
    testutil::write_ustar(&ar, &[("only.txt", b"x")]);
    vm.refresh(&eng).expect("refresh");
    assert_eq!(vm.entry_count(), 1);
    assert_eq!(vm.entries()[0].name, "only.txt");
}

#[test]
fn filter_name_substr_and_glob() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(
        &ar,
        &[
            ("root/a.txt", b"aa"),
            ("root/sub/b.txt", b"bb"),
            ("top.txt", b"t"),
        ],
    );
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");

    let view = vm.set_filter(Filter {
        name_substr: Some("top".into()),
        ..Default::default()
    });
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].name, "top.txt");

    // glob *.txt：整名命中（全部）
    let view = vm.set_filter(Filter {
        glob: Some("*.txt".into()),
        ..Default::default()
    });
    assert_eq!(view.len(), 3);

    // glob 整名：root/* → root/a.txt（跨层不命中 sub/b.txt——`*`
    // 跨 `/` 匹配，root/* 含 root/sub/b.txt？glob crate `*` 匹配 `/`，
    // root/* 会命中 root/sub/b.txt——用精确用例验证段级语义：
    // `*/b.txt` 命中 root/sub/b.txt（首段通配）
    let view = vm.set_filter(Filter {
        glob: Some("*/b.txt".into()),
        ..Default::default()
    });
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].name, "root/sub/b.txt");

    // 子串 + glob 交集
    let view = vm.set_filter(Filter {
        name_substr: Some("root".into()),
        glob: Some("*.txt".into()),
        ..Default::default()
    });
    assert_eq!(view.len(), 2);

    // 还原
    let view = vm.set_filter(Filter::default());
    assert_eq!(view.len(), 3);
}

#[test]
fn filter_hide_macosx() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(&ar, &[("__MACOSX/._a.txt", b"x"), ("real.txt", b"y")]);
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    let view = vm.set_filter(Filter {
        hide_macosx: true,
        ..Default::default()
    });
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].name, "real.txt");
}

#[test]
fn selection_toggle_and_select_all() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(&ar, &[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c")]);
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    assert!(vm.selection().is_empty());

    vm.toggle_select(1);
    vm.toggle_select(2);
    assert_eq!(vm.selection(), vec![1, 2]);
    vm.toggle_select(1); // 移除
    assert_eq!(vm.selection(), vec![2]);

    vm.select_all(false);
    assert_eq!(vm.selection(), vec![0, 1, 2]);
    vm.select_all(true);
    assert!(vm.selection().is_empty());
    assert_eq!(vm.selection_count(), 0);
}

// T-SEL 走查 #1：隐式目录双通道选择——按路径选/增删、整组替换、
// 子树展开（effective）、子树大小、select_all 只选实体集
#[test]
fn dir_selection_dual_channel_and_effective() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(
        &ar,
        &[
            ("dir/x.txt", b"1234"),
            ("dir/sub/y.txt", b"ab"),
            ("z.txt", b"c"),
        ],
    );
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");

    // 普通点选目录行（路径载荷）→ 目录单选
    vm.select_rows(&[], &["dir".to_string()]);
    assert_eq!(vm.dir_selection(), vec!["dir".to_string()]);
    assert_eq!(vm.selection_count(), 1, "目录行计入选择数");

    // Ctrl 增删目录
    vm.toggle_select_dir("dir");
    assert!(vm.dir_selection().is_empty());

    // 混合选择：目录路径 + 实体 id（Ctrl 组合形态）
    vm.toggle_select_dir("dir");
    vm.select_rows(&[2], &["dir".to_string()]); // z.txt 实体 + dir
    assert_eq!(vm.selection_count(), 2, "实体 + 目录合计");

    // 有效 id 展开：dir 子树（x.txt + sub/y.txt）+ z 实体
    let eff = vm.effective_selection_ids();
    assert_eq!(eff.len(), 3, "dir 两子条目 + z 实体（BTreeSet 去重）");
    // selection_names 同源展开（解压选定数据面）
    assert_eq!(vm.selection_names().len(), 3);

    // 子树大小：dir = 4 + 2 = 6 原始
    let (raw, packed) = vm.dir_subtree_sizes(&["dir".to_string()]);
    assert_eq!(raw, 6);
    assert!(packed > 0, "tar 无压缩");

    // select_rows 整组替换（双通道同源）
    vm.select_rows(&[2], &[]);
    assert!(vm.dir_selection().is_empty());
    assert_eq!(vm.selection(), vec![2]);

    // select_all：实体全集 + 目录清空
    vm.select_rows(&[], &["dir".to_string()]);
    vm.select_all(false);
    assert_eq!(vm.selection().len(), 3);
    assert!(vm.dir_selection().is_empty());
}

// select_ids（T-TVS3 行点击上行）：整组替换、越界忽略、空集清空、去重升序
#[test]
fn select_ids_replaces_ignores_out_of_range_and_clears() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(&ar, &[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c")]);
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");

    // 整组替换（BTreeSet 升序去重）
    vm.select_ids(&[2, 0, 2]);
    assert_eq!(vm.selection(), vec![0, 2]);
    // 越界静默忽略
    vm.select_ids(&[1, 99]);
    assert_eq!(vm.selection(), vec![1]);
    // 空集 = 清空（`..` 行点击语义）
    vm.select_ids(&[]);
    assert!(vm.selection().is_empty());
}

// —— T-SEL2：全选 / 反选 = 列表**可见行**（Ctrl+A 语义修正）——
//
// 与 `select_all`/`select_invert`（**整档**：过滤后的 `entries` 全集）的分野：
// 逐层浏览时 = 当前目录那一屏；搜索/过滤激活时 = 扁平命中结果。`..` 上一级行
// 是 UI 委托层合成的（`views/browse.rs` `DataRowRef::Parent`），结构上不在行集里。

/// 当前层语义：根层全选只选根层行——根层文件进实体通道、隐式目录行进目录
/// 通道；子目录内的条目**不**进实体通道（整档语义的根因缺陷）。
#[test]
fn select_visible_rows_limits_to_current_layer() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(
        &ar,
        &[("a.txt", b"a"), ("b.txt", b"b"), ("sub/c.txt", b"c")],
    );
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    vm.tree(); // 懒建树（生产路径 rebuild_browse_rows 同序）

    // 根层行集 = 当前目录直接子项（目录先行；无 `..` 合成行）
    let rows = vm.visible_rows();
    assert_eq!(
        rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["sub", "a.txt", "b.txt"],
        "根层可见行"
    );

    vm.select_visible_rows(&rows, false);
    let c_id = vm.entry_id_of_path("sub/c.txt").expect("c.txt 可定位");
    assert_eq!(vm.selection(), vec![0, 1], "实体通道恰为根层两个文件");
    assert!(!vm.selection().contains(&c_id), "子目录内条目不进实体通道");
    assert_eq!(
        vm.dir_selection(),
        vec!["sub".to_string()],
        "隐式目录行（tar 无目录条目）走目录通道"
    );
    // 注：`selection_names()` 是**有效集**——目录通道按路径前缀展开子树
    // （§2「选目录 = 选中其全部内容」），故它仍含 sub/c.txt；「不含当前目录
    // 之外的条目」约束看的是两个**原始通道**（上面两条断言）。
    assert_eq!(
        vm.selection_names(),
        vec![
            "a.txt".to_string(),
            "b.txt".to_string(),
            "sub/c.txt".to_string()
        ],
        "有效集 = 实体 ∪ 目录子树（复制/解压消费面）"
    );

    // 进 sub/ 再全选：只选该层（整档 `select_all` 在这一步仍会选上 4 条）
    vm.enter_dir("sub");
    let rows = vm.visible_rows();
    assert_eq!(
        rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["sub/c.txt"],
        "sub/ 可见行"
    );
    vm.select_visible_rows(&rows, false);
    assert_eq!(vm.selection(), vec![c_id], "只选该子目录内的条目");
    assert!(vm.dir_selection().is_empty(), "该层无隐式目录行");
    assert_eq!(vm.selection_names(), vec!["sub/c.txt".to_string()]);
}

/// 搜索/过滤态：行集 = **扁平命中结果** ⇒ 选择集 = 命中集（既不是整档，也不是
/// 当前目录那一层）。
#[test]
fn select_visible_rows_in_search_mode_is_flat_hits() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(
        &ar,
        &[
            ("a.txt", b"a"),
            ("b.txt", b"b"),
            ("sub/c.txt", b"c"),
            ("sub/d.txt", b"d"),
        ],
    );
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    vm.tree();
    assert_eq!(vm.entry_count(), 4);

    vm.set_filter(Filter {
        name_substr: Some("c.txt".into()),
        ..Default::default()
    });
    let rows = vm.visible_rows();
    assert_eq!(rows.len(), 1, "命中跨目录的一条：sub/c.txt");
    assert_eq!(rows[0].path, "sub/c.txt");
    assert!(
        rows[0].entry_id.is_some(),
        "搜索行带实体 id（非隐式目录行）"
    );

    vm.select_visible_rows(&rows, false);
    assert_eq!(
        vm.selection().len(),
        1,
        "命中集 1 条（整档 select_all 会是 4 条）"
    );
    assert_eq!(vm.selection_names(), vec!["sub/c.txt".to_string()]);
    assert!(vm.dir_selection().is_empty(), "扁平结果行全走实体通道");
}

/// `..` / 越界：选择集 ⊆ 行集 id；行集里没有 `..` 合成行 ⇒ 选择集不可能含它；
/// 空行集与 `clear=true` 等价（清空两通道）。
#[test]
fn select_visible_rows_stays_within_row_set_and_clears() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(
        &ar,
        &[("a.txt", b"a"), ("b.txt", b"b"), ("sub/c.txt", b"c")],
    );
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    vm.tree();

    let rows = vm.visible_rows();
    assert!(
        rows.iter().all(|r| r.path != ".." && r.name != ".."),
        "行集不含 `..` 合成行（它只在 UI 委托层）"
    );
    vm.select_visible_rows(&rows, false);
    let row_ids: std::collections::BTreeSet<usize> =
        rows.iter().filter_map(|r| r.entry_id).collect();
    assert!(
        vm.selection().iter().all(|id| row_ids.contains(id)),
        "实体选择 ⊆ 行集 id（`..` 无实体 id，结构上进不来）"
    );
    assert_eq!(
        vm.dir_selection(),
        rows.iter()
            .filter(|r| r.is_dir && r.entry_id.is_none())
            .map(|r| r.path.clone())
            .collect::<Vec<_>>(),
        "目录选择 == 行集里的隐式目录行"
    );

    // 空行集 = 清空两通道（`clear=true` 的同款出口）
    vm.select_visible_rows(&[], false);
    assert_eq!(vm.selection_count(), 0, "空行集清空实体 + 目录两通道");
    // `clear=true`（不依赖行集）
    vm.select_visible_rows(&rows, false);
    assert!(vm.selection_count() > 0, "先选上再来清");
    vm.select_visible_rows(&rows, true);
    assert!(
        vm.selection().is_empty() && vm.dir_selection().is_empty() && vm.selection_count() == 0
    );
}

/// 反选：同一行集内双通道互换（隐式目录行同参），连按两次回到原集。
#[test]
fn invert_rows_swaps_within_row_set() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(
        &ar,
        &[("a.txt", b"a"), ("b.txt", b"b"), ("sub/c.txt", b"c")],
    );
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    vm.tree();

    let rows = vm.visible_rows(); // sub + a.txt + b.txt
    vm.select_visible_rows(&rows, false); // 全选：实体 {0,1} + 目录 {sub}
    vm.invert_rows(&rows);
    assert_eq!(vm.selection_count(), 0, "行集内已全选 → 反选 = 全不选");
    assert!(vm.selection().is_empty() && vm.dir_selection().is_empty());

    vm.invert_rows(&rows);
    assert_eq!(vm.selection(), vec![0, 1], "再反选 = 行集内全选中");
    assert_eq!(vm.dir_selection(), vec!["sub".to_string()]);

    // 半选：先摘掉目录通道 → 反选后两通道互补，两次反选回到原集
    vm.toggle_select_dir("sub");
    let before = vm.selection();
    assert!(vm.dir_selection().is_empty());
    vm.invert_rows(&rows);
    assert!(vm.selection().is_empty(), "实体行原全选中 → 反选后全不选");
    assert_eq!(
        vm.dir_selection(),
        vec!["sub".to_string()],
        "隐式目录行原未选中 → 反选后选中"
    );
    vm.invert_rows(&rows);
    assert_eq!(vm.selection(), before, "两次反选回到原集");
    assert!(vm.dir_selection().is_empty());
}

#[test]
fn tree_lazy_build() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(
        &ar,
        &[
            ("root/a.txt", b"aa"),
            ("root/sub/b.txt", b"bb"),
            ("top.txt", b"t"),
        ],
    );
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    assert!(vm.tree_ref().is_none(), "树懒构建（未请求前不建）");
    {
        let t = vm.tree().as_ref().expect("树已构建");
        // 结构：root{a.txt, sub{b.txt}}, top.txt → 6 节点（含 root）
        assert_eq!(t.node_count(), 6);
        assert_eq!(t.name.as_str(), "");
    }
    // 二次请求值不变（缓存）
    let t2 = vm.tree_ref().expect("树缓存");
    assert_eq!(t2.node_count(), 6);
}

/// 组合档**延迟展开**（用户口径 2026-09-24）：打开 `.cpio.gz` **不**立刻展开
/// 内层 cpio——列表/树里只有一行 `init.cpio`（容器行：图标/类型列按容器、
/// 双击才加载）；`enter_deferred_layer` 之后才见到 cpio 内的真实结构。
///
/// 打开耗时/临时盘占用因此从「与内层容器等量」降到毫秒级（此前实测 940 MB
/// `.cpio.gz` 打开 10+ 秒、3.9 GB initramfs 落同量临时盘）。
#[test]
fn composite_open_defers_inner_container() {
    use hzip_format_testkit::cpio::{self, CpioVariant, EntrySpec};

    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("init.cpio.gz");
    let inner = cpio::build(
        CpioVariant::Newc,
        &[
            EntrySpec::dir("init"),
            EntrySpec::file("init/a.txt", b"aa"),
            EntrySpec::file("top.txt", b"t"),
        ],
    );
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut enc, &inner).unwrap();
    std::fs::write(&ar, enc.finish().unwrap()).unwrap();

    let eng = testutil::engine();
    let browser = eng
        .browse(
            hzip_format::Source::File(ar.clone()),
            &hzip_format::OpenOptions {
                defer_composite: true,
                ..Default::default()
            },
        )
        .expect("延迟打开");
    let mut vm = MainViewModel::new();
    vm.install_browser(browser, &ar).expect("装会话");

    // ① 列表只有一行：内层容器（容器语义，不是目录、不是可预览的文件）
    // （先构建树——`dir_children` 走 `tree_ref`，懒建不自动触发）
    assert_eq!(vm.tree().as_ref().expect("树已构建").children.len(), 1);
    vm.go_root_dir();
    let rows: Vec<_> = vm.dir_children();
    assert_eq!(rows.len(), 1, "延迟打开只给一行：{:?}", rows.len());
    let layer = &rows[0];
    assert_eq!(layer.name.to_string(), "init.cpio");
    assert!(!layer.is_dir, "延迟层是容器行（双击 = 加载），不是目录");
    assert!(layer.is_container, "按容器展示（图标/类型列）");
    let ui = crate::ui::binding::browse_row_from_tree_row(layer);
    assert_eq!(ui.type_text, "cpio", "类型列 = 容器格式名");
    assert_eq!(
        ui.size_text,
        crate::ui::binding::fmt_bytes(inner.len() as u64),
        "大小列 = 声明大小（gz 尾 ISIZE = 内层容器解压后大小）"
    );
    // 树是**同一行**（不再合成「内层容器」目录层——延迟展开后由真实条目承载）
    let t = vm.tree().as_ref().expect("树已构建");
    assert!(t.children.contains_key("init.cpio"));
    assert_eq!(vm.deferred_layer(), Some("init.cpio"));

    // ② 双击那一层 = 展开（唯一耗时的一步）→ 子会话见 cpio 内真实结构
    //    （与 `shell::open_container` 的延迟分支逐字同路：摘下浏览器 →
    //     **锁外/后台线程**展开 → 装子会话 → 原浏览器**放回本层帧**；
    //     见 `take_browser` 文档）
    let browser = vm.take_browser().expect("会话浏览器");
    let expanded = browser
        .enter_deferred(eng.registry())
        .expect("展开内层容器");
    let mut child = MainViewModel::new();
    child.install_browser(expanded, &ar).expect("装子会话");
    assert!(child.tree().is_some());
    child.go_root_dir();
    let names: Vec<String> = child
        .dir_children()
        .into_iter()
        .map(|r| r.name.to_string())
        .collect();
    assert!(names.contains(&"top.txt".to_string()), "{names:?}");
    assert!(names.contains(&"init".to_string()), "{names:?}");
    assert!(child.deferred_layer().is_none(), "展开后不再是延迟态");
    // 展开后仍是容器行：容器内的 `.tgz` 条目（若有）走同一套
    assert!(child.tree().is_some());

    // ②b 展开**不消费**本层延迟态：原浏览器放回本层后仍可再进（用户实测
    //     2026-10-14——退回本层再双击同一行，必须还走延迟展开这条路）
    vm.restore_browser(browser);
    assert!(vm.is_open(), "放回后本层仍是「有浏览态」");
    assert_eq!(vm.deferred_layer(), Some("init.cpio"), "延迟态必须保留");
    let browser = vm.take_browser().expect("会话浏览器");
    let again = browser
        .enter_deferred(eng.registry())
        .expect("同一行必须能再次展开（退回本层后再进）");
    assert_eq!(
        again.entries().len(),
        child.entries().len(),
        "再展开得到同一份内层结构"
    );
    vm.restore_browser(browser);

    // ③ 失败可重试：源被删 → 展开失败，但**会话延迟态保留**（再点一次仍可试，
    //    不会变成打不开的死局——用户口径 2026-09-24 的健壮性要求）
    let browser2 = eng
        .browse(
            hzip_format::Source::File(ar.clone()),
            &hzip_format::OpenOptions {
                defer_composite: true,
                ..Default::default()
            },
        )
        .expect("延迟打开");
    let mut vm2 = MainViewModel::new();
    vm2.install_browser(browser2, &ar).expect("装会话");
    let browser2 = vm2.take_browser().expect("会话浏览器");
    std::fs::remove_file(&ar).unwrap();
    assert!(
        browser2.enter_deferred(eng.registry()).is_err(),
        "源不在 → 展开必须失败（而不是静默成功）"
    );
    // 展开是纯读 → 浏览器原样放回就是原样重试（延迟态从未被拿走）
    vm2.restore_browser(browser2);
    assert_eq!(
        vm2.deferred_layer(),
        Some("init.cpio"),
        "失败后延迟态必须保留（可重试）"
    );
}
#[test]
fn preview_request_shape() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("t.tar");
    testutil::write_ustar(&ar, &[("a.bin", &[1u8; 100])]);
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    let (id, size) = vm.preview_request(0).expect("preview req");
    assert_eq!(id, 0);
    assert_eq!(size, 100);
    assert!(vm.preview_request(99).is_none());
}

#[test]
fn open_missing_archive_errors() {
    let d = tempfile::TempDir::new().unwrap();
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    let e = vm.open(&eng, &d.path().join("missing.tar")).unwrap_err();
    // 文件不存在：sniff 打开失败 → Io 系（1001..1004）
    assert!((1000..2000).contains(&e.code()), "码 {}", e.code());
    assert!(!vm.is_open());
    assert_eq!(vm.entry_count(), 0);
}

// 导航树根行（R-GUI4 #1/#2）：名称含扩展名；根默认展开（露出顶层目录），
// 子目录默认折叠（不展开不进快照）
#[test]
fn nav_rows_root_full_name_and_default_expanded() {
    let d = tempfile::TempDir::new().unwrap();
    let ar = d.path().join("我的档案.tar");
    testutil::write_ustar(&ar, &[("docs/a.txt", b"a"), ("top.txt", b"t")]);
    let eng = testutil::engine();
    let mut vm = MainViewModel::new();
    vm.open(&eng, &ar).expect("open");
    vm.tree(); // 懒建（nav_rows 经 tree_ref 读，不触发构建）
    let rows = vm.nav_rows();
    assert_eq!(rows[0].path, "", "首行 = 档案根虚拟节点");
    assert_eq!(rows[0].name, "我的档案.tar", "根名含扩展名（非 file_stem）");
    assert!(rows[0].expanded, "根默认展开（顶层目录可见）");
    let docs = rows
        .iter()
        .find(|r| r.path == "docs")
        .expect("顶层目录进树");
    assert!(docs.is_dir && !docs.expanded, "子目录默认折叠");
    assert_eq!(rows.len(), 2, "文件不进树：仅根 + docs");
}

mod copy_paste_tests {
    use super::*;

    /// 复制物化→内部剪贴板→粘贴变更集闭环（T-MENU8，B3 降级线）。
    ///
    /// 覆盖：物化路径落地（稳定布局）、内部剪贴板写入、粘贴侧组装 Append
    /// 变更集（且不带同名冲突——物化名与档案内名一致时算冲突，见断言）。
    #[test]
    fn copy_then_paste_changeset_loop() {
        let dir = tempfile::TempDir::new().unwrap();
        let ar = dir.path().join("t.tar");
        crate::viewmodel::testutil::write_ustar(
            &ar,
            &[("top.txt", b"hello"), ("docs/a.txt", b"world")],
        );
        let eng = crate::viewmodel::testutil::engine();
        let mut vm = MainViewModel::new();
        vm.open(&eng, &ar).unwrap();

        // 选中两个条目（id 由 VM entries 给出）
        let ids: Vec<EntryId> = vm.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 2);

        hzip_platform::clipboard::clear_internal();
        let (paths, failed) = vm.materialize_entries(&ids, &ar, None).unwrap();
        assert!(failed.is_empty(), "无失败条目: {failed:?}");
        assert_eq!(paths.len(), 2, "两条目各落一个物化文件");
        // 稳定布局：同一档案 → 同一 hash 目录
        let parents: std::collections::BTreeSet<_> =
            paths.iter().filter_map(|p| p.parent()).collect();
        assert_eq!(parents.len(), 1, "同一档案落在同一 hash 目录");

        hzip_platform::clipboard::set_internal(paths.clone());
        assert_eq!(hzip_platform::clipboard::internal_paths(), paths);

        // 粘贴侧：物化路径 → Append 变更集（契约 D：`(源路径, 档案内目标名)`
        // 二元组；名取末段——物化名与档案内名一致时算冲突，见断言）
        let existing: Vec<String> = vm.entries().iter().map(|e| e.name.clone()).collect();
        let pairs: Vec<(std::path::PathBuf, String)> = paths
            .iter()
            .filter_map(|p| {
                let n = p.file_name()?.to_string_lossy().into_owned();
                Some((p.clone(), n))
            })
            .collect();
        let (cs, _) =
            crate::app::state::append_changeset(&pairs, |n| existing.iter().any(|e| e == n));
        assert_eq!(cs.changes.len(), 2);
        for c in &cs.changes {
            match c {
                hzip_format::Change::Append { src, name } => {
                    assert!(src.is_file(), "物化源文件存在: {src:?}");
                    assert!(!name.as_str().is_empty());
                }
                other => panic!("应为 Append，实为 {other:?}"),
            }
        }
        hzip_platform::clipboard::clear_internal();
    }

    // 目录递归展开（T-MENU8 语义升级）：tar 无目录条目 → 隐式目录选择经
    // effective_selection_ids 展开为后代 id；materialize_in 反推目录根，
    // 产出「目录壳 + 相对路径后代」。断言壳目录存在、后代落在壳内、内容正确。
    #[test]
    fn materialize_dir_expands_descendants() {
        let dir = tempfile::TempDir::new().unwrap();
        let ar = dir.path().join("t.tar");
        crate::viewmodel::testutil::write_ustar(
            &ar,
            &[
                ("root/a.txt", b"aa"),
                ("root/sub/b.txt", b"bb"),
                ("top.txt", b"t"),
            ],
        );
        let eng = crate::viewmodel::testutil::engine();
        let mut vm = MainViewModel::new();
        vm.open(&eng, &ar).unwrap();

        // 隐式目录选择 root（Ctrl+单击目录行通道）
        vm.toggle_select_dir("root");
        let ids = vm.effective_selection_ids();
        assert_eq!(ids.len(), 2, "root 子树展开 = a.txt + sub/b.txt: {ids:?}");

        let (paths, failed) = vm.materialize_entries(&ids, &ar, None).unwrap();
        assert!(failed.is_empty(), "无失败条目: {failed:?}");
        // 目录壳 + 2 文件（sub 中间目录为壳的子目录，非独立清单项）
        assert_eq!(paths.len(), 3, "壳 + 两文件: {paths:?}");
        let names: std::collections::BTreeSet<String> = paths
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert!(names.contains("root_dir"), "目录壳带 _dir 后缀: {names:?}");
        assert!(
            names.contains("a.txt") && names.contains("b.txt"),
            "{names:?}"
        );
        // 内容正确 + 后代相对路径落位（b.txt 在壳/sub/ 下）
        let shell = paths
            .iter()
            .find(|p| p.file_name().is_some_and(|n| n == "root_dir"))
            .unwrap();
        assert!(shell.is_dir());
        let a = std::fs::read(shell.join("a.txt")).unwrap();
        assert_eq!(a, b"aa");
        let b = std::fs::read(shell.join("sub").join("b.txt")).unwrap();
        assert_eq!(b, b"bb");
    }

    // 多文件平铺选择 = 行为与旧一致（无目录壳）：即使手工选择恰好盖满
    // docs 子树（此处 top.txt + docs/a.txt 的变体），只要存在根外散件，
    // 就不构成「纯目录展开指纹」——按平铺文件物化（T-MENU8 语义升级的
    // 反向约束：目录展开只对纯目录选择生效）。tar 无法构造显式目录条目
    // （testutil 最小头无 typeflag=5），显式目录分支由 platform 侧
    // materialize_entry 单测覆盖；VM 侧 dir_roots 判定对显式/隐式同源。
    #[test]
    fn materialize_multi_file_unchanged_from_legacy() {
        let dir = tempfile::TempDir::new().unwrap();
        let ar = dir.path().join("t.tar");
        crate::viewmodel::testutil::write_ustar(
            &ar,
            &[
                ("root/a.txt", b"aa"),
                ("root/sub/b.txt", b"bb"),
                ("top.txt", b"t"),
            ],
        );
        let eng = crate::viewmodel::testutil::engine();
        let mut vm = MainViewModel::new();
        vm.open(&eng, &ar).unwrap();

        // 只选文件（无目录、无隐式目录选择）
        vm.toggle_select(0);
        vm.toggle_select(2);
        let ids = vm.effective_selection_ids();
        assert_eq!(ids.len(), 2);

        let (paths, failed) = vm.materialize_entries(&ids, &ar, None).unwrap();
        assert!(failed.is_empty(), "无失败条目: {failed:?}");
        assert_eq!(paths.len(), 2, "两文件各落一个物化文件（无壳）: {paths:?}");
        for p in &paths {
            assert!(p.is_file(), "文件直接落地（无目录包裹）: {p:?}");
        }
        // 内容映射正确（id 0 = root/a.txt，id 2 = top.txt）
        let c0 = std::fs::read(&paths[0]).unwrap();
        assert_eq!(c0, b"aa", "id0 = root/a.txt 内容");
        let c2 = std::fs::read(&paths[1]).unwrap();
        assert_eq!(c2, b"t", "id2 = top.txt 内容");
    }

    // 单条失败容错：id 越界进 failed（`#id` 形态），其余条目照常物化。
    #[test]
    fn materialize_partial_failure_tolerates_bad_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let ar = dir.path().join("t.tar");
        crate::viewmodel::testutil::write_ustar(&ar, &[("top.txt", b"t")]);
        let eng = crate::viewmodel::testutil::engine();
        let mut vm = MainViewModel::new();
        vm.open(&eng, &ar).unwrap();

        let ids = vec![0, 999]; // 999 越界
        let (paths, failed) = vm.materialize_entries(&ids, &ar, None).unwrap();
        assert_eq!(failed, vec!["#999"], "越界 id 进 failed: {failed:?}");
        assert_eq!(paths.len(), 1, "好条目照常物化");
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"t");
    }

    // 半选回归（判别指纹锁定）：全档案条目恰好被选中 ≠ 纯目录展开——
    // 选中 top.txt + root/sub/b.txt 时 root/sub 完整盖满但 top.txt 在根外，
    // 必须平铺两文件（不产 root/sub 壳）。守 materialize_multi_file 用例
    // 同一语义的嵌套变体（T-MENU8 走查）。
    #[test]
    fn materialize_half_selection_outside_root_stays_flat() {
        let dir = tempfile::TempDir::new().unwrap();
        let ar = dir.path().join("t.tar");
        crate::viewmodel::testutil::write_ustar(
            &ar,
            &[
                ("root/a.txt", b"aa"),
                ("root/sub/b.txt", b"bb"),
                ("top.txt", b"t"),
            ],
        );
        let eng = crate::viewmodel::testutil::engine();
        let mut vm = MainViewModel::new();
        vm.open(&eng, &ar).unwrap();

        // 手工选 root/sub/b.txt（完整盖满 root/sub）+ top.txt（根外散件）
        let b_id = vm
            .entries()
            .iter()
            .find(|e| e.name.as_str() == "root/sub/b.txt")
            .map(|e| e.id)
            .unwrap();
        let t_id = vm
            .entries()
            .iter()
            .find(|e| e.name.as_str() == "top.txt")
            .map(|e| e.id)
            .unwrap();
        vm.select_only(b_id);
        vm.toggle_select(t_id);
        let ids = vm.effective_selection_ids();
        assert_eq!(ids.len(), 2);

        let (paths, failed) = vm.materialize_entries(&ids, &ar, None).unwrap();
        assert!(failed.is_empty(), "无失败条目: {failed:?}");
        assert_eq!(paths.len(), 2, "平铺两文件（无壳）: {paths:?}");
        assert!(paths.iter().all(|p| p.is_file()));
    }
}
