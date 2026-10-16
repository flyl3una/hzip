//! 卷编辑器（`VolumeEditor`）的契约测试。
//!
//! 这里守的是**编辑器 ↔ 文件系统驱动**之间的那条隐式契约，不是一个具体格式：
//!
//! 1. **先列一次条目再应用变更**（2026-10-16 由 vmdk 代理实测暴露的缺陷）：驱动们的
//!    `EntryId` 是它们 `entries()` 里那张表的下标，`apply` 只认那张表 ⇒ 编辑器挂载后
//!    必须先列一次，否则 `Change::Replace`/`Delete` 全废（`Append` 不需要 id，所以早先
//!    的用例没暴露）。下面的 mock 就按这个语义设计：**没列过条目就 apply ⇒ 报错**。
//! 2. 提交前**原档字节不变**、取消即丢；提交只回放脏块。
//! 3. 自检不过 ⇒ 不提交（原档不受影响）。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hzip_format::{ArchiveEditor as _, Change, ChangeSet, CreateOptions, EntryInfo, Source};
use hzip_progress::{CancellationToken, ProgressBus};
use hzip_vol::device::WriteMode;
use hzip_vol::editor::VolumeEditor;
use hzip_vol::error::{VolError, VolResult};
use hzip_vol::{
    AuditSummary, BlockDevice, EntryLimit, EntryTree, FsCaps, FsVolume, Geometry, PartitionView,
    VolCaps, VolOptions, Volume, VolumeHandler,
};

// ------------------------------------------------------------------ mock

/// 模拟真实驱动的**惰性索引**语义：`apply` 前没列过条目 ⇒ 报「先调 entries()」。
///
/// `applied` 是**共享**记录器（`VolumeEditor` 从 handler 取走 fs 后测试看不到它，
/// 故由 handler 持有同一份 `Arc`）：测试据此断言路由后的**卷内**变更。
struct MockFs {
    indexed: AtomicBool,
    applied: Arc<Mutex<Vec<String>>>,
    audit_clean: bool,
}

impl MockFs {
    fn new(audit_clean: bool, applied: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            indexed: AtomicBool::new(false),
            applied,
            audit_clean,
        }
    }
}

impl FsVolume for MockFs {
    fn fs_id(&self) -> &'static str {
        "mockfs"
    }

    fn caps(&self) -> FsCaps {
        FsCaps {
            write: true,
            delete: true,
            rename: true,
            mkdir: true,
            symlink: false,
            times: false,
            mode: false,
            name_max: 255,
            read_only_reason: None,
        }
    }

    fn entries(&self, _limit: &EntryLimit, _cancel: &CancellationToken) -> VolResult<EntryTree> {
        self.indexed.store(true, Ordering::SeqCst);
        let mut tree = EntryTree::default();
        tree.entries.push(dir(0, "etc"));
        tree.entries.push(file(1, "etc/fstab", 13));
        Ok(tree)
    }

    fn read_at(&self, _id: usize, _offset: u64, buf: &mut [u8]) -> VolResult<usize> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn size_of(&self, _id: usize) -> VolResult<u64> {
        Ok(13)
    }

    fn apply(
        &mut self,
        change: &Change,
        _local: Option<&Path>,
        _cancel: &CancellationToken,
    ) -> VolResult<()> {
        if !self.indexed.load(Ordering::SeqCst) {
            // 真实驱动的原话：「条目 ID 越界（先调 entries()）」
            return Err(VolError::Image("条目 ID 越界: 1（先调 entries()）".into()));
        }
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(match change {
                Change::Replace { entry, .. } => format!("replace:{entry}"),
                Change::Delete(id) => format!("delete:{id}"),
                Change::Append { name, .. } => format!("append:{}", name.as_str()),
                Change::MakeDir { name } => format!("mkdir:{}", name.as_str()),
                Change::Rename { from, to } => {
                    format!("rename:{}->{}", from.as_str(), to.as_str())
                }
                _ => "other".to_string(),
            });
        Ok(())
    }

    fn flush(&mut self) -> VolResult<()> {
        Ok(())
    }

    fn audit(&self) -> VolResult<AuditSummary> {
        Ok(if self.audit_clean {
            AuditSummary::clean()
        } else {
            AuditSummary {
                findings: vec!["mock: 有问题".to_string()],
                skipped: false,
            }
        })
    }
}

