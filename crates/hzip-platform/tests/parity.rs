//! 三平台一致性表框架（02 §5.2；CI 同一份断言跑三平台 · 防行为漂移）。
//!
//! 组织：声明式用例表 [`CASES`] + [`parity_case!`] 注册宏；用例均「先在 Windows
//! 跑绿」；平台差异点断言写为「按 target_os 取期望值」而非跳过（03 §1.3 规则 1）。
//! unix-only 行为（`/proc` 检测、sigaction）的用例标 `#[ignore = "stage-b"]`
//! （`#[cfg(unix)]` 编译、Windows 上不编译），阶段 B 激活（03 §1.3 规则 4）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use hzip_platform::{console, dirs, fs_caps, fs_util, long_path, metadata, proc, symlink};
/// 用例上下文：独立临时目录。
struct Ctx {
    tmp: TempDir,
}

/// 用例结果（detail 写入失败报告供三平台 diff）。
struct Outcome {
    ok: bool,
    detail: String,
}

impl Outcome {
    fn ok() -> Self {
        Self {
            ok: true,
            detail: String::new(),
        }
    }
    fn fail(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: detail.into(),
        }
    }
}

/// 一致性用例（02 §5.2）。
struct ParityCase {
    name: &'static str,
    run: fn(&Ctx) -> Outcome,
}

/// 注册宏（02 §5.2 / 任务卡 §4.13）。
macro_rules! parity_case {
    ($name:literal, $f:path) => {
        ParityCase {
            name: $name,
            run: $f,
        }
    };
}

/// 一致性表：全部用例（Windows 阶段 A 全绿；平台期望按 target_os 声明）。
static CASES: &[ParityCase] = &[
    parity_case!("long_path_roundtrip", case_extended_path_roundtrip),
    parity_case!("max_path_len_positive", case_max_path_len),
    parity_case!("meta_roundtrip", case_meta_roundtrip),
    parity_case!("hardlink", case_hardlink),
    parity_case!("symlink_kind", case_symlink_kind),
    parity_case!("available_bytes", case_available_bytes),
    parity_case!("atomic_replace_same_vol", case_atomic_replace),
    parity_case!("remove_all_no_follow", case_remove_no_follow),
    parity_case!("proc_self_alive", case_proc_self_alive),
    parity_case!("dirs_nonempty_distinct", case_dirs),
    parity_case!("fs_caps_reserved", case_fs_caps_reserved),
    parity_case!("console_attach_never_panics", case_console_attach),
    parity_case!("case_sensitive_platform", case_case_sensitive_platform),
    parity_case!("fs_util_set_sparse", case_fs_util_set_sparse),
];

/// `to_extended/from_extended` 往返（平台无关断言；TC-WIN-001 支撑）。
fn case_extended_path_roundtrip(c: &Ctx) -> Outcome {
    let p = c
        .tmp
        .path()
        .join("very")
        .join("long")
        .join("dir")
        .join("x.txt");
    let ext = long_path::to_extended(&p);
    let back = long_path::from_extended(&ext);
    if back == p {
        Outcome::ok()
    } else {
        Outcome::fail(format!("roundtrip 不一致: {p:?} → {ext:?} → {back:?}"))
    }
}

/// `max_path_len > 0`。
fn case_max_path_len(_c: &Ctx) -> Outcome {
    let v = long_path::max_path_len();
    if v > 0 {
        Outcome::ok()
    } else {
        Outcome::fail("max_path_len() == 0")
    }
}

/// `read_meta/apply_meta` 往返：read_only + mtime（±2 s，FAT 精度）。
fn case_meta_roundtrip(c: &Ctx) -> Outcome {
    let f = c.tmp.path().join("meta.txt");
    std::fs::write(&f, b"x").unwrap();
    let want_mtime = time::OffsetDateTime::from_unix_timestamp_nanos(
        (time::OffsetDateTime::now_utc().unix_timestamp() - 3600) as i128 * 1_000_000_000,
    )
    .unwrap();
    let mut meta = metadata::FileMeta::default();
    meta.read_only = true;
    meta.mtime = Some(want_mtime);
    if let Err(e) = metadata::apply_meta(&f, &meta) {
        return Outcome::fail(format!("apply_meta 失败: {e}"));
    }
    let got = metadata::read_meta(&f).unwrap();
    if !got.read_only {
        return Outcome::fail("read_only 未恢复");
    }
    let Some(got_m) = got.mtime else {
        return Outcome::fail("mtime 缺失");
    };
    let diff = (got_m - want_mtime).abs();
    if diff > std::time::Duration::from_secs(2) {
        return Outcome::fail(format!("mtime 差 {diff:?} 超过 2 s"));
    }
    // 还原只读位，避免清理失败
    let mut clear = metadata::FileMeta::default();
    clear.read_only = false;
    let _ = metadata::apply_meta(&f, &clear);
    Outcome::ok()
}

