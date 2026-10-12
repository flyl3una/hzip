//! macOS 图标后端验收（T-ICON；由 `macos.rs` 的 `#[cfg(test)] #[path]` 引入）。
//!
//! 直接测被测后端的**可注入面**与**不变量**，不依赖「本机装了哪些 UTType」这类
//! 随机器变化的前提；只有确实需要 AppKit 真实取图的用例才碰系统，其余用
//! 合成数据钉住纯函数语义。
//!
//! 本文件只随 `macos.rs` 编译，而 `macos.rs` 已被 `#[cfg(target_os = "macos")]`
//! 门控（它依赖 target 门控的 objc2 家族）⇒ 测试模块本身不需再加 feature 门。

use super::*;

/// 一个「普通文件」请求。
fn file_req(ext: Option<&str>) -> IconRequest<'_> {
    IconRequest {
        ext,
        kind: IconKind::File,
    }
}

// ---------------------------------------------------------------- 纯函数

/// `tag` 主标签比较：大小写不敏感、`-`/`_` 分隔都认；空/畸形不 panic。
#[test]
fn primary_tag_matches_folds_case_and_separators() {
    assert!(primary_tag_matches("zh-Hans", "zh"));
    assert!(primary_tag_matches("zh_CN", "zh"));
    assert!(primary_tag_matches("ZH", "zh"));
    assert!(primary_tag_matches("en-GB", "en"));
    assert!(!primary_tag_matches("en", "zh"));
    // 空/无主段：不得 panic，一律不匹配
    assert!(!primary_tag_matches("", "zh"));
    assert!(!primary_tag_matches("-x", ""));
}

/// 预乘 RGBA → BGRA：只换 R/B，alpha 与预乘不变式（通道 ≤ alpha）保持。
#[test]
fn rgba_to_bgra_swaps_red_and_blue_only() {
    // 两像素：不透明红、半透明蓝（预乘后各通道 ≤ alpha）
    let mut px = vec![255, 0, 0, 255, 0, 0, 127, 127];
    rgba_to_bgra_in_place(&mut px);
    assert_eq!(px, vec![0, 0, 255, 255, 127, 0, 0, 127]);
    for c in px.chunks_exact(4) {
        assert!(
            c[0] <= c[3] && c[1] <= c[3] && c[2] <= c[3],
            "预乘不变式被破坏"
        );
    }
}

// ---------------------------------------------------------------- 真实取图

/// 取图不变量：尺寸恒 [`ICON_PX`]、字节数 = w·h·4、像素预乘、各位图互异且非空白。
///
/// 这是「与资源管理器逐类型一致」的可测投影：系统若拿不到某类型的图标，
/// `lookup` 返回 `None`（用例跳过该项），而不是给出空白/错尺寸位图。
#[test]
fn lookup_returns_premultiplied_bgra_of_icon_px() {
    let cases: [(Option<&str>, IconKind); 4] = [
        (None, IconKind::Dir),
        (Some("zip"), IconKind::File),
        (Some("txt"), IconKind::File),
        (Some("png"), IconKind::Container),
    ];
    let mut digests = std::collections::HashMap::new();
    for (ext, kind) in cases {
        let Some(icon) = lookup(&IconRequest { ext, kind }) else {
            // 无 AppKit/无 UI 会话的环境：跳过（与 win 后端 canary 同款约定）。
            println!("跳过 {ext:?}/{kind:?}：本环境 lookup 返回 None");
            continue;
        };
        assert_eq!(icon.width, ICON_PX, "{ext:?} 宽度非 ICON_PX");
        assert_eq!(icon.height, ICON_PX, "{ext:?} 高度非 ICON_PX");
        assert_eq!(
            icon.premultiplied_bgra.len(),
            (ICON_PX * ICON_PX * 4) as usize,
            "{ext:?} 字节数 ≠ w·h·4"
        );
        let total = (ICON_PX * ICON_PX) as usize;
        let mut opaque = 0usize;
        for i in 0..total {
            let (b, g, r, a) = (
                icon.premultiplied_bgra[i * 4] as u32,
                icon.premultiplied_bgra[i * 4 + 1] as u32,
                icon.premultiplied_bgra[i * 4 + 2] as u32,
                icon.premultiplied_bgra[i * 4 + 3] as u32,
            );
            assert!(
                b <= a && g <= a && r <= a,
                "{ext:?} 预乘不变式被破坏 @{i}: b={b} g={g} r={r} a={a}"
            );
            if a > 0 {
                opaque += 1;
            }
        }
        assert!(
            opaque * 20 >= total,
            "{ext:?} 不透明像素仅 {opaque}/{total}，疑似空白位图"
        );
        digests.insert(format!("{ext:?}/{kind:?}"), icon.premultiplied_bgra);
    }
    // 目录 / zip / txt / png 应当两两不同（同图 = 系统没按类型给图）。
    let keys: Vec<_> = digests.keys().cloned().collect();
    for i in 0..keys.len() {
        for j in (i + 1)..keys.len() {
            assert_ne!(
                digests[&keys[i]], digests[&keys[j]],
                "{} 与 {} 的位图相同——系统未按类型取图",
                keys[i], keys[j]
            );
        }
    }
}

