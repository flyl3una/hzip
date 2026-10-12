//! 图标 dump 工具：对固定键表逐一取系统图标，写 32bpp BMP 并打印指纹/预览。
//!
//! 用途：T-ICON 的**目视验收**——把 `.bmp` 与资源管理器/A1 期望值逐个对照，
//! ASCII 预览进 stdout 留档（CI 日志可读）。
//!
//! 零新增依赖：BMP 头按规范手写（14 字节 `BITMAPFILEHEADER` + 40 字节
//! `BITMAPINFOHEADER`），不引 `image`。
//!
//! # 为什么先在浅底上合成
//!
//! 系统图标是**预乘 BGRA**，透明区 = 全 0。若直接写进 BMP 的 `BI_RGB` 通道，多数
//! 看图器/照片应用把 alpha 忽略或当全透明（显示黑底），会误导目视验收。故先在
//! `#F0F0F0` 浅底上合成（`dst = premul + bg*(1-a)`）再输出；合成后 alpha 恒 255。
//!
//! 用法：`cargo run -p hzip-platform --example icon_dump -- out/icon-dump`
#![allow(clippy::print_stdout)]

use hzip_platform::file_icon::{IconKind, IconRequest, SystemIcon, lookup};

/// 亮度→字符映射（`@` 最亮）。
const PALETTE: &[u8] = b" .:-=+*#%@";

/// 合成用浅底（灰 #F0F0F0）。
const BG: u8 = 0xF0;

/// 一个 dump 键：`key` 决定文件名；`ext`/`kind` 即查询入参。
struct Case {
    key: &'static str,
    ext: Option<&'static str>,
    kind: IconKind,
}

/// 覆盖目录 / 常见容器 / 文档 / 可执行 / 媒体 / 无扩展名 / 未知扩展名 / 符号链接。
const CASES: &[Case] = &[
    Case {
        key: "dir",
        ext: None,
        kind: IconKind::Dir,
    },
    Case {
        key: "zip",
        ext: Some("zip"),
        kind: IconKind::File,
    },
    Case {
        key: "7z",
        ext: Some("7z"),
        kind: IconKind::File,
    },
    Case {
        key: "rar",
        ext: Some("rar"),
        kind: IconKind::File,
    },
    Case {
        key: "pdf",
        ext: Some("pdf"),
        kind: IconKind::File,
    },
    Case {
        key: "png",
        ext: Some("png"),
        kind: IconKind::File,
    },
    Case {
        key: "txt",
        ext: Some("txt"),
        kind: IconKind::File,
    },
    Case {
        key: "exe",
        ext: Some("exe"),
        kind: IconKind::File,
    },
    Case {
        key: "mp3",
        ext: Some("mp3"),
        kind: IconKind::File,
    },
    Case {
        key: "noext",
        ext: None,
        kind: IconKind::File,
    },
    Case {
        key: "unknown-ext",
        ext: Some("zzz9"),
        kind: IconKind::File,
    },
    Case {
        key: "symlink-txt",
        ext: Some("txt"),
        kind: IconKind::Symlink,
    },
];

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "out/icon-dump".to_owned());
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("创建输出目录 {out_dir} 失败: {e}");
        std::process::exit(1);
    }

    println!("icon_dump → {out_dir}");
    for case in CASES {
        let req = IconRequest {
            ext: case.ext,
            kind: case.kind,
        };
        let Some(icon) = lookup(&req) else {
            println!("[{}] <无系统图标>", case.key);
            continue;
        };

        // 指纹取**原始预乘像素**（与测试断言的数据源一致）。
        let fp = fnv1a64(&icon.premultiplied_bgra);
        // 输出/预览用浅底合成后的不透明像素。
        let composited = composite_over_light_bg(&icon);

        let path = format!("{out_dir}/{}.bmp", case.key);
        if let Err(e) = write_bmp(&path, icon.width, icon.height, &composited) {
            eprintln!("写 {} 失败: {e}", path);
            std::process::exit(1);
        }

        let ty = match &icon.type_name {
            Some(t) => format!("{t:?}"),
            None => "None".to_owned(),
        };
        println!(
            "[{}] {}x{} {} bytes fnv1a64=0x{fp:016x} type={ty}",
            case.key,
            icon.width,
            icon.height,
            icon.premultiplied_bgra.len(),
        );
        print_ascii_preview(icon.width, icon.height, &composited);
    }
}

