//! NTFS 文件系统镜像夹具：上游 `mkfs` + 上游写路径**现场造**（不录字节）。
//!
//! # 与 ext4 夹具的两处不同（如实记）
//!
//! 1. **不保证逐字节可重放**：上游 `mkfs` 与写路径用系统时钟填
//!    `$STANDARD_INFORMATION` 的时间戳（`record_build::nt_time_now`），故同一组入参
//!    两次调用得到的时间戳不同。结构、名字、内容仍然是确定的——用例**不要**拿两份
//!    字节做相等断言（ext4 夹具那条「逐字节相同」的红线在这里做不到，不是忽略了它）。
//! 2. **卷是「刚 format 出来」的 1.2 + `UPGRADE_ON_MOUNT`**：与 Windows
//!    `format.com` 的产物一致。上游第一次**写挂载**时会把它升到 3.1——本夹具**不做**
//!    这一步，正好让驱动侧的写挂载路径（`prepare_rw` + `upgrade_volume_version`）
//!    被真实地走一遍。
//!
//! # 四个形态
//!
//! | 夹具 | 卷的状态 | 服务的用例 |
//! |---|---|---|
//! | [`ntfs_image`] | 干净可写 | 列/读/五类变更往返 |
//! | [`ntfs_image_dirty`] | `$Volume` 的 DIRTY 位置位 | 拒写三态之「dirty」 |
//! | [`ntfs_image_pending_log`] | `$LogFile` 首页被清成零（非全 `0xFF`、解析不出重启页） | 拒写三态之「日志含事务」 |
//! | [`ntfs_image_hibernated`] | 根目录有 `hiberfil.sys` 且头部以 `hibr` 开头 | 拒写三态之「休眠」 |

use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo as _, PathIo};
use fs_ntfs::{data_runs, fsck, mft_io, mkfs, read, write};

/// 夹具卷容量（32 MiB）。
///
/// 下界由上游 `mkfs` 自身的布局决定：簇数 ≥ 1024，且 `$LogFile` 一份就 ~3.78 MiB，
/// 故 4 MiB 那种「最小卷」根本 format 不出来（夹具也就别假装能用）。
pub const NTFS_IMAGE_BYTES: u64 = 32 << 20;

/// 簇大小（4 KiB，与真实发行版默认一致）。
pub const NTFS_CLUSTER_SIZE: u32 = 4096;

/// MFT 记录大小（1 KiB，NTFS 常规值）。
pub const NTFS_MFT_RECORD_SIZE: u32 = 1024;

/// 固定卷序列号（= ASCII `HZIPNTFS`）。
pub const NTFS_SERIAL: u64 = 0x485A_4950_4E54_4653;

/// 固定卷标。
pub const NTFS_LABEL: &str = "HZIP";

/// `hiberfil.sys` 的头部长度（上游 `fsck::hibernation_io` 读这么多）。
const HIBERFIL_BYTES: usize = 4096;

/// 干净可写的 NTFS 卷，含给定文件（`"dir/file.txt"` 形式，父目录自动建）。
#[must_use]
pub fn ntfs_image(files: &[(&str, &[u8])]) -> Vec<u8> {
    build(files, Phase::Clean)
}

/// 同上，但把 `$Volume` 的 DIRTY 标志置位（未干净卸载）。
///
/// 用上游自己的 `fsck::set_dirty`，不是手改字节——夹具要造的是**上游认得**的状态。
#[must_use]
pub fn ntfs_image_dirty(files: &[(&str, &[u8])]) -> Vec<u8> {
    build(files, Phase::Dirty)
}

/// 同上，但让 `$LogFile` 呈现「含事务」：把日志首页清成零。
///
/// 后果是上游 `logfile_state_io` 得到 `Pending`（日志非全 `0xFF`，又解析不出重启页）
/// ⇒ 写挂载要求显式 `replay_journal`；而这次重放本身也会失败（没有可重放的客户记录），
/// 于是驱动**仍然只读**——这正是「拒写保护」要覆盖的状态。
#[must_use]
pub fn ntfs_image_pending_log(files: &[(&str, &[u8])]) -> Vec<u8> {
    build(files, Phase::PendingLog)
}

/// 同上，但根目录有 `hiberfil.sys` 且头部以 `hibr` 开头（Windows 休眠/快速启动残留）。
#[must_use]
pub fn ntfs_image_hibernated(files: &[(&str, &[u8])]) -> Vec<u8> {
    build(files, Phase::Hibernated)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Clean,
    Dirty,
    PendingLog,
    Hibernated,
}

