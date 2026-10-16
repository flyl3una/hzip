//! XFS 测试夹具：**稀疏内存盘** + 用上游 `mkfs` 现场造的 v5 镜像。
//!
//! # 为什么不是 `MemDevice`（testkit 的内存盘）
//!
//! 上游 `mkfs` 的容量下界是 **300 MiB**（`mkfs::MIN_DEVICE_BYTES`），而它造出来的
//! 镜像里**真正有内容的字节只有一两 MiB**（其余是零：64 MiB 日志的绝大部分、各 AG
//! 的空闲区）。`MemDevice` 老老实实分配 300 MiB ⇒ 每个用例 300 MiB、比较两份镜像要
//! 克隆 300 MiB。本文件这块盘按 **4 KiB 页稀疏存**（只留非零页），于是：一个夹具
//! 约占 1 MiB 内存、`snapshot()` 是页表深拷贝、差分（[`SparseDevice::differing_pages`]）
//! 与「设备有没有被写过」都是常数级的小事。密集设备仍按需用 testkit 的 `MemDevice`
//! （全零盘 / 太小盘这类用例）。
//!
//! # 确定性（一处诚实的偏差）
//!
//! 上游 `mkfs` 与写路径都用**系统时钟**给 inode 打时间戳（mkfs 给 3 个初始 inode、
//! `create_*`/`write` 给新 inode），且 0.14.1 **没有**时钟注入点（ext4 夹具的
//! `Runtime` 注入是 ext4 上游给的，XFS 这边没有对应物）。故夹具固定 UUID、块大小、
//! 容量与 AG 数 ⇒ **结构逐字节确定**（超级块、AG 头、btree 根、日志首记录、文件内容），
//! 而 inode 的时间戳随运行时刻变 —— `tests/read.rs::fixture_is_structurally_deterministic`
//! 把这件事量出来：两次构建的差异**恰好只有 inode chunk 那 8 页**。
//!
//! # 越界即错
//!
//! 与 `MemDevice` 同口径：越界读写返回 `Err`（不 panic、不静默截断），`read_at` 必须
//! 填满 `buf`，容量构造时固定（`set_len` 恒 `Err`）。

#![allow(clippy::unwrap_used, clippy::expect_used)]
// 夹具是测试专用代码：坏输入当场炸调用方
// 共用的测试模块：每个测试二进制只用其中一部分，未用到的项不算问题。
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use fs_core::{BlockDevice, BlockRead, Error as CoreError};
use fs_xfs::{Filesystem, mkfs};
use hzip_format::EntryId;
use hzip_fs_xfs::XfsDriver;
use hzip_progress::CancellationToken;
use hzip_vol::{EntryLimit, EntryTree, FsDriver, FsOptions, FsVolume};

/// 夹具容量（320 MiB；`mkfs` 的下界是 300 MiB，这里留出余量给 AG 划分）。
pub const IMAGE_BYTES: u64 = 320 << 20;

/// 夹具块大小（4 KiB；与真实发行版默认一致）。
pub const BLOCK_SIZE: u32 = 4096;

/// 固定卷 UUID（= ASCII `HZIPfixture-xfs0`）。
pub const VOLUME_UUID: [u8; 16] = *b"HZIPfixture-xfs0";

/// 固定卷标。
pub const LABEL: &str = "HZTEST";

/// 稀疏页大小（= 夹具块大小：与文件系统块对齐，省得页里混两件事）。
const PAGE: usize = 4096;

// ---------------------------------------------------------------- 稀疏内存盘

/// 按 4 KiB 页稀疏存的内存盘（见模块文档）。
///
/// `Clone` 是**共享底层**的句柄克隆（与 `MemDevice` 同口径）：先把镜像造好、`clone()`
/// 一份交给驱动去挂载，断言时看原句柄就够了。要独立副本用 [`Self::snapshot`]。
#[derive(Clone, Debug, Default)]
pub struct SparseDevice {
    inner: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    size: u64,
    /// 页号（`offset / PAGE`）→ 页内容；全零页不入表。
    pages: BTreeMap<u64, Box<[u8; PAGE]>>,
    /// 累计 `write_at` 调用次数与写入字节数（「设备一个字节没动」这类断言用它）。
    writes: u64,
    bytes_written: u64,
}