/// `create_hardlink` 内容一致性。
fn case_hardlink(c: &Ctx) -> Outcome {
    let src = c.tmp.path().join("h-src.txt");
    let dst = c.tmp.path().join("h-dst.txt");
    std::fs::write(&src, b"hardlink-content").unwrap();
    if let Err(e) = symlink::create_hardlink(&src, &dst) {
        return Outcome::fail(format!("create_hardlink 失败: {e}"));
    }
    match std::fs::read(&dst) {
        Ok(b) if b == b"hardlink-content" => Outcome::ok(),
        Ok(b) => Outcome::fail(format!("内容不一致: {b:?}")),
        Err(e) => Outcome::fail(format!("读取失败: {e}")),
    }
}

/// `symlink_kind` 判定（特权可用 → 真链接；不可用 → 降级路径产物为普通文件）。
fn case_symlink_kind(c: &Ctx) -> Outcome {
    let t = c.tmp.path().join("s-t.txt");
    let l = c.tmp.path().join("s-l.txt");
    std::fs::write(&t, b"x").unwrap();
    let pol = symlink::SymlinkPolicy {
        fallback: symlink::Fallback::Reject,
    };
    match symlink::create_symlink(&t, &l, &pol) {
        Ok(_) => match symlink::symlink_kind(&l) {
            Ok(symlink::LinkKind::File) => Outcome::ok(),
            Ok(k) => Outcome::fail(format!("期望 File 实得 {k:?}")),
            Err(e) => Outcome::fail(format!("symlink_kind 失败: {e}")),
        },
        Err(hzip_error::HzipError::Unsupported { .. }) => {
            // 无特权环境 + Reject 策略：不创建任何产物（防误建普通文件占位），
            // 返回 Unsupported 附提示。验证：目标路径确实未被创建。
            if l.exists() {
                Outcome::fail("Reject 拒绝后不应产生链接/占位文件")
            } else {
                Outcome::ok()
            }
        }
        Err(e) => Outcome::fail(format!("create_symlink 意外错误: {e}")),
    }
}

/// `available_bytes > 0`。
fn case_available_bytes(c: &Ctx) -> Outcome {
    match fs_util::available_bytes(c.tmp.path()) {
        Ok(v) if v > 0 => Outcome::ok(),
        Ok(v) => Outcome::fail(format!("available_bytes == {v}")),
        Err(e) => Outcome::fail(format!("available_bytes 失败: {e}")),
    }
}

/// `atomic_replace` 同卷：dst 内容 == src；无 `.hzip-tmp-*` 残留。
fn case_atomic_replace(c: &Ctx) -> Outcome {
    let src = c.tmp.path().join("a-src.txt");
    let dst = c.tmp.path().join("a-dst.txt");
    std::fs::write(&src, b"replaced").unwrap();
    std::fs::write(&dst, b"original").unwrap();
    if let Err(e) = fs_util::atomic_replace(&src, &dst) {
        return Outcome::fail(format!("atomic_replace 失败: {e}"));
    }
    if std::fs::read(&dst).unwrap() != b"replaced" {
        return Outcome::fail("dst 内容未替换");
    }
    let leftovers: Vec<_> = std::fs::read_dir(c.tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".hzip-tmp-"))
        .collect();
    if leftovers.is_empty() {
        Outcome::ok()
    } else {
        Outcome::fail(format!("残留临时文件: {leftovers:?}"))
    }
}

