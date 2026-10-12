//! ViewModel 测试共享工具（cfg(test)；真实 tar 档案最小构造 + 引擎）。
//!
//! # 定位（03 §2.7 TC-GUI 半场；02 §8.6）
//!
//! 单元测试需要**真实档案**驱动 `Engine::browse`/`Engine::submit` 与真解压管道
//! （密码/冲突请求来自真实 fmt 层）。GUI crate 依赖红线（02 §2.1 R2）禁止直接
//! 依赖 `hzip-fmt-*`——但本模块以**内嵌 ustar 头构造**产最小 tar（零 fmt 依赖，
//! 与 testkit `stub` 同为「自描述最小样本」精神），档案经 Engine 的注册表
//! （tar handler 随 core `default-formats` feature 注册）真实打开。真实密码
//! 场景（zip/7z 加密样本）测试卡允许以 testkit MockHandler 驱动，但核心
//! 请求-应答断言走真 tar 冲突路径（core 既有 mock_jobs 同型）。

use std::path::Path;

use hzip_core::engine::{Engine, EngineConfig};

/// locale 全局互斥（rust-i18n `set_locale` 是**进程级**全局：`AtomicStr`）。
///
/// # 机制（消除整类竞态，而非逐个用例加锁）
///
/// [`crate::tr`] 在测试构建下会检查「当前线程是否处于 locale 临界区」：
///
/// - **持锁线程**（用 [`locale_lock`] 进入）：按测试自己设定的 locale 查表——
///   切语言用例在自己临界区内断言 en/zh 都成立；
/// - **其他线程**：查表前短暂取本锁并把 locale 钉为 `zh-CN`——因此**永远不会**
///   观察到别的用例临时切过的语言（否则断言中文原文的用例会偶发失败）。
///
/// 结论：只有「需要断言非默认语言」的用例才需要用 [`locale_lock`]；断言中文
/// 的用例**无需**任何改动，天然确定。rust-i18n 的初始 locale 是硬编码 `en`，
/// 上锁路径同时把这一点也抹平了。
pub static LOCALE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    /// 当前线程是否已持有 [`LOCALE_LOCK`]（避免重入死锁 + 判定临界区身份）。
    static HOLDING_LOCALE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 当前线程是否处于 locale 临界区（由 [`crate::tr`] 在测试构建下查询）。
pub fn in_locale_critical_section() -> bool {
    HOLDING_LOCALE.with(|h| h.get())
}

/// 进入 locale 临界区：串行化 + 钉死 `zh-CN` 起点，退出时复位 `zh-CN`。
///
/// 临界区内可自由 `crate::init_locale("en")`；退出后其他线程仍看到 zh-CN。
/// 中毒（他用例 panic）不阻断其余用例——锁保护的只是串行化，无共享不变量。
pub fn locale_lock() -> LocaleGuard {
    let guard = LOCALE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    HOLDING_LOCALE.with(|h| h.set(true));
    rust_i18n::set_locale("zh-CN");
    LocaleGuard(guard)
}

/// [`locale_lock`] 的守卫：退出时复位语言与线程标记。
pub struct LocaleGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl Drop for LocaleGuard {
    fn drop(&mut self) {
        rust_i18n::set_locale("zh-CN");
        HOLDING_LOCALE.with(|h| h.set(false));
    }
}

/// 最小引擎（池 1/并发 1；无信号/清理副作用——测试免污染）。
pub fn engine() -> Engine {
    Engine::new(EngineConfig {
        pool_size: Some(1),
        max_parallel: 1,
        cancel_on_signal: false,
        orphan_cleanup: false,
    })
    .expect("engine 构建")
}

