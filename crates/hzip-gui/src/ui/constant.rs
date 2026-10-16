//! 界面**布局与颜色的默认值常量表**（用户 2026-09-21：界面代码里不得再写布局字面量）。
//!
//! # 它是什么
//!
//! hzip-gui 里每一个「尺寸 / 颜色」类的默认值，只有本模块一处定义。视图、对话框、
//! 横条、自绘 widget 一律引用这里的常量（`constant::DIALOG_W`），**不许**再就地写
//! `px(420.)` / `hsla(0., 0., 0., 0.32)` ——否则同一个语义在多个文件各抄一遍，
//! 改一处漏三处（这次收敛前：对话框默认宽 420 在 4 个文件里各写一次，
//! 窗口默认尺寸 6 处，列宽 6 处）。
//!
//! # 分工（三个出口，别串门）
//!
//! | 关注点 | 出口 | 例 |
//! |---|---|---|
//! | 界面尺寸/颜色的**基准值** | **本模块** `ui::constant` | `DIALOG_W`、`COL_NAME_W`、`SCRIM` |
//! | 随布局密度缩放的间距/内边距/行高/控件固有尺寸 | [`crate::ui::spacing`]（`metrics()` / `sp()`） | `metrics().gap_sm()` |
//! | 随主题变的颜色 | [`crate::ui::theme`]（`cx.theme()`） | `cx.theme().border` |
//!
//! 本模块的量**不随密度、不随主题**变（窗口与对话框尺寸、固定横条高、列宽、
//! 图标像素档、圆角、遮罩透明度…）。少数「基准值 × 密度系数」的量（设置卡片
//! 宽高、标题/底部定高）也定义在这里：数值仍只有一个定义处，乘法由消费方做
//! （`Pixels * f32`）。
//!
//! # 约定
//!
//! - 像素值一律 `Pixels` 常量（`px()` 是 `const fn`）⇒ 调用点写 `.w(COL_NAME_W)`，
//!   不再包一层 `px(...)`。只有参与**纯数值运算**的量用 `f32`：字形盒/行高倍率
//!   （`_EM`）、行高余量 [`TEXT_SLACK_PX`]（与字体度量 `f32` 相加）、透明度
//!   （`_ALPHA`）、相对宽（`_REL`）、密度步进数（`_STEPS`）。
//! - 命名 = 语义 + 量纲后缀（`_W` / `_H` / `_ALPHA` / `_EM` / `_STEPS`）；
//!   同一语义的「默认 / 最小」成对出现（`PARAMS_WINDOW_W` / `PARAMS_WINDOW_MIN_W`）。
//! - **结构性字面量不进本表**：`px(0.)`（显式清零，如弹层归零的 `.p(px(0.))`）、
//!   `.border_1()`（1px 已经写在组件库方法名里）不是「可调的默认值」，留在
//!   调用点更贴近语义。测试里的期望像素也不进（断言本来就该自证期望值）。

use gpui_kit::{Hsla, Pixels, px};

// ===================== 窗口默认尺寸 =====================
//
// 独立窗口的「开窗默认包围盒 + 最小尺寸」；由各自的 `open_*_window` 消费
// （`WindowOptions.window_bounds` / `window_min_size`）。值取**视口居中的逻辑像素**，
// 与显示器 DPI 无关（gpui `Bounds::centered` 自己换算）。

/// 主窗口默认尺寸（`shell::runtime` 建窗；1180×760 = 浏览页两栏 + 表格 6 列的舒适宽）。
pub const MAIN_WINDOW_W: Pixels = px(1180.);
/// 主窗口默认高。
pub const MAIN_WINDOW_H: Pixels = px(760.);

/// 压缩/解压进度窗默认宽（T-PW）。
pub const PROGRESS_WINDOW_W: Pixels = px(560.);
/// 压缩/解压进度窗默认高。
pub const PROGRESS_WINDOW_H: Pixels = px(520.);
/// 压缩/解压进度窗最小宽（可缩放，下限保证概要与输出区都还能看）。
pub const PROGRESS_WINDOW_MIN_W: Pixels = px(460.);
/// 压缩/解压进度窗最小高。
pub const PROGRESS_WINDOW_MIN_H: Pixels = px(400.);

