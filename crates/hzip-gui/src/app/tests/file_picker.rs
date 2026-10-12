use super::*;
use crate::ui::contracts::associable_extensions;
use hzip_core::engine::{Engine, EngineConfig};

/// 扩展清单派生：表驱动（`hzip_format::exts`）∩ 本构建已注册 handler。
///
/// 守住用户报的缺陷：**别名 `tgz` 必须在列**（缺它则 `*.gz` 过滤命中不了
/// `a.tgz`，选择器里看不见自己的 tgz 档案；右键菜单同一处遗漏导致菜单里
/// 没有解压项）。
#[test]
fn supported_extensions_cover_aliases_and_composites() {
    let engine = Engine::new(EngineConfig::default()).expect("默认 Engine 构造");
    let exts = supported_open_extensions(&engine);
    for core in ["zip", "7z", "tar", "gz", "tgz", "tbz2", "txz", "tzst"] {
        assert!(exts.iter().any(|e| e == core), "缺扩展 {core}：{exts:?}");
    }
    // 复合写法也在（表里的 `tar.gz`；用于档案判定与过滤）
    assert!(exts.iter().any(|e| e == "tar.gz"), "{exts:?}");
    // 全小写（数字与点允许：`7z` / `tar.gz`）
    assert!(
        exts.iter().all(|e| e
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')),
        "存在非小写扩展：{exts:?}"
    );
    // 严格递增 ⇒ 排序稳定 + 无重复
    assert!(
        exts.windows(2).all(|w| w[0] < w[1]),
        "未排序或有重复：{exts:?}"
    );
    // 关联候选是它的子集，且**不含复合写法**（Windows 关联看最后一个点）
    let assoc = associable_extensions(&engine);
    assert!(assoc.iter().all(|e| !e.contains('.')), "{assoc:?}");
    assert!(
        assoc.iter().all(|e| exts.contains(e)),
        "候选应 ⊆ 可打开集：{assoc:?} vs {exts:?}"
    );
    assert!(assoc.contains(&"tgz".to_string()), "{assoc:?}");
}

/// 表 ∩ 注册表：**逐项一致**——某格式的所有层都注册了 ⇔ 其扩展名在清单里。
///
/// 不能断言「全都在」：`rar` 由 `hzip-core/rar` feature 门控（unrar C++ 依赖），
/// 默认构建未编入，故候选里没有 `rar` —— 这正是「不广告未编入的格式」的
/// 期望行为。本用例守的是这条**规则**，故对任何 feature 组合都成立。
#[test]
fn extensions_track_registered_handlers() {
    let engine = Engine::new(EngineConfig::default()).expect("默认 Engine 构造");
    let exts = supported_open_extensions(&engine);
    let registry = engine.registry();
    for entry in hzip_format::exts::FORMAT_EXTS {
        let available =
            hzip_format::exts::components(entry.format).all(|c| registry.handler(c).is_some());
        for ext in entry.exts {
            assert_eq!(
                exts.iter().any(|e| e == ext),
                available,
                "{} 的 {ext} 出现与否须与 handler 注册一致：{exts:?}",
                entry.format
            );
        }
    }
    // 默认构建（无 rar）：rar 不出现；启用 rar feature 的构建应出现
    assert_eq!(
        exts.iter().any(|e| e == "rar"),
        registry.handler("rar").is_some(),
        "rar 是否列入须与 rar feature 一致"
    );
}
