//! `file_icon` 的公开面（类型 + 取图入口）。
//!
//! 为什么单列一个 `api.rs`：本仓约定 `mod.rs` 只放模块声明与再导出，不放定义
//! （`build/checks/layout-conventions.py` L1 红线；`context_menu/mod.rs` 同款）。

/// 图标源边长（px）。32 = Windows「大图标」档（等价于 7-Zip 的 `LVSIL_NORMAL`），
/// 也是资源管理器列表同款大小；调用方按 slot 缩放，不要求多档源。
pub const ICON_PX: u32 = 32;

/// 条目种类（只影响传给 shell 的**属性位**与回退图标选择）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IconKind {
    /// 目录（`FILE_ATTRIBUTE_DIRECTORY`）
    Dir,
    /// 符号链接（Windows 另加 `SHGFI_LINKOVERLAY` ⇒ 快捷方式箭头，与资源管理器一致）
    Symlink,
    /// 文件型容器（档内嵌套压缩包）——取图与普通文件同路，仅回退图标不同
    Container,
    /// 普通文件
    File,
}

/// 取图请求。
///
/// `ext` 由调用方**归一化**：小写、不含点、只取最后一段（`a.TAR.GZ` → `gz`；
/// 无扩展名 → `None`）。先例一致：7-Zip 传给 shell 的就是 `fileName + dotPos`
/// 即 `.ext` 段（`SysIconUtils.cpp`），从而天然规避档内非法字符/超长名。
#[derive(Debug, Clone, Copy)]
pub struct IconRequest<'a> {
    /// 归一化扩展名（`None` = 无扩展名 ⇒ 取系统默认文件图标）
    pub ext: Option<&'a str>,
    /// 条目种类
    pub kind: IconKind,
}

/// 系统图标位图。
#[derive(Clone, PartialEq, Eq)]
pub struct SystemIcon {
    /// 位图宽（px）
    pub width: u32,
    /// 位图高（px）
    pub height: u32,
    /// **预乘 BGRA**（8bit/通道；长度 = `width * height * 4`）。
    /// 与 gpui `RenderImage` 期望的格式一致 ⇒ 调用方可直通封装，无需换通道。
    pub premultiplied_bgra: Vec<u8>,
    /// 系统本地化的类型名（Windows = `SHGFI_TYPENAME`，如「ZIP 压缩文件」）；
    /// 平台无此概念 → `None`。**语言跟随系统**，调用方决定是否采用。
    pub type_name: Option<String>,
}

impl std::fmt::Debug for SystemIcon {
    /// 手写 Debug：不把 4 KB 位图字节打进日志/断言输出。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemIcon")
            .field("size", &(self.width, self.height))
            .field("bytes", &self.premultiplied_bgra.len())
            .field("type_name", &self.type_name)
            .finish()
    }
}

/// 查询系统图标；拿不到 → `None`（调用方回退自有图标）。
///
/// **线程**：可在任意线程调用（Windows 侧不要求 COM 套间，与先例一致）；
/// 但**不得**在 UI/渲染帧调用——可能阻塞数百毫秒。
pub fn lookup(req: &IconRequest<'_>) -> Option<SystemIcon> {
    #[cfg(target_os = "windows")]
    {
        return super::win::lookup(req);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return super::linux::lookup(req);
    }
    #[cfg(target_os = "macos")]
    {
        return super::macos::lookup(req);
    }
    #[allow(unreachable_code)]
    None
}
