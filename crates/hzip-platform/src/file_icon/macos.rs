//! macOS 后端：`NSWorkspace.iconForContentType:` → `NSBitmapImageRep` → 预乘 BGRA。
//!
//! # 路线（与另两个后端的关系）
//!
//! Windows 向 shell 要图标（`SHGetFileInfoW`）、Linux 查 XDG 主题文件；macOS 的
//! 等价入口是 AppKit：**扩展名 → `UTType` → `-[NSWorkspace iconForContentType:]`
//! → `NSImage`**，再画进 32bpp `NSBitmapImageRep` 取像素。入参同样只有扩展名与
//! 种类 ⇒ 压缩包**内**条目与真实文件走同一条路，不触碰文件系统（见 [`mod.rs`] 硬约定 1）。
//!
//! # 与资源管理器**逐类型一致**（2026-10-12 本机 macOS 15 实测）
//!
//! 对 `zip/txt/png/mp3/exe/7z/tar/无扩展名/目录` 九档，本路线的位图与
//! `-[NSWorkspace iconForFile:]`（= Finder 显示的那张）**逐字节相同**：
//!
//! | 名称 | UTI | 与 Finder 位图 |
//! |---|---|---|
//! | `a.zip` | `public.zip-archive` | 相同 |
//! | `b.txt` | `public.plain-text` | 相同 |
//! | `c.png` | `public.png` | 相同 |
//! | `d.mp3` | `public.mp3` | 相同 |
//! | `e.exe` | `com.microsoft.windows-executable` | 相同 |
//! | `f.7z` | `org.7-zip.7-zip-archive` | 相同 |
//! | `g.tar` | `public.tar-archive` | 相同 |
//! | `h.noext` | `public.data`（兜底） | 相同 |
//! | `adir/` | `public.folder` | 相同 |
//!
//! # 已知取舍（明确、可见的降级，不是静默失败）
//!
//! - **软链接**：Finder 画的是「目标图标 + `AliasBadgeIcon` 角标」。本后端只按
//!   扩展名取**目标类型图标**（无角标）——实测与 Finder 仅差角标那 49 个像素，
//!   而「通用软链接图标」要差 288–869 像素。**不合成角标**：那需要读
//!   `CoreTypes.bundle` 资源并把角标按 AppKit 的内部几何叠上去，等于把
//!   `iconForFile:` 重写一遍——超出「按类型取图」的范围，且得触碰文件系统。
//! - **速度**：AppKit 取图可能加载归档类 UTI 的图标提供方，首次调用可达数十
//!   毫秒 ⇒ 调用方**必须**在后台线程调用并缓存（与另两个后端同一约定）。
//! - **失败即 `None`**：`UTType` 解析不到、位图分配失败、像素指针为空一律回退
//!   自有图标；本模块绝不 panic、绝不返回空白占位。
//!
//! # 线程与内存
//!
//! 涉及类（`NSWorkspace`/`NSImage`/`NSBitmapImageRep`/`NSGraphicsContext`）在
//! objc2 里都**不是** `MainThreadOnly` ⇒ 可在调用方的后台线程取图（本机
//! 8 线程 × 50 次 × 4 键压力：1600/1600 全部成功且同键字节一致）。
//!
//! `iconForContentType:` 返回**自动释放**对象，且调用方（GUI 的
//! `gui-file-icon` 探测线程）是进程级常驻线程 ⇒ 每次取图包一层
//! `autoreleasepool`，否则自动释放对象要攒到线程结束才回收。
//!
//! # 类型名（[`SystemIcon::type_name`]）与「不中英混排」
//!
//! `UTType.localizedDescription` 的**语言由进程 bundle 的本地化集合决定**，
//! 不是由系统语言决定：实测同一台 zh_CN 机器，bundle 未声明本地化时得到
//! `"Zip archive"`（英文），声明 `CFBundleLocalizations`/`CFBundleDevelopmentRegion`
//! 后得到 `"Zip归档"`。若无条件采用，中文界面上会出现英文类型名——正是
//! `hzip-gui::ui::widgets::file_icon` 文档里要避免的「中英混排」。
//!
//! 故本后端只在**bundle 真的声明了与系统语言同主标签的本地化**时才给类型名
//! （见 [`localization_matches_system`]）；否则 `None`，由调用方回退
//! `row.type_text`（仓库自有本地化文案）。发行包因此需要声明本地化——见
//! `build/package/templates/macos/Info.plist.in`。

