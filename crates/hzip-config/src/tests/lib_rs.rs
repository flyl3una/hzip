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
