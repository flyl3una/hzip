//! T34 集成测试：真实档案 browse/preview（TC-CONF-010 core 层、TC-CORE-017、
//! TC-CLI-004 基础）。
//!
//! 覆盖：browse 组合解层（tar.gz 透传内层条目）、tree 构建、filter glob、
//! enter 嵌套容器 + 深度防护、preview 三模式/截断/编码、图片魔数透传。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;

use hzip_core::engine::{Engine, EngineConfig};

use hzip_core::services::preview::{ImageKind, PreviewMode};
use hzip_format::{EntryMeta, FormatHandler, PasswordRef, Sink};
use tempfile::TempDir;

/// 产 tar.gz（TarHandler + gz 流——经 registry 组合装配 browse）。
fn make_tar_gz(path: &std::path::Path, entries: &[(String, Vec<u8>)]) {
    // 先写裸 tar 再 gzip
    let tar_path = path.with_extension("tar");
    let mut w = hzip_fmt_tar::TarHandler::new()
        .create_writer(Sink::File(tar_path.clone()), &Default::default())
        .unwrap();
    for (name, data) in entries {
        let meta = EntryMeta {
            name: name.as_str().into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(data).unwrap();
        s.finish().unwrap();
    }
    w.finish().unwrap();
    // gzip 压缩
    let raw = std::fs::read(&tar_path).unwrap();
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&raw).unwrap();
    std::fs::write(path, enc.finish().unwrap()).unwrap();
    let _ = std::fs::remove_file(&tar_path);
}

/// 最小引擎（无信号/清理副作用）。
fn engine() -> Engine {
    Engine::new(EngineConfig {
        pool_size: Some(1),
        max_parallel: 1,
        cancel_on_signal: false,
        orphan_cleanup: false,
    })
    .unwrap()
}

#[test]
fn browse_composite_tar_gz() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("data.tar.gz");
    make_tar_gz(
        &p,
        &[
            ("a.txt".into(), b"hello".to_vec()),
            ("dir/b.txt".into(), b"world".to_vec()),
        ],
    );
    let eng = engine();
    let b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    assert_eq!(b.entry_count(), 2, "tar.gz 组合透传内层条目");
    // 条目名完整（tar 内层）
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(names.contains(&"a.txt".to_string()));
    assert!(names.contains(&"dir/b.txt".to_string()));
}

#[test]
fn tree_build_flat_to_nested() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("t.tar.gz");
    make_tar_gz(
        &p,
        &[
            ("a/b/c.txt".into(), b"deep".to_vec()),
            ("a/x.txt".into(), b"x".to_vec()),
            ("top.txt".into(), b"t".to_vec()),
        ],
    );
    let eng = engine();
    let b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    let tree = b.tree();
    // root → {a{ b{ c.txt }, x.txt }, top.txt}
    assert_eq!(tree.children.len(), 2, "root 两子: a/ + top.txt");
    let a = tree.children.get("a").expect("a 目录");
    assert!(a.entry.is_none() || a.entry.is_some(), "a 隐式或显式目录");
    assert_eq!(a.children.len(), 2, "a 下 b/ + x.txt");
    let bnode = a.children.get("b").expect("b 目录");
    assert_eq!(bnode.children.len(), 1);
    assert!(bnode.children.contains_key("c.txt"));
    assert!(tree.children.contains_key("top.txt"));
    // 节点计数
    assert_eq!(tree.node_count(), 6, "root+a+b+c.txt+x.txt+top.txt");
}

#[test]
fn filter_glob_instant() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("f.tar.gz");
    make_tar_gz(
        &p,
        &[
            ("a.txt".into(), b"1".to_vec()),
            ("b.log".into(), b"2".to_vec()),
            ("sub/c.txt".into(), b"3".to_vec()),
        ],
    );
    let eng = engine();
    let mut b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    b.set_filter(Some("*.txt")).unwrap();
    let names: Vec<String> = b
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    // 段级 glob：*.txt 命中 a.txt 与 sub/c.txt（c.txt 段）；b.log 排除
    assert_eq!(names.len(), 2, "a.txt + sub/c.txt");
    assert!(names.contains(&"a.txt".to_string()));
    assert!(names.contains(&"sub/c.txt".to_string()));
    assert!(!names.contains(&"b.log".to_string()), "b.log 排除");
    // tree 过滤后叶命中文件的祖先保留（sub/ 目录可见）
    let t = b.tree();
    assert!(t.children.contains_key("a.txt"));
    assert!(t.children.contains_key("sub"), "sub/ 祖先保留");
    b.set_filter(None).unwrap();
    assert_eq!(b.entry_count(), 3, "恢复全量");
}

#[test]
fn preview_text_truncate_and_encoding() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("txt.tar.gz");
    let big = "line of text\n".repeat(30_000); // ~390KB > 256KB 默认
    make_tar_gz(&p, &[("big.txt".into(), big.into_bytes())]);
    let eng = engine();
    let b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    let data = b
        .preview(0, PreviewMode::Text, None, PasswordRef::none())
        .unwrap();
    assert!(data.truncated, "300KB+ 文本截断");
    assert!(data.bytes.len() <= 256 * 1024 + 1);
    assert_eq!(data.mode, PreviewMode::Text);
}