use objc2::AnyThread as _;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{
    NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace, NSGraphicsContext, NSImage,
    NSWorkspace,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSBundle, NSString};
use objc2_uniform_type_identifiers::{UTType, UTTypeData, UTTypeFolder, UTTypeSymbolicLink};

use super::{ICON_PX, IconKind, IconRequest, SystemIcon};

/// 查询系统图标；拿不到 → `None`（调用方回退自有图标，本函数绝不 panic）。
pub(super) fn lookup(req: &IconRequest<'_>) -> Option<SystemIcon> {
    // 自动释放池：本函数在常驻探测线程上反复调用（见模块文档「线程与内存」），
    // 而 `iconForContentType:` 返回自动释放对象。
    autoreleasepool(|_pool| {
        let ut = content_type(req)?;
        let image = NSWorkspace::sharedWorkspace().iconForContentType(&ut);
        let premultiplied_bgra = render_bgra(&image)?;
        Some(SystemIcon {
            width: ICON_PX,
            height: ICON_PX,
            premultiplied_bgra,
            type_name: if localization_matches_system() {
                type_name_of(&ut)
            } else {
                None
            },
        })
    })
}

/// 请求 → `UTType`（**借用**静态量或本函数内的临时 `Retained`）。
///
/// 目录/软链接走系统静态 UTI（`public.folder` / `public.symlink`），其余按扩展名
/// 解析；无扩展名与解析失败都退回 `public.data`（= 系统默认文件图标，与 Finder
/// 对无扩展名文件的显示一致）。
///
/// 返回的 `Retained<UTType>` 需要活到取图结束，故统一以 `Option<Retained<UTType>>`
/// 返回：静态量走 `Retained::retain`（真持有，代价可忽略），临时量直接转移所有权。
fn content_type(req: &IconRequest<'_>) -> Option<Retained<UTType>> {
    // `UTTypeFolder`（而非 `UTTypeDirectory`）：实测前者才是 Finder 画的文件夹图标
    // 对应的 UTI（后者返回的是通用目录图标）。
    match req.kind {
        IconKind::Dir => retain_static(unsafe { UTTypeFolder }),
        IconKind::Symlink => retain_static(unsafe { UTTypeSymbolicLink }),
        IconKind::Container | IconKind::File => match req.ext {
            Some(ext) => {
                let ext = NSString::from_str(ext);
                // 解析不到该扩展名（未知格式）→ 退回 `public.data` 兜底。
                UTType::typeWithFilenameExtension(&ext)
                    .or_else(|| retain_static(unsafe { UTTypeData }))
            }
            None => retain_static(unsafe { UTTypeData }),
        },
    }
}

/// 静态 `&'static UTType` → 持有所有权的 `Retained`（`Retained::retain` 真持有）。
///
/// 用 `retain_static` 而不是把借用与所有权混在一个枚举里：调用方只需一种类型。
/// 静态 UTI 恒非空，故 `None` 分支实际不可达；这里仍返回 `Option` 而不是
/// `unreachable!()`——本模块的契约是**绝不 panic**，取不到就当缺图标处理。
fn retain_static(ut: &'static UTType) -> Option<Retained<UTType>> {
    // SAFETY：静态量在进程生命周期内恒有效，`retain` 只是把引用计数 +1。
    unsafe { Retained::retain(std::ptr::from_ref(ut).cast_mut()) }
}

