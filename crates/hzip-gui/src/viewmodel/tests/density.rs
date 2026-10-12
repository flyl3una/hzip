use super::*;

/// 未知 config 值回落标准（手改配置/旧值不 panic、不改变默认观感）。
#[test]
fn unknown_density_falls_back_to_standard() {
    for bad in ["", "  ", "COMPACT-XX", "密集", "1.5"] {
        assert_eq!(
            Density::from_config(bad),
            Density::Standard,
            "{bad:?} 应回落标准"
        );
    }
}

/// 大小写不敏感（手改配置常见 `Compact`/`LOOSE`）。
#[test]
fn density_parse_is_case_insensitive() {
    assert_eq!(Density::from_config("Compact"), Density::Compact);
    assert_eq!(Density::from_config("LOOSE"), Density::Loose);
    assert_eq!(Density::from_config(" standard "), Density::Standard);
}

/// 系数单调（紧凑 < 标准 < 宽松）且标准恰好 1.0。
#[test]
fn scales_are_ordered_and_standard_is_one() {
    assert!(Density::Compact.scale() < Density::Standard.scale());
    assert!(Density::Standard.scale() < Density::Loose.scale());
    assert_eq!(Density::Standard.scale(), 1.0);
}

/// 下标往返（面板 Index ↔ 档位）。
#[test]
fn index_round_trips() {
    for (ix, d) in Density::ALL.iter().enumerate() {
        assert_eq!(d.index(), ix);
        assert_eq!(Density::from_index(ix), *d);
    }
    assert_eq!(Density::from_index(9), Density::Standard, "越界回落");
}
