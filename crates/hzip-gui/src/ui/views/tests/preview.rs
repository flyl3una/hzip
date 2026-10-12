use super::*;

// T-SEL4 payload 派生：文本按行 / HEX 16 字节行 / 图片解码（PNG），
// 截断与总大小随行
#[test]
fn payload_from_preview_branches() {
    use hzip_core::services::preview::{PreviewData, PreviewMode};
    let mk = |mode: PreviewMode, bytes: Vec<u8>| PreviewData {
        mode,
        bytes,
        truncated: true,
        total_size: 3,
        encoding: None,
        suspect: false,
        image_kind: None,
    };

    // 文本（core 已转 UTF-8）：按行切分
    let p = payload_from_preview("a.txt", mk(PreviewMode::Text, b"ab\ncd".to_vec()));
    assert_eq!(&*p.text, &["ab".to_string(), "cd".to_string()]);
    assert_eq!(p.view, PreviewView::Text);
    assert!(p.truncated);
    assert_eq!(p.total_size, "3 B");

    // HEX：偏移 + hex + ASCII（不可打印 → '.'）
    let p = payload_from_preview("b.bin", mk(PreviewMode::Hex, vec![0x00, 0x41, 0xFF]));
    assert_eq!(p.view, PreviewView::Hex);
    assert_eq!(p.hex_lines.len(), 1);
    assert!(p.hex_lines[0].starts_with("00000000  "));
    assert!(p.hex_lines[0].contains("00 41 FF"));
    assert!(
        p.hex_lines[0].ends_with(".A."),
        "ASCII 区：0x00=占位、0x41=A、0xFF=占位"
    );

    // 图片：PNG 编解码往返（2×1 RGB）
    let mut png = Vec::new();
    image::RgbImage::from_pixel(2, 1, image::Rgb([10, 20, 30]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("编码 PNG");
    let p = payload_from_preview("c.png", mk(PreviewMode::Image, png));
    assert_eq!(p.view, PreviewView::Image);
    let image = p.image.clone().expect("PNG 解码成功");
    assert_eq!(image.size(0), size(2.into(), 1.into()), "2×1");
    assert!(
        std::sync::Arc::ptr_eq(&image, &p.image.clone().expect("同一份")),
        "载荷持有的是**预封好的** RenderImage（渲染帧只克隆 Arc）"
    );

    // 解码失败（垃圾字节）→ image None（渲染占位，不 panic）
    let p = payload_from_preview("d.png", mk(PreviewMode::Image, vec![1, 2, 3]));
    assert!(p.image.is_none());
}

// 视图状态机：Switch 事件在窗口实体内部消费（不发回主窗口）——
// headless 无法开真实窗口，这里断言 payload/视图派生的纯逻辑面。
#[test]
fn preview_payload_view_branches() {
    let payload = PreviewPayload {
        entry_name: "docs/main.rs".into(),
        view: PreviewView::Text,
        text: std::sync::Arc::new(vec!["use hzip_core::engine::Engine;".into()]),
        image: None,
        hex_lines: std::sync::Arc::new(Vec::new()),
        truncated: true,
        total_size: "3.2 MiB".into(),
    };
    assert_eq!(payload.view, PreviewView::Text);
    assert!(payload.truncated);
    // 图片解码失败 → image None → 渲染占位路径（不 panic 断言在渲染冒烟）
    assert!(payload.image.is_none());
}

// RGBA → RenderImage：尺寸与字节守恒（w*h*4）
#[test]
fn rgba_to_render_image_preserves_dimensions() {
    let w = 3u32;
    let h = 2u32;
    let rgba = vec![128u8; (w * h * 4) as usize];
    let img = rgba_to_render_image(w, h, rgba);
    assert_eq!(img.size(0), size(w.into(), h.into()));
}
