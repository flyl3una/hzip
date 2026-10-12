//! hzip 资源管理器上下文菜单 Shell 扩展（`IExplorerCommand`）。
//!
//! # 这个 DLL 做什么
//!
//! 在资源管理器右键菜单里提供「hzip」子菜单（解压 / 压缩条目），条目集来自
//! `%APPDATA%\hzip\shell-menu.json`（由 GUI 设置页的「应用」按钮写入）。
//!
//! # 两条装载路径，同一份实现
//!
//! | 路径 | 注册方式 | DLL 载入点 | 菜单形态 |
//! |---|---|---|---|
//! | 经典 | `HKCU\...\Classes\*\shell\hzip\ExplorerCommandHandler={CLSID}` | `explorer.exe` **进程内**（`InProcServer32`） | Windows 10 / Server 2016-2022 的唯一菜单；Windows 11 的「显示更多选项」 |
//! | 稀疏包 | 清单 `com:SurrogateServer` + `windows.fileExplorerContextMenus` | `dllhost.exe` **代理进程**（`SurrogateServer`） | **Windows 11 / Server 2025 的现代菜单顶层** |
//!
//! 两者共用同一 CLSID 之外的一切实现；CLSID 分别由 `hzip-platform` 的经典注册
//! 与包清单各自声明（刻意不同，避免两条注册路径争抢同一 CLSID 的解析）。
//!
//! # 硬约束
//!
//! 1. **进程内**：本 DLL 的崩溃 = 资源管理器崩溃。所有 COM 方法体都过
//!    [`guard`] 的 `catch_unwind`，且 `Cargo.toml` 固定 `panic = "unwind"`
//!    （`abort` 会直接杀死宿主进程）。
//! 2. **菜单构建路径必须快**：`GetTitle`/`GetIcon`/`GetState`/`EnumSubCommands`
//!    在 Explorer 的 UI 线程上被调用，**不得**做重 I/O。配置只在
//!    `EnumSubCommands` 时读一次文件、解析一次，随后在内存里复制。
//! 3. **不弹窗、不写日志文件**：失败一律静默降级（菜单不出现），诊断走
//!    [`guard::log_err`] 的 `OutputDebugStringW`。
//!
//! # 与主 workspace 的关系
//!
//! 本 crate **不属于**主 workspace（自带空 `[workspace]` 表），与 hzip 其余
//! crate 零代码依赖（02 §11.5）——只通过 `shell-menu.json` 这一个文件格式与
//! GUI 耦合。故 `build/checks/deps-direction.py` 无需为例外放行。

mod command;
mod enumerator;
mod guard;
mod launch;
mod menu_data;
mod trace;

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::BOOL;
use windows::core::{implement, IUnknown, Interface, Ref, Result, GUID, HRESULT};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_NOINTERFACE, HMODULE, S_OK};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::DisableThreadLibraryCalls;

/// 顶层菜单显示名（应用身份，不本地化）。
pub const MENU_TITLE: &str = "hzip";

/// 组件 CLSID（**全链路唯一**：经典注册、包清单 `com:Class/@Id`、本常量）。
///
/// # 为什么必须唯一（踩过的坑）
///
/// 曾经让经典路径与稀疏包各用一个 CLSID，理由是「避免两条注册争抢解析」——
/// **这个理由是错的，且会导致功能完全不可用**：经典注册把
/// `ExplorerCommandHandler` 指向另一个 CLSID，而本 DLL 的
/// [`DllGetClassObject`] 只认自己那一个，于是资源管理器加载 DLL 后拿到
/// `CLASS_E_CLASSNOTAVAILABLE`，**右键菜单一项都不显示**。
///
/// 两条路径本就不会同时生效（`install` 在稀疏包成功后会清掉经典残留），
/// 故共用一个 CLSID 没有任何冲突风险。
///
/// 与 `hzip-platform::context_menu::win::CLSID`、包清单 `{{CLSID}}`、
/// `build/shell-ext/build-shell-ext.ps1` 的 `$clsid` 必须完全一致——
/// `crates/hzip-platform/tests/context_menu.rs` 有一致性测试盯着这四处。
pub const CLSID: GUID = GUID::from_u128(0xA7C4D1E8_5B93_4F60_9E27_8D3A1B5C7F04);

