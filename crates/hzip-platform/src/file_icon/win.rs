//! Windows 后端：`SHGetFileInfoW` + HICON → 预乘 BGRA。
//!
//! 既定实现路线（先例核对结论，实现时不得偏离）：
//! 1. `SHGetFileInfoW(wide(".ext"), attrib, &mut sfi, size_of::<SHFILEINFOW>(),
//!    SHGFI_ICON | SHGFI_USEFILEATTRIBUTES | SHGFI_LARGEICON [| SHGFI_LINKOVERLAY]
//!    [| SHGFI_TYPENAME])`——`attrib`：目录 = `FILE_ATTRIBUTE_DIRECTORY`，其余
//!    `FILE_ATTRIBUTE_ARCHIVE`；`Symlink` 加 `SHGFI_LINKOVERLAY`（= 资源管理器箭头）。
//! 2. `hIcon` → 32bpp **顶朝下** `CreateDIBSection` + `CreateCompatibleDC` +
//!    `SelectObject` + `DrawIconEx(0,0,w,h,hIcon,0,null,DI_NORMAL)` ⇒ 直接得到
//!    **预乘 BGRA**（比 `GetIconInfo`+`GetDIBits` 稳：免手写掩码合成）。
//! 3. `DestroyIcon` + `DeleteObject`/`DeleteDC` **无论成败都要执行**（不泄漏 GDI 句柄）。
//!
//! # 为什么走 `SHGetFileInfoW` 而不是 COM
//!
//! 两个先例（7-Zip `SysIconUtils.cpp`、Bandizip）都只用 `SHGetFileInfoW`，均未引
//! `IImageList`/`IShellItemImageFactory`。本后端入参只有扩展名 + 属性位
//! （`SHGFI_USEFILEATTRIBUTES`），故压缩包**内**条目与真实文件走同一条路，且不需要
//! 真实路径、不读文件系统。
//!
//! # 线程、COM 套间与并发（2026-10-10，Win10 19045 实测）
//!
//! **COM 套间（必需，影响图标正确性）**：shell 图标通道要求调用线程已初始化 COM。
//! 缺省套间时 `SHGetFileInfoW` 走**降级路径**，多个类型的图标退化为通用页图标：
//! 单线程 dump 实测，无 COM 时 `.png` / `.mp3` / 无扩展名三者的预乘像素**完全相同**；
//! 初始化 COM 后三者各不相同、且各类型图标与资源管理器一致。故与仓库既有 shell
//! 调用点（`open.rs`/`assoc/win.rs`）一致，调用前走 [`crate::open::ensure_com_initialized`]。
//!
//! **并发（必需串行化）**：8 线程 × 50 次 × 6 键、冷启动 Barrier 同步压力：
//!
//! | 机制 | `None` 次数 / 轮 |
//! |---|---|
//! | COM 套间 **+ 锁** | 0（4 轮全过） |
//! | COM 套间，**无锁** | 每轮稳定 7 |
//! | **无 COM**，无锁 | 每轮稳定 7 |
//!
//! ⇒ 并发下 `SHGetFileInfoW` 可能 `ok != 0` 却 `hIcon == NULL`（`GetLastError == 0`），
//! 与套间**无关**；必需机制是**串行化取图**（[`SHELL_LOCK`]）。
//!
//! # 图标由 shell 决定
//!
//! 返回的就是 shell 给的那张图（本后端不做任何类型映射/替换），因此必然与资源管理器
//! 逐类型一致；shell 拿不到某个类型的图标时也照实回退，不由本模块臆造。

use core::mem::size_of;
use core::ptr::{null_mut, write_bytes};
use core::slice;
use std::sync::Mutex;

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_DIRECTORY};
use windows_sys::Win32::UI::Shell::{
    SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_LINKOVERLAY, SHGFI_TYPENAME,
    SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx, HICON};

use super::{ICON_PX, IconKind, IconRequest, SystemIcon};

/// `SelectObject` 失败时可能返回的哨兵值（`(HANDLE)-1`），windows-sys 未导出该常量。
const HGDI_ERROR: HGDIOBJ = usize::MAX as HGDIOBJ;

/// 目录查询名：不带点 ⇒ shell 不会去找扩展名关联，只按属性位返回文件夹图标。
const DIR_QUERY_NAME: &str = "__DIR__";
/// 无扩展名文件的查询名：同样不带点，落到系统默认文件图标。
const NO_EXT_QUERY_NAME: &str = "__FILE__";