/// `remove_all_no_follow` 不越界：目录内符号链接指向外部 → 外部原封不动。
fn case_remove_no_follow(c: &Ctx) -> Outcome {
    let root = c.tmp.path().join("rm-root");
    let outside = c.tmp.path().join("rm-outside");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("sub").join("f.txt"), b"x").unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();

    let link = root.join("esc");
    let pol = symlink::SymlinkPolicy {
        fallback: symlink::Fallback::Reject,
    };
    let linked = symlink::create_symlink(&outside, &link, &pol).is_ok();
    if let Err(e) = fs_util::remove_all_no_follow(&root) {
        return Outcome::fail(format!("remove_all_no_follow 失败: {e}"));
    }
    if root.exists() {
        return Outcome::fail("root 未删除");
    }
    if linked {
        // 链接建立成功：外部必须原封不动（防逃逸删除）
        match std::fs::read(outside.join("keep.txt")) {
            Ok(b) if b == b"keep" => Outcome::ok(),
            Ok(b) => Outcome::fail(format!("外部内容被改动: {b:?}")),
            Err(e) => Outcome::fail(format!("外部被删除: {e}")),
        }
    } else {
        // 无特权环境：链接未建立，仅验证删除树成功
        Outcome::ok()
    }
}

/// `is_alive(自身pid, self_created_ms()) == true`（02 §5.3.8 双判定）。
fn case_proc_self_alive(_c: &Ctx) -> Outcome {
    let pid = std::process::id();
    let created = proc::self_created_ms();
    if created == 0 {
        return Outcome::fail("self_created_ms == 0");
    }
    if proc::is_alive(pid, created) {
        Outcome::ok()
    } else {
        Outcome::fail("自身 pid + 创建时间判活失败")
    }
}

/// dirs 四目录非空且互异。
fn case_dirs(_c: &Ctx) -> Outcome {
    let list = [
        dirs::config_dir(),
        dirs::data_dir(),
        dirs::cache_dir(),
        dirs::log_dir(),
        dirs::temp_root(),
    ];
    if list.iter().any(|p| p.as_os_str().is_empty()) {
        return Outcome::fail("存在空目录路径");
    }
    if list.iter().any(|p| !p.is_absolute()) {
        return Outcome::fail("存在非绝对路径");
    }
    let mut dedup = list.to_vec();
    dedup.sort();
    dedup.dedup();
    // macOS：config 与 data 同为 `~/Library/Application Support/hzip`（平台约定，
    // 见 `dirs` 模块文档），允许这一对相同。
    #[cfg(target_os = "macos")]
    let want = 4;
    #[cfg(not(target_os = "macos"))]
    let want = 5;
    if dedup.len() == want {
        Outcome::ok()
    } else {
        Outcome::fail(format!(
            "目录互异失败：{} 个互异（期望 {want}）",
            dedup.len()
        ))
    }
}

/// `fs_caps.reserved_chars` 与平台语义一致：Windows 为 `<>:"|?*`（NTFS 语义层，
/// `/` 与 `\` 是路径分隔符、不属文件名保留字符）；unix 含 `/`。
fn case_fs_caps_reserved(c: &Ctx) -> Outcome {
    let caps = fs_caps::probe(c.tmp.path());
    #[cfg(windows)]
    {
        if caps.reserved_chars.contains('\\') || caps.reserved_chars.contains('/') {
            return Outcome::fail(format!(
                "Windows 保留字符不应含分隔符: {:?}",
                caps.reserved_chars
            ));
        }
        if caps.reserved_chars.is_empty() {
            return Outcome::fail("Windows 保留字符集为空");
        }
        Outcome::ok()
    }
    #[cfg(not(windows))]
    {
        if caps.reserved_chars.contains('/') {
            Outcome::ok()
        } else {
            Outcome::fail(format!("unix 保留字符应含 '/': {:?}", caps.reserved_chars))
        }
    }
}

/// `is_case_sensitive` 与平台期望一致（期望值按平台声明，CI 矩阵上
/// Windows/macOS NTFS/APFS 默认不敏感、Linux 默认敏感）。
fn case_case_sensitive_platform(c: &Ctx) -> Outcome {
    let got = fs_util::is_case_sensitive(c.tmp.path());
    let want = cfg!(target_os = "linux");
    if got == want {
        Outcome::ok()
    } else {
        Outcome::fail(format!("is_case_sensitive = {got}, 平台期望 {want}"))
    }
}