struct MockHandler {
    audit_clean: bool,
    /// 与 `MockFs` 共享的记录器（见 `MockFs` 文档）。
    applied: Arc<Mutex<Vec<String>>>,
}

impl VolumeHandler for MockHandler {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn open(&self, _dev: Arc<dyn BlockDevice>, _opts: &VolOptions) -> VolResult<Box<dyn Volume>> {
        Ok(Box::new(MockVolume {
            audit_clean: self.audit_clean,
            applied: self.applied.clone(),
        }))
    }
}

struct MockVolume {
    audit_clean: bool,
    applied: Arc<Mutex<Vec<String>>>,
}

impl Volume for MockVolume {
    fn geometry(&self) -> Geometry {
        Geometry {
            virtual_size: 1 << 20,
            sector_size: 512,
            table: None,
            is_sparse: false,
            has_backing: false,
        }
    }

    fn partitions(&self) -> VolResult<Vec<PartitionView>> {
        Ok(Vec::new())
    }

    fn open_fs(&self, _part: Option<usize>) -> VolResult<Option<Box<dyn FsVolume>>> {
        Ok(Some(Box::new(MockFs::new(
            self.audit_clean,
            self.applied.clone(),
        ))))
    }

    fn caps(&self) -> VolCaps {
        VolCaps {
            writable: true,
            in_place: true,
            read_only_reason: None,
            warnings: Vec::new(),
        }
    }

    fn flush(&mut self) -> VolResult<()> {
        Ok(())
    }
}

fn dir(id: usize, name: &str) -> EntryInfo {
    info(id, name, true, 0)
}

fn file(id: usize, name: &str, size: u64) -> EntryInfo {
    info(id, name, false, size)
}

fn info(id: usize, name: &str, is_dir: bool, size: u64) -> EntryInfo {
    EntryInfo {
        id,
        name: hzip_format::parse_entry_name(name).unwrap(),
        is_dir,
        is_symlink: false,
        symlink_target: None,
        size_raw: size,
        size_packed: 0,
        mtime: None,
        mode: Some(0o644),
        attributes: hzip_format::EntryAttrs::default(),
        encrypted: false,
        header_encrypted: false,
        crc32: None,
        codec: None,
        name_encoding: hzip_format::NameEncoding::Utf8,
        encryption: None,
        compression_level: None,
    }
}

// ------------------------------------------------------------------ 支架

fn base_file(dir_path: &Path) -> PathBuf {
    let p = dir_path.join("disk.img");
    std::fs::write(&p, vec![0xAAu8; 3 * 64 * 1024]).unwrap();
    p
}

struct Ctx {
    token: CancellationToken,
    bus: ProgressBus,
    opts: CreateOptions,
}

impl Ctx {
    fn new(mode: hzip_format::VolumeWriteMode) -> Self {
        Self {
            token: CancellationToken::new(),
            bus: ProgressBus::default(),
            opts: CreateOptions {
                volume_write: mode,
                ..Default::default()
            },
        }
    }

    fn write_ctx(&self) -> hzip_format::WriteCtx<'_> {
        hzip_format::WriteCtx {
            cancel: &self.token,
            progress: &self.bus,
            opts: &self.opts,
        }
    }
}

/// 造编辑器 + 拿到共享记录器（断言路由后的卷内变更用）。
fn editor_with(src: PathBuf, audit_clean: bool) -> (VolumeEditor<MockHandler>, Recorder) {
    let applied: Recorder = Arc::new(Mutex::new(Vec::new()));
    let ed = VolumeEditor::open(
        MockHandler {
            audit_clean,
            applied: applied.clone(),
        },
        Source::File(src),
        VolOptions::default(),
    )
    .unwrap();
    (ed, applied)
}

fn editor(src: PathBuf) -> VolumeEditor<MockHandler> {
    editor_with(src, true).0
}

