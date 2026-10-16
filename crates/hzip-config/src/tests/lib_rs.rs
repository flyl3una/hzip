use super::*;

fn tmp() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("tmpdir")
}

/// 环境变量互斥：HZIP_CONFIG 为进程级全局，涉改测试必须串行
/// （否则 set_var/remove_var 竞争 → 间歇性写错路径/丢失路径）。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn defaults_match_b1() {
    let c = Config::defaults();
    assert_eq!(c.defaults.format, "zip");
    assert_eq!(c.defaults.level, "normal");
    assert_eq!(c.security.max_ratio, 200);
    assert_eq!(c.security.max_file_gib, 32);
    assert_eq!(c.extract.conflict, "ask");
    assert_eq!(c.ui.theme, "system");
    assert_eq!(c.logging.level, "info");
    assert_eq!(c.logging.retention_days, 7);
    assert_eq!(c.jobs.max_parallel, 4);
    assert_eq!(c.preview.max_text_kb, 256);
    assert_eq!(c.ui.font_family, "");
    assert_eq!(c.ui.font_size, FONT_SIZE_DEFAULT);
    assert_eq!(c.ui.density, DENSITY_DEFAULT);
    // 快捷键：出厂 = 全空串 = 全部走内置默认键位（T-KEYS）
    assert_eq!(c.keys, Keys::default());
}

/// 圆角弧度归一：0 是**合法值**（直角），越界回落默认（0），界内原样。
#[test]
fn corner_radius_normalizes_to_default() {
    let c = Config::defaults();
    assert_eq!(c.ui.corner_radius_px(), CORNER_RADIUS_DEFAULT);
    assert_eq!(c.ui.corner_radius_px(), 0, "默认就是直角");
    for good in [CORNER_RADIUS_MIN, 1, 6, CORNER_RADIUS_MAX] {
        let mut c = Config::defaults();
        c.ui.corner_radius = good;
        assert_eq!(c.ui.corner_radius_px(), good, "界内值 {good} 应原样");
    }
    for bad in [CORNER_RADIUS_MAX + 1, u32::MAX] {
        let mut c = Config::defaults();
        c.ui.corner_radius = bad;
        assert_eq!(
            c.ui.corner_radius_px(),
            CORNER_RADIUS_DEFAULT,
            "越界 {bad} 应回落默认"
        );
    }
}

/// `[ui].line_height` 归一：漏键 / NaN / 越界 → 默认；有效值原样。
#[test]
fn line_height_normalizes_to_default() {
    let c = Config::defaults();
    assert_eq!(c.ui.line_height_em(), LINE_HEIGHT_DEFAULT);
    for bad in [0.0, -1.0, 0.5, 99.0, f32::NAN, f32::INFINITY] {
        let mut c = Config::defaults();
        c.ui.line_height = bad;
        assert_eq!(
            c.ui.line_height_em(),
            LINE_HEIGHT_DEFAULT,
            "越界/非法值 {bad} 应回落默认"
        );
    }
    let mut c = Config::defaults();
    c.ui.line_height = 1.8;
    assert_eq!(c.ui.line_height_em(), 1.8);
}

/// 手改配置里的 `[ui].line_height` 能被读进来（用户口径：配置文件可配）。
#[test]
fn parses_line_height_from_toml() {
    let d = tmp();
    let p = d.path().join("c.toml");
    std::fs::write(&p, "[ui]\nline_height = 1.75\n").expect("write");
    assert_eq!(Config::load_from(&p).ui.line_height, 1.75);
    // 旧配置文件（无此键）回落默认
    let p2 = d.path().join("c2.toml");
    std::fs::write(&p2, "[ui]\ntheme = \"dark\"\n").expect("write");
    assert_eq!(Config::load_from(&p2).ui.line_height, LINE_HEIGHT_DEFAULT);
}

