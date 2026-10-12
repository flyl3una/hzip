//! 图标资产链（T-G50；用户问题 #5 根因修复）。
//!
//! # 根因
//!
//! `gpui_kit::application()`（= `gpui_platform::application()`）默认**不挂任何
//! AssetSource**；`IconName::path()` 解析出的 `icons/xxx.svg` 无人服务 →
//! `svg()` 渲染空白。
//!
//! # 方案（gpui-kit-assets lib.rs 文档用法）
//!
//! `application().with_assets(ChainedAssets)`——链式资产源：
//! 1. 自有 [`HzipAssets`]（`assets/icons/*.svg`，RustEmbed 内嵌，构建离线）；
//! 2. miss 回落 `gpui_kit::assets::Assets`——**默认图标子集**（106 项，静态
//!    嵌入约 47 KB）。现存 `IconName` 用法全部落在该子集内（见
//!    `tests/assets.rs` 的契约用例
//!    `chained_assets_serves_the_default_subset_not_full_catalog`）。
//!
//! # 为什么不用全量 `AllAssets`（2026-10 复核后的口径）
//!
//! gpui-kit 0.7.1 把 `Assets` **收窄**为 gpui-component 的默认子集
//! （`gpui-kit-assets-0.7.1/default-icons.txt`，106 项），全量目录（1830 项）
//! 改名 `AllAssets`（同 crate `src/lib.rs`：「`Assets` for the default component
//! bundle, or `AllAssets` for the full catalog」）。回落取默认子集，因为当前
//! **没有一处需要子集之外的图标**：
//! - 全仓唯一直接使用 kit `IconName` 的位置是 `ui/widgets/breadcrumb.rs` 的
//!   `ChevronRight`（`default-icons.txt` 内）；
//! - 46 个 [`crate::ui::widgets::AppIcon`] 路径全部命中自有 [`HzipAssets`]；
//! - gpui-component 自身引用的图标枚举由同一份 `default-icons.txt` 生成。
//!
//! 静态嵌入量对比（实测）：默认子集 ≈ 46,936 B，全量 ≈ 749,008 B ⇒ 全量要多
//! **+702 KB**。这与本文件为省 60 KB 把 `assets/logo/` 排除在外是同一口径。
//! **若将来真要默认子集之外的 `IconName`**，改用 `gpui_kit::assets::AllAssets`
//! （load/list 两处，单点改动）——换之前先确认确有必要。
//!
//! 路径空间刻意同构（`icons/…`），链式回落才能命中。`assets/logo/`（程序图标
//! 源）被 `#[exclude]` 挡在资产源外——它只服务于 exe 图标资源，见 [`HzipAssets`]。
//! 资产入库：`build/icons/fetch-lucide-icons.sh`（一次性工具，结果提交 git；
//! 构建期零网络依赖）。lucide SVG 原始文件 `stroke="currentColor"`，
//! 随文字色走主题——禁止给图标硬编码颜色。

use gpui_kit::AssetSource;
use rust_embed::RustEmbed;
use std::borrow::Cow;

/// 自有 lucide 图标（`assets/` 相对 crate 根；与 gpui-kit-assets 路径空间同构）。
///
/// `assets/logo/` 是**程序图标源**（`hzip-<N>.png` + `hzip.ico`，由
/// `build/icons/make-icons.ps1` 生成、经 `hzip.rc` 内嵌进 exe 资源段），**不是**
/// 渲染资产 ⇒ 用 `exclude` 挡在资产源之外：否则那 60KB 会随 RustEmbed 一起
/// 进二进制（图标已在 PE 资源段里，等于存两份），`list("")` 也会多出无关条目。
#[derive(RustEmbed)]
#[folder = "assets"]
#[exclude = "logo/*"]
pub struct HzipAssets;

/// 链式资产源：先查自有，miss 回落 `gpui_kit::assets::Assets`（kit 的**默认图标
/// 子集**，106 项；0.7.1 起 `Assets` 已从全量收窄为它，全量叫 `AllAssets`，
/// 取舍理由见本文件头）。
pub struct ChainedAssets;

impl AssetSource for ChainedAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(data) = HzipAssets::get(path) {
            return Ok(Some(data.data));
        }
        // kit 侧 miss 语义为 Err(anyhow)——归一为 Ok(None)（gpui 渲染路径对
        // None 静默跳过；双源皆 miss 的路径不再炸渲染）。`Assets` 的
        // `impl_asset_source`（gpui-kit-assets src/native_assets.rs）与全量
        // `AllAssets` 同实现，该语义与收窄无关。
        Ok(gpui_kit::assets::Assets.load(path).ok().flatten())
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<gpui_kit::SharedString>> {
        let mut own: Vec<gpui_kit::SharedString> = HzipAssets::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect();
        // 链式语义：自有目录**遮蔽** kit 同名条目（与 `load` 的优先序一致），
        // 故清单必须去重——自有 46 个图标里有 28 个与 kit 默认子集同名，直接
        // 拼接会让消费方看到重复条目。
        let seen: std::collections::HashSet<gpui_kit::SharedString> = own.iter().cloned().collect();
        own.extend(
            gpui_kit::assets::Assets
                .list(path)?
                .into_iter()
                .filter(|p| !seen.contains(p)),
        );
        Ok(own)
    }
}

#[cfg(test)]
#[path = "tests/assets.rs"]
mod tests;