/// `fs_util::set_sparse`（T22 §4.7 新增 API）：先标记再写洞区间；
/// 标记成功（NTFS/unix）→ 稀疏成立且逻辑内容正确（洞区读零）。
///
/// 洞取 64 MiB：APFS 会把小于约 16 MiB 的洞在关闭文件时重新密集化（实测阈值在
/// 16~16.25 MiB 之间），小洞会因此误报「不稀疏」。64 MiB 在 NTFS/ext4/APFS
/// 三平台均稳定保留为洞，断言才有跨平台意义。
fn case_fs_util_set_sparse(c: &Ctx) -> Outcome {
    use std::io::{Seek, SeekFrom, Write};

    const HOLE: u64 = 64 << 20;
    let p = c.tmp.path().join("hole.bin");
    let mut f = match std::fs::File::create(&p) {
        Ok(f) => f,
        Err(e) => return Outcome::fail(format!("create 失败: {e}")),
    };
    let marked = fs_util::set_sparse(&f).unwrap_or(false);
    if let Err(e) = f.write_all(b"AAAA") {
        return Outcome::fail(format!("首区间写失败: {e}"));
    }
    if let Err(e) = f
        .seek(SeekFrom::Start(HOLE))
        .and_then(|_| f.write_all(b"BBBB"))
    {
        return Outcome::fail(format!("远端区间写失败: {e}"));
    }
    if let Err(e) = f.flush() {
        return Outcome::fail(format!("flush 失败: {e}"));
    }
    // 关闭写作句柄，使分配状态定稿后再判定（见上方 APFS 说明）
    drop(f);

    // 逻辑内容（平台无关，必断）
    match std::fs::read(&p) {
        Ok(b) => {
            if &b[..4] != b"AAAA"
                || &b[HOLE as usize..HOLE as usize + 4] != b"BBBB"
                || b.len() != HOLE as usize + 4
            {
                return Outcome::fail("稀疏文件逻辑内容错误（洞区/区间）");
            }
        }
        Err(e) => return Outcome::fail(format!("读回失败: {e}")),
    }

    if marked {
        // 标记成功（NTFS / unix）：文件应为稀疏（以读句柄查询——ioctl 需读访问）。
        match std::fs::File::open(&p).and_then(|rf| fs_util::sparse::is_sparse_file(&rf)) {
            Ok(true) => Outcome::ok(),
            Ok(false) => Outcome::fail("set_sparse 成功后文件不稀疏"),
            Err(e) => Outcome::fail(format!("is_sparse_file 失败: {e}")),
        }
    } else {
        Outcome::ok()
    }
}

/// 全部一致性用例跑一遍（Windows 阶段 A 全绿，02 §5.2）。
/// 控制台附着：**只在 Windows 有实现**，且任何环境下都不得 panic。
///
/// 无法断言「真的附上了」——测试进程本身可能有、也可能没有控制台，两种都合法。
/// 能钉住的是它与调用方共享的两条契约：① 不 panic（它跑在启动路径上，panic
/// 等于双击启动失败）；② 返回句柄时写出不 panic（写出目标是 `CONOUT$`，
/// 控制台可能在两次调用之间被关闭）。
fn case_console_attach(_ctx: &Ctx) -> Outcome {
    let handle = console::attach_parent_console();
    if let Some(w) = &handle {
        w.write("hzip parity console probe");
        w.write_line(" (line)");
    }
    if cfg!(windows) {
        Outcome::ok()
    } else {
        // 非 Windows 恒 None（平台差异点写成断言，而不是跳过；03 §1.3 规则 1）
        match handle {
            None => Outcome::ok(),
            Some(_) => Outcome::fail("非 Windows 平台不得返回控制台句柄"),
        }
    }
}

#[test]
fn run_all() {
    let tmp = TempDir::new("parity");
    let ctx = Ctx { tmp };
    let mut failures: Vec<String> = Vec::new();
    for case in CASES {
        let out = (case.run)(&ctx);
        if !out.ok {
            failures.push(format!("[{}] {}", case.name, out.detail));
        }
    }
    assert!(
        failures.is_empty(),
        "一致性表存在失败用例（detail 供三平台 diff）:\n{}",
        failures.join("\n")
    );
}

/// unix-only 行为：`/proc` 检测（02 §5.3.8 Linux 分支）——阶段 B 在 Linux CI 激活。
#[cfg(target_os = "linux")]
#[test]
#[ignore = "stage-b：Linux CI 激活（03 §1.3 规则 4）"]
fn proc_procfs_linux() {
    let pid = std::process::id();
    let created = proc::self_created_ms();
    assert!(created > 0);
    assert!(proc::is_alive(pid, created));
}