/// shell 取图的**串行化**锁：`SHGetFileInfoW` 的并发调用会失败（`ok != 0` 却
/// `hIcon == NULL`，`GetLastError == 0`），串行化后稳定。见模块文档实验表。
///
/// 作用域**只包住 shell 调用**：`draw_to_bgra` 用的是本线程自己的 GDI 句柄
/// （`CreateDIBSection`/`CreateCompatibleDC` 各自独立），不需要串行。
static SHELL_LOCK: Mutex<()> = Mutex::new(());

/// 查询系统图标；拿不到 → `None`（调用方回退自有图标，本函数绝不 panic）。
pub(super) fn lookup(req: &IconRequest<'_>) -> Option<SystemIcon> {
    let (name, attrs, extra_flags) = build_query(req);
    // `wide_str` 追加 NUL 终止符：shell 会读到结尾。
    let wide = crate::winutil::wide_str(&name);

    let mut sfi = SHFILEINFOW::default();
    let flags =
        SHGFI_ICON | SHGFI_USEFILEATTRIBUTES | SHGFI_LARGEICON | SHGFI_TYPENAME | extra_flags;

    // 套间：shell 图标通道要求调用线程已初始化 COM（与 open.rs / assoc 同款）。
    crate::open::ensure_com_initialized();

    // 不变式：`wide` 以 NUL 结尾且在本调用期间存活；`sfi` 是有效可写结构且大小与
    // `cbFileInfo` 一致；`attrs` 携带 USEFILEATTRIBUTES ⇒ 文件不存在也返回图标。
    // 并发：仅本条 shell 调用需要串行（见 [`SHELL_LOCK`] 与模块文档实验表）。
    let ok = {
        let _shell_guard = SHELL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                attrs,
                &mut sfi,
                size_of::<SHFILEINFOW>() as u32,
                flags,
            )
        }
    };
    if ok == 0 || sfi.hIcon.is_null() {
        return None;
    }

    // 自此 `hIcon` 归 guard 所有：任何提前返回都会经 Drop 调 `DestroyIcon`。
    let icon = IconGuard(sfi.hIcon);

    // GDI 绘制在锁外：只用本线程自己的句柄。
    let premultiplied_bgra = draw_to_bgra(icon.0)?;
    let type_name = decode_type_name(&sfi.szTypeName);

    Some(SystemIcon {
        width: ICON_PX,
        height: ICON_PX,
        premultiplied_bgra,
        type_name,
    })
}

/// 造查询名 + 属性位 + 额外 flags。
///
/// `ext` 已由调用方归一化（小写、无点、只取最后一段）；本函数只是拼 `.ext`，
/// 不校验内容——非法/超长扩展名会被 shell 退化为默认图标，不 panic。
fn build_query(req: &IconRequest<'_>) -> (String, u32, u32) {
    match req.kind {
        IconKind::Dir => (DIR_QUERY_NAME.to_owned(), FILE_ATTRIBUTE_DIRECTORY, 0),
        IconKind::Symlink => (
            query_name(req.ext),
            FILE_ATTRIBUTE_ARCHIVE,
            SHGFI_LINKOVERLAY,
        ),
        IconKind::Container | IconKind::File => (query_name(req.ext), FILE_ATTRIBUTE_ARCHIVE, 0),
    }
}

/// 文件查询名：有扩展名 ⇒ `".{ext}"`；无 ⇒ 固定占位名（无点 ⇒ 默认文件图标）。
fn query_name(ext: Option<&str>) -> String {
    match ext {
        Some(e) => {
            // 直接拼 `.ext`（`ext` 已归一化，无需再校验）。
            let mut s = String::with_capacity(e.len() + 1);
            s.push('.');
            s.push_str(e);
            s
        }
        None => NO_EXT_QUERY_NAME.to_owned(),
    }
}

