use super::*;

/// 行高倍率：配置只管「更松」，低于字形盒一律夹到 [`crate::ui::constant::GLYPH_BOX_EM`]。
#[test]
fn line_height_em_clamps_at_glyph_box() {
    // 线程局部量：本用例独占当前线程，结束前复位（同 density 用例的约定）
    let restore = metrics().line_height();
    set_line_height(0.8);
    assert_eq!(line_height_em(), INK_BOX_EM, "低于墨迹盒应夹到下限");
    set_line_height(1.2);
    assert_eq!(line_height_em(), INK_BOX_EM);
    set_line_height(INK_BOX_EM);
    assert_eq!(line_height_em(), INK_BOX_EM, "恰好等于下限时不应被改动");
    set_line_height(2.0);
    assert_eq!(line_height_em(), 2.0, "比下限松的配置照用");
    set_line_height(restore);
}

/// 密度三档与 `labels::DENSITY` 白名单顺序互锁（下标即下拉下标 = 落盘值）。
#[test]
fn density_matches_label_whitelist() {
    let table = crate::app::labels::DENSITY;
    assert_eq!(
        table.len(),
        Density::ALL.len(),
        "档位数与 labels::DENSITY 不一致"
    );
    for (ix, (_, value)) in table.iter().enumerate() {
        assert_eq!(
            Density::from_index(ix).config_value(),
            *value,
            "第 {ix} 档 config 值与 labels::DENSITY 不一致"
        );
        assert_eq!(
            Density::from_config(value).index(),
            ix,
            "config 值 {value} 反查下标不为 {ix}"
        );
    }
}

/// 标准档下 `space()` 与尾风助手逐值等价——这就是「改造不改观感」的证据：
/// `.gap_2()` 原本 = `rems(0.5)`，现在 = `metrics().space(2.)` = `rems(0.5)`。
#[test]
fn sp_at_equals_tailwind_step_at_standard() {
    set_density(Density::Standard);
    for n in [0., 1., 2., 3., 4., 6.] {
        let got = metrics().space(n).0;
        let want = n * 0.25;
        assert!(
            (got - want).abs() < f32::EPSILON,
            "space({n}) = {got} rem，尾风助手为 {want} rem"
        );
    }
}

/// 密度只缩间距系数（含非整数步进；1 步进 = 4px @16px 字号）。
#[test]
fn space_scales_with_density() {
    set_density(Density::Compact);
    assert!((metrics().space(4.).0 - 0.75).abs() < 1e-6);
    set_density(Density::Loose);
    assert!((metrics().space(4.).0 - 1.40).abs() < 1e-6);
    set_density(Density::Standard);
}

/// 语义 token 必须落在基础刻度的对应步进上（防「token 与刻度漂移」）。
///
/// 宽松档与紧凑档都测：token 是 `space(n)` 的别名，系数改了两边一起变。
#[test]
fn semantic_tokens_track_the_base_scale() {
    for d in Density::ALL {
        set_density(d);
        let m = metrics();
        assert_eq!(m.gap_xs(), m.space(1.), "{d:?} gap_xs ≠ space(1)");
        assert_eq!(m.gap_sm(), m.space(2.), "{d:?} gap_sm ≠ space(2)");
        assert_eq!(m.gap_md(), m.space(3.), "{d:?} gap_md ≠ space(3)");
        assert_eq!(m.gap_lg(), m.space(4.), "{d:?} gap_lg ≠ space(4)");
        assert_eq!(m.gap_xl(), m.space(6.), "{d:?} gap_xl ≠ space(6)");
        assert_eq!(m.pad_page(), m.space(4.), "{d:?} pad_page ≠ space(4)");
        assert_eq!(m.pad_group(), m.space(4.), "{d:?} pad_group ≠ space(4)");
        assert_eq!(m.pad_cell(), m.space(1.), "{d:?} pad_cell ≠ space(1)");
    }
    set_density(Density::Standard);
}

/// 行间隙逐档硬像素（用户钦定：紧凑 0 / 标准 1 / 宽松 2）——目录树与
/// 文件列表每行的上下留白口径。与系数无关（不乘 scale）。
#[test]
fn row_gap_is_flat_three_step() {
    for (d, want) in [
        (Density::Compact, 0.),
        (Density::Standard, 1.),
        (Density::Loose, 2.),
    ] {
        set_density(d);
        assert_eq!(f32::from(metrics().row_gap()), want, "{d:?} 行间隙");
    }
    set_density(Density::Standard);
}

/// 行高随密度等比（标准档 = 32px 的既有基准）。
#[test]
fn row_heights_scale_with_density() {
    set_density(Density::Standard);
    assert_eq!(f32::from(metrics().row_band()), 32.);

    set_density(Density::Compact);
    assert!((f32::from(metrics().row_band()) - 24.).abs() < 0.01);

    set_density(Density::Loose);
    assert!(
        (f32::from(metrics().row_band()) - 44.8).abs() < 0.01,
        "宽松档行带应明显高于标准（32 → 44.8），否则用户「看不出差异」"
    );
    set_density(Density::Standard);
}

/// 密度 → 组件尺寸档：紧凑压到 `Small`，标准/宽松保持 `Medium`
/// （宽松不用 `Large`：那会把控件字号提到 text_base，密度不该动字号）。
#[test]
fn component_size_follows_density() {
    use gpui_kit::component::Size;

    set_density(Density::Compact);
    assert_eq!(
        metrics().component_size(),
        Size::Small,
        "紧凑档应压到 Small"
    );
    set_density(Density::Standard);
    assert_eq!(
        metrics().component_size(),
        Size::Medium,
        "标准档保持 Medium"
    );
    set_density(Density::Loose);
    assert_eq!(
        metrics().component_size(),
        Size::Medium,
        "宽松档保持 Medium"
    );
    set_density(Density::Standard);
}

/// `set_density` → `metrics()` 往返（读写一致；用例末尾复位标准档）。
#[test]
fn set_density_round_trips() {
    for d in [Density::Compact, Density::Loose, Density::Standard] {
        set_density(d);
        assert_eq!(metrics().density(), d);
        assert!((metrics().scale() - d.scale()).abs() < 1e-6);
    }
    set_density(Density::Standard);
    assert_eq!(density(), Density::Standard, "用例结束复位标准档");
}
