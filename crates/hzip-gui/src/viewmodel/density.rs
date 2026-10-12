//! 布局密度（紧凑/标准/宽松）——设置-常规 ▸ 布局的**纯数据**半场。
//!
//! # 为什么在 viewmodel 层
//!
//! 依赖方向是 `ui → app → viewmodel` 单向（07 §3 红线）：密度既要被
//! [`crate::app::settings_registry`]（注册表默认值/load/store）引用，也要被
//! [`crate::ui::spacing`]（间距刻度）引用 ⇒ 定义必须落在两者都能看见的最低层。
//! 本模块**不依赖 gpui**（纯枚举 + 常量），UI 侧的实际缩放见 `ui::spacing`。
//!
//! # 契约
//!
//! - 枚举顺序 = 设置页下拉下标 = [`crate::app::labels::DENSITY`] 顺序
//!   （互锁单测 `ui::spacing::tests::density_matches_label_whitelist`）。
//! - `config_value()` 是**落盘契约值**（永不本地化）；展示文案在
//!   `locales/*.toml` 的 `opt.density.*`。

/// 布局密度三档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    /// 紧凑：0.75× 间距。
    Compact,
    /// 标准：1.0×（默认——**既有界面即此档**，切到它不改变观感）。
    #[default]
    Standard,
    /// 宽松：1.4× 间距（用户实测：1.2× 与标准「看不出差异」——多数组件只差
    /// 1~2px；1.4 是肉眼可辨的最小步进）。
    Loose,
}

impl Density {
    /// 全档位（下标 = [`Self::index`]）。
    pub const ALL: [Self; 3] = [Self::Compact, Self::Standard, Self::Loose];

    /// 密度系数（下标同 [`Self::ALL`]）。
    ///
    /// 取值由来：0.75 让「一行多个勾选项」这类密集排布明显收拢、字仍不挤；
    /// 1.4 是「明显更松」的最小可见步进（1.2 在多数组件上只差 1~2px，用户实测
    /// 「和标准看不出差异」）。
    const SCALES: [f32; 3] = [0.75, 1.0, 1.4];

    /// 设置页下拉下标。
    pub fn index(self) -> usize {
        match self {
            Self::Compact => 0,
            Self::Standard => 1,
            Self::Loose => 2,
        }
    }

    /// 下标 → 档位（越界回落标准——旧快照/坏配置不 panic）。
    pub fn from_index(ix: usize) -> Self {
        Self::ALL.get(ix).copied().unwrap_or_default()
    }

    /// config 契约值（写盘/读回；**永不本地化**）。
    pub fn config_value(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Standard => "standard",
            Self::Loose => "loose",
        }
    }

    /// config 值 → 档位（ASCII 大小写不敏感；未知/空 = 标准）。
    pub fn from_config(value: &str) -> Self {
        let v = value.trim();
        Self::ALL
            .into_iter()
            .find(|d| d.config_value().eq_ignore_ascii_case(v))
            .unwrap_or_default()
    }

    /// 间距系数（1.0 = 标准）。
    pub fn scale(self) -> f32 {
        Self::SCALES[self.index()]
    }
}

#[cfg(test)]
#[path = "tests/density.rs"]
mod tests;
