//! Windows 图标后端集成测试（只用公开 API）。
//!
//! 目标不是「非空」这类废话断言，而是钉死四条不变量：
//! 1. 输出尺寸/字节数恒为 `ICON_PX`²/×4；
//! 2. 像素是 **预乘 BGRA**（每通道 ≤ alpha）且位图非空白；
//! 3. shell 真的**按类型**返回了不同图标（dir/zip/txt/exe 两两不同）；
//! 4. 并发取图确定性（8 线程 × 50 次全成功、同键字节一致）。
//!
//! 需要 shell 的用例先做 canary 探测；无 shell 的精简镜像/容器里打印跳过说明并
//! 正常返回（全量测试仍绿），不 panic（验收标准 5）。
//!
//! # 本机实测留档（2026-10-10，Windows 10.0.19045）
//!
//! **COM 套间**：不初始化 COM 时 `SHGetFileInfoW` 走**降级路径**，`.png`/`.mp3`/无扩展名
//! 的位图完全相同（都退回通用页图标）；调用 `crate::open::ensure_com_initialized()` 后
//! 各类型各不相同。故 win.rs 在 shell 调用前初始化套间（与仓库其它 shell 调用点一致）。
//!
//! **互异集合取 dir/zip/txt/exe**：`.png`/`.mp3` 这类「关联是否注册」在精简镜像/容器里
//! 可能缺失（此时 shell 会如实回退默认图标），故不作为必然互异的硬断言；用例另设
//! 「zip/txt/exe 与默认图标不同」的跨机稳健护栏。
#![cfg(target_os = "windows")]
#![allow(clippy::print_stdout)]

use std::sync::{Arc, Barrier};

use hzip_platform::file_icon::{ICON_PX, IconKind, IconRequest, SystemIcon, lookup};

/// canary：本进程 shell 图标通道是否可用（无 shell 的精简镜像/容器 → `false`）。
fn shell_available() -> bool {
    lookup(&IconRequest {
        ext: Some("txt"),
        kind: IconKind::File,
    })
    .is_some()
}

/// 查询并断言成功（仅在 canary 已确认可用后调用）。
fn get(ext: Option<&str>, kind: IconKind) -> SystemIcon {
    lookup(&IconRequest { ext, kind })
        .unwrap_or_else(|| panic!("canary 之后 lookup 不应失败：ext={ext:?} kind={kind:?}"))
}

/// 无 shell 环境：打印一行说明后跳过（不 panic）。
fn skip(what: &str) {
    println!("跳过 {what}：本环境 shell 图标通道不可用（lookup 恒 None）");
}

/// 尺寸与字节数不变量。
fn check_shape(icon: &SystemIcon) {
    assert_eq!(icon.width, ICON_PX, "宽度非 ICON_PX");
    assert_eq!(icon.height, ICON_PX, "高度非 ICON_PX");
    assert_eq!(
        icon.premultiplied_bgra.len(),
        (icon.width * icon.height * 4) as usize,
        "字节数 ≠ w*h*4"
    );
}

/// 预乘不变式：每像素 B/G/R ≤ A；且不透明像素占比 ≥ 5%（证非空白）。
fn check_premultiplied(icon: &SystemIcon) {
    let px = &icon.premultiplied_bgra;
    let total = (icon.width * icon.height) as usize;
    let mut opaque = 0usize;
    for i in 0..total {
        let b = px[i * 4] as u32;
        let g = px[i * 4 + 1] as u32;
        let r = px[i * 4 + 2] as u32;
        let a = px[i * 4 + 3] as u32;
        assert!(
            b <= a && g <= a && r <= a,
            "预乘不变式被破坏 @{i}: b={b} g={g} r={r} a={a}"
        );
        if a > 0 {
            opaque += 1;
        }
    }
    assert!(
        opaque * 20 >= total,
        "不透明像素仅 {opaque}/{total}，疑似空白位图"
    );
}

