//! 卷读取器投影的契约测试：**分区即顶层目录**、名字规则、惰性读、能力聚合、提取。
//!
//! 用 mock 的 `Volume`/`FsVolume`（不依赖任何具体格式驱动）——测的是投影逻辑本身：
//! 名字怎么定、前缀怎么加、id 怎么映射、只读怎么聚合、`volume_rows` 提供什么。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 集成测试：仓库惯例

use std::collections::HashMap;
use std::io::Read as _;

use hzip_format::{ArchiveReader as _, EntryName, ExtractCtx, PasswordSource, Selection};
use hzip_progress::{CancellationToken, ProgressBus};
use hzip_vol::error::VolResult;
use hzip_vol::reader::VolumeReader;
use hzip_vol::{
    EntryLimit, EntryTree, FsCaps, FsVolume, Geometry, PartitionView, ReadOnlyReason, VolCaps,
    Volume,
};

// ------------------------------------------------------------------ mock

struct MockEntry {
    path: &'static str,
    is_dir: bool,
    data: Vec<u8>,
}

/// 一个假文件系统：条目按给定顺序编号（id = 下标）。
struct MockFs {
    id: &'static str,
    entries: Vec<MockEntry>,
    caps: FsCaps,
    /// 计数器：抓「读是否真的走了偏移量」。
    reads: std::sync::atomic::AtomicUsize,
}

impl MockFs {
    fn new(id: &'static str, entries: Vec<MockEntry>, caps: FsCaps) -> Self {
        Self {
            id,
            entries,
            caps,
            reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl FsVolume for MockFs {
    fn fs_id(&self) -> &'static str {
        self.id
    }

    fn caps(&self) -> FsCaps {
        self.caps
    }

    fn entries(&self, limit: &EntryLimit, _cancel: &CancellationToken) -> VolResult<EntryTree> {
        let mut tree = EntryTree::default();
        for (i, e) in self.entries.iter().enumerate() {
            // 尊重调用方给的额度（截断用例靠它；默认额度下全部列出）
            if tree.entries.len() >= limit.max_entries {
                tree.truncated = true;
                break;
            }
            tree.entries.push(hzip_format::EntryInfo {
                id: i,
                name: hzip_format::parse_entry_name(e.path).unwrap(),
                is_dir: e.is_dir,
                is_symlink: false,
                symlink_target: None,
                size_raw: if e.is_dir { 0 } else { e.data.len() as u64 },
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
            });
        }
        Ok(tree)
    }

    fn read_at(&self, id: usize, offset: u64, buf: &mut [u8]) -> VolResult<usize> {
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let e = self
            .entries
            .get(id)
            .ok_or_else(|| hzip_vol::VolError::Image("bad id".into()))?;
        let off = usize::try_from(offset).unwrap_or(usize::MAX);
        if off >= e.data.len() {
            return Ok(0);
        }
        let n = (e.data.len() - off).min(buf.len());
        buf[..n].copy_from_slice(&e.data[off..off + n]);
        Ok(n)
    }

    fn size_of(&self, id: usize) -> VolResult<u64> {
        Ok(self
            .entries
            .get(id)
            .map(|e| e.data.len() as u64)
            .unwrap_or(0))
    }

    fn apply(
        &mut self,
        _change: &hzip_format::Change,
        _local: Option<&std::path::Path>,
        _cancel: &CancellationToken,
    ) -> VolResult<()> {
        Err(hzip_vol::VolError::ReadOnly(ReadOnlyReason::BySetting))
    }

    fn flush(&mut self) -> VolResult<()> {
        Ok(())
    }

    fn audit(&self) -> VolResult<hzip_vol::AuditSummary> {
        Ok(hzip_vol::AuditSummary::clean())
    }
}

struct MockVolume {
    parts: Vec<PartitionView>,
    fses: HashMap<usize, FsCaps>,
    size: u64,
    writable: bool,
}

impl Volume for MockVolume {
    fn geometry(&self) -> Geometry {
        Geometry {
            virtual_size: self.size,
            sector_size: 512,
            table: Some(hzip_vol::TableKind::Mbr),
            is_sparse: false,
            has_backing: false,
        }
    }

    fn partitions(&self) -> VolResult<Vec<PartitionView>> {
        Ok(self.parts.clone())
    }

    fn open_fs(&self, part: Option<usize>) -> VolResult<Option<Box<dyn FsVolume>>> {
        let Some(id) = part else {
            return Ok(None);
        };
        let Some(caps) = self.fses.get(&id) else {
            return Ok(None); // 未识别卷
        };
        let entries = match id {
            0 => vec![
                MockEntry {
                    path: "EFI",
                    is_dir: true,
                    data: Vec::new(),
                },
                MockEntry {
                    path: "EFI/BOOTX64.EFI",
                    is_dir: false,
                    data: b"boot".to_vec(),
                },
            ],
            1 => vec![
                MockEntry {
                    path: "etc",
                    is_dir: true,
                    data: Vec::new(),
                },
                MockEntry {
                    path: "etc/fstab",
                    is_dir: false,
                    data: b"LABEL=root /\n".to_vec(),
                },
                MockEntry {
                    path: "big.bin",
                    is_dir: false,
                    data: vec![7u8; 3 * 1024 * 1024],
                },
            ],
            _ => vec![],
        };
        Ok(Some(Box::new(MockFs::new("mockfs", entries, *caps))))
    }

    fn caps(&self) -> VolCaps {
        VolCaps {
            writable: self.writable,
            in_place: true,
            read_only_reason: (!self.writable).then_some(ReadOnlyReason::Dirty),
            warnings: Vec::new(),
        }
    }

    fn flush(&mut self) -> VolResult<()> {
        Ok(())
    }
}

fn part(id: usize, label: &str, fs: Option<hzip_vol::FsKind>) -> PartitionView {
    PartitionView {
        id,
        start: (id as u64 + 1) << 20,
        len: 8 << 20,
        type_byte: Some(0x83),
        // 空串 = 「无标签」（MBR 分区没有标签概念）
        label: (!label.is_empty()).then(|| label.to_string()),
        type_name: "Linux".to_string(),
        fs,
    }
}

fn rw_caps() -> FsCaps {
    FsCaps {
        write: true,
        delete: true,
        rename: true,
        mkdir: true,
        symlink: true,
        times: true,
        mode: true,
        name_max: 255,
        read_only_reason: None,
    }
}

fn build(vol: MockVolume) -> VolumeReader {
    VolumeReader::build(
        Box::new(vol),
        &EntryLimit::default(),
        &CancellationToken::new(),
    )
    .unwrap()
}

fn two_partition_volume() -> MockVolume {
    let mut fses = HashMap::new();
    fses.insert(0, rw_caps());
    fses.insert(1, rw_caps());
    MockVolume {
        parts: vec![
            part(0, "EFI System", Some(hzip_vol::FsKind::Fat)),
            part(1, "root", Some(hzip_vol::FsKind::Ext4)),
        ],
        fses,
        size: 32 << 20,
        writable: true,
    }
}

// ------------------------------------------------------------------ 测试

#[test]
fn partitions_become_top_level_directories_with_labels() {
    let r = build(two_partition_volume());
    let names: Vec<String> = r
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "EFI System", // 分区行（label）
            "EFI System/EFI",
            "EFI System/EFI/BOOTX64.EFI",
            "root", // 第二个分区行（label）
            "root/etc",
            "root/etc/fstab",
            "root/big.bin",
        ],
        "分区即顶层目录，子条目带分区前缀"
    );
    let entries = r.entries().unwrap();
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(e.id, i, "投影必须把 id 重编号成下标：{}", e.name.as_str());
    }
    assert!(entries[0].is_dir, "分区行是目录");
    assert!(entries[1].is_dir, "分区内目录仍是目录");
    assert_eq!(entries[2].size_raw, 4, "文件大小来自文件系统");
}