/// 在线更新检查地址（用户口径：**由配置文件指定**——换源不必重新打包）。
#[test]
fn update_check_url_comes_from_config() {
    // 默认指向本项目仓库的发布元数据端点（迁托管时这里必须跟着改，见 T-LIC §4.1）
    assert_eq!(
        Config::defaults().update.check_url,
        "https://api.github.com/repos/flyl3una/hzip/releases/latest"
    );
    // 用户改写生效（原样读入，不做归一）
    let d = tmp();
    let p = d.path().join("c.toml");
    std::fs::write(
        &p,
        "[update]\ncheck_url = \"https://mirror.example.org/hzip.json\"\n",
    )
    .expect("write");
    assert_eq!(
        Config::load_from(&p).update.check_url,
        "https://mirror.example.org/hzip.json"
    );
    // 旧配置文件（无 `[update]` 节）回落默认
    let p2 = d.path().join("c2.toml");
    std::fs::write(&p2, "[ui]\ntheme = \"dark\"\n").expect("write");
    assert_eq!(
        Config::load_from(&p2).update.check_url,
        Config::defaults().update.check_url
    );
}

#[test]
fn parses_toml_with_partial_keys() {
    let d = tmp();
    let p = d.path().join("c.toml");
    std::fs::write(&p, "[ui]\ntheme = \"dark\"\n").expect("write");
    let c = Config::load_from(&p);
    assert_eq!(c.ui.theme, "dark");
    // 缺键回落默认
    assert_eq!(c.logging.level, "info");
    assert_eq!(c.defaults.format, "zip");
}

#[test]
fn unknown_keys_ignored() {
    let d = tmp();
    let p = d.path().join("c.toml");
    std::fs::write(&p, "[ui]\ntheme=\"light\"\n[future]\nwhatever=1\n").expect("write");
    let c = Config::load_from(&p);
    assert_eq!(c.ui.theme, "light");
}

#[test]
fn corrupt_file_falls_back() {
    let d = tmp();
    let p = d.path().join("c.toml");
    std::fs::write(&p, "not [ valid toml ==\n").expect("write");
    let c = Config::load_from(&p);
    assert_eq!(c.logging.level, "info");
}

#[test]
fn ensure_default_writes_template() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let d = tmp();
    let cfg_path = d.path().join("cfg/config.toml");
    // Rust 2024 set_var unsafe
    unsafe { std::env::set_var("HZIP_CONFIG", &cfg_path) };
    let created = Config::ensure_default_config();
    unsafe { std::env::remove_var("HZIP_CONFIG") };
    assert!(created, "首次应新建");
    assert!(cfg_path.exists(), "模板应落盘");
    let content = std::fs::read_to_string(&cfg_path).expect("read");
    assert!(content.contains("[ui]"), "模板含 [ui]");
    // 二次调用不覆盖
    let d2 = tmp();
    let _ = d2;
    std::fs::write(&cfg_path, "[ui]\ntheme=\"dark\"\n").expect("write");
    unsafe { std::env::set_var("HZIP_CONFIG", &cfg_path) };
    let again = Config::ensure_default_config();
    unsafe { std::env::remove_var("HZIP_CONFIG") };
    assert!(!again, "已存在不覆盖");
    let content2 = std::fs::read_to_string(&cfg_path).expect("read");
    assert!(content2.contains("dark"), "用户内容保留");
}

/// 异步写盘（审计 B6）：`save_async` 返回后**不保证**已落盘；
/// `flush_pending_saves` 之后必须已在盘上，且中间态只留最新一份。
#[test]
fn save_async_coalesces_and_flush_persists_latest() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let d = tmp();
    let cfg_path = d.path().join("cfg/config.toml");
    unsafe { std::env::set_var("HZIP_CONFIG", &cfg_path) };
    let mut a = Config::defaults();
    a.ui.tree_width = 111;
    a.save_async();
    let mut b = Config::defaults();
    b.ui.tree_width = 222;
    b.save_async();
    let flushed = Config::flush_pending_saves();
    unsafe { std::env::remove_var("HZIP_CONFIG") };
    assert!(flushed, "flush 应在上限内完成");
    let text = std::fs::read_to_string(&cfg_path).expect("read");
    assert!(text.contains("222"), "落盘的是**最新一份**：{text}");
    assert!(!text.contains("111"), "中间态不落盘：{text}");
    // 再等一会儿也不该有残留（写线程已退出、待写槽已空）
    assert!(Config::flush_pending_saves(), "空闲时应立即返回 true");
}