fn build(files: &[(&str, &[u8])], phase: Phase) -> Vec<u8> {
    let tmp = tempfile::Builder::new()
        .prefix("hzip-spool-ntfs-fixture-")
        .tempfile()
        .expect("夹具：建临时镜像");
    // `mkfs` 要求设备长度 ≥ 请求的格式化长度，故先定长再格式化。
    tmp.as_file()
        .set_len(NTFS_IMAGE_BYTES)
        .expect("夹具：定长镜像");
    let path = tmp.path().to_path_buf();

    {
        let mut dev = PathIo::open_rw(&path).expect("夹具：打开镜像 rw");
        mkfs::format_filesystem(
            &mut dev,
            NTFS_IMAGE_BYTES,
            NTFS_CLUSTER_SIZE,
            NTFS_MFT_RECORD_SIZE,
            Some(NTFS_LABEL),
            Some(NTFS_SERIAL),
        )
        .expect("夹具：上游 mkfs 格式化失败");

        if phase == Phase::Hibernated {
            let mut header = vec![0u8; HIBERFIL_BYTES];
            header[..4].copy_from_slice(b"hibr");
            put_file(&mut dev, "/hiberfil.sys", &header);
        }
        for (name, data) in files {
            put_file(&mut dev, name, data);
        }
    }

    match phase {
        Phase::Clean | Phase::Hibernated => {}
        Phase::Dirty => {
            fsck::set_dirty(&path).expect("夹具：置 dirty 标志");
        }
        Phase::PendingLog => zero_logfile_head(&path),
    }

    // 造完复核一次：夹具造出来的必须是**上游认得**的卷，否则用例会在别处莫名其妙地炸。
    let mut ro = PathIo::open_ro(&path).expect("夹具：打开镜像 ro");
    let info = read::read_volume_info(&mut ro).expect("夹具：造出来的不是可解析的 NTFS 卷");
    assert_eq!(info.cluster_size, NTFS_CLUSTER_SIZE, "簇大小不符");
    assert_eq!(info.serial_number, NTFS_SERIAL, "序列号不符");

    read_bytes(&path)
}

/// 建父目录（逐级）+ 建文件 + 写内容。`name` 形如 `dir/file.txt` 或 `/dir/file.txt`。
fn put_file(io: &mut PathIo, name: &str, data: &[u8]) {
    let full = if name.starts_with('/') {
        name.to_string()
    } else {
        format!("/{name}")
    };
    let (parent, base) = match full.rfind('/') {
        Some(at) => (full[..at].to_string(), full[at + 1..].to_string()),
        None => (String::new(), full.clone()),
    };
    let mut current = String::new();
    for segment in parent.split('/') {
        if segment.is_empty() {
            continue;
        }
        let child = format!("{current}/{segment}");
        if read::resolve_path(io, &child).is_err() {
            write::mkdir_io(io, &current, segment).expect("夹具：建目录失败");
        }
        current = child;
    }
    write::create_file_io(io, &current, &base).expect("夹具：建文件失败");
    write::write_file_contents_io(io, &full, data).expect("夹具：写内容失败");
}

/// 把 `$LogFile` 的首页清成零：非全 `0xFF`（不是 Empty），又解析不出重启页 ⇒ `Pending`。
fn zero_logfile_head(path: &Path) {
    let mut io = PathIo::open_rw(path).expect("夹具：打开镜像 rw");
    let rec = read::resolve_path(&mut io, "/$LogFile").expect("夹具：找不到 $LogFile");
    let (params, record) =
        mft_io::read_mft_record_io(&mut io, rec).expect("夹具：读 $LogFile 记录");
    let loc = attr_io::find_attribute(&record, AttrType::Data, None)
        .expect("夹具：$LogFile 没有无名 $DATA");
    let pairs = loc
        .non_resident_mapping_pairs_offset
        .expect("夹具：$LogFile 的 $DATA 应当非常驻") as usize;
    let runs =
        data_runs::decode_runs(&record[loc.attr_offset + pairs..loc.attr_offset + loc.attr_length])
            .expect("夹具：解 $LogFile 的 run list 失败");
    let first = runs.first().expect("夹具：$LogFile 的 run list 为空");
    let lcn = first.lcn.expect("夹具：$LogFile 首个 run 是空洞");
    let offset = lcn * params.cluster_size;
    let page = usize::try_from(first.length * params.cluster_size)
        .expect("夹具：日志 run 长度超出 usize")
        .min(4096);
    io.write_all_at(offset, &vec![0u8; page])
        .expect("夹具：清 $LogFile 首页失败");
    io.sync().expect("夹具：落盘失败");
}

fn read_bytes(path: &Path) -> Vec<u8> {
    let mut file = File::open(path).expect("夹具：读回镜像失败");
    let len = file.metadata().expect("夹具：stat").len();
    let mut out = vec![0u8; usize::try_from(len).expect("夹具：镜像超出 usize")];
    file.read_exact(&mut out).expect("夹具：读回镜像失败");
    out
}