#[test]
fn volume_rows_carry_label_fs_and_geometry_for_the_ui() {
    let r = build(two_partition_volume());
    let rows = r.volume_rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "EFI System");
    assert_eq!(rows[0].label.as_deref(), Some("EFI System"));
    assert_eq!(rows[0].fs.as_deref(), Some("mockfs"));
    assert_eq!(rows[0].index, 0);
    assert_eq!(
        rows[0].start,
        1 << 20,
        "分区起始进元数据（信息面板/诊断用）"
    );
    assert_eq!(rows[1].name, "root");
    assert_eq!(rows[1].len, 8 << 20);
}

#[test]
fn unlabeled_partitions_get_stable_ascii_names_and_no_label_meta() {
    let mut vol = two_partition_volume();
    vol.parts[0].label = None; // 无 label
    vol.parts[1].label = Some("Partition 1".to_string()); // 与回退名冲突 ⇒ 去重
    let r = build(vol);
    let rows = r.volume_rows();
    assert_eq!(rows[0].name, "Partition 1", "无 label ⇒ 稳定 ASCII 名");
    assert_eq!(
        rows[0].label, None,
        "无 label 时元数据留空，界面按语言显示「分区 N」"
    );
    assert_eq!(
        rows[1].name, "Partition 1 (2)",
        "重名追加序号（树路径必须唯一）"
    );
    let names: Vec<String> = r
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        !names.contains(&"root/etc/fstab".to_string()),
        "label 被换掉后不应再有 root"
    );
    assert!(names.contains(&"Partition 1 (2)/etc/fstab".to_string()));
}

