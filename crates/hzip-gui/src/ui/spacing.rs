//! 布局密度**提供器**（`LayoutMetrics`）与全部间距/内边距/行高的唯一出口。
//!
//! # 它是什么（对应「像主题那样统一」的要求）
//!
//! 主题（`Theme`）是**全局唯一**的模式量：`cx.theme().xxx` 一读，全界面同源；
//! 切换主题后所有样式统一变。布局密度要的是同一件事——只是它的作用面是
//! **间距/内边距/行高/控件固有尺寸**，而不是颜色。
//!
//! 于是本模块就是「布局的 Theme」：
//!
//! | 关注点 | 主题 | 布局密度 |
//! |---|---|---|
//! | 模式量 | `ThemeMode`（浅/深/跟随系统） | [`Density`]（紧凑/标准/宽松） |
//! | 读入口 | `cx.theme()`（gpui `Global`） | [`metrics()`]（本模块） |
//! | 具体值 | `theme.background` … | [`LayoutMetrics::gap_sm`] / [`LayoutMetrics::pad_menu_x`] / [`LayoutMetrics::row_band`] / [`LayoutMetrics::component_size`] … |
//! | 切换生效 | `theme::apply_theme_replay` + 重绘 | [`set_density`] + 重绘（泵消费 `SettingApplied(Density)`） |
//!
//! # 为什么不是 gpui `Global`（像 `Theme` 那样挂 `cx`）
//!
//! **元素构建期没有 `App`**：`.gap(...)`/`.px(...)` 是纯样式方法，`cx` 不在
//! 作用域内（这正是旧实现到处写死数字、密度管不到的原因）。而布局度量几乎只
//! 在构建样式时被读——若做成 `Global`，每个调用点都得把 `cx` 传进整条构建链，
//! 成本远大于收益。故实现为**UI 线程局部**的进程内唯一来源：语义与 `Theme`
//! 全局量一致（一处切、处处变），读写点也各自唯一（[`set_density`] 只有两个
//! 调用者：启动装配与主线程泵）。
//!
//! # 读什么（写 UI 时按语义取，不要逐处发明数字）
//!
//! - 间隔：[`gap_xs`] 4px、[`gap_sm`] 8px、[`gap_md`] 12px、[`gap_lg`] 16px、
//!   [`gap_xl`] 24px（值均为**标准档**下的像素数；实际值 = 该值 × 密度系数）
//! - 内边距：[`pad_page`] / [`pad_group`] / [`pad_cell`]
//! - 行高：文字行框 [`text_line_floor`] / [`line_height_em`]（配置 `[ui].line_height`，
//!   生效值夹到字形盒下限）；设置页行带 [`row_band`]
//! - 控件固有尺寸：[`component_size`]（组件库 `Size`：紧凑 = `Small`）
//! - 没有语义对应的一般间距：用基础刻度 [`space`]（= 旧 `sp(n)`，n/4 rem × 系数）
//!
//! # 密度系数
//!
//! | 档 | 系数 | 说明 |
//! |---|---|---|
//! | 紧凑 Compact | 0.75 | 密集排布（一行多选项）仍不挤字 |
//! | 标准 Standard | 1.00 | **既有观感**：切换不影响任何像素 |
//! | 宽松 Loose | 1.40 | 用户实测：1.2× 与标准「看不出差异」（1.1~1.2 的多数组件只差 1px），故取 1.4——肉眼可辨的最小步进 |
//!
//! # 契约
//!
//! - 密度只乘**间距/内边距/行高/控件固有尺寸**；字号、图标尺寸、描边宽度、
//!   圆角一律不动——「字体大小」是另一个旋钮（见 `ui::theme` 的 `font.size`）。
//! - 标准档下 [`space`] 与 gpui 尾风助手**逐值等价**（`.gap_2()` = `rems(0.5)`
//!   = `space(2.)`）⇒ 本次改造不改任何既有像素；回归锚点
//!   `sp_at_equals_tailwind_step_at_standard`。

use std::cell::Cell;

use gpui_kit::Rems;

use crate::ui::constant::{INK_BOX_EM, ROW_BAND_BASE, TAB_RAIL_W_BASE, TEXT_SLACK_PX};

pub use crate::viewmodel::density::Density;

