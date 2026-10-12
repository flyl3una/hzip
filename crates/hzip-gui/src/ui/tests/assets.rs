use super::*;

// 链式命中：自有图标优先（本 crate 资产目录入库的 svg 可加载）
#[test]
fn chained_assets_serves_own_icons() {
    let a = ChainedAssets;
    let data = a.load("icons/folder.svg").expect("load err");
    let bytes = data.expect("folder.svg missing");
    assert!(bytes.starts_with(b"<svg"), "自有图标应为 svg 文本");
}

// 工具栏按钮图标（用户口径 2026-09-29 换成 `layout-list`）：
// **枚举登记的名字必须真有对应资产**——`IconName::path()` 解析出的路径无人
// 服务时 gpui 静默跳过，界面表现为「按钮没有图标」（不报错、不 panic）。
// 本断言把「枚举 → 文件名 → 资产」这条链钉住（拼错文件名即失败）。
#[test]
fn toolbar_view_icon_resolves_to_an_embedded_asset() {
    use crate::ui::widgets::AppIcon;
    use gpui_kit::component::IconNamed as _;

    for icon in [AppIcon::LayoutList, AppIcon::ChevronDown] {
        let path = icon.path().to_string();
        let bytes = ChainedAssets
            .load(&path)
            .unwrap_or_else(|e| panic!("load {path} 失败: {e}"))
            .unwrap_or_else(|| panic!("资产缺失: {path}（界面会渲染成没有图标的按钮）"));
        assert!(bytes.starts_with(b"<svg"), "{path} 应为 svg 文本");
    }
}

// 链式回落：自有缺失 → gpui-kit-assets 内置 IconName 图标仍可服务
#[test]
fn chained_assets_falls_back_to_kit() {
    let a = ChainedAssets;
    // gpui-kit-assets 的默认图标子集（0.7.1 起 `Assets` = 106 项；全量 1830 项
    // 叫 `AllAssets`）内含 `close.svg`（`default-icons.txt:31`），自有目录未收录
    let data = a.load("icons/close.svg").expect("load err");
    assert!(data.is_some(), "回落 gpui-kit-assets 应命中 close.svg");
    // 双方都无 → Ok(None)（kit Err(anyhow) 归一，渲染路径不炸）
    let miss = a.load("icons/definitely-not-here.svg").expect("load err");
    assert!(miss.is_none());
}

// 回落源口径钉死（2026-10 复核后定，理由见 ui/assets.rs 文件头）：回落的是 gpui-kit
// 的**默认图标子集**（106 项，静态嵌入 ≈47 KB），不是全量 `AllAssets`（1830 项，
// ≈749 KB）。取前者是因为当前没有任何一处需要子集之外的图标。
// 探针 `icons/activity.svg` 在**全量有、默认子集无**：若有人把回落源换成全量，
// 本断言立刻失败——那就得把 +702 KB 的决定写进文件头与任务卡，而不是悄悄发生。
#[test]
fn chained_assets_serves_the_default_subset_not_full_catalog() {
    use gpui_kit::component::{IconName, IconNamed as _};

    let a = ChainedAssets;
    // 子集内且在用的图标：必须能回落命中（`ChevronRight` 是面包屑的图标）
    let path = IconName::ChevronRight.path().to_string();
    assert!(
        a.load(&path).expect("load err").is_some(),
        "默认子集内的 {path} 应可回落命中"
    );
    // 子集外：不该命中（命中即说明回落源已切成全量 `AllAssets`）
    assert!(
        a.load("icons/activity.svg").expect("load err").is_none(),
        "回落源应为默认子集；能命中 activity.svg 说明已切成 AllAssets（+702 KB）"
    );
}

// 链式清单语义：自有目录**遮蔽** kit 同名条目（与 `load` 的优先序一致）。
// 自有 46 个图标里有 28 个与 kit 默认子集同名，直接拼接会让 `list` 返回重复
// 条目——按清单渲染（图标选择器之类）的消费方会看到两遍。
#[test]
fn chained_assets_list_is_deduplicated() {
    let all = ChainedAssets.list("icons/").expect("list err");
    let names: std::collections::HashSet<String> = all.iter().map(|q| q.to_string()).collect();
    assert_eq!(
        all.len(),
        names.len(),
        "资产清单出现重复条目（自有与 kit 同名时未去重）"
    );
    for path in HzipAssets::iter().filter(|p| p.starts_with("icons/")) {
        assert!(
            names.contains(path.as_ref()),
            "自有条目 {path} 不该从清单里消失（自有必须遮蔽 kit，而不是被 kit 覆盖）"
        );
    }
}

// 程序图标源（`assets/logo/`）不得进资产源：它已经由 `hzip.rc` 内嵌进
// exe 资源段（`1 ICON …`），再随 RustEmbed 走一遍就是二进制里存两份；
// 资产清单也不该多出渲染用不到的条目。锚点 = `#[exclude = "logo/*"]`。
#[test]
fn logo_dir_is_not_an_asset() {
    assert!(
        HzipAssets::get("logo/hzip.ico").is_none(),
        "assets/logo/ 下的程序图标不该可加载"
    );
    assert!(
        !HzipAssets::iter().any(|p| p.starts_with("logo/")),
        "assets/logo/ 不该出现在资产清单里"
    );
}