/// mock 文件系统收到的**卷内**变更（已路由：剥前缀 / 局部 id）。
type Recorder = Arc<Mutex<Vec<String>>>;

/// 取记录快照（锁毒化不影响断言）。
fn recorded(r: &Recorder) -> Vec<String> {
    r.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Replace 夹具：**档案级** id（读侧投影 id 含根行 ⇒ 2 = 卷内 id 1 = `etc/fstab`）。
fn changes_replace() -> ChangeSet {
    ChangeSet {
        changes: vec![Change::Replace {
            entry: 2,
            src: PathBuf::from("/tmp/does-not-matter"),
        }],
    }
}

// ------------------------------------------------------------------ 测试

#[test]
fn replace_and_delete_work_because_the_editor_indexes_first() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path());
    let (ed, applied) = editor_with(src.clone(), true);
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);

    // ① Replace（会经过驱动那张按需建立的 id 表 —— 没建表就必然失败）
    //    档案级 id 2 = 根行(0) 之后的第 2 条 = 卷内 id 1（`etc/fstab`）
    let report = ed
        .write_in_place(changes_replace(), &ctx.write_ctx())
        .expect("Replace 必须可用（编辑器要先列一次条目）");
    assert_eq!(report.entries_written, 1);

    // ② Delete 同理：档案级 id 1 = 卷内 id 0（`etc`）
    let del = ChangeSet {
        changes: vec![Change::Delete(1)],
    };
    ed.write_in_place(del, &ctx.write_ctx())
        .expect("Delete 必须可用");

    // 落到驱动手里的必须是**卷内 id**（投影 id 含根行 ⇒ 减 1）
    assert_eq!(recorded(&applied), vec!["replace:1", "delete:0"]);
}

/// 名基变更（Append/MakeDir/Rename）必须剥掉分区前缀：读侧投影的名字带前缀
/// （`Volume/etc/x`），驱动要的是卷内相对路径（`etc/x`）。**不剥就会在卷内写一个
/// 名叫 `Volume/etc/x` 的错路径**（2026-10-16 路由实现前正是如此）。
#[test]
fn name_based_changes_are_stripped_to_volume_relative_paths() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path());
    let (ed, applied) = editor_with(src.clone(), true);
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);
    // mock 卷无分区表 ⇒ 根名 `Volume`（与读侧投影同一份命名逻辑）
    let local = base_file(tmp.path());
    let cs = ChangeSet {
        changes: vec![
            Change::Append {
                src: local.clone(),
                name: "Volume/etc/new.txt".into(),
            },
            Change::MakeDir {
                name: "Volume/etc/sub".into(),
            },
            Change::Rename {
                from: "Volume/etc/a".into(),
                to: "Volume/etc/b".into(),
            },
        ],
    };
    let report = ed.write_in_place(cs, &ctx.write_ctx()).expect("应成功");
    assert_eq!(report.entries_written, 3);
    assert_eq!(
        recorded(&applied),
        vec!["append:etc/new.txt", "mkdir:etc/sub", "rename:etc/a->etc/b"]
    );
}

/// 路由不成立时**拒绝**（而不是猜路径去写）：
/// - 名字不带本卷前缀（不是冲这个卷来的）；
/// - id 指向根行本身（分区/整卷行不是卷内条目）。
#[test]
fn changes_that_cannot_be_routed_are_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path());
    let (ed, applied) = editor_with(src.clone(), true);
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);

    for cs in [
        ChangeSet {
            changes: vec![Change::Append {
                src: base_file(tmp.path()),
                name: "etc/no-prefix.txt".into(),
            }],
        },
        ChangeSet {
            changes: vec![Change::Delete(0)],
        },
        ChangeSet {
            changes: vec![Change::Replace {
                entry: 0,
                src: base_file(tmp.path()),
            }],
        },
    ] {
        let err = ed.write_in_place(cs, &ctx.write_ctx()).unwrap_err();
        assert!(
            matches!(&err, hzip_error::HzipError::Unsupported { what } if what == "vol.unsupported.change-route"),
            "应按键拒绝：{err:?}"
        );
    }
    assert!(
        recorded(&applied).is_empty(),
        "拒绝的变更一条都不能落到驱动"
    );
}