/// `SHFILEINFOW::szTypeName`（UTF-16，NUL 结尾，空 = 无类型名）→ `String`。
fn decode_type_name(buf: &[u16; 80]) -> Option<String> {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    if end == 0 {
        return None;
    }
    let name = String::from_utf16_lossy(&buf[..end]);
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// 把 HICON 画进 32bpp **顶朝下** DIB section，返回预乘 BGRA 像素副本。
///
/// 一切 GDI 对象由 RAII guard 持有：任一失败路径都不泄漏句柄。
fn draw_to_bgra(hicon: HICON) -> Option<Vec<u8>> {
    let size = ICON_PX as i32;

    // 顶朝下（biHeight 负）⇒ 内存行序与 `SystemIcon`（左上→右下）一致。
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };

    let mut bits: *mut core::ffi::c_void = null_mut();
    // 不变式：`bmi` 在本调用期间存活且描述完整；`bits` 出参由 GDI 写回像素缓冲首址。
    // 传 `hdc = null`（DIB 与设备无关）、`hsection = null`（GDI 分配内存）。
    let bitmap =
        unsafe { CreateDIBSection(null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0) };
    if bitmap.is_null() || bits.is_null() {
        return None;
    }
    // 位图句柄归 guard：此后所有提前返回都会 DeleteObject。
    let bitmap = BitmapGuard(bitmap);

    let len = (ICON_PX * ICON_PX * 4) as usize;
    // 不变式：`bits` 是刚创建位图的有效缓冲，容量恰好 `len` 字节。
    // 必须清零：DrawIconEx 只写图标覆盖的不透明/半透明像素，透明区域否则是未初始化内容。
    unsafe { write_bytes(bits.cast::<u8>(), 0, len) };

    // 不变式：`null` 内存 DC 合法（无需窗口）；返回句柄归 guard。
    let dc = unsafe { CreateCompatibleDC(null_mut()) };
    if dc.is_null() {
        return None;
    }
    let dc = DcGuard(dc);

    // 不变式：`dc` 有效；`bitmap.0` 是有效位图句柄。返回旧默认对象（1x1 单色位图）。
    let old = unsafe { SelectObject(dc.0, bitmap.0) };
    if old.is_null() || old == HGDI_ERROR {
        return None;
    }
    // 旧对象归 guard：Drop 时选回 DC，保证后续 DeleteDC/DeleteObject 合法。
    let _restore = SelectedGuard { dc: dc.0, old };

    // 不变式：`dc` 已选中目标位图；`hicon` 是本函数持有的有效图标句柄；
    // `DI_NORMAL`（= 掩码 + 图像）触发 GDI 的 alpha 合成 ⇒ 目标得到预乘 BGRA。
    let drawn = unsafe { DrawIconEx(dc.0, 0, 0, hicon, size, size, 0, null_mut(), DI_NORMAL) };
    if drawn == 0 {
        return None;
    }

    // 不变式：位图仍被选中、guard 仍存活 ⇒ `bits` 指向 `len` 字节有效数据。
    let pixels = unsafe { slice::from_raw_parts(bits.cast::<u8>(), len) };
    Some(pixels.to_vec())
}

/// HICON 所有权守卫（shell 分配 ⇒ 必须 `DestroyIcon`）。
struct IconGuard(HICON);
impl Drop for IconGuard {
    fn drop(&mut self) {
        // 不变式：`self.0` 由 `SHGetFileInfoW` 分配且尚未销毁。
        unsafe { DestroyIcon(self.0) };
    }
}

/// HBITMAP 所有权守卫（`CreateDIBSection` 分配 ⇒ 必须 `DeleteObject`）。
struct BitmapGuard(HBITMAP);
impl Drop for BitmapGuard {
    fn drop(&mut self) {
        // 不变式：`self.0` 是有效位图句柄，且此时已从 DC 取消选中。
        unsafe { DeleteObject(self.0) };
    }
}

/// HDC 所有权守卫（`CreateCompatibleDC` 分配 ⇒ 必须 `DeleteDC`）。
struct DcGuard(HDC);
impl Drop for DcGuard {
    fn drop(&mut self) {
        // 不变式：`self.0` 是有效 DC 句柄，且此时已还原选中对象。
        unsafe { DeleteDC(self.0) };
    }
}

/// DC 选中对象还原守卫：删除 DC/位图前必须把旧对象选回。
struct SelectedGuard {
    dc: HDC,
    old: HGDIOBJ,
}
impl Drop for SelectedGuard {
    fn drop(&mut self) {
        // 不变式：`self.dc` 与 `self.old` 均有效；还原是 `DeleteObject(bitmap)` 的前置条件。
        unsafe { SelectObject(self.dc, self.old) };
    }
}
