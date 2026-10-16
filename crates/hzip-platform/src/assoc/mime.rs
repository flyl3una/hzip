//! 扩展名 → MIME 类型（Linux 后端用；`.desktop` 的 `MimeType=` 与
//! `mimeapps.list` 的键都取自本表）。
//!
//! # 为什么单独一张表
//!
//! `MimeType=` 里写错一个 MIME（例如给 `.tgz` 写 `application/gzip`）不会报错，
//! 只会让该扩展名的「打开方式」永远不出现 hzip——这类**静默失效**必须在
//! 一处集中、可测。表值取 freedesktop `shared-mime-info` 的规范名。
//!
//! # 与 `hzip-format::exts` 的关系
//!
//! 候选扩展名清单的**唯一来源**是 `hzip-format::exts::FORMAT_EXTS`（本 crate 不
//! 依赖它——红线 4：hzip-platform 只依赖 hzip-error）。两张表的**一致性**由
//! `hzip-gui` 的测试互锁（该 crate 同时依赖两者）：凡出现在文件关联候选里的
//! 单点扩展名，必须在本表里能查到 MIME。
//!
//! 复合写法（`tar.gz`/`cpio.gz`）不在表内：Windows/macOS 关联以**最后一个点**
//! 为准，Linux 侧同理（`.gz` 命中 `application/gzip`），故候选集本来就只含
//! 单点扩展名（见 `hzip-gui::ui::contracts::associable_extensions`）。

/// 一个扩展名的 MIME 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExtMime {
    /// 扩展名（小写、不含点）。
    pub ext: &'static str,
    /// freedesktop MIME 类型。
    pub mime: &'static str,
}

/// 受支持扩展名 → MIME（顺序即「打开方式」列表与 `MimeType=` 的顺序）。
pub(super) const EXT_MIMES: &[ExtMime] = &[
    ExtMime {
        ext: "zip",
        mime: "application/zip",
    },
    ExtMime {
        ext: "7z",
        mime: "application/x-7z-compressed",
    },
    ExtMime {
        ext: "rar",
        mime: "application/vnd.rar",
    },
    ExtMime {
        ext: "tar",
        mime: "application/x-tar",
    },
    ExtMime {
        ext: "tgz",
        mime: "application/x-compressed-tar",
    },
    ExtMime {
        ext: "tbz",
        mime: "application/x-bzip-compressed-tar",
    },
    ExtMime {
        ext: "tbz2",
        mime: "application/x-bzip-compressed-tar",
    },
    ExtMime {
        ext: "txz",
        mime: "application/x-xz-compressed-tar",
    },
    ExtMime {
        ext: "tzst",
        mime: "application/x-zstd-compressed-tar",
    },
    ExtMime {
        ext: "cpio",
        mime: "application/x-cpio",
    },
    ExtMime {
        ext: "gz",
        mime: "application/gzip",
    },
    ExtMime {
        ext: "bz2",
        mime: "application/x-bzip2",
    },
    ExtMime {
        ext: "xz",
        mime: "application/x-xz",
    },
    ExtMime {
        ext: "zst",
        mime: "application/zstd",
    },
    // —— 磁盘镜像（T-IMG 计划）。取值口径：先按 freedesktop shared-mime-info 的规范名
    //    核对，查不到就用 `application/octet-stream` 兜底（**不编造** MIME）。本表只用于
    //    Linux 的 `.desktop` `MimeType=` 与 `mimeapps.list` 键，兜底不会造成功能缺失，
    //    但一个不存在的 MIME 会让「打开方式」条目静默失效。
    ExtMime {
        ext: "raw",
        mime: "application/octet-stream",
    },
    ExtMime {
        ext: "img",
        mime: "application/octet-stream",
    },
    ExtMime {
        ext: "dd",
        mime: "application/octet-stream",
    },
    ExtMime {
        ext: "qcow2",
        mime: "application/octet-stream",
    },
    ExtMime {
        ext: "qcow",
        mime: "application/octet-stream",
    },
    ExtMime {
        ext: "vmdk",
        mime: "application/octet-stream",
    },
];

/// 扩展名 → MIME（未知扩展名 → `None`；大小写不敏感、容忍前导点）。
pub(super) fn mime_of(ext: &str) -> Option<&'static str> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    EXT_MIMES.iter().find(|m| m.ext == ext).map(|m| m.mime)
}

/// 扩展名 → MIME 的 **crate 内复用入口**（T-ICON Linux 图标后端按扩展名取图标名）。
///
/// 语义与 [`mime_of`] 逐字相同，差别只在可见性：图标查表在 `file_icon::linux`，
/// 够不到 `pub(super)` 的模块内入口。**表只有一份**：两处各写一张，迟早会让
/// 「打开方式」与「图标」对同一扩展名给出不同 MIME（静默不一致，无处报错）。
///
/// 随 Linux 图标后端同条件编译——别的平台没有消费方，留着只会触发 unused 告警。
#[cfg(any(all(unix, not(target_os = "macos")), feature = "cross-check"))]
pub(crate) fn mime_of_ext(ext: &str) -> Option<&'static str> {
    mime_of(ext)
}

#[cfg(test)]
#[path = "tests/mime.rs"]
mod tests;