/// 压缩/解压**参数窗**默认宽（基本/高级/加密三段表单）。
pub const PARAMS_WINDOW_W: Pixels = px(560.);
/// 压缩/解压参数窗默认高。
pub const PARAMS_WINDOW_H: Pixels = px(660.);
/// 压缩/解压参数窗最小宽。
pub const PARAMS_WINDOW_MIN_W: Pixels = px(520.);
/// 压缩/解压参数窗最小高。
pub const PARAMS_WINDOW_MIN_H: Pixels = px(560.);

/// 请求-应答提示窗（密码/冲突）默认宽——冲突一行 4 按钮 + 复选框，留足不溢出。
pub const PROMPT_WINDOW_W: Pixels = px(560.);
/// 请求-应答提示窗默认高（密码态只有一行输入 + 底部按钮，刻意小巧）。
pub const PROMPT_WINDOW_H: Pixels = px(230.);

/// 「正在打开档案」进度窗默认宽（内容只有一行 spinner + 档案名）。
pub const OPEN_PROGRESS_WINDOW_W: Pixels = px(520.);
/// 「正在打开档案」进度窗默认高。
pub const OPEN_PROGRESS_WINDOW_H: Pixels = px(180.);
/// 「正在打开档案」进度窗最小宽。
pub const OPEN_PROGRESS_WINDOW_MIN_W: Pixels = px(460.);
/// 「正在打开档案」进度窗最小高。
pub const OPEN_PROGRESS_WINDOW_MIN_H: Pixels = px(160.);

/// 标准消息弹窗默认宽（图标 + 标题 + 可滚动详情 + 关闭按钮）。
pub const MESSAGE_WINDOW_W: Pixels = px(560.);
/// 标准消息弹窗默认高。
pub const MESSAGE_WINDOW_H: Pixels = px(320.);
/// 标准消息弹窗最小宽（再窄标题就换行到不成样子）。
pub const MESSAGE_WINDOW_MIN_W: Pixels = px(400.);
/// 标准消息弹窗最小高（保证详情至少露出两行）。
pub const MESSAGE_WINDOW_MIN_H: Pixels = px(200.);

/// 预览窗默认宽（文本/HEX 行都要放得下：行宽 + 右侧滚动条）。
pub const PREVIEW_WINDOW_W: Pixels = px(720.);
/// 预览窗默认高。
pub const PREVIEW_WINDOW_H: Pixels = px(520.);

// ===================== 主窗口横条（chrome）=====================

/// 菜单条定高（`flex_shrink_0`，不许被下方工具栏挤走——见 `shell::mod` 注释）。
pub const MENUBAR_H: Pixels = px(28.);

// ===================== 对话框 =====================
//
// 宽度按**档位**给（同一个尺寸档被多个对话框共用；同一档今天同值、明天要一起
// 微调时只改一处）。各档的构成口径见 06 §对话框。

/// 窄档：字段最少（密码输入）。
pub const DIALOG_W_NARROW: Pixels = px(400.);
/// 常规档：单列表单 + 摘要（关于、编辑确认、覆盖确认、压缩参数）。
pub const DIALOG_W: Pixels = px(420.);
/// 中档：选项文案更长（编辑「冲突时怎么办」三选一）。
pub const DIALOG_W_MID: Pixels = px(440.);
/// 宽档：两列表单或带详情列表（冲突确认、编辑失败详情）。
pub const DIALOG_W_WIDE: Pixels = px(460.);

/// 表单标签列宽（`Form::horizontal().label_width(..)`，T-G56 对齐口径）。
pub const FORM_LABEL_W: Pixels = px(96.);

/// 1px 细线（两方向共用）：对话框分区标题右侧的横向分隔线、设置面板竖排页签
/// 与内容之间的竖线——都是「一道 hairline」，一个常量够。
pub const HAIRLINE: Pixels = px(1.);