/// 布局度量提供器（Copy；读到的就是**当前模式**下的具体值）。
///
/// 用法：`metrics().gap_sm()` / `metrics().component_size()`。不要缓存实例
/// 跨帧使用——模式切换后旧实例即过期（这正是每次现读的意义）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutMetrics {
    density: Density,
    scale: f32,
    /// 行高倍率（`[ui].line_height` 归一值；墨迹盒下限由 [`LayoutMetrics::text_line_floor`]
    /// 按字体度量夹，或由 [`line_height_em`] 按[`crate::ui::constant::GLYPH_BOX_EM`]常量夹）
    line_height: f32,
}

impl LayoutMetrics {
    /// 基础刻度：尾风步进 `n`（= `n/4` rem）× 当前密度。
    ///
    /// 步进取 `f32` 而非整数，是为了与 `.gap(...)`/`.px(...)` 同一签名族
    /// （读 `.gap(metrics().space(3.))` 一眼能对回原 `.gap_3()`）。没有语义
    /// token 可用时才用它——能命名的一律取命名 token（见模块文档）。
    pub fn space(&self, step: f32) -> Rems {
        gpui_kit::rems(step * 0.25 * self.scale)
    }

    /// 档位（供需要分支的少数场景：如组件库 `Size` 映射）。
    pub fn density(&self) -> Density {
        self.density
    }

    /// 密度系数（1.0 = 标准）。
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// 配置的行高倍率**原值**（未夹字形盒下限；生效值见 [`line_height_em`] /
    /// [`LayoutMetrics::text_line_floor`]）。
    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    // —— 语义间隔（标准档像素值见括号）——

    /// 4px：紧邻元素之间（图标与文字、勾选框与标签）。
    pub fn gap_xs(&self) -> Rems {
        self.space(1.)
    }

    /// 8px：同一行/同一组内的常规间隔。
    pub fn gap_sm(&self) -> Rems {
        self.space(2.)
    }

    /// 12px：组内小节之间。
    pub fn gap_md(&self) -> Rems {
        self.space(3.)
    }

    /// 16px：区块之间。
    pub fn gap_lg(&self) -> Rems {
        self.space(4.)
    }

    /// 24px：大区块之间（页面级留白）。
    pub fn gap_xl(&self) -> Rems {
        self.space(6.)
    }

    // —— 内边距 ——

    /// 页面/面板内容区留白（16px）。
    pub fn pad_page(&self) -> Rems {
        self.space(4.)
    }

    /// 分组框内容区内边距（16px）。
    pub fn pad_group(&self) -> Rems {
        self.space(4.)
    }

    /// 密集格子内边距（4px：文件关联勾选格、图标格）。
    pub fn pad_cell(&self) -> Rems {
        self.space(1.)
    }

    // —— 行高（px；与字号无关，只随密度）——

    /// 设置页行带基准高（标准 32px）——行内「首行」的垂直基准。
    pub fn row_band(&self) -> gpui_kit::Pixels {
        ROW_BAND_BASE * self.scale
    }

    /// 行间隙：目录树 / 文件列表**每行**的上下余量（树行内边距 y + 表格
    /// 单元格上/下 padding 的统一口径）。用户逐档钦定：紧凑 0、标准 1、
    /// 宽松 2px——**硬像素，不乘系数**（「行与行贴多紧」的手感值，不是间距刻度）。
    pub fn row_gap(&self) -> gpui_kit::Pixels {
        gpui_kit::px(match self.density {
            Density::Compact => 0.,
            Density::Standard => 1.,
            Density::Loose => 2.,
        })
    }