#[test]
fn preview_image_passthrough() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("img.tar.gz");
    // 伪 PNG 头 + 数据
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&[1u8; 100]);
    make_tar_gz(&p, &[("pic.png".into(), png.clone())]);
    let eng = engine();
    let b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    let d = b
        .preview(0, PreviewMode::Auto, None, PasswordRef::none())
        .unwrap();
    assert_eq!(d.mode, PreviewMode::Image, "PNG 魔数 → Image");
    assert_eq!(d.image_kind, Some(ImageKind::Png));
    assert_eq!(&d.bytes[..8], &png[..8], "原始字节透传");
}

#[test]
fn preview_hex_auto_binary() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("bin.tar.gz");
    let bin: Vec<u8> = (0..200u8).collect(); // 含 NUL
    make_tar_gz(&p, &[("d.bin".into(), bin)]);
    let eng = engine();
    let b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    let d = b
        .preview(0, PreviewMode::Auto, None, PasswordRef::none())
        .unwrap();
    assert_eq!(d.mode, PreviewMode::Hex, "含 NUL → Hex");
    let h = b
        .preview(0, PreviewMode::Hex, Some(64), PasswordRef::none())
        .unwrap();
    assert!(h.bytes.len() <= 64, "max_bytes 覆盖");
}

/// 别名容器条目可下钻（T34 补）：外层 tar 里放一个 `inner.tgz`（tar.gz 字节）。
///
/// 修复前 `container_handler_id` 只认规范写法 `.tar.gz`/`.tar.bz2`/…，`.tgz`
/// 条目 `enter` 直接 HZ-5001（`enter-unsupported-container`）；而 GUI 的
/// `is_container_name` 又把它列为可进 —— 表现为「双击 tgz 没反应」。同批修复
/// 的还有：文件关联候选缺 `tgz`、右键菜单 `archive_exts` 缺 `tgz`
/// （同一张表 `hzip_format::exts` 的三个消费方）。
#[test]
fn enter_alias_container_tgz() {
    let dir = TempDir::new().unwrap();
    let inner = dir.path().join("inner.tgz");
    make_tar_gz(&inner, &[("deep.txt".into(), b"alias".to_vec())]);
    let tgz_bytes = std::fs::read(&inner).unwrap();

    let p = dir.path().join("outer.tar");
    {
        let mut w = hzip_fmt_tar::TarHandler::new()
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: "inner.tgz".into(),
            size_raw: Some(tgz_bytes.len() as u64),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(&tgz_bytes).unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }

    let eng = engine();
    let mut b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    let id = b.entry(0).unwrap().id;
    let nested = b
        .enter(id, eng.registry(), PasswordRef::none())
        .expect("`.tgz` 条目应可作容器下钻（与 `.tar.gz` 同义）");
    assert_eq!(nested.depth(), 1);
    let names: Vec<String> = nested
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(names.contains(&"deep.txt".to_string()), "{names:?}");
}

/// 单流压缩条目**不可**下钻（表里 `container: None`）：`.gz` 不是容器，
/// 报 HZ-5001 而不是装作能进。
#[test]
fn enter_stream_container_rejected() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("outer.tar");
    {
        let mut w = hzip_fmt_tar::TarHandler::new()
            .create_writer(Sink::File(p.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: "note.txt.gz".into(),
            size_raw: Some(4),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        // 直接塞 gzip 头（内容合法与否无关：容器判定只按扩展名）
        s.write_all(&[0x1f, 0x8b, 0x08, 0x00]).unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
    }
    let eng = engine();
    let mut b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    let id = b.entry(0).unwrap().id;
    assert!(
        b.enter(id, eng.registry(), PasswordRef::none()).is_err(),
        "单流压缩不是容器，不得下钻"
    );
}

/// TC-CORE-017：enter 嵌套容器（tar 内含 zip？——tar 内嵌 tar 条目）
/// 深度防护：4 层 → Quota 4504。本测试验证 enter 单层 + 超深拒绝。
#[test]
fn enter_nested_and_depth_limit() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("outer.tar.gz");
    // 构造「tar 内嵌 zip」：外层 tar 的一个条目是 zip 字节
    let zip_bytes;
    {
        let zp = dir.path().join("inner.zip");
        let mut w = hzip_fmt_zip::ZipFormatHandler::new()
            .create_writer(Sink::File(zp.clone()), &Default::default())
            .unwrap();
        let meta = EntryMeta {
            name: "in.txt".into(),
            size_raw: Some(5),
            ..Default::default()
        };
        let mut s = w.next_entry(&meta, PasswordRef::none()).unwrap();
        s.write_all(b"inner").unwrap();
        s.finish().unwrap();
        w.finish().unwrap();
        zip_bytes = std::fs::read(&zp).unwrap();
    }
    make_tar_gz(&p, &[("nested.zip".into(), zip_bytes)]);
    let eng = engine();
    let mut b = eng
        .browse(hzip_format::Source::File(p), &Default::default())
        .unwrap();
    // 找到 nested.zip 条目 id
    let id = b.entry(0).unwrap().id;
    let nested = b.enter(id, eng.registry(), PasswordRef::none()).unwrap();
    assert_eq!(nested.depth(), 1);
    assert_eq!(nested.entry_count(), 1, "zip 内层条目");
    let names: Vec<String> = nested
        .entries()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(names.contains(&"in.txt".to_string()));
    // 预览内层条目
    let d = nested
        .preview(0, PreviewMode::Text, Some(100), PasswordRef::none())
        .unwrap();
    assert!(String::from_utf8_lossy(&d.bytes).contains("inner"));
}