impl SparseDevice {
    /// 造一块 `size` 字节、全零的稀疏盘。
    #[must_use]
    pub fn new(size: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                size,
                ..State::default()
            })),
        }
    }

    /// 深拷贝（页表逐页复制）：与本体是**两份独立字节**。
    #[must_use]
    pub fn snapshot(&self) -> Self {
        let g = self.guard();
        Self {
            inner: Arc::new(Mutex::new(State {
                size: g.size,
                pages: g.pages.clone(),
                writes: g.writes,
                bytes_written: g.bytes_written,
            })),
        }
    }

    /// 非零页数（诊断用）。
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.guard().pages.len()
    }

    /// 累计 `write_at` 次数（`reset_writes` 之后算起）。
    #[must_use]
    pub fn writes(&self) -> u64 {
        self.guard().writes
    }

    /// 累计写入字节数。
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.guard().bytes_written
    }

    /// 把写计数清零（造完夹具、要断言「读侧没写」时用）。
    pub fn reset_writes(&self) {
        let mut g = self.guard();
        g.writes = 0;
        g.bytes_written = 0;
    }

    /// 读一段字节（越界即 panic：这是夹具，不是被测代码）。
    #[must_use]
    pub fn read_bytes(&self, offset: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        BlockRead::read_at(self, offset, &mut buf).expect("夹具读越界");
        buf
    }

    /// 与 `other` 不同的**页号**列表（逐页比，跳过两边都没有的页）。
    #[must_use]
    pub fn differing_pages(&self, other: &Self) -> Vec<u64> {
        let a = self.guard();
        let b = other.guard();
        let mut keys: Vec<u64> = a.pages.keys().chain(b.pages.keys()).copied().collect();
        keys.sort_unstable();
        keys.dedup();
        keys.into_iter()
            .filter(|k| a.pages.get(k) != b.pages.get(k))
            .collect()
    }

    /// 首 4 KiB（驱动 `supports` 的复核窗口用）。
    #[must_use]
    pub fn head(&self) -> Vec<u8> {
        self.read_bytes(0, 4096)
    }

    /// 取锁。中毒只可能来自本模块自身的中断 ⇒ 直接炸调用方的测试。
    fn guard(&self) -> MutexGuard<'_, State> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(_) => panic!("SparseDevice: 内部锁中毒"),
        }
    }
}

impl BlockRead for SparseDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        let g = self.guard();
        if offset
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > g.size)
        {
            return Err(CoreError::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size: g.size,
            });
        }
        buf.fill(0);
        let mut pos = offset;
        while pos < offset + buf.len() as u64 {
            let page = pos / PAGE as u64;
            let start = (pos % PAGE as u64) as usize;
            let done = (pos - offset) as usize;
            let n = (PAGE - start).min(buf.len() - done);
            if let Some(data) = g.pages.get(&page) {
                buf[done..done + n].copy_from_slice(&data[start..start + n]);
            }
            pos += n as u64;
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.guard().size
    }
}

impl BlockDevice for SparseDevice {
    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()> {
        let mut g = self.guard();
        let size = g.size;
        if offset
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > size)
        {
            return Err(CoreError::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size,
            });
        }
        let mut pos = offset;
        while pos < offset + buf.len() as u64 {
            let page = pos / PAGE as u64;
            let start = (pos % PAGE as u64) as usize;
            let done = (pos - offset) as usize;
            let n = (PAGE - start).min(buf.len() - done);
            let empty = {
                let slot = g.pages.entry(page).or_insert_with(|| Box::new([0u8; PAGE]));
                slot[start..start + n].copy_from_slice(&buf[done..done + n]);
                slot.iter().all(|b| *b == 0)
            };
            if empty {
                g.pages.remove(&page); // 写回全零 = 与从未写过等价（稀疏盘的核心）
            }
            pos += n as u64;
        }
        g.writes += 1;
        g.bytes_written += buf.len() as u64;
        Ok(())
    }

    fn flush(&self) -> fs_core::Result<()> {
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }

    fn set_len(&self, new_len: u64) -> fs_core::Result<()> {
        let size = self.guard().size;
        Err(CoreError::Custom(format!(
            "SparseDevice: 卷容量构造时固定（拒绝 set_len({new_len})，容量 {size}）"
        )))
    }
}