    /// 文字行框高（px；**字体度量精确版**）：`[ui].line_height` × 字号，夹到
    /// 「字体墨迹盒 + 上下各 [`TEXT_SLACK_PX`]」下限，向上取整。
    ///
    /// 字形盒下限不是可选项：行框小于它时字形下降部被 `overflow_hidden` 裁掉
    /// 用途：文件列表行（`row_h = 本值 + 2×row_gap`）、目录树行框、紧凑列表行。
    /// **行盒与文字行框必须取同一个返回值**（调用方把这个 px 值同时挂到行上），
    /// 否则又回到「行盒装不下文字行框」的老问题。
    ///
    /// # 为什么下限不是 `ascent + |descent|`
    ///
    /// `ascent + |descent|`（本机 16px：16.4 + 4.4 = 20.8px = 1.30em）只是字体
    /// **声明**的字身框；它**不覆盖全部墨迹** —— 字体真正的墨迹盒
    /// （`bounding_box`，全部字形叠加的外接矩形）实测上方 17.90px、下方 3.92px
    /// ⇒ 行框至少要 `(A+D) + 2·max(墨迹上−A, 墨迹下−D) = 20.8 + 2×1.5 = 23.8px
    /// ≈ 1.49em` 才装得下。
    ///
    /// 按 1.30em 排行框的后果正是用户实测那一幕：文件名里的 `_` 与带下降部的
    /// 字形**底部被裁**（紧凑档「`_` 根本看不见」、标准档「在最底部马上就要溢出」）。
    pub fn text_line_floor(
        &self,
        window: &gpui_kit::Window,
        font_px: gpui_kit::Pixels,
    ) -> gpui_kit::Pixels {
        let ts = window.text_system();
        let style = window.text_style();
        let id = ts.resolve_font(&style.font());
        let a = f32::from(ts.ascent(id, font_px));
        let d = f32::from(-ts.descent(id, font_px)); // 取正
        let bb = ts.bounding_box(id, font_px);
        // 本机口径（实测）：`origin.y` 为基线**下方**量、`origin.y + height` 为
        // 基线**上方**量（16px 实测 3.92 / 17.90）。
        let ink_up = f32::from(bb.origin.y + bb.size.height);
        let ink_down = f32::from(-bb.origin.y);
        // 「装得下」之外还要「不贴边」：墨迹盒恰好等于行框时，`_`/降部与行框底
        // 只剩不到 1px（用户实测「标准档 `_` 过于贴近行底部」）——上下各留
        // [`TEXT_SLACK_PX`] 的常量余量。它是**常量**：密度只调行内 padding，
        // 不调行框（用户口径）。
        let ink_need = (a + d) + 2. * (ink_up - a).max(ink_down - d).max(0.) + 2. * TEXT_SLACK_PX;
        let want = f32::from(font_px) * self.line_height;
        gpui_kit::px(want.max(ink_need)).ceil()
    }

    // —— 控件固有尺寸（组件库 `Size`）——

    /// 设置面板页签轨道宽（标准 **120px** × 密度系数）。
    pub fn tab_rail_w(&self) -> gpui_kit::Pixels {
        TAB_RAIL_W_BASE * self.scale
    }

    /// 组件库的固有尺寸档：紧凑 → `Small`，其余 → `Medium`。
    ///
    /// 这一层管的是 `space()` 碰不到的东西——按钮/下拉/开关/输入框/表格的行高
    /// 与内边距由 gpui-component 按 `Size` 算（`Size::table_row_height()` 等）。
    /// 宽松档**不用** `Size::Large`：那会把控件字号提到 `text_base`，而密度
    /// 不该动字号（宽松的「更松」由间距系数 1.4 与 [`Self::row_band`] 承担）。
    pub fn component_size(&self) -> gpui_kit::component::Size {
        match self.density {
            Density::Compact => gpui_kit::component::Size::Small,
            Density::Standard | Density::Loose => gpui_kit::component::Size::Medium,
        }
    }
}

thread_local! {
    /// 行高倍率 ×1000（定点；默认 = `[ui].line_height` 的出厂值）——UI 线程局部。
    static LINE_HEIGHT_MILLI: Cell<u32> =
        const { Cell::new((hzip_config::LINE_HEIGHT_DEFAULT * 1000.) as u32) };
    /// 当前密度系数 ×1000（定点；默认 1000 = 标准）——**UI 线程**局部。
    ///
    /// 为什么不是进程原子量：元素构建/布局只发生在 UI 线程（样式方法同步执行，
    /// gpui 的 render/layout/paint 都在主线程），写入点也只有一个。进程级量会
    /// 让**并行跑的测试互相看见对方的密度**（实测：密度用例切到紧凑档时，另一条
    /// 线程上的压缩参数窗点击用例按标准档几何点击而落空）。
    static DENSITY_MILLI: Cell<u32> = const { Cell::new(1000) };
}

