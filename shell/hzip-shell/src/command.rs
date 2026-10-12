//! `IExplorerCommand` 实现（菜单本体）。
//!
//! # 一个类型，两种角色
//!
//! [`HzipCommand`] 用 [`Role`] 区分：
//!
//! | 角色 | 对应 shell 概念 | 行为 |
//! |---|---|---|
//! | [`Role::Root`] | 顶层「hzip」项 | `ECF_HASSUBCOMMANDS`，`EnumSubCommands` 产出子项 |
//! | [`Role::Leaf`] | 一个具体动作 | 显示标签；`Invoke` 启动 `hzip-gui` |
//!
//! `EnumSubCommands` **拿不到**选中项（COM 契约如此），故它无条件产出全部条目；
//! 由 `GetState`（**有**选中项）对不适用者返回 `ECS_HIDDEN`。这是 shell 的
//! 标准分工，也是本 DLL 唯一可行的做法。
//!
//! # 适用性规则（对齐 7-Zip / Bandizip 的用户预期）
//!
//! | 条目组 | 何时出现 |
//! |---|---|
//! | [`Group::ArchiveFile`]（解压类） | **全部**选中项都是已关联档案 |
//! | [`Group::Any`]（压缩类） | 始终 |
//!
//! 注意压缩类**不是**「否则才出现」——在 `.zip` 上右键时用户既想解压也想再压缩，
//! 7-Zip/Bandizip 两者都给。故这里是「解压组附加」而非「二选一」。
//!
//! # 空白处右键
//!
//! Win11 由清单的 `Directory\Background` ItemType 分发，shell 传入的是该文件夹
//! 本身。压缩类条目对文件夹本就适用（`Group::Any` 恒显示），故无需特殊处理。

use std::sync::Arc;

use windows::core::BOOL;
use windows::core::{implement, Ref, Result, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItem, IShellItemArray,
    SHStrDupW, ECF_DEFAULT, ECF_HASSUBCOMMANDS, ECS_ENABLED, ECS_HIDDEN, SIGDN_FILESYSPATH,
};

use crate::enumerator::HzipEnum;
use crate::guard::{guard_out, log_err};
use crate::menu_data::{Item, MenuConfig};

/// 条目组（决定 `GetState` 的适用性）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// 档案文件上的解压类条目。
    ArchiveFile,
    /// 任意文件 / 文件夹上的压缩类条目。
    Any,
}

/// 命令角色。
pub enum Role {
    /// 顶层父项：只负责产出子项。
    Root,
    /// 叶子项：显示标签并执行。
    Leaf {
        /// 条目落盘 key（诊断用）。
        key: String,
        /// 显示标签（已本地化，来自配置）。
        label: String,
        /// 命令行参数（含占位符，`Invoke` 时展开）。
        args: Vec<String>,
        /// 所属组（适用性判定用）。
        group: Group,
    },
}

/// 上下文菜单命令（COM 对象）。
#[implement(IExplorerCommand)]
pub struct HzipCommand {
    /// 配置快照：`EnumSubCommands` 当次构建时读取（增删条目无需重启 Explorer）。
    config: Arc<MenuConfig>,
    /// 本实例的角色。
    role: Role,
}

impl HzipCommand {
    /// 顶层项。
    pub fn root(config: Arc<MenuConfig>) -> Self {
        Self {
            config,
            role: Role::Root,
        }
    }

    /// 叶子项。
    fn leaf(config: Arc<MenuConfig>, item: &Item, group: Group) -> Self {
        Self {
            config,
            role: Role::Leaf {
                key: item.key.clone(),
                label: item.label.clone(),
                args: item.args.clone(),
                group,
            },
        }
    }

    /// 当前选中项的文件系统路径（非文件系统项被跳过）。
    fn selected_paths(psiitemarray: Ref<IShellItemArray>) -> Vec<String> {
        let Ok(array) = psiitemarray.ok() else {
            return Vec::new();
        };
        let count = unsafe { array.GetCount() }.unwrap_or(0);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let Ok(item) = (unsafe { array.GetItemAt(i) }) else {
                continue;
            };
            if let Some(p) = item_path(&item) {
                paths.push(p);
            }
        }
        paths
    }

    /// 该叶子项在当前选中项下是否适用。
    fn is_applicable(&self, group: Group, paths: &[String]) -> bool {
        match group {
            // 压缩类恒适用（文件、文件夹、空白处）
            Group::Any => true,
            // 解压类：必须全部选中项都是已关联档案（混合选择时不给解压，
            // 否则会对着 .txt 执行 --extract-here）
            Group::ArchiveFile => {
                !paths.is_empty()
                    && !self.config.archive_file.is_empty()
                    && paths.iter().all(|p| self.config.is_archive_path(p))
            }
        }
    }
}