#[test]
fn commit_leaves_the_original_untouched_when_audit_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path());
    let before = std::fs::read(&src).unwrap();
    let (ed, _applied) = editor_with(src.clone(), false);
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);
    let err = ed
        .write_in_place(changes_replace(), &ctx.write_ctx())
        .unwrap_err();
    assert!(
        matches!(err, hzip_error::HzipError::Unsupported { ref what } if what == "vol.err.audit"),
        "{err:?}"
    );
    assert_eq!(
        std::fs::read(&src).unwrap(),
        before,
        "自检不过 ⇒ 原档一字节不动"
    );
}

#[test]
fn read_only_mode_refuses_before_touching_anything() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path());
    let before = std::fs::read(&src).unwrap();
    let ed = editor(src.clone());
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::ReadOnly);
    let err = ed
        .write_in_place(changes_replace(), &ctx.write_ctx())
        .unwrap_err();
    assert!(
        matches!(err, hzip_error::HzipError::Unsupported { ref what } if what == "vol.readonly.by-setting"),
        "{err:?}"
    );
    assert_eq!(std::fs::read(&src).unwrap(), before);
    // 模式映射（配置值 → 设备链末端）也在此钉一下：只读模式不得被当成覆盖写
    assert_eq!(
        WriteMode::from(hzip_format::VolumeWriteMode::ReadOnly),
        WriteMode::ReadOnly
    );
}

/// `Rename.to` 的两种合法形态都要能过（契约与 GUI 形态不同，见 `route_change` 文档）：
///
/// | 形态 | 来源 | 路由后 |
/// |---|---|---|
/// | 完整路径 `Volume/etc/b` | GUI（`commit_name_edit` 传完整新路径） | 剥前缀 ⇒ `etc/b` |
/// | 裸叶名 `c` | 契约文档口径（驱动按 `parent(from) + '/' + to` 拼） | 原样 ⇒ `c` |
/// | 带 `/` 又无本卷前缀 `other/x` | 指向别处 | **拒绝** |
#[test]
fn rename_accepts_both_target_forms_and_refuses_a_foreign_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path());
    let (ed, applied) = editor_with(src.clone(), true);
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);

    let cs = ChangeSet {
        changes: vec![
            Change::Rename {
                from: "Volume/etc/a".into(),
                to: "Volume/etc/b".into(),
            },
            Change::Rename {
                from: "Volume/etc/a".into(),
                to: "c".into(),
            },
        ],
    };
    ed.write_in_place(cs, &ctx.write_ctx()).expect("应成功");
    assert_eq!(
        recorded(&applied),
        vec!["rename:etc/a->etc/b", "rename:etc/a->c"]
    );

    let bad = ChangeSet {
        changes: vec![Change::Rename {
            from: "Volume/etc/a".into(),
            to: "other/x".into(),
        }],
    };
    let err = ed.write_in_place(bad, &ctx.write_ctx()).unwrap_err();
    assert!(
        matches!(&err, hzip_error::HzipError::Unsupported { what } if what == "vol.unsupported.change-route"),
        "跨卷前缀应拒绝：{err:?}"
    );
    assert_eq!(
        recorded(&applied).len(),
        2,
        "拒绝的变更不得落到驱动（记录数不变）"
    );
}

/// 尾 `/` 是**语义标记**（驱动契约：`Append` 名以 `/` 结尾 ⇒ **只建目录**），路由必须原样保住它。
///
/// 不保的后果是静默写错：`parse_entry_name` 按 N5 丢弃空段，`etc/sub/` 会被规范化成
/// `etc/sub`，驱动于是建一个**空文件**（用户看到的是「新建文件夹」变成了 0 字节文件）。
#[test]
fn append_keeps_the_trailing_slash_into_the_driver() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = base_file(tmp.path());
    let (ed, applied) = editor_with(src.clone(), true);
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);
    let cs = ChangeSet {
        changes: vec![Change::Append {
            src: base_file(tmp.path()),
            name: "Volume/etc/sub/".into(),
        }],
    };
    ed.write_in_place(cs, &ctx.write_ctx()).expect("应成功");
    assert_eq!(
        recorded(&applied),
        vec!["append:etc/sub/"],
        "尾 `/` 必须保到驱动那边"
    );
}

