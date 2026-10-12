//! 全部 label↔value 白名单映射收口（T-GUI T12）。
//!
//! 原则：**一处定义，双向映射，测试互锁**。此前同一语义的 label↔value
//! 映射散落 dialogs/state/settings_layout 三处，靠镜像测试防漂移；本模块
//! 收拢后镜像测试改锚此处。
//!
//! | 白名单 | label（UI 显示） | value（config 落盘值） |
//! |---|---|---|
//! | [`FORMAT`] | zip/7z/… | 同 label |
//! | [`LEVEL`] | 存储/最快/… | store/fastest/… |
//! | [`CONFLICT`] | 询问/覆盖/… | ask/overwrite/… |
//! | [`CODEPAGE`] | 自动/GBK/… | auto/gbk/…（首项 None） |
//! | [`LANG`] | 跟随系统/… | system/zh-CN/en |
//! | [`THEME`] | 跟随系统/浅色/深色 | system/light/dark |
//! | [`DENSITY`] | 紧凑/标准/宽松 | compact/standard/loose |
//! | [`FILE_ICONS`] | 与系统一致/内置 | system/builtin |
//! | [`METHOD`] | Deflate/… | deflate/bzip2/lzma2/zstd |
//! | [`SPLIT`] | 不分卷/10 MB/… | none/10m/…/custom |
//! | [`THREADS`] | 自动/1/… | 0/1/…（0 = 自动） |
//! | [`ENC_METHOD`] | 无加密/ZipCrypto/… | none/zipcrypto/aes128/aes256 |
//! | [`DICT`] | 自动/64 KB/… | auto/64k/1m/…/256m |
//! | [`UPDATE_MODE`] | 新建档案/追加到现有档案 | new/append |
//!
//! `value_of` / `index_of_value` 供双向映射；未知值回落 0（向后兼容旧配置）。

/// 白名单行（**label i18n key** + 落盤 value）。
///
/// 第一列是 key 而非文案：下拉候选的展示文字在**渲染时**查表求值，运行时
/// 切语言即可生效（`SelectState` 的候选项本身是构建期快照，见
/// `settings_layout::SettingsPanel::reload` 的重建逻辑）。
/// 第二列是 config 契约值——**永不本地化**（`store_*` 写盘、`index_of_value` 回读都用它）。
pub type Pair = (&'static str, &'static str);

/// 压缩格式（label == value；与 T41 `COMPRESS_FORMATS` 对齐）。
pub const FORMAT: [Pair; 11] = [
    ("opt.format.zip", "zip"),
    ("opt.format.7z", "7z"),
    ("opt.format.tar", "tar"),
    // 组合格式（tar + 编码器）：core 走两遍法写 `.tar.gz` 一步到位
    ("opt.format.tar.gz", "tar.gz"),
    ("opt.format.tar.bz2", "tar.bz2"),
    ("opt.format.tar.xz", "tar.xz"),
    ("opt.format.tar.zst", "tar.zst"),
    ("opt.format.gz", "gz"),
    ("opt.format.bz2", "bz2"),
    ("opt.format.xz", "xz"),
    ("opt.format.zst", "zst"),
];

/// 界面可选项 = [`FORMAT`] 的**单格式子集**（2026-09 用户要求：组合格式
/// `tar.gz`/`tar.bz2`/`tar.xz`/`tar.zst` 先在界面屏蔽——两遍法产物尚不稳妥，
/// 待修好再放回 [`FORMAT`] 即自动露出）。
///
/// 下标域规则：**消费 `Index(usize)` 的 UI 面（压缩参数窗格式下拉、设置-默认
/// 格式下拉）一律以本表为界**；[`FORMAT`] 仅剩 config 契约用途
/// （`store_format` 写盘值、`index_of_value_ci` 读回、file picker 扩展清单）。
/// 两表重叠区（前 3 项）下标一致，故旧快照/旧配置的下标不需迁移。
pub const UI_FORMAT: [Pair; 7] = [
    ("opt.format.zip", "zip"),
    ("opt.format.7z", "7z"),
    ("opt.format.tar", "tar"),
    ("opt.format.gz", "gz"),
    ("opt.format.bz2", "bz2"),
    ("opt.format.xz", "xz"),
    ("opt.format.zst", "zst"),
];