// ---------------------------------------------------------------- 镜像夹具

/// 夹具里的一个条目。
pub enum Node<'a> {
    /// 常规文件（内容给全）。
    File(&'a str, &'a [u8]),
    /// 目录（含逐级隐式父目录）。
    Dir(&'a str),
    /// 符号链接（目标原样存储）。
    Symlink(&'a str, &'a str),
}

/// 造一个**空**的 v5 镜像（只 `mkfs`；UUID/块大小/容量全固定）。
#[must_use]
pub fn format_empty() -> SparseDevice {
    let dev = SparseDevice::new(IMAGE_BYTES);
    let handle: Arc<dyn BlockDevice> = Arc::new(dev.clone());
    mkfs::format(
        handle.as_ref(),
        &mkfs::Options {
            block_size: BLOCK_SIZE,
            agcount: None,
            label: Some(LABEL.to_string()),
            uuid: Some(VOLUME_UUID),
        },
    )
    .expect("夹具：XFS 格式化应成功");
    dev
}

/// 造一个含给定条目的**干净**镜像（写完之后 `unmount` ⇒ 日志收尾、superblock 计数写对）。
#[must_use]
pub fn image(nodes: &[Node<'_>]) -> SparseDevice {
    build(nodes, true)
}

/// 同上，但**不** `unmount`：日志里留着没落盘的记录 ⇒ 需要重放（脏卷）。
#[must_use]
pub fn dirty_image(nodes: &[Node<'_>]) -> SparseDevice {
    build(nodes, false)
}

fn build(nodes: &[Node<'_>], clean: bool) -> SparseDevice {
    let dev = format_empty();
    let handle: Arc<dyn BlockDevice> = Arc::new(dev.clone());
    let fs = Filesystem::mount_rw(handle).expect("夹具：挂载刚格式化出来的卷");
    for node in nodes {
        match node {
            Node::File(path, data) => {
                let (dir, base) = fixture_parent(&fs, path);
                let (ino, _) = fs
                    .create_file(dir, base.as_bytes(), 0o644)
                    .expect("夹具：建文件");
                if !data.is_empty() {
                    fs.write(ino, 0, data).expect("夹具：写文件");
                }
            }
            Node::Dir(path) => {
                fixture_dir(&fs, path);
            }
            Node::Symlink(path, target) => {
                let (dir, base) = fixture_parent(&fs, path);
                fs.create_symlink(dir, base.as_bytes(), target.as_bytes())
                    .expect("夹具：建符号链接");
            }
        }
    }
    if clean {
        fs.unmount().expect("夹具：干净卸载");
    } else {
        drop(fs); // 不 unmount ⇒ 日志带待重放记录
    }
    dev.reset_writes();
    dev
}

/// 建出 `path` 的整条目录链（含末段；末段已存在则跳过）。
fn fixture_dir(fs: &Filesystem, path: &str) {
    let mut parent = fs.superblock().rootino;
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        let child = {
            let dir = fs.open_ino(parent).expect("夹具：打开父目录");
            dir.open_child(segment.as_bytes()).map(|f| f.inode().ino)
        };
        parent = match child {
            Ok(ino) => ino,
            Err(fs_xfs::Error::NotFound) => {
                fs.create_directory(parent, segment.as_bytes(), 0o755)
                    .expect("夹具：建目录")
                    .0
            }
            Err(e) => panic!("夹具：找目录失败 {e}"),
        };
    }
}

/// 建出 `path` 的父目录链，返回（父目录 inode, 末段名）。
fn fixture_parent(fs: &Filesystem, path: &str) -> (u64, String) {
    let mut parent = fs.superblock().rootino;
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let base = segments.pop().expect("夹具：路径不能为空").to_string();
    for segment in segments {
        // `File` 借的是打开时那份 inode 快照，故取值（inode 号）在块里就取完。
        let child = {
            let dir = fs.open_ino(parent).expect("夹具：打开父目录");
            dir.open_child(segment.as_bytes()).map(|f| f.inode().ino)
        };
        parent = match child {
            Ok(ino) => ino,
            Err(fs_xfs::Error::NotFound) => {
                fs.create_directory(parent, segment.as_bytes(), 0o755)
                    .expect("夹具：建目录")
                    .0
            }
            Err(e) => panic!("夹具：找目录失败 {e}"),
        };
    }
    (parent, base)
}

// ---------------------------------------------------------------- 挂载与读取助手

/// 只读挂载（`FsOptions::default()` = 不请求可写）。
#[must_use]
pub fn mount_ro(image: &SparseDevice) -> Box<dyn FsVolume> {
    let dev: Arc<dyn hzip_vol::BlockDevice> = Arc::new(image.clone());
    XfsDriver::new()
        .mount(dev, &FsOptions::default())
        .expect("夹具：只读挂载")
}

/// 可写挂载（请求可写 + 干净卷 + 可写设备）。
#[must_use]
pub fn mount_rw(image: &SparseDevice) -> Box<dyn FsVolume> {
    let dev: Arc<dyn hzip_vol::BlockDevice> = Arc::new(image.clone());
    XfsDriver::new()
        .mount(
            dev,
            &FsOptions {
                writable: true,
                ..FsOptions::default()
            },
        )
        .expect("夹具：可写挂载")
}

/// 宽裕的列表上限（专门的截断用例用小值）。
#[must_use]
pub fn limit() -> EntryLimit {
    EntryLimit {
        max_entries: 10_000,
        max_depth: 32,
    }
}

/// 没人取消的令牌。
#[must_use]
pub fn cancel() -> CancellationToken {
    CancellationToken::new()
}

/// 列全树。
#[must_use]
pub fn tree(fs: &dyn FsVolume) -> EntryTree {
    fs.entries(&limit(), &cancel()).expect("夹具：列树")
}

/// 按名字取 `EntryId`（找不到就炸调用方的测试）。
#[must_use]
pub fn id_of(fs: &dyn FsVolume, name: &str) -> EntryId {
    tree(fs)
        .by_name(name)
        .map(|e| e.id)
        .unwrap_or_else(|| panic!("夹具：卷里没有 {name}"))
}

/// 按名字全量读出内容（目录 ⇒ 空）。
#[must_use]
pub fn read_name(fs: &dyn FsVolume, name: &str) -> Vec<u8> {
    read_all(fs, id_of(fs, name))
}

/// 按 id 全量读出内容。
#[must_use]
pub fn read_all(fs: &dyn FsVolume, id: EntryId) -> Vec<u8> {
    let size = fs.size_of(id).expect("夹具：取大小");
    let mut out = vec![0u8; usize::try_from(size).expect("夹具：大小超出 usize")];
    let n = fs.read_at(id, 0, &mut out).expect("夹具：读内容");
    out.truncate(n);
    out
}

/// 写一个本地文件（`Change::Replace`/`Append` 的 `src`），并钉死它的 mtime。
///
/// mtime 用固定时刻（`std::fs::File::set_modified`）：夹具不读系统时钟，且
/// 「写内容后保本地 mtime」这条断言才有确定的期望值。
#[must_use]
pub fn local_file(name: &str, data: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().expect("夹具：临时目录");
    let path = dir.path().join(name);
    std::fs::write(&path, data).expect("夹具：写本地文件");
    let f = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("夹具：打开本地文件");
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(FIXED_MTIME_SECS))
        .expect("夹具：钉 mtime");
    (dir, path)
}

/// 本地文件被钉死的 mtime（秒）——与 ext4 夹具的固定时钟同刻。
pub const FIXED_MTIME_SECS: u64 = 1_700_000_000;