/// 配置缓存（审计 B11/B12）：同一次运行内重复 `load()` 不重复读盘，
/// 但**外部改写后立刻可见**（mtime/size 守卫）。
#[test]
fn load_cache_sees_external_edit() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let d = tmp();
    let cfg_path = d.path().join("cfg/config.toml");
    std::fs::create_dir_all(cfg_path.parent().expect("parent")).expect("mkdir");
    unsafe { std::env::set_var("HZIP_CONFIG", &cfg_path) };
    std::fs::write(&cfg_path, "[ui]\ntree_width = 300\n").expect("write");
    assert_eq!(Config::load().ui.tree_width, 300);
    assert_eq!(Config::load().ui.tree_width, 300, "第二次命中缓存");
    // 外部进程改写（模拟）：内容 + 长度都变 → 守卫失效，立刻可见
    std::fs::write(&cfg_path, "[ui]\ntree_width = 4242\n").expect("rewrite");
    assert_eq!(Config::load().ui.tree_width, 4242, "外部改写必须立刻可见");
    unsafe { std::env::remove_var("HZIP_CONFIG") };
}

#[test]
fn template_parses_and_matches_builtin_defaults() {
    // 模板须可解析，且**与内置默认逐字段一致**（单一来源，只允许注释差异）。
    //
    // 这条不变式曾被破坏：模板写着 `logging.dir = "logs"`，代码默认却是 `""`
    // ——用户照着模板改配置，日志却落在平台目录，表现为「配置了 logs 却没有
    // 日志」（实测）。任何字段漂移都必须在这里失败，而不是等用户去猜。
    let parsed: Config = toml::from_str(DEFAULT_TEMPLATE).expect("模板可解析");
    assert_eq!(parsed.defaults.format, "zip");
    assert_eq!(parsed.logging.retention_days, 7);
    assert_eq!(
        toml::to_string(&parsed).expect("模板可序列化"),
        toml::to_string(&Config::defaults()).expect("默认可序列化"),
        "etc/hzip.toml 与内置默认漂移（模板是单一来源，注释之外不得有差异）"
    );
}

/// 界面字号归一：0 / 越界 → 默认 16（手改配置常见 `font_size = 0`）。
///
/// 归一落在 config 层而非 GUI：CLI/其他消费方读同一份配置时不该各自解释
/// 「0 是什么意思」——一处定义、一处兜底。
#[test]
fn font_size_px_normalizes_out_of_range() {
    let mut c = Config::defaults();
    c.ui.font_size = 0;
    assert_eq!(c.ui.font_size_px(), FONT_SIZE_DEFAULT, "0 → 默认");
    c.ui.font_size = 9;
    assert_eq!(c.ui.font_size_px(), FONT_SIZE_DEFAULT, "低于下界 → 默认");
    c.ui.font_size = 999;
    assert_eq!(c.ui.font_size_px(), FONT_SIZE_DEFAULT, "高于上界 → 默认");
    c.ui.font_size = FONT_SIZE_MIN;
    assert_eq!(c.ui.font_size_px(), FONT_SIZE_MIN, "下界本身有效");
    c.ui.font_size = FONT_SIZE_MAX;
    assert_eq!(c.ui.font_size_px(), FONT_SIZE_MAX, "上界本身有效");
    c.ui.font_size = 18;
    assert_eq!(c.ui.font_size_px(), 18, "区内原样");
}