/// 工具栏**拆分按钮**（split button）中间分隔竖线的高度。
///
/// 拆分按钮 = 主动作半 + 下拉箭头半（用户口径 2026-09-29：「合到一起，
/// 不要看起来像两个按钮」）：两半相贴、中间一条随主题边框色的竖线（宽 = [`HAIRLINE`]）。
/// 取固定值而非按按钮高换算——按钮高随密度在 24~32px 间变化时它仍视觉居中。
pub const SPLIT_DIVIDER_H: Pixels = px(16.);

/// 压缩参数「自定义分卷大小」的数字输入宽（`NumberInput` 默认太宽，会顶掉单位前缀）。
pub const SPLIT_INPUT_W: Pixels = px(88.);

// ===================== 设置面板（卡片）=====================
//
// 以下四个是**标准档基准值**，实际值 = 基准 × `spacing::density_scale()`
// （用户四轮口径：设置弹窗默认尺寸也随布局密度走）。

/// 设置卡片默认宽基准（标准 820 / 紧凑 615 / 宽松 1148）。
pub const CARD_W_BASE: Pixels = px(820.);
/// 设置卡片默认高基准（高度用默认值**固定**，不按内容收短；内容过长则滚动）。
pub const CARD_H_BASE: Pixels = px(600.);
/// 设置卡片标题栏定高基准（× 密度系数）。
pub const CARD_TITLE_H: Pixels = px(52.);
/// 设置卡片底部按钮行定高基准（× 密度系数）。
pub const CARD_FOOTER_H: Pixels = px(48.);

/// 卡片上下各 1px 边框合计（正文高 = 卡片高 − chrome − 本值）。
pub const CARD_BORDER_H: Pixels = px(2.);
/// 卡片贴视口的最小留白（宽、高各自夹到 `视口 − 本值`）。
pub const CARD_VIEWPORT_MARGIN: Pixels = px(48.);
/// 卡片默认纵向位置的上移量（`视口/10` 的结果再夹到 `视口 − 本值`）。
pub const CARD_VIEWPORT_TOP_INSET: Pixels = px(160.);

/// 竖排页签**轨道宽**基准（标准 120px × 密度系数）。
pub const TAB_RAIL_W_BASE: Pixels = px(120.);
/// 设置页左侧**名称列**宽基准（标准 140px × 密度系数；走查 #11：110 → 140）。
pub const LABEL_COL_W_BASE: Pixels = px(140.);
/// 设置页**行带**基准高（标准 32px × 密度系数）——行内「首行」的垂直基准。
pub const ROW_BAND_BASE: Pixels = px(32.);

/// 竖排页签选中态的左侧竖直指示条宽（只靠底色对比不够，见 `settings_layout` 注释）。
pub const TAB_RAIL_MARK_W: Pixels = px(2.);
/// 竖排页签选中态指示条高。
pub const TAB_RAIL_MARK_H: Pixels = px(14.);
// ===================== 浏览页 =====================
//
// 表格列缺省宽（用户没拖过列宽时用它；拖过则以记忆值为准——R-GUI4 #1）。

/// 名称列缺省宽。
pub const COL_NAME_W: Pixels = px(280.);
/// 类型列缺省宽。
pub const COL_TYPE_W: Pixels = px(96.);
/// 大小列缺省宽。
pub const COL_SIZE_W: Pixels = px(88.);
/// 压缩后列缺省宽。
pub const COL_PACKED_W: Pixels = px(88.);
/// 编码列缺省宽。
pub const COL_ENCODING_W: Pixels = px(72.);
/// 修改时间列缺省宽。
pub const COL_MTIME_W: Pixels = px(160.);
/// 压缩级别列缺省宽。
pub const COL_LEVEL_W: Pixels = px(72.);
/// 加密算法列缺省宽。
pub const COL_ENCRYPTION_W: Pixels = px(88.);
/// CRC 校验列缺省宽。
pub const COL_CRC_W: Pixels = px(88.);
/// 文件属性列缺省宽。
pub const COL_ATTRS_W: Pixels = px(120.);