/// 取 shell 项的本地路径（`SIGDN_FILESYSPATH`；失败返回 `None`）。
fn item_path(item: &IShellItem) -> Option<String> {
    // SAFETY: 出参由 shell 经 CoTaskMemAlloc 分配，下面显式释放
    let raw = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()?;
    if raw.is_null() {
        return None;
    }
    // SAFETY: shell 返回以 NUL 结尾的宽字符串
    let s = unsafe { raw.to_string() }.ok()?;
    // SAFETY: 所有权移交调用方
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(raw.0 as *const _)) };
    Some(s)
}

/// 转为 shell 拥有的宽字符串（`SHStrDupW`：分配 CoTaskMem，shell 负责释放）。
fn dup(s: &str) -> Result<PWSTR> {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide 以 NUL 结尾且在本调用期间存活
    unsafe { SHStrDupW(PCWSTR(wide.as_ptr())) }
}

impl IExplorerCommand_Impl for HzipCommand_Impl {
    fn GetTitle(&self, _psiitemarray: Ref<IShellItemArray>) -> Result<PWSTR> {
        crate::trace::trace("GetTitle");
        guard_out("GetTitle", || match &self.role {
            // 顶层项固定文案：它是应用身份，不随界面语言变
            Role::Root => dup(crate::MENU_TITLE),
            Role::Leaf { label, .. } => dup(label),
        })
    }

    fn GetIcon(&self, _psiitemarray: Ref<IShellItemArray>) -> Result<PWSTR> {
        crate::trace::trace("GetIcon");
        guard_out("GetIcon", || match &self.role {
            Role::Root => dup(&format!("{},0", self.config.exe)),
            // 叶子项不设图标：继承父项，避免整列相同图标（7-Zip 同款观感）
            Role::Leaf { .. } => Err(E_NOTIMPL.into()),
        })
    }

    fn GetToolTip(&self, _psiitemarray: Ref<IShellItemArray>) -> Result<PWSTR> {
        // 不提供工具提示（契约用 E_NOTIMPL 表达）
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(GUID::zeroed())
    }

    fn GetState(&self, psiitemarray: Ref<IShellItemArray>, _foktobeslow: BOOL) -> Result<u32> {
        crate::trace::trace("GetState");
        guard_out("GetState", || {
            // 未在设置页点过「应用」→ 整棵菜单隐藏（不留空壳菜单）
            if !self.config.is_usable() {
                return Ok(ECS_HIDDEN.0 as u32);
            }
            match &self.role {
                Role::Root => Ok(ECS_ENABLED.0 as u32),
                Role::Leaf { group, .. } => {
                    let paths = HzipCommand::selected_paths(psiitemarray);
                    if self.is_applicable(*group, &paths) {
                        Ok(ECS_ENABLED.0 as u32)
                    } else {
                        Ok(ECS_HIDDEN.0 as u32)
                    }
                }
            }
        })
    }

    fn Invoke(&self, psiitemarray: Ref<IShellItemArray>, _pbc: Ref<IBindCtx>) -> Result<()> {
        crate::trace::trace("Invoke");
        guard_out("Invoke", || {
            let Role::Leaf {
                key,
                label,
                args,
                group,
            } = &self.role
            else {
                // 顶层项不可点：它只展开子菜单
                return Ok(());
            };
            let paths = HzipCommand::selected_paths(psiitemarray);
            if !self.is_applicable(*group, &paths) || paths.is_empty() {
                return Ok(());
            }
            for argv in crate::menu_data::expand_args(args, &paths) {
                if let Err(e) = crate::launch::spawn(&self.config.exe, &argv) {
                    log_err("Invoke", &format!("启动失败（{label} / {key}）: {e}"));
                }
            }
            Ok(())
        })
    }

    fn GetFlags(&self) -> Result<u32> {
        crate::trace::trace("GetFlags");
        Ok(match self.role {
            // 有子命令 → Win11 归入应用归属 flyout；Win10 表现为级联子菜单
            Role::Root => ECF_HASSUBCOMMANDS.0 as u32,
            Role::Leaf { .. } => ECF_DEFAULT.0 as u32,
        })
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        crate::trace::trace("EnumSubCommands");
        guard_out("EnumSubCommands", || {
            if !matches!(self.role, Role::Root) {
                return Err(E_NOTIMPL.into());
            }
            // 顺序：解压组在前（对齐 7-Zip/Bandizip 的习惯）
            let mut commands: Vec<IExplorerCommand> = Vec::new();
            for item in &self.config.archive_file {
                commands
                    .push(HzipCommand::leaf(self.config.clone(), item, Group::ArchiveFile).into());
            }
            for item in &self.config.any {
                commands.push(HzipCommand::leaf(self.config.clone(), item, Group::Any).into());
            }
            if commands.is_empty() {
                // 无可展示条目 → 空枚举（shell 会显示空子菜单，但 GetState 已隐藏父项）
                return Err(E_NOTIMPL.into());
            }
            Ok(IEnumExplorerCommand::from(HzipEnum::new(commands)))
        })
    }
}
