//! macOS 服务菜单（Services）轨：宿主侧的 Objective-C provider 注册。
//!
//! # 为什么需要它
//!
//! `Info.plist` 的 `NSServices` 只声明「本 app 提供哪些服务」——条目会出现在
//! Finder 右键底部的「服务」子菜单里，**但点击时系统调用的是本进程内注册的
//! 同名 selector**。宿主必须在启动后调 `NSRegisterServicesProvider(provider,
//! "hzip")`，把 `NSPortName`（= `hzip`）解析到一个实现了这些 selector 的对象上；
//! 否则服务菜单项存在却点了没反应（selector 无实现）。
//!
//! # 与 FinderSync 轨的关系（两条轨共用同一条派发通道）
//!
//! 两轨都是「把一次用户动作交给宿主执行」。FinderSync appex 在**沙盒**内，服务
//! provider 则在**宿主进程内**——但两者都走同一张 [`VERBS`] 表、同一条意图队列
//! （[`super::ContextMenu::append_extension_intent`]），故动词语义与命令行、
//! 二实例转发完全同源，扩展侧绝不自己拼 argv。
//!
//! 菜单可见性由 `NSServicesStatus`（宿主 [`super::ContextMenu::install`] 里翻转）
//! 控制；本模块只负责**注册 provider** 让条目真正可点。
//!
//! # 选择器命名
//!
//! 三个方法名必须与 `Info.plist` 的 `NSServices[].NSMessage` **逐字一致**：
//! `extractHereWithHzip:` / `extractToWithHzip:` / `compressWithHzip:`。参数是
//! pasteboard（`NSPasteboard *`），从其文件项取选中路径（现代
//! `NSPasteboardTypeFileURL`，回退经典 `NSFilenamesPboardType`）→ 落意图队列。
//!
//! [`VERBS`]: super::VERBS

use std::sync::LazyLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSRegisterServicesProvider};
use objc2_foundation::{NSArray, NSString, NSURL};

use super::{ExtensionIntent, backend};

/// 服务端的 `NSPortName`（与 `Info.plist` `NSServices[].NSPortName` 一致）。
const SERVICE_PORT_NAME: &str = "hzip";

/// 首次访问即注册 Services provider（幂等）。
///
/// # 为什么是「泄漏 provider」而不是静态持有
///
/// provider 类是 `MainThreadOnly`（AppKit 约定：服务派发在主线程），而非 `Sync`
/// ——无法放进 `static`（静态量须 `Sync`）。而 `NSRegisterServicesProvider` **不**
/// 接管所有权，必须有一份活到进程结束的强引用，否则 selector 会打到已释放对象。
/// 故此处 `mem::forget` 一份 `Retained`（进程级一次性泄漏，量极小），静态只留
/// `()` 的初始化标记。
static REGISTER: LazyLock<()> = LazyLock::new(|| {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        tracing::warn!("Services provider 未注册：非主线程（服务项点击将无响应）");
        return;
    };
    let provider = HzipServiceProvider::new(mtm);
    let name = NSString::from_str(SERVICE_PORT_NAME);
    // SAFETY: provider 类型正确（实现了三个 NSMessage selector）；名字是本 app
    // 的 NSPortName。注册后由 `mem::forget` 的强引用保活。
    unsafe {
        NSRegisterServicesProvider(Some(&*provider as &AnyObject), &name);
    }
    // 保活：不 drop（见上「为什么泄漏」）。
    std::mem::forget(provider);
    tracing::info!("macOS Services provider 已注册（NSPortName={SERVICE_PORT_NAME}）");
});

/// 注册 Services provider（幂等，仅 macOS）。
///
/// 必须在 `NSApplication` 已启动后调用（服务派发依赖主线程 run loop）；重复调用
/// 无副作用。非主线程调用只告警不 panic（最坏后果 = 服务项点了没反应）。
pub fn register_services_provider() {
    LazyLock::force(&REGISTER);
}

