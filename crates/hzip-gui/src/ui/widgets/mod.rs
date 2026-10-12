//! 自绘 widgets（T-GUI T10 归位：gpui-kit 没有的自绘件；原 components/）。
//!
//! - [`icon`]：AppIcon 业务图标收口（kit 无业务图标，必须保留）
//! - [`status`]：busy 条/状态栏
//! - [`breadcrumb`]：面包屑条（kit Breadcrumb 包装）
//! - [`checkbox`]：复选框封装（修正 kit 行高写死导致的下降部裁切）
//! - [`number`]：数值步进控件（设置-常规 ▸ 界面字号）
//! - [`file_icon`]：文件类型图标（系统图标 + 回退；T-ICON）

pub mod breadcrumb;
pub mod checkbox;
pub mod file_icon;
pub mod icon;
pub mod number;
pub mod status;

pub use checkbox::checkbox;
pub use icon::AppIcon;
pub use number::number_stepper;
