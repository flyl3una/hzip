//! UI 测试清单（T-G00 骨架态：`#[ignore]` 占位，保留 TC 编号；旧框架冒烟测试
//! 不搬运）。
//!
//! 各卡新增测试落各自文件；本文件承载跨模块集成冒烟（T-G30 实体化）与
//! 尚未接入实现面的占位用例。**占位 panic 不算失败**（`#[ignore]`）。

// crate 根函数（非任何业务模块）的用例：`init_locale` 的语言落地（T-ICON 拍板3）。
mod locale;

// 卷层文案表完整性（T-IMG）：每个 `vol.*` 键两种语言都必须有文案。
mod vol_locale;

// TC-GUI-001：启动冒烟（AppShell 3 s 内建好无 panic）——T-G30 实体化。
#[test]
#[ignore = "T-G30: AppShell 实体化后启用"]
fn tc_gui_001_startup_smoke() {
    todo!("T-G30: smoke_startup")
}

// TC-GUI-002：黄金档案打开 → BrowseRow 数 == expected——T-G30 实体化。
#[test]
#[ignore = "T-G30: smoke_open_archive"]
fn tc_gui_002_open_archive() {
    todo!("T-G30: smoke_open_archive")
}

// TC-GUI-004：压缩流（默认参数 → 任务行 → 完成 → 产物可再打开）——T-G30。
#[test]
#[ignore = "T-G30: smoke_compress_flow"]
fn tc_gui_004_compress_flow() {
    todo!("T-G30: smoke_compress_flow")
}

// TC-GUI-005：密码弹窗三态 + 重试上限 + 取消语义——T-G30。
#[test]
#[ignore = "T-G30: smoke_password_dialog"]
fn tc_gui_005_password_dialog() {
    todo!("T-G30: smoke_password_dialog")
}

// 冲突 apply-all 决策缓存（02 §11.4）——T-G30。
#[test]
#[ignore = "T-G30: smoke_conflict_apply_all"]
fn tc_gui_conflict_apply_all() {
    todo!("T-G30: smoke_conflict_apply_all")
}

// 二实例转发 → 队列收到路径（02 §11.2）——T-G30。
#[test]
#[ignore = "T-G30: single_instance_forward"]
fn tc_gui_single_instance_forward() {
    todo!("T-G30: single_instance_forward")
}

// 运行中关窗 ≤2 s + TempArea 清理（02 §5.3.9）——T-G30。
#[test]
#[ignore = "T-G30: exit_grace_cleanup"]
fn tc_gui_exit_grace_cleanup() {
    todo!("T-G30: exit_grace_cleanup")
}

// TC-GUI-006：预览三视图渲染不 panic——T-G12 实体化。
#[test]
#[ignore = "T-G12: preview_three_views"]
fn tc_gui_006_preview_views() {
    todo!("T-G12: 三视图渲染冒烟")
}

// TC-GUI-007：50k 行虚拟滚动 p95 < 33 ms（release 手动）——T-G20 实体化。
#[test]
#[ignore = "T-G20: virtual_browse_scroll（release + --ignored）"]
fn tc_gui_007_virtual_scroll() {
    todo!("T-G20: virtual_browse_scroll")
}

// TC-GUI-008：主题/语言切换即时生效——T-G30（语言挂账 i18n）。
#[test]
#[ignore = "T-G30: theme_switch"]
fn tc_gui_008_theme_switch() {
    todo!("T-G30: 主题切换冒烟")
}