/// `[ui].file_type_icons`（T-ICON 决策 #1）：默认 `system`；旧配置缺键回落默认；
/// 并**照抄同结构 `lang`/`theme`/`view_style` 的惯例**——本层直存原字符串、
/// 不做白名单归一（大小写不敏感解析与未知/空值→`system` 的裁定在消费方）。
///
/// 用例即契约：干净值原样读回；脏值亦**原样保留**（若这里改成「写回默认」，
/// 就与 `theme`/`lang` 的直存语义分叉，旧配置里的自定义值会被悄悄抹掉）。
#[test]
fn file_type_icons_defaults_to_system_and_stores_verbatim() {
    // ① 默认配置
    assert_eq!(Config::defaults().ui.file_type_icons, "system");

    let d = tmp();

    // ② 缺键的旧 TOML：反序列化回落默认，不报错
    let p = d.path().join("old.toml");
    std::fs::write(&p, "[ui]\ntheme = \"dark\"\n").expect("write");
    let c = Config::load_from(&p);
    assert_eq!(c.ui.theme, "dark", "同文件其余键照常生效");
    assert_eq!(c.ui.file_type_icons, "system", "缺键回落默认");

    // ③ 脏值：本层直存（与 theme/lang/view_style 一致），不发明第二套惯例
    for raw in ["SYSTEM", "nonsense", ""] {
        let p = d.path().join("dirty.toml");
        std::fs::write(&p, format!("[ui]\nfile_type_icons = \"{raw}\"\n")).expect("write");
        assert_eq!(
            Config::load_from(&p).ui.file_type_icons,
            raw,
            "本层直存：{raw:?} 不应被改写"
        );
    }

    // ④ 合法值读回
    let p = d.path().join("builtin.toml");
    std::fs::write(&p, "[ui]\nfile_type_icons = \"builtin\"\n").expect("write");
    assert_eq!(Config::load_from(&p).ui.file_type_icons, "builtin");
}

/// 新增的 `[ui]` 键在**旧配置文件**里缺席时回落默认（向后兼容：老用户
/// 的 config.toml 没有这三键，启动不该报错、也不该改变既有观感）。
#[test]
fn legacy_toml_without_font_keys_loads() {
    let d = tmp();
    let p = d.path().join("c.toml");
    std::fs::write(&p, "[ui]\ntheme = \"dark\"\n").expect("write");
    let c = Config::load_from(&p);
    assert_eq!(c.ui.theme, "dark");
    assert_eq!(c.ui.font_family, "");
    assert_eq!(c.ui.font_size_px(), FONT_SIZE_DEFAULT);
    assert_eq!(c.ui.density, DENSITY_DEFAULT);
}

// T-G54：旧 TOML（缺 [ui] 节/缺新键）可读 + 新键回落默认（向后兼容）
#[test]
fn legacy_toml_without_ui_node_loads() {
    let d = tmp();
    let p = d.path().join("c.toml");
    std::fs::write(
        &p,
        "[defaults]\nformat = \"zip\"\n[logging]\nlevel = \"warn\"\n",
    )
    .expect("write");
    let c = Config::load_from(&p);
    assert_eq!(c.defaults.format, "zip");
    assert_eq!(c.logging.level, "warn");
    // 缺失 ui 新键回落默认
    assert_eq!(c.ui.theme, "system");
    assert_eq!(c.ui.default_format, "zip");
    assert_eq!(c.ui.default_level, "normal");
    assert_eq!(c.ui.columns.order, Columns::default().order);
    // 2026-09-25：hidden 出厂默认含五新列（level/encryption/crc/attrs/comment）
    // ——「这几个列数据默认不显示」的用户口径
    assert_eq!(c.ui.columns.hidden, Columns::default().hidden);
}

// T-G54：save → load round-trip（ui.columns 等新键持久化）
#[test]
fn save_then_load_roundtrip() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let d = tmp();
    let cfg_path = d.path().join("nested/cfg/config.toml");
    unsafe { std::env::set_var("HZIP_CONFIG", &cfg_path) };
    let mut c = Config::defaults();
    c.ui.theme = "dark".into();
    c.ui.default_format = "7z".into();
    c.ui.default_level = "ultra".into();
    c.ui.columns.order = vec![
        "name".into(),
        "mtime".into(),
        "size".into(),
        "packed".into(),
        "encoding".into(),
    ];
    c.ui.columns.hidden = vec!["encoding".into(), "packed".into()];
    c.save().expect("save 应成功（父目录自动创建）");
    unsafe { std::env::remove_var("HZIP_CONFIG") };

    assert!(cfg_path.exists(), "save 应落盘");
    // 落盘文本必须含新键（用户口径：设置项要能在配置文件里看到——
    // 「配了却看不见」等于不可配；`ui.recent_archives` 的走查同此）
    let text = std::fs::read_to_string(&cfg_path).expect("read");
    assert!(
        text.contains("line_height"),
        "落盘配置必须写出 [ui].line_height，实际：{text}"
    );
    let loaded = Config::load_from(&cfg_path);
    assert_eq!(loaded.ui.line_height, LINE_HEIGHT_DEFAULT);
    assert_eq!(loaded.ui.theme, "dark");
    assert_eq!(loaded.ui.default_format, "7z");
    assert_eq!(loaded.ui.default_level, "ultra");
    assert_eq!(loaded.ui.columns, c.ui.columns);
}

