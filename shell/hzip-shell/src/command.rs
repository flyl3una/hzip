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
//! | [`Group::ArchiveFile`]（解压类） | **全部**选中项都是文件（不论扩展名） |
//! | [`Group::Any`]（压缩类） | 始终 |
//!
//! 解压类**不再按扩展名过滤**（用户口径 2026-10-26）：改了后缀名的档案
//! （`disk.img` / `a.qcow2` / 无扩展名）同样可能是 zip/7z，藏起来就等于没有入口。
//! 是否真的是档案由引擎按魔数嗅探判定，不然就弹错误窗——比「菜单里什么都没有」
//! 可诊断得多。
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
        is_applicable(&self.config, group, paths)
    }
}

/// 适用性判定（**纯函数**：便于单测，不碰 COM）。
pub(crate) fn is_applicable(config: &MenuConfig, group: Group, paths: &[String]) -> bool {
    match group {
        // 压缩类恒适用（文件、文件夹、空白处）
        Group::Any => true,
        // 解压类：**任意文件**都显示，不再看扩展名。
        //
        // 理由（用户口径 2026-10-26）：扩展名不是证据——`disk.img`、`a.qcow2`、
        // 甚至无扩展名的文件都可能就是 zip/7z（厂商改过后缀、传输被重命名）。
        // 真正的判定交给引擎：GUI 打开/解压时按**魔数**嗅探（zip 等魔数优先，
        // 扩展名只是最后兜底），不是档案就弹错误窗。若这里按扩展名藏起来，
        // 用户永远没有入口去试。
        //
        // 文件夹不参与解压（目录没有「解压」语义；压缩类已独立覆盖），因此
        // 混合选择里只要出现目录就不给解压项。
        Group::ArchiveFile => {
            !paths.is_empty()
                && !config.archive_file.is_empty()
                && paths
                    .iter()
                    .all(|p| !std::path::Path::new(p.as_str()).is_dir())
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn cfg(archive_items: usize) -> MenuConfig {
        let item = |key: &str| Item {
            key: key.to_string(),
            label: key.to_string(),
            args: vec![],
        };
        MenuConfig {
            exe: r"C:\Program Files\hzip\hzip-gui.exe".to_string(),
            archive_file: (0..archive_items).map(|_| item("extract_here")).collect(),
            any: vec![item("add_zip")],
        }
    }

    fn temp_file(name: &str) -> String {
        let dir = std::env::temp_dir().join("hzip-cm-applicability");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let p = dir.join(name);
        std::fs::write(&p, b"PK\x03\x04").expect("write");
        p.to_string_lossy().into_owned()
    }

    /// 解压类必须出现在**任意文件**上（用户口径 2026-10-26）：
    /// 改了后缀名的档案（`.bin` / 无扩展名）也要有入口——扩展名不是证据。
    #[test]
    fn extract_items_show_on_any_file_regardless_of_extension() {
        for name in ["renamed.bin", "disk.img", "noext", "actually-a-zip.dat"] {
            let paths = vec![temp_file(name)];
            assert!(
                is_applicable(&cfg(1), Group::ArchiveFile, &paths),
                "{} 上必须给解压项（可能是改过后缀的压缩包）",
                name
            );
        }
    }

    /// 目录不参与解压；空选择不出菜单；设置里没勾解压项时整组隐藏。
    #[test]
    fn extract_items_are_hidden_for_directories_empty_selection_and_disabled_group() {
        let dir = std::env::temp_dir();
        let dirs = vec![dir.to_string_lossy().into_owned()];
        assert!(
            !is_applicable(&cfg(1), Group::ArchiveFile, &dirs),
            "目录没有解压语义"
        );
        assert!(!is_applicable(&cfg(1), Group::ArchiveFile, &[]), "空选择");
        assert!(
            !is_applicable(&cfg(0), Group::ArchiveFile, &[temp_file("x.zip")]),
            "设置里未启用解压项 ⇒ 整组不出现"
        );
    }

    /// 压缩类恒适用（文件 / 目录 / 空白处）。
    #[test]
    fn compress_items_always_apply() {
        assert!(is_applicable(&cfg(0), Group::Any, &[]));
        assert!(is_applicable(
            &cfg(1),
            Group::Any,
            &[std::env::temp_dir().to_string_lossy().into_owned()]
        ));
    }

    /// 端到端（COM 层）：用**真实 shell 项**调 `GetState`，而不是只测纯函数。
    ///
    /// 用户口径 2026-10-26：`.msi`/`.jar` 右键里也要有「用 hzip 打开 / 解压」。
    /// 这里把真实文件路径喂进 `IShellItemArray`，走 `selected_paths` → `is_applicable`
    /// → `ECS_ENABLED/HIDDEN` 的完整链路；任何一环（含 `is_dir` 判定）写错都会红。
    #[test]
    fn get_state_enables_extract_items_for_msi_jar_and_unknown_extensions() {
        use windows::core::HSTRING;
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{
            IShellItem, IShellItemArray, SHCreateItemFromParsingName,
            SHCreateShellItemArrayFromShellItem,
        };

        // SAFETY: 测试进程内初始化 STA（重复初始化返回 S_FALSE，可忽略）
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        const SAMPLE: &str = r#"{"schema":1,"exe":"C:\\hzip\\hzip-gui.exe","items":{
            "archive_file":[{"key":"extract_here","label":"解压到当前文件夹","args":["--extract-here","%SEL"]}],
            "any":[{"key":"add_zip","label":"添加到 zip","args":["--compress","%SELS","--format","zip"]}]}}"#;
        let config = std::sync::Arc::new(crate::menu_data::parse(SAMPLE).expect("样例配置"));
        let root: windows::Win32::UI::Shell::IExplorerCommand = HzipCommand::root(config.clone()).into();
        let array_of = |path: &std::path::Path| -> IShellItemArray {
            let item: IShellItem =
                // SAFETY: 路径来自临时目录里的真实文件
                unsafe { SHCreateItemFromParsingName(&HSTRING::from(path.to_string_lossy().into_owned()), None) }
                    .expect("SHCreateItemFromParsingName");
            // SAFETY: item 有效
            unsafe { SHCreateShellItemArrayFromShellItem(&item) }.expect("array")
        };
        let extract_leaf = || {
            // SAFETY: root 为本地构造的 COM 对象，接口有效
            let e = unsafe { root.EnumSubCommands() }.expect("EnumSubCommands");
            let mut out: [Option<windows::Win32::UI::Shell::IExplorerCommand>; 1] = [None];
            let mut fetched = 0u32;
            // SAFETY: 出参指向 1 个槽位
            unsafe { e.Next(&mut out, Some(&mut fetched)) }.ok().expect("Next");
            assert_eq!(fetched, 1);
            out[0].clone().expect("首个叶子")
        };

        let dir = std::env::temp_dir().join("hzip-cm-getstate");
        std::fs::create_dir_all(&dir).expect("temp dir");
        for name in ["pkg.msi", "app.jar", "blob.bin", "noext"] {
            let p = dir.join(name);
            std::fs::write(&p, b"whatever").expect("write");
            // SAFETY: 同上（本地 COM 对象 + 真实 shell 项数组）
            let state = unsafe { extract_leaf().GetState(&array_of(&p), false) }.expect("GetState");
            assert_eq!(
                state,
                ECS_ENABLED.0 as u32,
                "{name} 上必须_show 解压项（扩展名不是证据）"
            );
        }
        // 目录仍然不给解压
        // SAFETY: 同上
        let state = unsafe { extract_leaf().GetState(&array_of(&dir), false) }.expect("GetState");
        assert_eq!(state, ECS_HIDDEN.0 as u32, "目录不给解压项");
    }
}