/// 左栏目录树可拖宽下限。
pub const TREE_W_MIN: Pixels = px(160.);
/// 左栏目录树可拖宽上限。
pub const TREE_W_MAX: Pixels = px(600.);

/// 列表单元格**左右**内边距基准（标准 8px × 密度系数；上下用 `row_gap()`）。
pub const CELL_GUTTER_BASE: Pixels = px(8.);

/// 过滤输入框定宽（面包屑吃满剩余宽度，过滤框收在行尾——2026-09-15 用户要求）。
pub const FILTER_INPUT_W: Pixels = px(200.);

/// 图标视图单元格宽。
pub const ICON_CELL_W: Pixels = px(96.);
/// 图标视图的图标像素档（kit `Size` 档位到 `Large` = 24px 封顶，桌面图标视图要更大）。
pub const ICON_CELL_ICON: Pixels = px(32.);

/// 列表行高补偿：kit 给行挂 `border_b_1()`，而 gpui 是 border-box（边框算进行高）
/// ⇒ 不减这 1px，单元格内容盒就比文字行框矮 1px（文字上下各被裁 0.5px）。
pub const LIST_ROW_BORDER_H: Pixels = px(1.);

// ===================== 任务中心页 =====================

/// 任务表格列的最小可拖宽。
pub const JOB_COL_MIN_W: Pixels = px(48.);
/// 任务表格「进度」列里进度条的定宽。
pub const JOB_PROGRESS_W: Pixels = px(110.);

// ===================== 独立窗内容 / 自绘 widget =====================

/// 进度窗输出框的最小高（吃满剩余空间时的「防塌陷」保底）。
pub const OUTPUT_BOX_MIN_H: Pixels = px(120.);
/// 等宽文本行高：进度窗输出行、预览文本/HEX 行共用（预览仍走固定行高虚拟列表）。
pub const MONO_ROW_H: Pixels = px(20.);
/// 进度窗输出列表的**上下过绘高度**（`ListState` 的 overdraw 参数）。
///
/// 输出行高可变（长路径/长原因折行 ⇒ 20px 的整数倍），过绘留出十余行，
/// 滚动时不会因「下一行比预估高」而闪烁或跳位。
pub const OUTPUT_LIST_OVERDRAW: Pixels = px(240.);
/// 独立窗内容列的最大宽（软约束、居中；窗口再宽内容也不再拉长）。
pub const CONTENT_MAX_W: Pixels = px(720.);
/// 进度窗内容列的最大高。
pub const PROGRESS_CONTENT_MAX_H: Pixels = px(640.);
/// 状态栏任务药丸里那条细进度条的相对宽（`relative(..)`，0~1）。
pub const PILL_PROGRESS_REL: f32 = 0.25;

/// 复选框**文字相对方框的光学下沉量**（px；本机主题字体、标签字号 16px）。
///
/// # 推导（全部取自真实字体度量，不是随手取数）
///
/// 标签行框 `L = 16 × 1.5 = 24`，字身框 `F = ascent + |descent| = 16.4 + 4.4 = 20.8`
/// ⇒ 字身框在行框内居中，基线 `= (L − F)/2 + ascent = 18.0`。
/// 眼睛看的是**字面**而非字身框：拉丁小写字面落在 x-height 带
/// （`x_height = 8.256`）内，其中心在基线上方 `x_height/2 = 4.13` ⇒ 字面中心
/// `= 18.0 − 4.13 = 13.87`，而方框中心 = 行框中心 `12` ⇒ **方框偏高 1.87px**。
/// 中文标签同理（CJK 字面在字身框内亦偏下，用户实测两处都偏高）。
///
/// 对策见 [`crate::ui::widgets::checkbox`]：标签块下内边距 `2 × 本值`（行高随标签
/// 块增长、方框随行垂直居中 ⇒ 恰好把方框下移本值）。
///
/// # 观感校准（用户两轮实测）
///
/// | 取值 | 用户观察 |
/// |---|---|
/// | 0（= 只按字身框居中） | 「checkbox 高一些，文字低一些」——明显 |
/// | 1.87（= 理论值，x-height 带中心） | 「checkbox 比文字稍微低了一点点」——很细微 |
///
/// ⇒ 理论值略过冲（拉丁小写字面在 x-height 带内偏下、而数字/大写只到 cap 带，
/// 同一排标签混合时眼睛取的是两者的折中），故取 **1.5**（理论值与 cap 带对齐值
/// 12.42→nudge 0.42 之间、靠近理论的一侧）。
///
/// 回归锚点：`ui::widgets::checkbox::tests::box_centers_on_text_ink` 用**实时字体
/// 度量**重算理论期望值并比对（容差 0.75px 覆盖这段观感校准；换字体/改字号后
/// 该用例会给出新的期望值，失败即提示重算）。
pub const CHECKBOX_OPTICAL_SHIFT: f32 = 1.5;