/// 写最小 ustar tar（零 fmt 依赖：内嵌 512 B 头 + 数据 + 对齐）。
///
/// 头布局：name(100) mode(8) uid(8) gid(8) size(12) mtime(12) chksum(8)
/// typeflag(1) linkname(100) magic(6="ustar\0") version(2="00")
/// uname(32) gname(32) devmajor(8) devminor(8) prefix(155)——共 512 B。
/// chksum = 头部全部字节（chksum 字段按空格）的八进制和。
///
/// 注：tar 头 chksum 字段以「6 位八进制 + NUL + 空格」写入（POSIX 约定），
/// 计算时该 8 字节按 `b' '` 计。
/// 写最小 zip（真实 registry 写路径——`caps.edit=true`，重命名/新建目录用例），
/// 返回条目名（与写入序一致）。
pub fn write_zip(path: &Path, entries: &[(&str, &[u8])]) -> Vec<String> {
    let engine = engine();
    let mut w = engine
        .registry()
        .handler("zip")
        .expect("zip handler")
        .create_writer(
            hzip_format::Sink::File(path.to_path_buf()),
            &Default::default(),
        )
        .expect("create_writer");
    for (name, data) in entries {
        let meta = hzip_format::EntryMeta {
            name: (*name).into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut sink = w
            .next_entry(&meta, hzip_format::PasswordRef::none())
            .expect("next_entry");
        std::io::Write::write_all(&mut sink, data).expect("write entry");
        sink.finish().expect("finish entry");
    }
    w.finish().expect("finish zip");
    entries.iter().map(|(n, _)| (*n).to_string()).collect()
}

/// 写 AES-256 加密 zip（用户口径 2026-09-29 现场形态：加密档里的重命名/新建）。
///
/// 逐条目加密；打开/列表**不需**密码（zip 无头加密），读条目才需要。
pub fn write_zip_encrypted(path: &Path, entries: &[(&str, &[u8])], pw: &str) {
    let engine = engine();
    let pwz = zeroize::Zeroizing::new(pw.to_string());
    let mut w = engine
        .registry()
        .handler("zip")
        .expect("zip handler")
        .create_writer(
            hzip_format::Sink::File(path.to_path_buf()),
            &hzip_format::CreateOptions {
                encryption: Some(hzip_format::EncryptionRequest::Aes(
                    hzip_format::AesKey::Bits256,
                )),
                ..Default::default()
            },
        )
        .expect("create_writer");
    for (name, data) in entries {
        let meta = hzip_format::EntryMeta {
            name: (*name).into(),
            size_raw: Some(data.len() as u64),
            ..Default::default()
        };
        let mut sink = w
            .next_entry(&meta, hzip_format::PasswordRef::new(&pwz))
            .expect("next_entry");
        std::io::Write::write_all(&mut sink, data).expect("write entry");
        sink.finish().expect("finish entry");
    }
    w.finish().expect("finish zip");
}

pub fn write_ustar(path: &Path, entries: &[(&str, &[u8])]) {
    let mut out = Vec::new();
    for (name, data) in entries {
        out.extend_from_slice(&ustar_header(name, data.len() as u64));
        out.extend_from_slice(data);
        let pad = (512 - (data.len() % 512)) % 512;
        out.resize(out.len() + pad, 0u8);
    }
    // 两个 512 B 零尾块（tar 终结）
    out.extend_from_slice(&[0u8; 1024]);
    std::fs::write(path, &out).expect("写 tar");
}

fn oct_field(value: u64, width: usize) -> Vec<u8> {
    let s = format!("{value:o}");
    let mut v = vec![b'0'; width];
    let start = width.saturating_sub(s.len());
    v[start..].copy_from_slice(s.as_bytes());
    v
}

fn ustar_header(name: &str, size: u64) -> [u8; 512] {
    let mut h = [0u8; 512];
    // name（<100 且 ascii）
    let nb = name.as_bytes();
    assert!(nb.len() < 100, "测试名过长");
    h[..nb.len()].copy_from_slice(nb);
    // mode 0644
    h[100..108].copy_from_slice(&oct_field(0o644, 8));
    // uid/gid 0
    h[108..116].copy_from_slice(&oct_field(0, 8));
    h[116..124].copy_from_slice(&oct_field(0, 8));
    // size（octal 12 位；< 8 GiB 样本）
    h[124..136].copy_from_slice(&oct_field(size, 12));
    // mtime 0
    h[136..148].copy_from_slice(&oct_field(0, 12));
    // chksum 占位（空格）
    h[148..156].copy_from_slice(b"        ");
    // typeflag '0'（regular）
    h[156] = b'0';
    // magic "ustar\0" + version "00"
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    // chksum：字段 148..156 按空格求和
    let sum: u32 = h.iter().map(|&b| b as u32).sum::<u32>()
        - h[148..156].iter().map(|&b| b as u32).sum::<u32>()
        + 8 * 32;
    let cks = format!("{sum:06o}");
    h[148..154].copy_from_slice(cks.as_bytes());
    h[154] = 0; // NUL
    h[155] = b' '; // 空格
    h
}
