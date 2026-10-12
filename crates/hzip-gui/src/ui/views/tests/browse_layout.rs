// 顶层 glob：嵌套模块的 `use super::*` 依赖此处转出（`super` 现在指向本测试模块）。
use super::*;

mod view_mode_tests {
    use super::*;
    use crate::ui::contracts::AppStateUiSnapshot;

    fn tree_row(path: &str, is_dir: bool) -> crate::viewmodel::rows::TreeRow {
        crate::viewmodel::rows::TreeRow {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            depth: 0,
            is_dir,
            entry_id: Some(1),
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

    // 图标选择（用户实测反馈）：压缩包（档案根 / 内层容器）必须是压缩包图标，
    // 不能画成文件或文件夹图标——否则「双击能进去」在界面上看不出来。
    #[test]
    fn container_rows_use_archive_icon_not_file_or_folder() {
        use crate::ui::binding::TreeRowMeta;
        let dir = TreeRowMeta {
            path: "d".into(),
            is_dir: true,
            is_container: false,
        };
        let container = TreeRowMeta {
            path: "sub/a.7z".into(),
            is_dir: false,
            is_container: true,
        };
        // 树：档案根 / 内层压缩包 = 压缩包图标；普通目录 = 文件夹系
        assert_eq!(tree_row_icon(true, None, true), AppIcon::FolderZip);
        assert_eq!(
            tree_row_icon(false, Some(&container), true),
            AppIcon::FolderZip,
            "内层压缩包节点必须用压缩包图标"
        );
        assert_eq!(tree_row_icon(false, Some(&dir), false), AppIcon::Folder);
        assert_eq!(tree_row_icon(false, Some(&dir), true), AppIcon::FolderOpen);

        // 列表/图标网格/紧凑列表：同一口径（旧实现容器行画的是文件图标）
        let mut row = tree_row("sub/a.7z", false);
        row.is_container = true;
        let list_row = crate::ui::binding::browse_row_from_tree_row(&row);
        assert_eq!(
            crate::ui::views::browse::row_type_icon(&list_row),
            AppIcon::FolderZip,
            "容器行在列表里也必须用压缩包图标"
        );
        let mut dir_row = tree_row("docs", true);
        dir_row.is_container = false;
        assert_eq!(
            crate::ui::views::browse::row_type_icon(&crate::ui::binding::browse_row_from_tree_row(
                &dir_row
            )),
            AppIcon::Folder
        );
        let mut link = tree_row("l", false);
        link.is_symlink = true;
        assert_eq!(
            crate::ui::views::browse::row_type_icon(&crate::ui::binding::browse_row_from_tree_row(
                &link
            )),
            AppIcon::Link,
            "软链接标记优先"
        );
    }

    /// 行清单（T-MENU9 icon/list 共用）：`..` 行随 show_dotdot、条目行按序；
    /// 两模式与 detail 看到同一批行（交互语义对齐的机制保证）。
    #[test]
    fn mode_rows_mirror_parent_row_and_entries() {
        let snap = AppStateUiSnapshot {
            archive_name: "a.zip".into(),
            current_dir: "docs".into(),
            crumbs: crate::ui::contracts::crumbs_from_current_dir("a.zip", "docs"),
            dir_rows: std::sync::Arc::new(vec![
                tree_row("docs/x.txt", false),
                tree_row("docs/sub", true),
            ]),
            show_dotdot: true,
            ..Default::default()
        };
        let rows = mode_rows(&snap);
        assert_eq!(rows.len(), 3, ".. + 2 条目");
        assert!(rows[0].0, "首行是 `..`（非根目录 + show_dotdot）");
        assert!(rows[1].1.as_ref().is_some_and(|r| r.name == "x.txt"));
        assert!(
            rows[2].1.as_ref().is_some_and(|r| r.is_dir),
            "目录行 is_dir 保留（双击进入判据）"
        );

        // 关 show_dotdot → 无 `..` 行
        let mut hidden = snap.clone();
        hidden.show_dotdot = false;
        let rows = mode_rows(&hidden);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(is_p, _)| !is_p));

        // 根目录（crumbs 单段）→ 不显 `..` 行
        let root = AppStateUiSnapshot {
            archive_name: "a.zip".into(),
            current_dir: String::new(),
            crumbs: crate::ui::contracts::crumbs_from_current_dir("a.zip", ""),
            dir_rows: std::sync::Arc::new(vec![tree_row("x.txt", false)]),
            show_dotdot: true,
            ..Default::default()
        };
        let rows = mode_rows(&root);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].0, "根目录无 `..` 行");
    }

    /// `..` 行目标段推导（双击跳上级）与 detail 同源（`parent_seg_of`）。
    #[test]
    fn parent_segment_of_matches_detail_semantics() {
        assert_eq!(crate::ui::views::browse::parent_seg_of("docs/src"), "docs");
        assert_eq!(crate::ui::views::browse::parent_seg_of("docs"), "");
        assert_eq!(crate::ui::views::browse::parent_seg_of(""), "");
    }

    /// 选中树行画**统一选中色层**（用户要求：目录树高亮与文件列表同色）。
    ///
    /// 边界探针：选中行有 `tree-selected-layer` 命中区，未选中行没有——
    /// 该层的颜色来自 `selection_row_bg`（其浅/深主题语义另有单测把守）。
    #[gpui_kit::test]
    fn selected_tree_row_paints_unified_selection_layer(cx: &mut gpui_kit::TestAppContext) {
        use crate::ui::contracts::ShellHandlers;
        use crate::ui::shell::AppShell;
        use gpui_kit::component::tree::TreeItem;

        struct Host {
            tree: Entity<TreeState>,
            shell: WeakEntity<AppShell>,
        }
        impl gpui_kit::Render for Host {
            fn render(
                &mut self,
                _: &mut Window,
                _cx: &mut gpui_kit::Context<Self>,
            ) -> impl IntoElement {
                let shell = self.shell.clone();
                let tree = self.tree.clone();
                let meta = crate::ui::binding::TreeMeta::new();
                Tree::new(&self.tree, move |ix, entry, selected, window, cx| {
                    render_tree_item(
                        &shell, &tree, ix, entry, selected, &meta, "a.zip", window, cx,
                    )
                })
            }
        }

        let (host, vtcx) = cx.add_window_view(|_window, cx| {
            gpui_kit::component::init(cx);
            crate::ui::theme::init(cx);
            let shell = cx.new(|cx| AppShell::new(cx, ShellHandlers::default()));
            let tree = cx.new(|cx| TreeState::new(cx));
            tree.update(cx, |t, cx| {
                t.set_items(
                    vec![TreeItem::new("", "a.zip"), TreeItem::new("docs", "docs")],
                    cx,
                );
                t.set_selected_index(Some(0), cx);
            });
            Host {
                tree,
                shell: shell.downgrade(),
            }
        });
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            vtcx.debug_bounds("tree-selected-layer").is_some(),
            "选中树行必须画出统一选中色层（否则退回 kit 的全量 accent）"
        );

        // 反向：清空选中 → 该层必须消失（证明它随选中态出现，而非恒在）
        let tree = vtcx.read(|cx| host.read(cx).tree.clone());
        vtcx.update(|_window, cx| {
            tree.update(cx, |t, cx| t.set_selected_index(None, cx));
        });
        vtcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            vtcx.debug_bounds("tree-selected-layer").is_none(),
            "无选中行时不得残留选中层"
        );
        drop(host);
    }

    /// 目录树行上下留白随密度（用户逐档口径：紧凑 0 / 标准 1 / 宽松 2px）。
    ///
    /// 库内 `ListItem` 写死 `py_1().px_3()`（上下 4px、左右 12px），但应用的
    /// `refine_style(自身样式)` 语义是**传入者覆盖**（gpui-base 0.7.1
    /// `StyledExt::refine_style`（styled.rs:79）直接转调 `Style::refine`；derive 生成体是
    /// `if let Some(v) = refinement.f { self.f = v }`
    /// ——gpui-pre-derive-refineable 0.3.8 `derive_refineable.rs:144`，语义 0.3.4 →
    /// 0.3.8 未变）⇒ `render_tree_item` 的
    /// `.py(row_gap())` / `.px(0)` 胜出。
    ///
    /// **断言面 = 行距**（相邻两行 body 的 y 差）。不能量选中层：它是
    /// `absolute inset_0`，命中的是**内容盒**（不含 padding），量不出留白变化。
    /// gpui 的 `uniform_list` 按「首行测得的整行高」平铺，故行距即整行高。
    /// 每档新建窗口：行高来自测量缓存，同窗口内换密度量不到真实差异。
    #[gpui_kit::test]
    fn tree_row_padding_follows_density(cx: &mut gpui_kit::TestAppContext) {
        use crate::ui::contracts::ShellHandlers;
        use crate::ui::shell::AppShell;
        use crate::viewmodel::density::Density;
        use gpui_kit::component::tree::TreeItem;

        struct Host {
            tree: Entity<TreeState>,
            shell: WeakEntity<AppShell>,
        }
        impl gpui_kit::Render for Host {
            fn render(
                &mut self,
                _: &mut Window,
                _cx: &mut gpui_kit::Context<Self>,
            ) -> impl IntoElement {
                let shell = self.shell.clone();
                let tree = self.tree.clone();
                let meta = crate::ui::binding::TreeMeta::new();
                Tree::new(&self.tree, move |ix, entry, selected, window, cx| {
                    render_tree_item(
                        &shell, &tree, ix, entry, selected, &meta, "a.zip", window, cx,
                    )
                })
            }
        }

        let mut pitches = Vec::new();
        // 参照系：同一窗口的字体字形盒（ascent − descent，px）——独立于渲染路径
        // 直接向 `TextSystem` 要，用来判「渲染出的行框有没有小于字形盒（= 裁字）」
        let mut glyph_box = 0.0f32;
        // 字体**墨迹盒**下限（`bounding_box`）：字形盒只是声明值，装不下全部墨迹
        let mut ink_need = 0.0f32;
        for density in [Density::Compact, Density::Standard, Density::Loose] {
            crate::ui::spacing::set_density(density);
            let (host, vtcx) = cx.add_window_view(|_window, cx| {
                gpui_kit::component::init(cx);
                crate::ui::theme::init(cx);
                let shell = cx.new(|cx| AppShell::new(cx, ShellHandlers::default()));
                let tree = cx.new(|cx| TreeState::new(cx));
                tree.update(cx, |t, cx| {
                    t.set_items(
                        vec![TreeItem::new("", "a.zip"), TreeItem::new("docs", "docs")],
                        cx,
                    );
                });
                Host {
                    tree,
                    shell: shell.downgrade(),
                }
            });
            vtcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            vtcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let row0 = vtcx.debug_bounds("tree-row-0").expect("首行 body 锚点");
            let row1 = vtcx.debug_bounds("tree-row-1").expect("次行 body 锚点");
            pitches.push(f32::from(row1.origin.y - row0.origin.y));
            (glyph_box, ink_need) = vtcx.update(|window, cx| {
                let ts = cx.text_system();
                let id = ts.resolve_font(&window.text_style().font());
                let size = window.rem_size();
                let a = f32::from(ts.ascent(id, size));
                let d = f32::from(-ts.descent(id, size));
                let bb = ts.bounding_box(id, size);
                let ink_up = f32::from(bb.origin.y + bb.size.height);
                let ink_down = f32::from(-bb.origin.y);
                (a - d, (a + d) + 2. * (ink_up - a).max(ink_down - d).max(0.))
            });
            drop(host);
        }
        crate::ui::spacing::set_density(Density::Standard);

        let (cmp_p, std_p, los_p) = (pitches[0], pitches[1], pitches[2]);
        assert!(
            (std_p - cmp_p - 2.).abs() < 0.51,
            "紧凑档行应比标准矮 2px（上下各 0px）：{cmp_p} vs {std_p}"
        );
        assert!(
            (los_p - std_p - 2.).abs() < 0.51,
            "宽松档行应比标准高 2px（上下各 2px）：{los_p} vs {std_p}"
        );
        // 行框 = 字体**墨迹盒**下限（`bounding_box`，不是 `ascent − descent`）：
        // ① 不小于**墨迹盒**要求（`(A+D) + 2·max(墨迹上−A, 墨迹下−D)`）——比字形盒
        //    大约 3px；按字形盒排框正是用户实测的「文件名里的 `_` 底部被裁」
        //    （紧凑档看不见 `_`、标准档「在最底部马上就要溢出」）；
        // ② 不大于墨迹盒要求 + 1px（否则又是白留白）。
        // 两条同时成立 ⇒「文字/图标上下 padding 恒 0」+「文字必须完整」的交集。
        for (pitch, gap, tag) in [
            (cmp_p, 0., "紧凑"),
            (std_p, 1., "标准"),
            (los_p, 2., "宽松"),
        ] {
            let content_h = pitch - gap * 2.;
            assert!(
                content_h >= ink_need - 0.51,
                "{tag}档行框（{content_h}）小于字体墨迹盒下限（{ink_need:.2}）——会裁掉 `_`/降部"
            );
            // 上限 = 墨迹盒 + 上下各 `TEXT_SLACK_PX`（常量余量，「`_` 不贴行底」）
            let want = ink_need + 2. * crate::ui::constant::TEXT_SLACK_PX;
            assert!(
                content_h <= want + 1.01,
                "{tag}档行框（{content_h}）超出墨迹盒+余量（{want:.2}）>1px——多余留白"
            );
        }
        let _ = glyph_box;
    }

    /// 目录树行高亮层**盖满整行**（用户实测：行高亮只在文字/图标周围，露出
    /// 一圈 kit 全量 accent）。库把选中底色画在行背景槽、应用样式只能盖在
    /// 内容盒上 ⇒ 行内 padding 一旦非 0，高亮层就短一截。故：行内上下 padding
    /// 恒 0、间隔改挂内容外边距，高亮层（`inset_0`）高度必须 == 行高（行距）。
    #[gpui_kit::test]
    fn tree_row_highlight_fills_whole_row(cx: &mut gpui_kit::TestAppContext) {
        use crate::ui::contracts::ShellHandlers;
        use crate::ui::shell::AppShell;
        use crate::viewmodel::density::Density;
        use gpui_kit::component::tree::TreeItem;

        struct Host {
            tree: Entity<TreeState>,
            shell: WeakEntity<AppShell>,
        }
        impl gpui_kit::Render for Host {
            fn render(
                &mut self,
                _: &mut Window,
                _cx: &mut gpui_kit::Context<Self>,
            ) -> impl IntoElement {
                let shell = self.shell.clone();
                let tree = self.tree.clone();
                let meta = crate::ui::binding::TreeMeta::new();
                Tree::new(&self.tree, move |ix, entry, selected, window, cx| {
                    render_tree_item(
                        &shell, &tree, ix, entry, selected, &meta, "a.zip", window, cx,
                    )
                })
            }
        }

        for (density, gap) in [
            (Density::Compact, 0.),
            (Density::Standard, 1.),
            (Density::Loose, 2.),
        ] {
            crate::ui::spacing::set_density(density);
            let (host, vtcx) = cx.add_window_view(|_window, cx| {
                gpui_kit::component::init(cx);
                crate::ui::theme::init(cx);
                let shell = cx.new(|cx| AppShell::new(cx, ShellHandlers::default()));
                let tree = cx.new(|cx| TreeState::new(cx));
                tree.update(cx, |t, cx| {
                    t.set_items(
                        vec![
                            TreeItem::new("", "a.zip"),
                            TreeItem::new("docs", "docs"),
                            TreeItem::new("x.txt", "x.txt"),
                        ],
                        cx,
                    );
                    t.set_selected_index(Some(0), cx);
                });
                Host {
                    tree,
                    shell: shell.downgrade(),
                }
            });
            vtcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            vtcx.update(|window, cx| {
                let _ = window.draw(cx);
            });

            let layer = vtcx
                .debug_bounds("tree-selected-layer")
                .expect("选中行应有自绘层");
            let r0 = vtcx.debug_bounds("tree-row-0").expect("首行 body 锚点");
            let r1 = vtcx.debug_bounds("tree-row-1").expect("次行 body 锚点");
            let pitch = f32::from(r1.origin.y - r0.origin.y);
            let layer_h = f32::from(layer.size.height);
            assert!(
                (layer_h - pitch).abs() < 0.51,
                "{density:?} 高亮层必须盖满整行（行距 {pitch}），实得 {layer_h}"
            );
            // 行顶 = 内容顶 − 行外间隔（间隔挂在内容外边距上，行内 padding 恒 0）
            let row_top = f32::from(r0.origin.y) - gap;
            assert!(
                (f32::from(layer.origin.y) - row_top).abs() < 0.51,
                "{density:?} 高亮层顶边应与行顶对齐（不留 accent 边圈）：\
             层顶 {} vs 行顶 {row_top}",
                f32::from(layer.origin.y)
            );
            // 横向同样要铺满：缩进是内容外边距 ⇒ 层必须从行左沿（x=0）起，
            // 铺满整行宽——否则缩进那截会露出 kit 的全量 accent（用户实测：
            // 「文字/图标是一色、文字/图标左侧的行 padding 是另一色」）。
            // 缩进换成内容外边距后必须仍然生效（层级可读性）：depth 0 行内容
            // 左沿 = sp(3.) = 12px × 密度系数
            let want_indent = 12. * density.scale();
            assert!(
                (f32::from(r0.origin.x) - want_indent).abs() < 0.51,
                "{density:?} 行内容缩进应 = {want_indent}px（sp(3.)），\
             实得 {}",
                f32::from(r0.origin.x)
            );
            let win_w = vtcx.update(|w, _| f32::from(w.bounds().size.width));
            assert!(
                f32::from(layer.origin.x).abs() < 0.51,
                "{density:?} 高亮层左沿应在行左沿（x=0），实得 {}",
                f32::from(layer.origin.x)
            );
            assert!(
                (f32::from(layer.size.width) - win_w).abs() < 0.51,
                "{density:?} 高亮层应铺满整行宽（缩进区也要着色）：\
             层宽 {} vs 行宽 {win_w}",
                f32::from(layer.size.width)
            );
            drop(host);
        }
        crate::ui::spacing::set_density(Density::Standard);
    }

    /// 目录树重新显示：按记忆宽度插回索引 0（用户实测「很长的目录树」回归锚点）。
    ///
    /// 隐藏期只剩表格（索引 0 持全宽）；重新显示若沿用这份尺寸，树会继承
    /// 表格全宽。断言插回后索引 0 == 记忆宽度（而非 1000 全宽）；已双面板
    /// 不重复插（防面板数错位）。
    ///
    /// 余量重分配（表格拿 container - 树宽）依赖实测容器宽度，此处无布局
    /// 测量（bounds 零）故不断言绝对值——那是 kit `insert_panel` 自身的行为。
    #[gpui_kit::test]
    fn restore_tree_panel_width_puts_tree_back_at_memory(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::base::ResizableState;
        cx.update(|cx| {
            let split = cx.new(|_| ResizableState::default());
            // 隐藏期形态：单面板（表格）持全宽
            split.update(cx, |s, cx| {
                s.insert_panel(Some(px(1000.)), None, cx);
            });
            restore_tree_panel_width(&split, 280., cx);
            let sizes = split.read(cx).sizes().clone();
            assert_eq!(sizes.len(), 2, "重新显示后应为双面板");
            assert_eq!(
                sizes[0],
                px(280.),
                "树宽 = 记忆宽度（不是表格的 1000 全宽）"
            );

            // 已双面板 → 不重复插（防面板数错位）
            restore_tree_panel_width(&split, 280., cx);
            assert_eq!(split.read(cx).sizes().len(), 2, "重复调用不增面板");
        });
    }
}
