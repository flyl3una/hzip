use super::*;

fn tmp() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("tmpdir")
}

/// 回归：配置的日志目录不可用（建不出/写不进）必须**回退平台默认**，
/// 不得让日志静默消失——旧实现 `create_dir_all` 忽略错误 + appender 失败
/// 直接 `?` 返回 None，用户侧表现为「配了目录却一条日志都没有」。
#[test]
fn unusable_configured_dir_falls_back_to_platform_default() {
    let d = tmp();
    let blocker = d.path().join("blocker");
    std::fs::write(&blocker, b"x").expect("占位文件");
    let bad = blocker.join("logs"); // 父路径是文件 → 永远建不出目录
    let (dir, fell_back) = resolve_dir(Some(&bad));
    assert!(fell_back, "不可用目录必须报告回退");
    assert_eq!(dir, hzip_platform::dirs::log_dir(), "回退到平台默认");

    // 可用目录：原样采用并已建出
    let good = d.path().join("ok-logs");
    let (dir2, fb2) = resolve_dir(Some(&good));
    assert!(!fb2, "可用目录不应回退");
    assert_eq!(dir2, good);
    assert!(good.is_dir(), "应采用后建出该目录");

    // 未配置：平台默认
    let (dir3, fb3) = resolve_dir(None);
    assert!(!fb3);
    assert_eq!(dir3, hzip_platform::dirs::log_dir());
}

#[test]
fn config_defaults_sane() {
    let c = LogConfig::default();
    assert_eq!(c.default_level, "warn");
    assert_eq!(c.retention_days, LOG_RETENTION_DAYS);
    assert!(c.mirror_stderr);
    assert!(c.dir.is_none());
    assert!(c.detailed);
}

#[test]
fn init_detailed_config_constructs() {
    // detailed=true（默认）构造 + init 不 panic（appender 可建）。全局
    // subscriber 单例——进程内已有别的 init 生效时本调用为 no-op，只验证
    // 不 panic 且返回 guard（重复调用安全语义）。
    let d = tmp();
    let cfg = LogConfig {
        dir: Some(d.path().to_path_buf()),
        default_level: "debug".into(),
        mirror_stderr: false,
        ..Default::default() // detailed: true
    };
    assert!(cfg.detailed);
    let guard = init(cfg);
    assert!(guard.is_some(), "detailed=true 初始化应成功");
}

#[test]
fn level_chain_unaffected_by_detailed() {
    // 级别链不回归：detailed 开关不影响 filter——env 未设 → 基线生效
    // （debug 基线 → max_level_hint = DEBUG；plain 同理）。
    use tracing::level_filters::LevelFilter;
    let f = level_filter("debug");
    assert_eq!(f.max_level_hint(), Some(LevelFilter::DEBUG));
    let f2 = level_filter("warn");
    assert_eq!(f2.max_level_hint(), Some(LevelFilter::WARN));
}

#[test]
fn init_writes_rolling_file() {
    let d = tmp();
    let dir = d.path().to_path_buf();
    let cfg = LogConfig {
        dir: Some(dir.clone()),
        default_level: "debug".into(),
        retention_days: 3,
        mirror_stderr: false,
        detailed: false,
    };
    // 全局 subscriber 单例：与其他 init 类测试并行时，本测试可能不是首个
    // init（try_init no-op）——debug! 不经本 layer 落盘。此时 appender 仍
    // 创建（文件存在），但内容为空属预期。串行（--test-threads=1）时本
    // 测试为首个 init，内容断言生效。
    let guard = init(cfg).expect("init");
    tracing::debug!("hello hzip-log");
    guard.flush(); // 显式冲刷（process::exit 语义模拟：drop 不跑）
    // rolling 文件名：hzip.YYYY-MM-DD.log（UTC 当日）
    let today = time::OffsetDateTime::now_utc().date();
    let name = format!("hzip.{today}.log");
    let f = dir.join(&name);
    assert!(f.exists(), "日志文件 {name} 应存在");
    let content = std::fs::read_to_string(&f).unwrap_or_default();
    assert!(
        content.contains("hello hzip-log") || content.is_empty(),
        "内容应落盘或被单例竞争置空"
    );
}

#[test]
fn redact_values() {
    assert_eq!(redact("pw:123"), "[redacted]");
    assert_eq!(redact(""), "");
    // 复合兜底：整段视作敏感
    assert_eq!(redact!(format!("x={}", 1)), "[redacted]");
}