/// FNV-1a 64 位（跨线程/跨调用比对位图字节是否一致）。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[test]
fn typed_icons_are_pairwise_distinct_and_premultiplied() {
    if !shell_available() {
        skip("逐类型互异断言");
        return;
    }

    let dir = get(None, IconKind::Dir);
    let zip = get(Some("zip"), IconKind::File);
    let txt = get(Some("txt"), IconKind::File);
    let exe = get(Some("exe"), IconKind::File);
    let default = get(None, IconKind::File);

    let set: [(&str, &SystemIcon); 5] = [
        ("dir", &dir),
        ("zip", &zip),
        ("txt", &txt),
        ("exe", &exe),
        ("default", &default),
    ];
    for (_name, icon) in set {
        check_shape(icon);
        check_premultiplied(icon);
    }

    // 跨机稳健：无桌面 / 无关联注册的精简镜像里，txt/zip/exe 可能全部退回默认图标，
    // 此时无从判别「按类型取图」，打印说明后跳过（不误报失败）。
    let typed: [(&str, &SystemIcon); 3] = [("zip", &zip), ("txt", &txt), ("exe", &exe)];
    let distinct_from_default = typed
        .iter()
        .filter(|(_, i)| i.premultiplied_bgra != default.premultiplied_bgra)
        .count();
    if distinct_from_default == 0 {
        println!("跳过逐类型互异断言：本环境 zip/txt/exe 均无专属图标（退回默认文件图标）");
        return;
    }

    assert_ne!(
        dir.premultiplied_bgra, default.premultiplied_bgra,
        "目录图标 == 默认文件图标"
    );
    assert!(
        distinct_from_default >= 2,
        "zip/txt/exe 中仅 {distinct_from_default} 个有专属图标（期望 ≥2）"
    );

    // 逐类型可区分：证明 shell 真的按扩展名/属性位返回了不同图标。
    let set: [(&str, &SystemIcon); 4] =
        [("dir", &dir), ("zip", &zip), ("txt", &txt), ("exe", &exe)];
    for a in 0..set.len() {
        for b in (a + 1)..set.len() {
            assert_ne!(
                set[a].1.premultiplied_bgra, set[b].1.premultiplied_bgra,
                "{} 与 {} 位图相同",
                set[a].0, set[b].0
            );
        }
    }
}

#[test]
fn no_extension_and_unknown_extension_get_default_icon() {
    if !shell_available() {
        skip("默认/未知扩展名用例");
        return;
    }

    let noext = get(None, IconKind::File);
    let unknown = get(Some("zzz9"), IconKind::File);
    let txt = get(Some("txt"), IconKind::File);
    let zip = get(Some("zip"), IconKind::File);

    for icon in [&noext, &unknown] {
        check_shape(icon);
        check_premultiplied(icon);
    }

    // 无扩展名与未知扩展名都落回同一个默认文件图标。
    assert_eq!(
        noext.premultiplied_bgra, unknown.premultiplied_bgra,
        "无扩展名与未知扩展名的图标不同（都应回退默认文件图标）"
    );

    // 默认/未知文件的图标必须与「有专属图标」的类型不同。参照系取 txt/zip：两者在
    // 正常桌面镜像上必有专属图标；`.png`/`.mp3` 的关联在精简镜像里可能缺失（shell 会
    // 如实回退默认图标），故不作参照（见文件头「互异集合」说明）。
    assert_ne!(
        noext.premultiplied_bgra, txt.premultiplied_bgra,
        "noext == txt"
    );
    assert_ne!(
        noext.premultiplied_bgra, zip.premultiplied_bgra,
        "noext == zip"
    );
    assert_ne!(
        unknown.premultiplied_bgra, txt.premultiplied_bgra,
        "zzz9 == txt"
    );
    assert_ne!(
        unknown.premultiplied_bgra, zip.premultiplied_bgra,
        "zzz9 == zip"
    );
}

#[test]
fn type_names_are_populated() {
    if !shell_available() {
        skip("系统类型名用例");
        return;
    }

    let txt = get(Some("txt"), IconKind::File);
    assert!(
        txt.type_name.as_deref().is_some_and(|s| !s.is_empty()),
        "txt 未返回系统类型名（SHGFI_TYPENAME）：{:?}",
        txt.type_name
    );

    let dir = get(None, IconKind::Dir);
    assert!(
        dir.type_name.as_deref().is_some_and(|s| !s.is_empty()),
        "目录未返回系统类型名（SHGFI_TYPENAME）：{:?}",
        dir.type_name
    );
}

