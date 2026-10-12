use super::*;
use hzip_codec::consts::*;

#[test]
fn dict_map_matches_02_table() {
    // 02 §6.3 表第 3 列：Fastest 64KB / Fast 1MB / Normal 16MB /
    // Maximum 64MB / Ultra 256MB（roundtrip 六级别矩阵见 tests/conformance）
    let cases = [
        (Level::Fastest, LZMA2_DICT_FASTEST as u32),
        (Level::Fast, LZMA2_DICT_FAST as u32),
        (Level::Normal, LZMA2_DICT_NORMAL as u32),
        (Level::Maximum, LZMA2_DICT_MAXIMUM as u32),
        (Level::Ultra, LZMA2_DICT_ULTRA as u32),
    ];
    for (lvl, dict) in cases {
        let o = lzma2_options(lvl, 0, None);
        assert_eq!(o.dict_size, dict, "{lvl:?}");
    }
}

#[test]
fn store_uses_min_dict_approx() {
    // Store 行（copy）：sevenz 写路径无 COPY coder → 4096 近似（rustdoc 记录）
    let o = lzma2_options(Level::Store, 0, None);
    assert_eq!(o.dict_size, LZMA2_DICT_MIN);
}

/// 「字典大小」覆写：`Some` 取代级别派生值，且钳在编码器合法域内。
///
/// 守的是 UI 里那个下拉真的生效——覆写被忽略（或方向搞反，级别反过来盖掉
/// 用户选择）时，用户看到的字典与实际写入的会不一致。
#[test]
fn dict_override_replaces_level_derived() {
    // 覆盖：标准档默认 16 MB，覆写成 1 MB
    let base = lzma2_options(Level::Normal, 0, None);
    assert_eq!(base.dict_size, LZMA2_DICT_NORMAL as u32, "基线随级别");
    let ov = lzma2_options(Level::Normal, 0, Some(1024 * 1024));
    assert_eq!(ov.dict_size, 1024 * 1024, "覆写应取代级别派生值");
    // 反向（更大）也成立——不是「只能调小」
    let ov_big = lzma2_options(Level::Fastest, 0, Some(64 * 1024 * 1024));
    assert_eq!(ov_big.dict_size, 64 * 1024 * 1024);

    // 钳制：下限 4096、上限 256 MiB（lzma-rust 合法域）
    assert_eq!(
        lzma2_options(Level::Normal, 0, Some(1)).dict_size,
        LZMA2_DICT_MIN
    );
    assert_eq!(
        lzma2_options(Level::Normal, 0, Some(u64::MAX)).dict_size,
        LZMA2_DICT_MAX
    );
    // 0 = 不清算为「Store 近似」以外的东西：与 None@Store 同值
    assert_eq!(
        lzma2_options(Level::Ultra, 0, Some(0)).dict_size,
        LZMA2_DICT_MIN
    );
}
