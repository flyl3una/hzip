//! Linux 图标后端查表验收（T-ICON A2；由 `linux.rs` 的 `#[cfg(test)] #[path]` 引入）。
//!
//! 直接测 [`lookup_in`]：搜索根是显式入参，**不读任何环境变量** ⇒ 用例彼此并行
//! 不串扰，结果也与开发机实际装了哪些主题无关。合成主题树立在系统临时目录
//! （自建 `TempDir`，与 `tests/parity.rs` 同款——`hzip-platform` 不引 tempfile
//! dev-dep，见任务卡 §Cargo.toml 约束）。
//!
//! 本文件只随 `linux.rs` 编译，而 `linux.rs` 已被
//! `#[cfg(any(all(unix, not(target_os = "macos")), feature = "cross-check"))]` 门控
//! ⇒ 测试模块本身不需要再加 feature 门。

use super::*;

use std::path::{Path, PathBuf};

/// 一个「普通文件」请求（扩展名走 MIME 表）。
fn file_req(ext: &str) -> IconRequest<'_> {
    IconRequest {
        ext: Some(ext),
        kind: IconKind::File,
    }
}

/// 生成 `px`×`px`、带透明度的 PNG 并落盘（父目录缺则建）。
///
/// 四种像素覆盖 `A=255`（左上外）、`A=0`（左上 1/4）、`A=128`（右下 1/4），
/// 一次性钉住「乘过 alpha」与「alpha 本身没被改写」两件事。落盘用 `image`
/// （hzip-platform 的正式依赖，测试不加 dev-dep）。
fn write_png(path: &Path, px: u32) {
    let mut img = image::RgbaImage::from_pixel(px, px, image::Rgba([200, 40, 90, 255]));
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        if x < px / 2 && y < px / 2 {
            *pixel = image::Rgba([10, 20, 30, 0]);
        } else if x >= px / 2 && y >= px / 2 {
            *pixel = image::Rgba([255, 255, 255, 128]);
        }
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    img.save(path).unwrap();
}

/// 预乘不变式：每像素 `B,G,R ≤ A`；且至少 5% 像素 `A>0`（否则位图等同全空）。
fn assert_premultiplied(icon: &SystemIcon) {
    let total = (icon.width * icon.height) as usize;
    let mut opaque = 0usize;
    for pixel in icon.premultiplied_bgra.chunks_exact(4) {
        let (b, g, r, a) = (pixel[0], pixel[1], pixel[2], pixel[3]);
        assert!(b <= a && g <= a && r <= a, "BGRA 未预乘：{pixel:?}");
        if a > 0 {
            opaque += 1;
        }
    }
    assert!(opaque * 20 >= total, "不透明像素不足 5%：{opaque}/{total}");
}

/// `hicolor/<size>/mime/<mime>.png` 命中：尺寸、字节数、预乘不变式。
#[test]
fn hicolor_mime_png_is_decoded_premultiplied() {
    let tmp = TempDir::new("hit");
    let root = tmp.path();
    write_png(&root.join("hicolor/32x32/mime/application-zip.png"), 32);

    let icon = lookup_in(&[root.to_path_buf()], &file_req("zip")).expect("应命中 hicolor 32x32");
    assert_eq!((icon.width, icon.height), (32, 32));
    assert_eq!(icon.premultiplied_bgra.len(), 32 * 32 * 4);
    assert!(icon.type_name.is_none(), "Linux 无系统本地化类型名");
    assert_premultiplied(&icon);
}

/// 选档：同时有 16 与 32 ⇒ 必须取最接近 [`ICON_PX`] 的 32。
#[test]
fn closest_size_dir_wins() {
    let tmp = TempDir::new("size");
    let root = tmp.path();
    write_png(&root.join("hicolor/16x16/mime/application-zip.png"), 16);
    write_png(&root.join("hicolor/32x32/mime/application-zip.png"), 32);

    let icon = lookup_in(&[root.to_path_buf()], &file_req("zip")).expect("应命中");
    assert_eq!(
        (icon.width, icon.height),
        (ICON_PX, ICON_PX),
        "应选最接近 ICON_PX 的档位而非先出现者"
    );
    assert_eq!(
        icon.premultiplied_bgra.len(),
        (ICON_PX * ICON_PX * 4) as usize
    );
}