#[test]
fn read_entry_is_random_repeatable_and_chunked() {
    let r = build(two_partition_volume());
    let entries = r.entries().unwrap();
    let big = entries
        .iter()
        .position(|e| e.name.as_str().ends_with("big.bin"))
        .unwrap();
    let small = entries
        .iter()
        .position(|e| e.name.as_str().ends_with("fstab"))
        .unwrap();

    // 乱序 + 重复
    let mut a = Vec::new();
    r.read_entry(small, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut a)
        .unwrap();
    assert_eq!(a, b"LABEL=root /\n");
    let mut b = Vec::new();
    r.read_entry(big, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut b)
        .unwrap();
    assert_eq!(b.len(), 3 * 1024 * 1024, "大条目流式读全");
    assert!(b.iter().all(|&x| x == 7));
    let mut c = Vec::new();
    r.read_entry(small, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut c)
        .unwrap();
    assert_eq!(c, a, "重复读同一 id 结果一致");

    // 分区行（目录）取流：给空流而不是报错
    let mut d = Vec::new();
    r.read_entry(0, hzip_format::PasswordRef::none())
        .unwrap()
        .read_to_end(&mut d)
        .unwrap();
    assert!(d.is_empty());
}

#[test]
fn unknown_filesystem_partition_keeps_its_row_but_has_no_children() {
    let mut fses = HashMap::new();
    fses.insert(0, rw_caps()); // 1 号分区无驱动 ⇒ open_fs → None
    let vol = MockVolume {
        parts: vec![
            part(0, "boot", Some(hzip_vol::FsKind::Ext4)),
            part(1, "data", Some(hzip_vol::FsKind::Other)),
        ],
        fses,
        size: 32 << 20,
        writable: true,
    };
    let r = build(vol);
    let names: Vec<String> = r
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str().to_string())
        .collect();
    assert!(
        names.contains(&"data".to_string()),
        "未识别卷仍要有行（显示为未知卷）"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("data/")),
        "未识别卷没有子条目"
    );
    let rows = r.volume_rows();
    assert_eq!(rows[1].fs, None, "元数据里 fs 为空 ⇒ 界面显示「未知卷」");
}

#[test]
fn caps_are_the_conjunction_of_partition_filesystems() {
    // 两个分区都可写 ⇒ 可编辑
    let r = build(two_partition_volume());
    assert!(r.caps().edit, "全部可写 ⇒ 可编辑");

    // 一个分区只读 ⇒ 整体不可编辑，且理由可查
    let mut fses = HashMap::new();
    fses.insert(0, rw_caps());
    fses.insert(1, FsCaps::read_only(ReadOnlyReason::JournalPending));
    let vol = MockVolume {
        parts: vec![
            part(0, "boot", Some(hzip_vol::FsKind::Ext4)),
            part(1, "root", Some(hzip_vol::FsKind::Ext4)),
        ],
        fses,
        size: 32 << 20,
        writable: true,
    };
    let r = build(vol);
    assert!(!r.caps().edit, "任一分区只读 ⇒ 编辑关闭");
    assert_eq!(r.read_only_reason(), Some(ReadOnlyReason::JournalPending));

    // 卷级只读优先
    let mut vol2 = two_partition_volume();
    vol2.writable = false;
    let r2 = build(vol2);
    assert!(!r2.caps().edit);
    assert_eq!(
        r2.read_only_reason(),
        Some(ReadOnlyReason::Dirty),
        "卷级理由优先"
    );
}

#[test]
fn entry_limit_truncates_and_is_reported() {
    let vol = two_partition_volume();
    let reader = VolumeReader::build(
        Box::new(vol),
        &EntryLimit {
            max_entries: 2,
            max_depth: 64,
        },
        &CancellationToken::new(),
    )
    .unwrap();
    // mock 每个文件系统最多给 2 条 ⇒ 截断标记应被传播
    assert!(
        reader.truncated(),
        "截断必须如实上报（界面提示「部分列出」）"
    );
    assert!(reader.entries().unwrap().len() >= 2);
}

#[test]
fn extract_writes_files_under_the_partition_prefix() {
    let r = build(two_partition_volume());
    let tmp = tempfile::TempDir::new().unwrap();
    let bus = ProgressBus::default();
    let token = CancellationToken::new();
    let ctx = ExtractCtx {
        cancel: &token,
        progress: &bus,
        passwords: &NoPasswords,
        conflicts: hzip_format::ConflictPolicy::Overwrite,
        sanitize: &hzip_format::SanitizeConfig::default(),
        quota: &hzip_format::QuotaLimits::default(),
        best_effort: false,
        codepage: None,
    };
    let report = r.extract(&Selection::All, &ctx, tmp.path()).unwrap();
    assert!(report.entries_done >= 5, "目录+文件都算条目：{report:?}");
    assert_eq!(
        std::fs::read(tmp.path().join("root/etc/fstab")).unwrap(),
        b"LABEL=root /\n"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("EFI System/EFI/BOOTX64.EFI")).unwrap(),
        b"boot"
    );
    assert!(tmp.path().join("root/etc").is_dir(), "目录被建出来");
}

/// 无密码源（卷内条目不加密）。
struct NoPasswords;

impl PasswordSource for NoPasswords {
    fn password(&self, _entry: Option<&EntryName>) -> Option<zeroize::Zeroizing<String>> {
        None
    }
}