// T-G54：save 覆盖已有文件（幂等二写）
#[test]
fn save_overwrites_existing() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let d = tmp();
    let cfg_path = d.path().join("config.toml");
    std::fs::write(&cfg_path, "junk [ not valid").expect("write");
    unsafe { std::env::set_var("HZIP_CONFIG", &cfg_path) };
    Config::defaults().save().expect("save 覆盖");
    unsafe { std::env::remove_var("HZIP_CONFIG") };
    let loaded = Config::load_from(&cfg_path);
    assert_eq!(loaded.ui.theme, "system", "覆盖后应为新内容");
}

/// 写后立读（2026-10-09 用户实报「设置字号/圆角点一次不生效、再点一次才生效」）：
/// `save_async` 入队即须在**本进程**可见——GUI 的持久化尾步是 `save_async`，而
/// 生效半场（`Config::load()`）/菜单 cfg 缓存**紧接着**就读。旧实现要等后台线程
/// 落盘（实测 200/200 读到旧值）⇒ 每次点击差一拍。
#[test]
fn load_sees_value_enqueued_by_save_async() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let d = tmp();
    let cfg_path = d.path().join("cfg/config.toml");
    std::fs::create_dir_all(cfg_path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&cfg_path, "[ui]\nfont_size = 16\n").expect("write");
    unsafe { std::env::set_var("HZIP_CONFIG", &cfg_path) };

    // 预热：与 GUI 稳态同形（文件已存在 + 进程内缓存已建）
    assert_eq!(Config::load().ui.font_size, 16);

    // ① 文件已存在：入队后立读 = 新值
    let mut c = Config::load();
    c.ui.font_size = 17;
    c.save_async();
    assert_eq!(
        Config::load().ui.font_size,
        17,
        "save_async 入队后立读必须是新值（否则设置差一拍）"
    );
    assert!(Config::flush_pending_saves(), "flush 应在上限内完成");
    assert_eq!(Config::load().ui.font_size, 17, "落盘后仍是同一份");

    // ② 首次运行（盘上还没有文件）：入队后立读也不得回落默认值
    let cfg_new = d.path().join("first/config.toml");
    unsafe { std::env::set_var("HZIP_CONFIG", &cfg_new) };
    assert!(!cfg_new.exists(), "前置：盘上无文件");
    let mut c2 = Config::load();
    c2.ui.font_size = 19;
    c2.save_async();
    assert_eq!(
        Config::load().ui.font_size,
        19,
        "首次运行保存后立读不得回落默认值"
    );
    assert!(Config::flush_pending_saves());
    assert_eq!(Config::load().ui.font_size, 19);

    unsafe { std::env::remove_var("HZIP_CONFIG") };
}

// ===== [keys] 快捷键（T-KEYS；切片 A） =====

/// 冻结契约：14 个动作字段（顺序 = 设置页「快捷键」页签行序 = T-KEYS §2 表）。
const KEYS_FIELDS: [&str; 14] = [
    "open",
    "extract",
    "new_window",
    "select_all",
    "select_invert",
    "copy",
    "paste",
    "find",
    "test_integrity",
    "settings",
    "show_jobs",
    "refresh",
    "delete",
    "quit",
];