// ------------------------------------------------------------------ 多分区归集（2026-10-16）

/// 两分区 mock：分区 0 = `etc/*`、分区 1 = `boot/*`（无标签 ⇒ 顶层行名 `Partition 1/2`）。
///
/// 用户实报「该镜像含多个卷（分区），暂不支持在其内部编辑」——多分区镜像（EFI + root 是
/// 常态）此前一律拒绝写回。本组用例守的就是「按分区归集」：**id 型变更按 id 区间、
/// 名字型变更按根名前缀**选分区，选不中一律拒绝，且 id 算术必须与读侧投影逐位一致。
struct TwoPartFs {
    part: usize,
    indexed: AtomicBool,
    applied: Recorder,
}

impl TwoPartFs {
    fn new(part: usize, applied: Recorder) -> Self {
        Self {
            part,
            indexed: AtomicBool::new(false),
            applied,
        }
    }
}

impl FsVolume for TwoPartFs {
    fn fs_id(&self) -> &'static str {
        "mockfs"
    }

    fn caps(&self) -> FsCaps {
        FsCaps {
            write: true,
            delete: true,
            rename: true,
            mkdir: true,
            symlink: false,
            times: false,
            mode: false,
            name_max: 255,
            read_only_reason: None,
        }
    }

    fn entries(&self, _limit: &EntryLimit, _cancel: &CancellationToken) -> VolResult<EntryTree> {
        self.indexed.store(true, Ordering::SeqCst);
        let mut tree = EntryTree::default();
        match self.part {
            0 => {
                tree.entries.push(dir(0, "etc"));
                tree.entries.push(file(1, "etc/fstab", 13));
            }
            _ => {
                tree.entries.push(dir(0, "boot"));
                tree.entries.push(file(1, "boot/vmlinuz", 5));
            }
        }
        Ok(tree)
    }

    fn read_at(&self, _id: usize, _offset: u64, buf: &mut [u8]) -> VolResult<usize> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn size_of(&self, _id: usize) -> VolResult<u64> {
        Ok(13)
    }

    fn apply(
        &mut self,
        change: &Change,
        _local: Option<&Path>,
        _cancel: &CancellationToken,
    ) -> VolResult<()> {
        if !self.indexed.load(Ordering::SeqCst) {
            return Err(VolError::Image("条目 ID 越界: 1（先调 entries()）".into()));
        }
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(match change {
                Change::Replace { entry, .. } => format!("replace:{entry}"),
                Change::Delete(id) => format!("delete:{id}"),
                Change::Append { name, .. } => format!("append:{}", name.as_str()),
                Change::MakeDir { name } => format!("mkdir:{}", name.as_str()),
                Change::Rename { from, to } => {
                    format!("rename:{}->{}", from.as_str(), to.as_str())
                }
                _ => "other".to_string(),
            });
        Ok(())
    }

    fn flush(&mut self) -> VolResult<()> {
        Ok(())
    }

    fn audit(&self) -> VolResult<AuditSummary> {
        Ok(AuditSummary::clean())
    }
}

struct TwoPartHandler {
    /// 每个分区一份记录器（断言「变更落到了哪个分区」）。
    applied: [Recorder; 2],
}

impl VolumeHandler for TwoPartHandler {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn open(&self, _dev: Arc<dyn BlockDevice>, _opts: &VolOptions) -> VolResult<Box<dyn Volume>> {
        Ok(Box::new(TwoPartVolume {
            applied: self.applied.clone(),
        }))
    }
}

struct TwoPartVolume {
    applied: [Recorder; 2],
}

impl Volume for TwoPartVolume {
    fn geometry(&self) -> Geometry {
        Geometry {
            virtual_size: 32 << 20,
            sector_size: 512,
            table: None,
            is_sparse: false,
            has_backing: false,
        }
    }