/// 预乘 BGRA → 浅底合成后的不透明 BGRA（alpha 恒 255）。
fn composite_over_light_bg(icon: &SystemIcon) -> Vec<u8> {
    let px = &icon.premultiplied_bgra;
    let mut out = Vec::with_capacity(px.len());
    for c in px.chunks_exact(4) {
        let a = u32::from(c[3]);
        let bg_term = u32::from(BG) * (255 - a);
        for &ch in &c[..3] {
            out.push((u32::from(ch) + (bg_term + 127) / 255).min(255) as u8);
        }
        out.push(255);
    }
    out
}

/// 写 32bpp BMP：`BITMAPFILEHEADER`(14) + `BITMAPINFOHEADER`(40)，行自下而上。
fn write_bmp(path: &str, w: u32, h: u32, bgra: &[u8]) -> std::io::Result<()> {
    let invalid = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_owned());

    let expected = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| invalid("位图尺寸溢出"))?;
    if bgra.len() != expected {
        return Err(invalid(&format!(
            "位图字节数 {} ≠ {w}*{h}*4={expected}",
            bgra.len()
        )));
    }

    let width = i32::try_from(w).map_err(|_| invalid("宽度超出 i32"))?;
    let height = i32::try_from(h).map_err(|_| invalid("高度超出 i32"))?;
    let data_len = u32::try_from(expected).map_err(|_| invalid("位图字节数超出 u32"))?;
    let file_size = 54u32
        .checked_add(data_len)
        .ok_or_else(|| invalid("BMP 文件大小溢出 u32"))?;

    let mut out = Vec::with_capacity(54 + expected);
    // BITMAPFILEHEADER
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // bfReserved1
    out.extend_from_slice(&0u16.to_le_bytes()); // bfReserved2
    out.extend_from_slice(&54u32.to_le_bytes()); // bfOffBits
    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&width.to_le_bytes()); // biWidth
    out.extend_from_slice(&height.to_le_bytes()); // biHeight > 0 ⇒ 自下而上
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&data_len.to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // 源是顶朝下 BGRA；BMP 需自下而上 ⇒ 按行倒序写（32bpp 无需行内补齐）。
    let row = (w as usize) * 4;
    for y in (0..h as usize).rev() {
        out.extend_from_slice(&bgra[y * row..(y + 1) * row]);
    }

    std::fs::write(path, out)
}

/// FNV-1a 64 位指纹（用于报告里跨机核对位图是否一致）。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 16×16 ASCII 预览：按块平均亮度映射到 [`PALETTE`]（输入为浅底合成后的不透明像素，
/// 故无「全透明」概念）。
fn print_ascii_preview(w: u32, h: u32, bgra: &[u8]) {
    let w = w as usize;
    let h = h as usize;
    let block_w = (w / 16).max(1);
    let block_h = (h / 16).max(1);

    for cy in 0..16 {
        let mut line = String::with_capacity(18);
        line.push_str("  ");
        for cx in 0..16 {
            let mut sum_l: u64 = 0;
            let mut n: u64 = 0;
            for yy in cy * block_h..((cy + 1) * block_h).min(h) {
                for xx in cx * block_w..((cx + 1) * block_w).min(w) {
                    let i = (yy * w + xx) * 4;
                    let b = u64::from(bgra[i]);
                    let g = u64::from(bgra[i + 1]);
                    let r = u64::from(bgra[i + 2]);
                    sum_l += (b + g + r) / 3;
                    n += 1;
                }
            }
            // n 为块内像素数（≥1）；`checked_div` 让 clippy 满意且免 panic。
            let lum = sum_l.checked_div(n).unwrap_or(0) as usize;
            let ch = if n == 0 {
                ' '
            } else {
                let idx = (lum * (PALETTE.len() - 1) / 255).min(PALETTE.len() - 1);
                PALETTE[idx] as char
            };
            line.push(ch);
        }
        println!("{line}");
    }
}