/// 各字段的内置默认键位（GUI `keymap::ACTIONS::default_spec` 的同一批字面量；
/// 模板注释里写的就是这批）。本 crate 不认识键位语义，这里只用来守住模板不漂移。
const KEYS_DEFAULTS: [(&str, &str); 14] = [
    ("open", "ctrl-o"),
    ("extract", "ctrl-e"),
    ("new_window", "ctrl-shift-n"),
    ("select_all", "ctrl-a"),
    ("select_invert", "ctrl-i"),
    ("copy", "ctrl-c"),
    ("paste", "ctrl-v"),
    ("find", "ctrl-f"),
    ("test_integrity", "ctrl-t"),
    ("settings", "ctrl-,"),
    ("show_jobs", "ctrl-j"),
    ("refresh", "f5"),
    ("delete", "delete"),
    ("quit", "ctrl-q"),
];

/// 取出 `DEFAULT_TEMPLATE` 里 `[keys]` 段的字段行（模板要求**全部注释掉**），
/// 返回 `(键名, 去掉行首注释符后的整行)`；同时断言段内没有解注释的赋值行。
fn template_keys_assignments() -> Vec<(String, String)> {
    let is_assignment = |body: &str| -> Option<String> {
        let name: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let rest = body[name.len()..].trim_start();
        (!name.is_empty() && rest.starts_with('=')).then_some(name)
    };
    let mut out: Vec<(String, String)> = Vec::new();
    let mut in_keys = false;
    for line in DEFAULT_TEMPLATE.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_keys = t == "[keys]";
            continue;
        }
        if !in_keys {
            continue;
        }
        match t.strip_prefix('#') {
            Some(rest) => {
                let body = rest.trim();
                if let Some(name) = is_assignment(body) {
                    out.push((name, body.to_string()));
                }
            }
            // 模板是**给人手改的初始状态**：全默认 ⇒ 每一行都该是注释
            None => assert!(
                is_assignment(t).is_none() || t.is_empty(),
                "[keys] 模板不得预置解注释的字段行：{t:?}"
            ),
        }
    }
    out
}

#[test]
fn keys_field_unknown_key_is_none() {
    let mut k = Keys::default();
    // 未知键名 → None（不 panic、不猜一个「最像」的字段）
    assert_eq!(k.field("nope"), None);
    assert_eq!(k.field(""), None);
    assert_eq!(k.field("delete_all"), None);
    // 大小写敏感：TOML 键名就是小写下划线字面量，不做大小写折叠
    assert_eq!(k.field("OPEN"), None);
    assert!(k.field_mut("nope").is_none());
    assert!(k.field_mut("").is_none());
    assert!(k.field_mut("SelectAll").is_none());
}

#[test]
fn keys_field_roundtrip_all_known_keys() {
    let mut k = Keys::default();
    for (i, name) in KEYS_FIELDS.iter().enumerate() {
        let raw = format!("ctrl-{i}");
        *k.field_mut(name).unwrap_or_else(|| panic!("未知键 {name}")) = raw.clone();
        assert_eq!(k.field(name), Some(raw.as_str()), "{name} 写入后应读回");
    }
    // 两个 match（field / field_mut）的键名集合必须一致：换个前缀逐个再验一遍
    for (i, name) in KEYS_FIELDS.iter().enumerate() {
        *k.field_mut(name).expect("field_mut 认识该键") = format!("alt-{i}");
        assert_eq!(k.field(name), Some(format!("alt-{i}").as_str()));
    }
    // 序列化键名 + 顺序 = 冻结字段表（改字段名/换顺序会在这里失败；
    // 顺序即设置页行序，GUI 那边不再单独维护一份）
    let text = toml::to_string(&Keys::default()).expect("序列化");
    let names: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split('=').next())
        .map(str::trim)
        .collect();
    assert_eq!(names, KEYS_FIELDS, "Keys 字段名/顺序与契约漂移");
}