/// 模块内活对象计数（DLL 卸载判定）。
static OBJECT_COUNT: AtomicU32 = AtomicU32::new(0);

/// 对象存活登记/注销（`HzipCommand` / `HzipEnum` / 类工厂经 `IUnknownImpl` 调用）。
pub(crate) fn add_ref() {
    OBJECT_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// 见 [`add_ref`]。
pub(crate) fn release_ref() {
    // 只减不加（防止下溢把卸载判定搞反）
    let _ = OBJECT_COUNT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_sub(1))
    });
}

// ---------------------------------------------------------------- 类工厂

/// 创建 [`command::HzipCommand`]（配置当次读取；读不到 = 菜单隐藏）。
#[implement(IClassFactory)]
struct HzipClassFactory;

impl IClassFactory_Impl for HzipClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        guard::guard_out("CreateInstance", || {
            trace::trace("CreateInstance");
            if !punkouter.is_null() {
                // 不支持聚合
                return Err(CLASS_E_CLASSNOTAVAILABLE.into());
            }
            if ppvobject.is_null() || riid.is_null() {
                return Err(windows::Win32::Foundation::E_POINTER.into());
            }
            // SAFETY: 已判空；read_unaligned 对任意对齐安全
            let iid = unsafe { std::ptr::read_unaligned(riid) };
            let config = std::sync::Arc::new(menu_data::load().unwrap_or_default());
            let obj: IUnknown = command::HzipCommand::root(config).into();
            // SAFETY: ppvobject 非空；QueryInterface 失败返回 E_NOINTERFACE
            let _ = unsafe { obj.query(&iid, ppvobject) };
            if unsafe { (*ppvobject).is_null() } {
                return Err(E_NOINTERFACE.into());
            }
            Ok(())
        })
    }

    fn LockServer(&self, flock: BOOL) -> Result<()> {
        if flock.as_bool() {
            add_ref();
        } else {
            release_ref();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- 导出

/// DLL 入口（仅禁用线程通知——我们不需要 `DllMain` 里的线程回调）。
///
/// # Safety
///
/// 由加载器调用；`hinstance` 为模块句柄。
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    hinstance: HMODULE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> BOOL {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        // SAFETY: 加载器上下文内的合法调用
        unsafe { DisableThreadLibraryCalls(hinstance) }.ok();
    }
    BOOL::from(true)
}

/// COM 类工厂入口。
///
/// # Safety
///
/// 由 COM 调用；三个指针按 `IClassFactory` 契约给出。
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    guard::guard_hr("DllGetClassObject", || {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return windows::Win32::Foundation::E_POINTER;
        }
        trace::trace("DllGetClassObject");
        // 用 read_unaligned 而非 `*p`：调用方（COM 通常对齐，但不能假定）若给未对齐
        // 指针，直接解引用在 debug 会 abort、release 是 UB。进程内载入 Explorer 时
        // 一次 abort 就是整个桌面外壳崩溃，故在 FFI 边界一律按未对齐读取处理。
        // SAFETY: 两个指针均已判空；read_unaligned 对任意对齐安全。
        let clsid = unsafe { std::ptr::read_unaligned(rclsid) };
        if clsid != CLSID {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let iid = unsafe { std::ptr::read_unaligned(riid) };
        let factory: IUnknown = HzipClassFactory.into();
        // SAFETY: ppv 非空
        // `IUnknown::query` 在本版本返回 HRESULT（不抛错）
        unsafe { factory.query(&iid, ppv) }
    })
}

/// 是否可卸载（无活对象即可）。
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    guard::guard_hr("DllCanUnloadNow", || {
        if OBJECT_COUNT.load(Ordering::Relaxed) == 0 {
            S_OK
        } else {
            windows::Win32::Foundation::S_FALSE
        }
    })
}