/// `Inherits=` 链：子主题自身没有该图标 ⇒ 命中其声明的父主题。
#[test]
fn inherits_chain_reaches_parent_theme() {
    let tmp = TempDir::new("inherits");
    let root = tmp.path();
    // 子主题只有 index.theme（用真主题的写法：节头 + Inherits）。
    std::fs::create_dir_all(root.join("hzip-dark")).unwrap();
    std::fs::write(
        root.join("hzip-dark/index.theme"),
        "[Icon Theme]\nName=hzip-dark\nInherits=hicolor\n",
    )
    .unwrap();
    write_png(&root.join("hicolor/32x32/mime/application-zip.png"), 32);

    let icon = lookup_in(&[root.to_path_buf()], &file_req("zip"))
        .expect("Inherits=hicolor 应命中父主题里的图标");
    assert_eq!((icon.width, icon.height), (32, 32));
}

/// 目录种类 → 固定 MIME `inode-directory`（不是扩展名路径）。
#[test]
fn directory_kind_uses_inode_directory() {
    let tmp = TempDir::new("dir");
    let root = tmp.path();
    write_png(&root.join("hicolor/32x32/mime/inode-directory.png"), 32);

    let req = IconRequest {
        ext: None,
        kind: IconKind::Dir,
    };
    let icon = lookup_in(&[root.to_path_buf()], &req).expect("目录应查 inode-directory");
    assert_eq!((icon.width, icon.height), (32, 32));
}

/// 未知扩展名 / 只有 SVG / 空搜索根 / 空根目录 ⇒ 一律 `None`，且不 panic。
#[test]
fn unknowns_are_none_without_panic() {
    let tmp = TempDir::new("none");
    let base = tmp.path();

    // 1) 扩展名不在 MIME 表里：连盘都不查。
    assert!(lookup_in(&[base.to_path_buf()], &file_req("nope")).is_none());

    // 2) 表里有该扩展名，但主题只提供 SVG：本后端不解码 SVG ⇒ None。
    let svg_root = base.join("svg-only");
    std::fs::create_dir_all(svg_root.join("hicolor/32x32/mime")).unwrap();
    std::fs::write(
        svg_root.join("hicolor/32x32/mime/application-zip.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .unwrap();
    assert!(lookup_in(&[svg_root], &file_req("zip")).is_none());

    // 3) 空搜索根列表。
    assert!(lookup_in(&[], &file_req("zip")).is_none());

    // 4) 搜索根存在但里面什么都没有。
    let empty_root = base.join("empty");
    std::fs::create_dir_all(&empty_root).unwrap();
    assert!(lookup_in(&[empty_root], &file_req("zip")).is_none());

    // 5) 扩展名为 None（无扩展名）⇒ 不做 MIME 猜测。
    assert!(
        lookup_in(
            &[base.to_path_buf()],
            &IconRequest {
                ext: None,
                kind: IconKind::File,
            }
        )
        .is_none()
    );
}

/// 兼查 `mimetypes/`（freedesktop 规范与真实主题的实际目录名）。
#[test]
fn mimetypes_context_dir_is_also_searched() {
    let tmp = TempDir::new("mimetypes");
    let root = tmp.path();
    write_png(
        &root.join("hicolor/32x32/mimetypes/application-zip.png"),
        32,
    );

    let icon = lookup_in(&[root.to_path_buf()], &file_req("zip")).expect("应命中 mimetypes/");
    assert_eq!((icon.width, icon.height), (32, 32));
}

/// 两级兜底：平铺档位 `<size>/<name>.png`、主题根 `<theme>/mime/<name>.png`。
#[test]
fn flat_size_dir_and_theme_root_are_fallbacks() {
    let tmp = TempDir::new("fallback");
    let base = tmp.path();

    let flat = base.join("flat");
    write_png(&flat.join("hicolor/32x32/application-zip.png"), 32);
    assert!(
        lookup_in(&[flat], &file_req("zip")).is_some(),
        "平铺档位应作为一级兜底"
    );

    let theme_root = base.join("theme-root");
    write_png(&theme_root.join("hicolor/mime/application-zip.png"), 32);
    assert!(
        lookup_in(&[theme_root], &file_req("zip")).is_some(),
        "主题根 mime/ 应作为二级兜底"
    );
}

// ---------------------------------------------------------------- 工具

/// 极简临时目录（不引第三方测试依赖；与 `tests/parity.rs` 同款）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hzip-icon-linux-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
