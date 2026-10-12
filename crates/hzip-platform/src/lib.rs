//! hzip 平台差异收口层（02 §5）：全 workspace 唯一的「操作系统相关行为」入口。
//!
//! # 收口规则（红线 5，02 §2.1）
//!
//! 1. `windows-sys` / `core-foundation` / `objc` / `libc` 等平台 crate **只允许**出现在
//!    本 crate 的依赖里；任何其他 crate 的 `Cargo.toml` 出现它们即 CI 报错
//!    （`build/checks/deps-direction.py` 机械校验）。
//! 2. 平台特定常量（如 Windows 路径上限 32767、FILETIME 纪元偏移）一律经本 crate 的
//!    语义层 API 暴露，不外泄——调用方完全无感知（02 §5.2 平台分发统一流程）。
//! 3. 任何 crate 需要触碰「操作系统相关行为」时只允许 import `hzip-platform`。
//!
//! # 模块结构（02 §5.1 总表）
//!
//! | 子模块 | 职责 | 平台差异点 |
//! |---|---|---|
//! | [`long_path`] | Windows `\\?\` 前缀注入/还原 | 仅 win；unix 直通 |
//! | [`assoc`] | 文件关联（默认打开程序 + 「打开方式」列表） | win 注册表 ProgId/OpenWithProgids / linux `.desktop`+`mimeapps.list` / mac `lsregister`+`LSHandlers` |
//! | [`console`] | 附着父进程控制台（GUI 子系统程序输出 `--help`/`--version`） | 仅 win 有实现；非 Windows 恒 None |
//! | [`context_menu`] | 资源管理器右键上下文菜单（trait + 各平台后端隔离） | win 稀疏包+注册表 / mac FinderSync+Services / linux Nautilus·Dolphin·Thunar |
//! | [`metadata`] | 统一属性（mtime/权限/隐藏/只读/系统/可执行）读写 | win 属性位 ↔ unix mode |
//! | [`symlink`] | 符号链接/硬链接创建、读取、类型判定 | win 需特权，降级链 |
//! | [`fs_util`] | 磁盘空间、稀疏检测、大小写敏感性实测、安全删除、原子替换 | rename 跨卷回退 |
//! | [`temp_area`] | 双击打开/编辑保存的受监控临时目录 | 各平台临时目录约定 |
//! | [`watch`] | 文件变更监听（notify 封装）+ 去抖 | inotify / FSEvents / ReadDirectoryChangesW |
//! | [`open`] | 用系统程序打开 / 在文件管理器中显示（reveal） | ShellExecute / open / xdg-open |
//! | [`exec`] | 外部查看器/编辑器进程派生与脱离 | 三平台 spawn 语义 |
//! | [`proc`] | 进程存活检测（PID + 创建时间双判定） | 防 Windows PID 复用 |
//! | [`signal`] | SIGINT/SIGTERM/控制台事件 → 取消令牌桥接 | win 控制台处理器 |
//! | [`dirs`] | 配置/数据/缓存/日志/临时目录解析 | XDG / Library / AppData 约定 |
//! | [`file_lock`] | 进程独占文件锁（GUI 单实例载体；T42） | win LockFileEx / unix flock |
//! | [`fs_caps`] | 文件系统能力探测 | NTFS / APFS / ext4 |
//! | [`file_icon`] | 系统文件类型图标（与文件管理器逐类型一致；T-ICON） | win `SHGetFileInfoW`+GDI / mac `NSWorkspace`+`NSBitmapImageRep` / linux XDG 主题 |
//! | [`lang`] | 系统界面语言探测（T-ICON 拍板3：语言「跟随系统」） | `GetUserDefaultLocaleName` / `LC_ALL`·`LANG` |
//! | [`machine`] | 机型识别（`hw.model`）与触控栏机型的辅助功能逃生判定 | `sysctlbyname("hw.model")`；非 macOS 恒 `None`/`false` |
//! | [`consts`] | 编译期常量（02 附录 B.2） | — |
//!
//! # 冻结范围（02 §13.1）
//!
//! 本 crate 全部公开 API 属冻结范围；后续任务需要新 API 只能**新增**并同步补
//! `tests/parity.rs` 一致性表用例（05 §5 规则）。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod assoc;
pub mod clipboard;
pub mod console;
pub mod consts;
pub mod context_menu;
pub mod dirs;
pub mod exec;
pub mod file_icon;
pub mod file_lock;
pub mod fs_caps;
pub mod fs_util;
pub mod kb;
pub mod lang;
pub mod long_path;
pub mod machine;
pub mod metadata;
pub mod notification;
pub mod open;
pub mod proc;
pub mod signal;
pub mod symlink;
pub mod temp_area;
pub mod watch;

#[cfg(windows)]
pub(crate) mod winutil;

#[cfg(test)]
#[path = "tests/testutil.rs"]
mod testutil;