/// `NSImage` → 32×32 预乘 BGRA。
///
/// 画进 32bpp `NSBitmapImageRep`（`deviceRGB`、`hasAlpha`）：该构造在 macOS 上
/// 产出**预乘、alpha 在后**的 RGBA（2026-10-12 实测：`R=0.8,G=0.4,B=0.2,A=0.5`
/// → `[102,51,26,128]`，即各通道 ≤ alpha），故只需换成 BGRA 即为
/// [`SystemIcon`] 约定的格式。
///
/// 失败点（位图分配、图形上下文、像素指针）全部排在 `saveGraphicsState` **之前**
/// 或 `restoreGraphicsState` **之后** ⇒ 图形状态栈不会失衡；`draw` 段内无提前返回。
fn render_bgra(image: &NSImage) -> Option<Vec<u8>> {
    let size = ICON_PX as isize;

    // SAFETY：`initWithBitmapDataPlanes` 的 `planes = null` 表示「由 AppKit 自行
    // 分配像素」，与 bytesPerRow/bitsPerPixel 一致；`NSDeviceRGBColorSpace` 是
    // 系统静态量；返回 `Option`（分配失败即 `None`）。
    let rep: Retained<NSBitmapImageRep> = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            size,
            size,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            size * 4,
            32,
        )?
    };
    // SAFETY：上下文构造不依赖当前图形状态，先建好再入栈。
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;

    // SAFETY：save/restore 成对；`ctx` 与 `image` 在此期间存活；`drawInRect` 的
    // 目标矩形为 32×32 pt（与位图像素 1:1），`from_rect` 取 `CGRect::default()`
    // （= 全图），`SourceOver`/`1.0` 即「原样绘制」。
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    image.drawInRect_fromRect_operation_fraction(
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(f64::from(ICON_PX), f64::from(ICON_PX)),
        ),
        CGRect::default(),
        NSCompositingOperation::SourceOver,
        1.0,
    );
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();

    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    let len = (ICON_PX * ICON_PX * 4) as usize;
    // SAFETY：位图由本函数独占，`bitmapData` 指向 `ICON_PX² × 4` 字节的可读缓冲
    // （构造时已按该尺寸分配，见 `bytesPerRow = size * 4`）。
    let mut pixels = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
    rgba_to_bgra_in_place(&mut pixels);
    Some(pixels)
}

/// 预乘 RGBA → 预乘 BGRA（**原地**换 R/B；alpha 位不动）。
///
/// 预乘不变式（各通道 ≤ alpha）在换通道后自动保持。
fn rgba_to_bgra_in_place(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

/// `UTType` 的本地化类型名（`None` = 系统没给/为空）。
fn type_name_of(ut: &UTType) -> Option<String> {
    // SAFETY：`localizedDescription` 返回自动释放对象，池由 `lookup` 提供。
    let desc = ut.localizedDescription()?;
    let text = desc.to_string();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// 本进程 bundle 是否声明了**与系统语言同主标签**的本地化。
///
/// 依据见模块文档「类型名」：`localizedDescription` 的语言跟着 bundle 的本地化
/// 集合走。裸二进制（`cargo run`）与未声明本地化的发行包都得不到本地化结果，
/// 此时返回 `false`，调用方回退自有文案，不出现中英混排。
///
/// 系统语言探测不到（`None`）也返回 `false`——没有依据时不猜。
fn localization_matches_system() -> bool {
    let Some(system) = crate::lang::system_language() else {
        return false;
    };
    let Some(primary) = system.split(['-', '_']).next() else {
        return false;
    };
    if primary.is_empty() {
        return false;
    }
    let bundle = NSBundle::mainBundle();
    let localizations = bundle.localizations();
    localizations
        .iter()
        .any(|tag| primary_tag_matches(&tag.to_string(), primary))
}

/// `tag` 的主语言标签是否等于 `primary`（大小写不敏感；`zh-Hans`/`zh_CN` → `zh`）。
///
/// `primary` 为空一律不匹配（空主标签不是有效语言码，`"-x"` 拆出的空段不得
/// 与空 `primary` 互相命中）。
fn primary_tag_matches(tag: &str, primary: &str) -> bool {
    if primary.is_empty() {
        return false;
    }
    tag.split(['-', '_'])
        .next()
        .is_some_and(|p| p.eq_ignore_ascii_case(primary))
}

#[cfg(test)]
#[path = "tests/macos.rs"]
mod tests;