/// 压缩级别。
pub const LEVEL: [Pair; 6] = [
    ("opt.level.store", "store"),
    ("opt.level.fastest", "fastest"),
    ("opt.level.fast", "fast"),
    ("opt.level.normal", "normal"),
    ("opt.level.maximum", "maximum"),
    ("opt.level.ultra", "ultra"),
];

/// 冲突策略（value 与 hzip-cli `CONFLICTS` / 02 §8.1 / `dialog_vm::conflict_from_str`
/// 同值域——注册表落盘值即此列）。
pub const CONFLICT: [Pair; 5] = [
    ("opt.conflict.ask", "ask"),
    ("opt.conflict.overwrite", "overwrite"),
    ("opt.conflict.skip", "skip"),
    ("opt.conflict.rename", "rename"),
    ("opt.conflict.newer", "newer"),
];

/// 文件名编码（首项「自动」= None 语义，由消费方特判）。
pub const CODEPAGE: [Pair; 7] = [
    ("opt.codepage.auto", "auto"),
    ("opt.codepage.gbk", "gbk"),
    ("opt.codepage.big5", "big5"),
    ("opt.codepage.sjis", "sjis"),
    ("opt.codepage.euc_kr", "euc-kr"),
    ("opt.codepage.cp1252", "cp1252"),
    ("opt.codepage.utf8", "utf-8"),
];

/// 设置-语言。
pub const LANG: [Pair; 3] = [
    ("opt.lang.system", "system"),
    ("opt.lang.zh_cn", "zh-CN"),
    ("opt.lang.en", "en"),
];

/// 设置-主题（顺序 = `ThemeModeChoice` 下标）。
pub const THEME: [Pair; 3] = [
    ("opt.theme.system", "system"),
    ("opt.theme.light", "light"),
    ("opt.theme.dark", "dark"),
];

/// 设置-布局密度（顺序 = `ui::spacing::Density::ALL` 下标；
/// 单测 `spacing::tests::density_matches_label_whitelist` 互锁两表）。
pub const DENSITY: [Pair; 3] = [
    ("opt.density.compact", "compact"),
    ("opt.density.standard", "standard"),
    ("opt.density.loose", "loose"),
];

/// 工具栏三态（T-TVS2 内容语义：图标+文字/仅图标/隐藏；菜单/设置双入口。
/// 旧值 large/small 由 `toolbar_style_of` 无感回落 icon_text）。
pub const TOOLBAR_STYLE: [Pair; 3] = [
    ("opt.toolbar_style.icon_text", "icon_text"),
    ("opt.toolbar_style.icon_only", "icon_only"),
    ("opt.toolbar_style.hidden", "hidden"),
];

/// 视图样式（T-MENU5：detail 默认；icon/list 归 T-MENU9 启用）。
pub const VIEW_STYLE: [Pair; 3] = [
    ("opt.view_style.detail", "detail"),
    ("opt.view_style.icon", "icon"),
    ("opt.view_style.list", "list"),
];

/// 文件类型图标来源（T-ICON 决策 #1：`system` 默认 = 与系统文件管理器逐类型
/// 一致；`builtin` = 只用自有 lucide 图标，关闭后零平台调用）。
pub const FILE_ICONS: [Pair; 2] = [
    ("opt.file_icons.system", "system"),
    ("opt.file_icons.builtin", "builtin"),
];

/// 压缩方式（02 §6.3；value = `hzip_codec::traits::CodecMethod::as_str`）。
///
/// 候选**实际可用子集**由格式 `FormatCaps::methods` 决定——本表只提供
/// label↔value 映射，不表达「哪个格式支持哪个方式」（那是格式层的事实）。
pub const METHOD: [Pair; 4] = [
    ("opt.method.deflate", "deflate"),
    ("opt.method.bzip2", "bzip2"),
    ("opt.method.lzma2", "lzma2"),
    ("opt.method.zstd", "zstd"),
];