/// 从 pasteboard 读选中文件路径。
///
/// 读两种类型（顺序：现代 → 经典），因为**系统按 Info.plist `NSSendTypes` 过滤**
/// 交给 provider 的 pasteboard——两岸都声明、两岸都读，才不会出现「声明了 A 却
/// 只认 B，结果路径读不到」。Finder 对文件同时提供这两种。
///
/// 1. `NSPasteboardTypeFileURL`（现代，`public.file-url`）：逐项 `file://` URL 字符串，
///    经 `NSURL` 解析成 POSIX 路径（免手写百分号解码）；
/// 2. `NSFilenamesPboardType`（经典，废弃但 Finder 仍给）：路径字符串数组，
///    已是 POSIX 路径，直接用。
fn paths_from_pasteboard(pasteboard: &NSPasteboard) -> Vec<String> {
    // ① 现代：逐项读 file URL。
    let mut out = Vec::new();
    if let Some(items) = pasteboard.pasteboardItems() {
        for item in items.iter() {
            let Some(s) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else {
                continue;
            };
            let Some(url) = NSURL::URLWithString(&s) else {
                continue;
            };
            if let Some(path) = url.path() {
                out.push(path.to_string());
            }
        }
    }
    if !out.is_empty() {
        return out;
    }
    // ② 经典：NSFilenamesPboardType 是路径字符串数组（property list）。
    // 该常量自 10.13 起废弃，但 Finder 至今仍提供；`NSFilenamesPboardType` 在
    // Info.plist 的 NSSendTypes 里声明，故此处必须也读它（见函数文档）。
    #[allow(deprecated)]
    if let Some(plist) =
        pasteboard.propertyListForType(unsafe { objc2_app_kit::NSFilenamesPboardType })
        && let Some(arr) = plist.downcast_ref::<NSArray>()
    {
        for i in 0..arr.count() {
            let obj = arr.objectAtIndex(i);
            if let Some(s) = obj.downcast_ref::<NSString>() {
                out.push(s.to_string());
            }
        }
    }
    out
}

/// 三个 Service 动作的共同实现：读路径 → 落意图队列（走与 appex 同一条队列）。
fn dispatch(key: &str, pasteboard: &NSPasteboard) {
    let paths = paths_from_pasteboard(pasteboard);
    if paths.is_empty() {
        tracing::warn!(key, "Services 动作无可读路径（pasteboard 为空）");
        return;
    }
    let intent = ExtensionIntent {
        key: key.to_string(),
        paths,
    };
    match backend().append_extension_intent(&intent) {
        Ok(()) => tracing::info!(key, "Services 动作已入意图队列"),
        Err(e) => tracing::error!(key, error = %e, "Services 动作写意图队列失败"),
    }
}

// SAFETY: `NSObject` 无子类化要求；本类无 `Drop`（ivar 为 `()`），
// `define_class!` 可代为管理。
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "HzipServiceProvider"]
    #[ivars = ()]
    struct HzipServiceProvider;

    unsafe impl NSObjectProtocol for HzipServiceProvider {}

    impl HzipServiceProvider {
        /// `NSServices[].NSMessage = extractHereWithHzip`（解压到当前文件夹）。
        #[unsafe(method(extractHereWithHzip:))]
        fn extract_here(&self, pasteboard: &NSPasteboard) {
            dispatch("extract_here", pasteboard);
        }

        /// `NSServices[].NSMessage = extractToWithHzip`（解压到「压缩文件名/」）。
        #[unsafe(method(extractToWithHzip:))]
        fn extract_to(&self, pasteboard: &NSPasteboard) {
            dispatch("extract_to", pasteboard);
        }

        /// `NSServices[].NSMessage = compressWithHzip`（压缩为 zip）。
        #[unsafe(method(compressWithHzip:))]
        fn compress(&self, pasteboard: &NSPasteboard) {
            dispatch("add_zip", pasteboard);
        }
    }
);

impl HzipServiceProvider {
    fn new(mtm: objc2::MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `NSObject`'s `init` is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}