    fn partitions(&self) -> VolResult<Vec<PartitionView>> {
        Ok((0..2)
            .map(|id| PartitionView {
                id,
                start: (id as u64 + 1) << 20,
                len: 8 << 20,
                type_byte: Some(0x83),
                label: None, // 无标签 ⇒ 顶层行名 `Partition N`（界面同款）
                type_name: "Linux".to_string(),
                fs: Some(hzip_vol::FsKind::Ext4),
            })
            .collect())
    }

    fn open_fs(&self, part: Option<usize>) -> VolResult<Option<Box<dyn FsVolume>>> {
        let ix = part.unwrap_or(0);
        Ok(Some(Box::new(TwoPartFs::new(ix, self.applied[ix].clone()))))
    }

    fn caps(&self) -> VolCaps {
        VolCaps {
            writable: true,
            in_place: true,
            read_only_reason: None,
            warnings: Vec::new(),
        }
    }

    fn flush(&mut self) -> VolResult<()> {
        Ok(())
    }
}

/// 两分区编辑器 + 两份记录器。
fn two_part_editor(src: PathBuf) -> (VolumeEditor<TwoPartHandler>, Recorder, Recorder) {
    let p0: Recorder = Arc::new(Mutex::new(Vec::new()));
    let p1: Recorder = Arc::new(Mutex::new(Vec::new()));
    let ed = VolumeEditor::open(
        TwoPartHandler {
            applied: [p0.clone(), p1.clone()],
        },
        Source::File(src),
        VolOptions::default(),
    )
    .unwrap();
    (ed, p0, p1)
}

fn cs(changes: Vec<Change>) -> ChangeSet {
    ChangeSet { changes }
}

/// id 型变更：`Replace{entry}`/`Delete(id)` 按**读侧投影的 id 区间**落到对应分区。
///
/// 投影（两分区 × 各 2 条）：`0 = Partition 1`、`1 = Partition 1/etc`、
/// `2 = Partition 1/etc/fstab`、`3 = Partition 2`、`4 = Partition 2/boot`、
/// `5 = Partition 2/boot/vmlinuz`。
#[test]
fn multi_partition_id_changes_land_in_the_right_partition() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ed, p0, p1) = two_part_editor(base_file(tmp.path()));
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);

    // 分区 2 的第 2 条（全局 5 → 卷内 1 = boot/vmlinuz）
    ed.write_in_place(
        cs(vec![Change::Replace {
            entry: 5,
            src: PathBuf::from("/tmp/x"),
        }]),
        &ctx.write_ctx(),
    )
    .expect("分区 2 的替换应成功");
    // 分区 1 的第 2 条（全局 2 → 卷内 1 = etc/fstab）
    ed.write_in_place(cs(vec![Change::Delete(2)]), &ctx.write_ctx())
        .expect("分区 1 的删除应成功");

    assert_eq!(recorded(&p0), vec!["delete:1"], "分区 1 只该收到自己那一条");
    assert_eq!(
        recorded(&p1),
        vec!["replace:1"],
        "分区 2 只该收到自己那一条（id 需减去本分区基数 4）"
    );
}

/// 名字型变更：`Append`/`MakeDir`/`Rename` 按 `"{根名}/"` 前缀选分区并剥掉前缀。
#[test]
fn multi_partition_name_changes_land_in_the_right_partition() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ed, p0, p1) = two_part_editor(base_file(tmp.path()));
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);
    let local = base_file(tmp.path());

    ed.write_in_place(
        cs(vec![
            Change::Append {
                src: local.clone(),
                name: "Partition 2/boot/new.txt".into(),
            },
            Change::MakeDir {
                name: "Partition 1/etc/sub/".into(),
            },
        ]),
        &ctx.write_ctx(),
    )
    .expect("按名字选分区应成功");

    assert_eq!(
        recorded(&p0),
        vec!["mkdir:etc/sub/"],
        "尾 `/` 必须保到驱动（只建目录语义）"
    );
    assert_eq!(recorded(&p1), vec!["append:boot/new.txt"]);

    // Rename 两种形态都过：完整路径（GUI 形态）与裸叶名（契约形态）
    ed.write_in_place(
        cs(vec![Change::Rename {
            from: "Partition 2/boot/vmlinuz".into(),
            to: "Partition 2/boot/vmlinuz2".into(),
        }]),
        &ctx.write_ctx(),
    )
    .expect("分区 2 内改名应成功");
    ed.write_in_place(
        cs(vec![Change::Rename {
            from: "Partition 1/etc/fstab".into(),
            to: "fstab2".into(),
        }]),
        &ctx.write_ctx(),
    )
    .expect("分区 1 内改名（裸叶名）应成功");

    assert_eq!(
        recorded(&p1),
        vec!["append:boot/new.txt", "rename:boot/vmlinuz->boot/vmlinuz2"]
    );
    assert_eq!(
        recorded(&p0),
        vec!["mkdir:etc/sub/", "rename:etc/fstab->fstab2"]
    );
}