/// 分卷大小预设（value = `none` / `NNNm` / `custom`；字节换算见
/// `dialog_vm::split_from_str`）。
pub const SPLIT: [Pair; 7] = [
    ("opt.split.none", "none"),
    ("opt.split.10m", "10m"),
    ("opt.split.100m", "100m"),
    ("opt.split.700m", "700m"),
    ("opt.split.1g", "1g"),
    ("opt.split.4g", "4g"),
    ("opt.split.custom", "custom"),
];

/// 线程数（value = 十进制字符串；`0` = 自动，与 `CompressSpec.threads` 同语义）。
pub const THREADS: [Pair; 6] = [
    ("opt.threads.auto", "0"),
    ("opt.threads.1", "1"),
    ("opt.threads.2", "2"),
    ("opt.threads.4", "4"),
    ("opt.threads.8", "8"),
    ("opt.threads.16", "16"),
];

/// 加密方式（value 与 `hzip_format::EncryptionCaps` / `EncryptionRequest` 对齐：
/// `none` = 该格式无加密能力；`zipcrypto` = zip 传统加密；`aes128`/`aes256` = WinZip AES）。
pub const ENC_METHOD: [Pair; 4] = [
    ("opt.enc.none", "none"),
    ("opt.enc.zipcrypto", "zipcrypto"),
    ("opt.enc.aes128", "aes128"),
    ("opt.enc.aes256", "aes256"),
];

/// LZMA2 字典大小（value = `auto` / `NNNk|m`；字节换算见 `dialog_vm::dict_from_str`）。
///
/// 越出编码器合法域（4096 B ~ 256 MiB）由 7z 写侧钳制；本表不列超限档
/// （列了也会被钳到同一值，等于假选项）。
pub const DICT: [Pair; 7] = [
    ("opt.dict.auto", "auto"),
    ("opt.dict.64k", "64k"),
    ("opt.dict.1m", "1m"),
    ("opt.dict.4m", "4m"),
    ("opt.dict.16m", "16m"),
    ("opt.dict.64m", "64m"),
    ("opt.dict.256m", "256m"),
];

/// 更新方式（value 与 `CompressSpec.append` 对齐：`new` = 新建、`append` = 追加）。
pub const UPDATE_MODE: [Pair; 2] = [("opt.update.new", "new"), ("opt.update.append", "append")];

/// label **key** 序列（Select 下拉显示顺序 = 下标）。
pub const fn label_keys<const N: usize>(table: [Pair; N]) -> [&'static str; N] {
    let mut out = [""; N];
    let mut i = 0;
    while i < N {
        out[i] = table[i].0;
        i += 1;
    }
    out
}

/// value → 下标（未知值回落 0——向后兼容旧配置）。
pub fn index_of_value(table: &[Pair], value: &str) -> usize {
    table.iter().position(|(_, v)| *v == value).unwrap_or(0)
}

/// 下标 → value（越界回落 0 项 value——防守白名单漂移）。
pub fn value_of(table: &[Pair], ix: usize) -> &'static str {
    table.get(ix).map(|(_, v)| *v).unwrap_or(table[0].1)
}

/// 下标 → 本地化展示文案（越界回落 0 项）。
pub fn label_of(table: &[Pair], ix: usize) -> String {
    crate::tr(table.get(ix).map(|(k, _)| *k).unwrap_or(table[0].0))
}

/// label key 序列的本地化文案（Select 候选项）。
pub fn labels_of(table: &[Pair]) -> Vec<String> {
    table.iter().map(|(k, _)| crate::tr(k)).collect()
}

/// value 序列（下标 = Select 白名单下标）。
pub const fn values<const N: usize>(table: [Pair; N]) -> [&'static str; N] {
    let mut out = [""; N];
    let mut i = 0;
    while i < N {
        out[i] = table[i].1;
        i += 1;
    }
    out
}

#[cfg(test)]
#[path = "tests/labels.rs"]
mod tests;