/// 读**当前模式**的布局度量（像 `cx.theme()` 之于颜色）。
pub fn metrics() -> LayoutMetrics {
    let milli = DENSITY_MILLI.with(Cell::get);
    let density = Density::ALL
        .into_iter()
        .find(|d| ((d.scale() * 1000.).round() as u32) == milli)
        .unwrap_or_default();
    LayoutMetrics {
        density,
        scale: milli as f32 / 1000.,
        line_height: LINE_HEIGHT_MILLI.with(Cell::get) as f32 / 1000.,
    }
}

/// 设定当前线程的文字行高倍率（启动装配 + 设置重载）。
pub fn set_line_height(em: f32) {
    let milli = (em * 1000.).round().clamp(0., u32::MAX as f32) as u32;
    LINE_HEIGHT_MILLI.with(|c| c.set(milli));
}

/// 行高倍率（`relative(...)` 用的纯倍率版）：配置值夹到 [`crate::ui::constant::GLYPH_BOX_EM`] 下限。
///
/// 拿不到字体度量时的口径，与 [`LayoutMetrics::text_line_floor`]（精确版）语义一致：
/// **配置只能更松，不能更紧**。
pub fn line_height_em() -> f32 {
    let em = metrics().line_height;
    if em > INK_BOX_EM { em } else { INK_BOX_EM }
}

/// 单行文字行框下限（px）的简写（= [`LayoutMetrics::text_line_floor`]）。
pub fn text_line_floor(window: &gpui_kit::Window, font_px: gpui_kit::Pixels) -> gpui_kit::Pixels {
    metrics().text_line_floor(window, font_px)
}

/// 设定当前线程的布局密度（启动装配 + 泵消费 `SettingApplied(Density)`）。
pub fn set_density(density: Density) {
    let milli = (density.scale() * 1000.).round() as u32;
    DENSITY_MILLI.with(|c| c.set(milli));
}

/// 当前密度档（[`metrics().density()`](LayoutMetrics::density) 的简写）。
pub fn density() -> Density {
    metrics().density()
}

/// 当前密度系数（1.0 = 标准；[`metrics().scale()`](LayoutMetrics::scale) 的简写）。
pub fn density_scale() -> f32 {
    metrics().scale()
}

/// 基础刻度的简写（= [`metrics().space(n)`](LayoutMetrics::space)）。
///
/// 保留它是因为既有 106 处调用点写的是 `.gap(sp(2.))`：语义等价、零迁移风险。
/// **新代码优先取命名 token**（`metrics().gap_sm()` 等），只在没有语义对应时
/// 才用裸步进。
pub fn sp(n: f32) -> Rems {
    metrics().space(n)
}

/// 组件固有尺寸的简写（= [`metrics().component_size()`](LayoutMetrics::component_size)）。
pub fn component_size() -> gpui_kit::component::Size {
    metrics().component_size()
}

/// 设置面板竖排页签轨道的宽度（标准档 132px × 密度系数）。
///
/// 竖排页签（用户要求）后弹窗里多出一列：轨道宽度随密度缩，页内容宽度 = 弹窗
/// 宽 − 轨道宽 − 两列间距（故总宽调窄后页内容比原先窄一截，正合用户口径）。
pub fn settings_tab_rail_w() -> gpui_kit::Pixels {
    metrics().tab_rail_w()
}

// 页签轨宽 : 卡片宽 的目标占比（标准档 ≈ 120/760 = 15.8%）——锚点
// `settings_dialog_default_height_and_pinned_footer` 逐档断言两侧**同乘**密度
// 系数、占比恒定（用户五轮口径：密度只切整窗尺寸，不改 tabs 与右侧的左右占比）。

/// 行间隙的简写（= [`metrics().row_gap()`](LayoutMetrics::row_gap)）。
pub fn row_gap() -> gpui_kit::Pixels {
    metrics().row_gap()
}

/// 行带高的简写（= [`metrics().row_band()`](LayoutMetrics::row_band)）。
pub fn row_band() -> gpui_kit::Pixels {
    metrics().row_band()
}

/// 按配置装配全局密度（`run_ui` 启动序列调用一次）。
///
/// 缺这一步的症状：改过密度的用户**每次启动都回到标准档**，直到他去设置里
/// 再点一下（线程局部的初值只能来自启动装配）。
pub fn init_from_config() {
    let ui = hzip_config::Config::load().ui;
    set_density(Density::from_config(&ui.density));
    set_line_height(ui.line_height_em());
}

#[cfg(test)]
#[path = "tests/spacing.rs"]
mod tests;