/// 数字输入的值文本最小宽度（单位是**密度步进**，不是 px：`sp(6.)` = 1.5rem）。
pub const NUMBER_VALUE_MIN_STEPS: f32 = 6.;

// ===================== 行高 / 字形盒（`spacing` 的基准值）=====================

/// 字形盒倍率的保守上界（em）：本机主题字体实测 `ascent + |descent| ≈ 1.3em`
/// （Segoe UI @16px = 16.4 + 4.4 = 20.8px）。
///
/// 只在**拿不到 `Window`**（构建相对行高 `relative(...)` 时）用它夹下限：
/// 那边是纯倍率、解析交给文本系统，故只能用常量兜底。回归锚点
/// `settings_layout::tests::assoc_labels_fit_font_metrics` 用**真实字体度量**
/// 断言渲染后的标签行框 ≥ `ascent + |descent|`——换字体后若常量不够大，该用例
/// 会失败并提示改这个数。
pub const GLYPH_BOX_EM: f32 = 1.35;

/// 行框在「字体墨迹盒」之外**上下各留**的常量余量（px）。
///
/// 为什么不能停在墨迹盒：墨迹盒是「全部字形叠加的外接矩形」，行框 == 墨迹盒时
/// `_`、降部与行框边缘重合（本机 16px 实测间隙 < 1px），肉眼就是「贴边/被裁」
/// （用户逐档实测：紧凑档 `_` 看不见、标准档「在最底部马上就要溢出」、宽松档正常
/// ——三档差的正是这点余量 + 行内 padding）。取 2px：16px 字号下行框 = 墨迹盒
/// 23.8 + 4 = 27.8 → 28px 的观感与「宽松档正常」一致，且与字号同比可接受。
///
/// 与密度**无关**（用户口径：紧凑/标准/宽松只调行内上下 padding）。
pub const TEXT_SLACK_PX: f32 = 2.;

/// 设置页相对行高兜底用的「墨迹盒 + 余量」倍率（em）：`12/8` = 1.5em
/// （本机 16px 实测墨迹盒 21.82px = 1.364em，+ 4px = 1.61em；取 1.6 保守）。
pub const INK_BOX_EM: f32 = 1.6;

// ===================== 颜色默认值 =====================
//
// 只有**不属于主题**的裸色值放这里（主题色一律走 `cx.theme()`）。`Hsla` 字段全
// 公开 ⇒ 可直接 const 构造。

/// 设置弹窗自绘遮罩的黑色（32% 不透明）：kit 的 `.overlay(false)` 后由 `settings_layout`
/// 自己铺一层，点它 = 关闭。
pub const SCRIM: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.,
    a: 0.32,
};

/// 全透明黑：清掉 kit 弹层自带的底色（`.bg(..)` 归零），不影响父层。
pub const TRANSPARENT_BLACK: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.,
    a: 0.,
};

/// 选中行底色相对主题 accent 的不透明度（`selection_row_bg`：浅色下不刺眼、深色下可辨）。
pub const SELECTION_ROW_BG_ALPHA: f32 = 0.30;