/// unix-only 行为：sigaction 桥（02 §5.3.9）——阶段 B 在 POSIX CI 激活。
#[cfg(unix)]
#[test]
#[ignore = "stage-b：POSIX CI 激活（03 §1.3 规则 4）；T10 hzip-progress 合入后补全（任务卡 §2）"]
fn signal_bridge_posix() {
    let _uninstall = hzip_platform::signal::install_cancel_handler(());
}

/// 系统剪贴板文件列表写侧 round-trip（T-MENU8）：set_system_file_list →
/// CF_HDROP 读回逐路径比对 + CF_UNICODETEXT 非 null。
///
/// `#[ignore]` 原因：**污染真机剪贴板**——测试会覆盖用户当前剪贴板内容
/// （且 CI 无交互会话时 OpenClipboard 可能失败），故只允许人工触发
/// （`cargo test -p hzip-platform -- --ignored`）。
#[cfg(windows)]
#[test]
#[ignore = "污染真机剪贴板（覆盖用户当前内容）；仅人工触发验证"]
fn clipboard_file_list_roundtrip_windows() {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
    use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};

    // 两个临时文件（路径含中文，验证 UTF-16 fWide 编码往返）。
    let tmp = TempDir::new("clipboard-roundtrip");
    let p1 = tmp.path().join("a-复制.txt");
    let p2 = tmp.path().join("b-粘贴.txt");
    std::fs::write(&p1, b"one").unwrap();
    std::fs::write(&p2, b"two").unwrap();
    let paths = vec![p1.clone(), p2.clone()];

    // 写侧。
    assert!(
        hzip_platform::clipboard::set_system_file_list(&paths),
        "set_system_file_list 应成功"
    );

    unsafe {
        // 读侧前置检查：CF_HDROP 可用（不打开剪贴板即可查）。
        assert!(
            IsClipboardFormatAvailable(CF_HDROP as u32) != 0,
            "IsClipboardFormatAvailable(CF_HDROP) 应非零"
        );

        assert!(
            OpenClipboard(std::ptr::null_mut()) != 0,
            "OpenClipboard 失败: {:?}",
            std::io::Error::last_os_error()
        );

        // CF_HDROP 还原：DragQueryFileW 逐个取路径。
        let hdrop_raw = GetClipboardData(CF_HDROP as u32);
        assert!(!hdrop_raw.is_null(), "GetClipboardData(CF_HDROP) 应非 null");
        let hdrop = hdrop_raw as HDROP;

        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
        assert_eq!(count, paths.len() as u32, "文件数应一致");

        let mut recovered: Vec<PathBuf> = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
            assert!(len > 0, "DragQueryFileW({i}) 长度应 > 0");
            // 长度不含结尾 NUL，缓冲区多留 1。
            let mut buf = vec![0u16; len as usize + 1];
            let copied = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
            assert_eq!(copied, len, "DragQueryFileW 复制长度应与查询一致");
            buf.truncate(copied as usize);
            recovered.push(OsString::from_wide(&buf).into());
        }

        // 大小写/前缀按 Windows 规范比对（NTFS 不敏感 + \\?\ 前缀等价），
        // 不用直接字符串相等（Path::eq 走 BytesOrd，盘符大小写即翻车）。
        assert_eq!(recovered.len(), paths.len(), "还原条数应一致");
        for (got, want) in recovered.iter().zip(paths.iter()) {
            let got_norm = got.canonicalize().unwrap_or_else(|_| got.clone());
            let want_norm = want.canonicalize().unwrap_or_else(|_| want.clone());
            assert_eq!(
                got_norm.to_string_lossy().to_lowercase(),
                want_norm.to_string_lossy().to_lowercase(),
                "路径比对失败: got={got:?} want={want:?}"
            );
        }

        // 文本降级格式也写入成功。
        let htext = GetClipboardData(CF_UNICODETEXT as u32);
        assert!(
            !htext.is_null(),
            "GetClipboardData(CF_UNICODETEXT) 应非 null"
        );

        CloseClipboard();
    }
}

// ---------------------------------------------------------------- 工具

/// 极简临时目录（不引入第三方测试依赖）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "hzip-parity-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs_util::remove_all_no_follow(&self.path);
    }
}