/// 定不到目标的变更**一律拒绝**（顶层行、越界 id、无前缀名、跨分区改名），且不落驱动。
#[test]
fn unroutable_changes_are_refused_without_touching_any_partition() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ed, p0, p1) = two_part_editor(base_file(tmp.path()));
    let ctx = Ctx::new(hzip_format::VolumeWriteMode::Overlay);
    let want_refusal = |cs: ChangeSet, why: &str| {
        let err = ed.write_in_place(cs, &ctx.write_ctx()).unwrap_err();
        assert!(
            matches!(&err, hzip_error::HzipError::Unsupported { what }
                if what == "vol.unsupported.change-route"),
            "{why} 应拒绝：{err:?}"
        );
    };

    want_refusal(cs(vec![Change::Delete(0)]), "顶层行（分区 1 本身）");
    want_refusal(cs(vec![Change::Delete(3)]), "顶层行（分区 2 本身）");
    want_refusal(
        cs(vec![Change::Replace {
            entry: 99,
            src: PathBuf::from("/x"),
        }]),
        "越界 id",
    );
    want_refusal(
        cs(vec![Change::Append {
            src: PathBuf::from("/x"),
            name: "boot/new.txt".into(),
        }]),
        "没有分区前缀的名字",
    );
    want_refusal(
        cs(vec![Change::Rename {
            from: "Partition 1/etc/fstab".into(),
            to: "Partition 2/boot/fstab".into(),
        }]),
        "跨分区改名",
    );

    assert!(recorded(&p0).is_empty(), "被拒的变更不得落到分区 1");
    assert!(recorded(&p1).is_empty(), "被拒的变更不得落到分区 2");
}

/// **id 算术必须与读侧投影一致**（防止写侧偏移与读侧漂移——漂了就写到隔壁分区去）。
///
/// 直接拿同一份两分区卷跑 [`VolumeReader`]，把它的行序/名字钉下来；上面的用例正是按
/// 这份投影取 id 的（`entry_base` 的语义由此锚定）。
#[test]
fn editor_id_space_matches_the_read_projection() {
    use hzip_format::ArchiveReader as _;
    use hzip_format_testkit::vol::MemDevice;
    use hzip_vol::reader::VolumeReader;

    let handler = TwoPartHandler {
        applied: [
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        ],
    };
    let dev: Arc<dyn BlockDevice> = Arc::new(MemDevice::new(1 << 20));
    let vol = handler.open(dev, &VolOptions::default()).unwrap();
    let reader = VolumeReader::build(vol, &EntryLimit::default(), &CancellationToken::new())
        .expect("读侧投影应能建起来");
    let rows: Vec<(usize, String)> = reader
        .entries()
        .unwrap()
        .iter()
        .map(|e| (e.id, e.name.as_str().to_string()))
        .collect();
    assert_eq!(
        rows,
        vec![
            (0, "Partition 1".to_string()),
            (1, "Partition 1/etc".to_string()),
            (2, "Partition 1/etc/fstab".to_string()),
            (3, "Partition 2".to_string()),
            (4, "Partition 2/boot".to_string()),
            (5, "Partition 2/boot/vmlinuz".to_string()),
        ],
        "读侧投影的 id/名字序即写侧路由的锚点"
    );
}