/// 无扩展名与未知扩展名都必须给出**系统默认文件图标**（不 panic、不 `None`）。
///
/// 依据：`UTType` 解析不到时退回 `public.data`，与 Finder 对无扩展名文件的显示一致。
#[test]
fn unknown_and_missing_ext_fall_back_to_default_icon() {
    let (Some(missing), Some(unknown)) =
        (lookup(&file_req(None)), lookup(&file_req(Some("zzzznope"))))
    else {
        println!("跳过：本环境 lookup 返回 None");
        return;
    };
    assert_eq!(
        missing.premultiplied_bgra, unknown.premultiplied_bgra,
        "无扩展名与未知扩展名应取到同一张默认文件图标"
    );
    // 且默认图标 != 目录图标（证明走的不是「碰巧落到目录」的路径）。
    if let Some(dir) = lookup(&IconRequest {
        ext: None,
        kind: IconKind::Dir,
    }) {
        assert_ne!(missing.premultiplied_bgra, dir.premultiplied_bgra);
    }
}

/// 并发取图确定性：多线程反复取同一组键，全部成功且字节一致。
///
/// 与 win 后端同款压力（那边发现过并发下 shell 会失败）。macOS 侧实测
/// 1600/1600 干净；本用例把该结论钉在 CI 可跑的形式上。
#[test]
fn concurrent_lookup_is_deterministic() {
    use std::sync::{Arc, Mutex};

    let keys: Vec<(Option<&'static str>, IconKind)> = vec![
        (None, IconKind::Dir),
        (Some("zip"), IconKind::File),
        (Some("txt"), IconKind::File),
        (Some("png"), IconKind::File),
    ];
    // 先取一遍作为基准；任一键拿不到就整体跳过（无 AppKit 的环境）。
    let mut expected = Vec::new();
    for (ext, kind) in &keys {
        match lookup(&IconRequest {
            ext: *ext,
            kind: *kind,
        }) {
            Some(icon) => expected.push(icon.premultiplied_bgra),
            None => {
                println!("跳过：本环境 lookup 返回 None");
                return;
            }
        }
    }
    let expected = Arc::new(expected);
    let mismatches = Arc::new(Mutex::new(0usize));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let expected = Arc::clone(&expected);
        let mismatches = Arc::clone(&mismatches);
        let keys = keys.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..25 {
                for (i, (ext, kind)) in keys.iter().enumerate() {
                    match lookup(&IconRequest {
                        ext: *ext,
                        kind: *kind,
                    }) {
                        Some(icon) if icon.premultiplied_bgra == expected[i] => {}
                        _ => {
                            *mismatches.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                        }
                    }
                }
            }
        }));
    }
    for h in handles {
        h.join().expect("探测线程不得 panic");
    }
    let bad = *mismatches.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(bad, 0, "并发取图出现 {bad} 次失败/不一致");
}