#[test]
fn symlink_overlay_differs_from_plain_file() {
    if !shell_available() {
        skip("符号链接覆盖层用例");
        return;
    }

    let plain = get(Some("txt"), IconKind::File);
    let link = get(Some("txt"), IconKind::Symlink);

    check_shape(&link);
    check_premultiplied(&link);

    // `SHGFI_LINKOVERLAY`（资源管理器快捷方式箭头）的唯一回归护栏。
    assert_ne!(
        plain.premultiplied_bgra, link.premultiplied_bgra,
        "Symlink 覆盖层未生效（与普通文件位图逐字节相同）"
    );
}

#[test]
fn concurrent_lookup_is_deterministic() {
    if !shell_available() {
        skip("并发确定性用例");
        return;
    }

    const KEYS: [(Option<&str>, IconKind); 6] = [
        (None, IconKind::Dir),
        (Some("zip"), IconKind::File),
        (Some("txt"), IconKind::File),
        (Some("pdf"), IconKind::File),
        (None, IconKind::File),
        (Some("zzz9"), IconKind::File),
    ];
    const THREADS: usize = 8;
    const REPS: usize = 50;

    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut none = 0u32;
                let mut changed = 0u32;
                let mut last: Vec<Option<u64>> = vec![None; KEYS.len()];
                barrier.wait();
                for _ in 0..REPS {
                    for (i, &(ext, kind)) in KEYS.iter().enumerate() {
                        match lookup(&IconRequest { ext, kind }) {
                            None => none += 1,
                            Some(icon) => {
                                let f = fnv1a64(&icon.premultiplied_bgra);
                                if last[i].is_some_and(|prev| prev != f) {
                                    changed += 1;
                                }
                                last[i] = Some(f);
                            }
                        }
                    }
                }
                (none, changed, last)
            })
        })
        .collect();

    let mut results = Vec::new();
    for h in handles {
        results.push(h.join().unwrap_or_else(|_| panic!("并发压力线程 panic")));
    }

    let total_none: u32 = results.iter().map(|r| r.0).sum();
    let total_changed: u32 = results.iter().map(|r| r.1).sum();
    let total = (THREADS * REPS * KEYS.len()) as u32;
    assert_eq!(
        total_none, 0,
        "并发下 {total_none}/{total} 次 lookup 返回 None"
    );
    assert_eq!(
        total_changed, 0,
        "并发下同键位图发生变化 {total_changed} 次（疑未清零/竞态）"
    );

    // 跨线程同键位图逐字节一致。
    for i in 0..KEYS.len() {
        let first = results[0].2[i].unwrap_or_else(|| panic!("线程 0 的键 {i} 从未成功"));
        for r in &results {
            assert_eq!(r.2[i], Some(first), "键 {i} 跨线程位图不一致");
        }
    }
}

#[test]
fn edge_inputs_do_not_panic() {
    // 调用方本应归一化扩展名；后端不得 panic，返回 Some 时也必须满足尺寸/预乘不变量。
    let long = "e".repeat(300);
    let cases: [Option<&str>; 8] = [
        Some(""),
        Some("a"),
        Some("verylongext0123456789"),
        Some("tar.gz"),
        Some("a\0b"),
        Some("文档"),
        Some(&long),
        None,
    ];
    for ext in cases {
        let req = IconRequest {
            ext,
            kind: IconKind::File,
        };
        if let Some(icon) = lookup(&req) {
            check_shape(&icon);
            check_premultiplied(&icon);
        }
    }

    // 其余种类 + 边界组合也不得 panic。
    let _ = lookup(&IconRequest {
        ext: Some("txt"),
        kind: IconKind::Symlink,
    });
    let _ = lookup(&IconRequest {
        ext: None,
        kind: IconKind::Symlink,
    });
    let _ = lookup(&IconRequest {
        ext: Some("7z"),
        kind: IconKind::Container,
    });
    let _ = lookup(&IconRequest {
        ext: None,
        kind: IconKind::Dir,
    });
}
