//! AppShell 动作定义（`actions!` 单元动作 + `OpenRecent`/`SetToolbarStyle`/`SetViewStyle`
//! 载荷动作）——自 `shell/mod.rs` 拆分。

use gpui_kit::actions;

actions!(
    hzip_gui,
    [
        Open,
        Extract,
        Reveal,
        NewWindow,
        Settings,
        Quit,
        TestIntegrity,
        SelectAll,
        SelectInvert,
        RefreshView,
        ShowJobs,
        ToggleJobs,
        FindFile,
        AddFiles,
        DeleteFiles,
        CopyFiles,
        ClearMru,
        Paste,
        ToggleTree,
        About,
        CheckUpdate,
    ]
);

/// 打开最近文件（T-MENU4；payload = 完整路径）。
///
/// gpui `Action` derive：`no_json`（键位表不加载本动作——纯菜单点击派发）；
/// `no_register`（无需按名注册/反序列化）。菜单装配点为**每个 MRU 条目**
/// 构造独立实例（`Box::new(OpenRecent { path })`），点击事件携带完整路径。
#[derive(gpui_kit::Action, Clone, PartialEq, Eq)]
#[action(namespace = hzip_gui, no_json, no_register)]
pub(crate) struct OpenRecent {
    /// 档案完整路径
    pub path: String,
}

/// 设置工具栏三态（T-TVS2；payload = "icon_text"/"icon_only"/"hidden"）。
#[derive(gpui_kit::Action, Clone, PartialEq, Eq)]
#[action(namespace = hzip_gui, no_json, no_register)]
pub(crate) struct SetToolbarStyle {
    /// config 落盘值
    pub value: String,
}

/// 设置视图样式（T-MENU5；payload = "detail"/"icon"/"list"）。
#[derive(gpui_kit::Action, Clone, PartialEq, Eq)]
#[action(namespace = hzip_gui, no_json, no_register)]
pub(crate) struct SetViewStyle {
    /// config 落盘值
    pub value: String,
}