/// 三态原样存取：`""`（内置默认）/ `"none"`（禁用）/ 自定义串——**逐字**往返，
/// 本层不做任何归一（非法串回落默认是 GUI 消费方的运行时裁定）。
#[test]
fn keys_three_state_roundtrips_verbatim() {
    let mut k = Keys::default();
    k.field_mut("select_all").expect("键存在").clear(); // "" = 内置默认
    *k.field_mut("select_invert").expect("键存在") = "none".into(); // 禁用
    *k.field_mut("open").expect("键存在") = "ctrl-alt-x".into(); // 自定义
    *k.field_mut("quit").expect("键存在") = "kbd-nonsense-".into(); // 非法串也直存

    // ① 结构层往返
    let text = toml::to_string(&k).expect("序列化");
    assert_eq!(toml::from_str::<Keys>(&text).expect("反序列化"), k);

    // ② 整份配置落盘再读（GUI 的真实路径：save → 冷启动 load）
    let mut cfg = Config::defaults();
    cfg.keys = k.clone();
    let d = tmp();
    let p = d.path().join("cfg.toml");
    std::fs::write(&p, toml::to_string(&cfg).expect("序列化")).expect("write");
    let back = Config::load_from(&p);
    assert_eq!(back.keys, k, "三态不得被归一或改写");
    assert_eq!(back.keys.field("select_all"), Some(""), "空串 = 用内置默认");
    assert_eq!(back.keys.field("select_invert"), Some("none"), "禁用哨兵");
    assert_eq!(back.keys.field("open"), Some("ctrl-alt-x"));
    assert_eq!(back.keys.field("quit"), Some("kbd-nonsense-"));
}

/// 旧配置（没有 `[keys]` 节）必须静默回落默认，而不是报错/丢整份配置。
#[test]
fn keys_section_absent_or_partial_falls_back_to_default() {
    // ① 完全没有 [keys] 节
    let c: Config = toml::from_str("[ui]\ntheme = \"dark\"\n").expect("解析");
    assert_eq!(c.keys, Keys::default(), "缺 [keys] ⇒ 全默认（全空串）");
    assert_eq!(c.ui.theme, "dark", "同文件其余节照常生效");

    // ② 只写了一个字段：其余各自回落
    let c: Config = toml::from_str("[keys]\nopen = \"none\"\n").expect("解析");
    assert_eq!(c.keys.field("open"), Some("none"));
    assert_eq!(c.keys.field("extract"), Some(""));

    // ③ 未知动作名忽略（旧版本删掉的动作 / 用户手滑）
    let c: Config =
        toml::from_str("[keys]\nopen = \"f2\"\nfuture_action = \"ctrl-y\"\n").expect("解析");
    assert_eq!(c.keys.field("open"), Some("f2"));

    // ④ 空表也算「写了但什么都没设」
    let c: Config = toml::from_str("[keys]\n").expect("解析");
    assert_eq!(c.keys, Keys::default());
}

/// 模板（编译期内嵌单一来源）必须覆盖全部 14 个动作，且注释里给出的默认键位与
/// 冻结表一致——用户照着模板手改时看到的就是「恢复默认」会回到的那条键位。
#[test]
fn default_template_documents_all_keys() {
    assert!(
        DEFAULT_TEMPLATE.lines().any(|l| l.trim() == "[keys]"),
        "模板缺 [keys] 段"
    );
    let lines = template_keys_assignments();
    assert_eq!(
        lines.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        KEYS_FIELDS,
        "模板 [keys] 字段名/顺序与 Keys 漂移（漏写或多写）"
    );

    // 解注释后必须能解析：守住「模板里写的是合法 TOML + 合法字段名」。
    // 整组解注释后按**整份配置**解析（`[keys]` 是表头，直接解析成 `Keys` 会
    // 因为根表里没有 open 而静默回落全默认——那样这条测试就白测了）。
    let text = format!(
        "[keys]\n{}\n",
        lines
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let parsed = toml::from_str::<Config>(&text)
        .expect("模板 [keys] 段解注释后可解析")
        .keys;
    for (name, want) in KEYS_DEFAULTS {
        assert_eq!(
            parsed.field(name),
            Some(want),
            "模板里 {name} 的默认键位漂移"
        );
    }

    // 模板整份解析出来仍是「全空串」= 全用内置默认（注释掉的部分不参与）
    assert_eq!(
        toml::from_str::<Config>(DEFAULT_TEMPLATE)
            .expect("模板可解析")
            .keys,
        Keys::default()
    );
}
