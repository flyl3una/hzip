//! Read path: virtual offset -> L1 -> L2 -> host offset -> file read.
//!
//! Supported now:
//! - Uncompressed clusters, read and written
//! - Zlib-compressed clusters (raw deflate per the spec), read
//! - Zstd-compressed clusters (v3 `compression_type = 1`), read
//! - Backing-file chain (recursive, only when opened by path — the
//!   on-device API has no path context to resolve a parent against),
//!   with copy-up when a write lands on a cluster the parent supplies
//! - Sparse / v3 zero-flagged clusters
//! - Cluster allocation and refcount maintenance
//!
//! Not yet:
//! - Internal snapshots, encryption, external data file, extended L2
//! - Writing compressed clusters: a write to a compressed cluster
//!   allocates an uncompressed one in its place.
//!
//! ## Backing storage
//!
//! The reader is generic over [`fs_core::BlockDevice`]. Open from a path
//! via [`Qcow2Reader::open`] / [`Qcow2Reader::open_rw`] (the file is
//! wrapped in a [`fs_core::FileDevice`] internally), or hand in any
//! other `BlockDevice` via [`Qcow2Reader::open_on_device`] /
//! [`Qcow2Reader::open_rw_on_device`]. The on-device variants are how
//! the qcow2 layer stacks on top of an FSKit-supplied block resource,
//! a slice reader, or any other host-managed device.

use crate::error::{Error, Result};
use crate::header::Header;
use flate2::read::DeflateDecoder;
use fs_core::{BlockDevice, FileDevice};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Bits 9..55 inclusive — host cluster offset for L1 and uncompressed L2.
const OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;

/// Bytes per entry in the refcount table and in an L1 or L2 table.
///
/// All three are arrays of big-endian `u64`, and the `8` was written
/// out at thirteen sites where it could equally have been a byte count,
/// a bit width or an alignment.
pub(crate) const TABLE_ENTRY_BYTES: u64 = 8;

/// Bytes per refcount-*block* entry — the only width this crate walks.
///
/// The format allows `1 << refcount_order` bits per entry; this crate
/// implements `refcount_order == 4` alone, and every caller goes
/// through [`Qcow2Reader::refcount_entries_per_block`], which refuses
/// anything else rather than walking it at the wrong stride.
const REFCOUNT_ENTRY_BYTES: u64 = 2;

/// The unit the compressed-cluster descriptor counts in.
///
/// Fixed by the specification at 512 bytes. It is **not** the cluster
/// size and **not** the backing device's block size — a compressed
/// cluster's span is measured in these regardless of either.
const COMPRESSED_SECTOR_SIZE: u64 = 512;

/// Where a cluster's refcount entry is, or why there is not one.
///
/// The absent cases are separate variants rather than a single `None`
/// because the two callers answer them differently and the error text
/// differs: "past table coverage" and "block not allocated" are
/// distinguishable corruptions.
enum RefcountEntryLocation {
    /// The entry exists, in the refcount block at `block_off`.
    At {
        block_off: u64,
        /// Byte offset of the entry within that block.
        byte_in_block: usize,
    },
    /// The refcount table does not reach this cluster.
    PastTableCoverage,
    /// The table reaches it, but the block it points at is a hole.
    BlockNotAllocated,
}

/// Where a virtual byte offset lives, in the format's own terms.
///
/// qcow2 addresses a cluster through two levels: an L1 entry selects an
/// L2 table, and an L2 entry inside it selects the host cluster. The
/// rule tying them together is that **one L1 entry covers
/// `l2_entries` clusters** — and that rule was asserted at every site
/// that needed an address, and owned by none of them.
///
/// Naming the address also names the read: `addr.l1_index` says what
/// `(virt / cluster_size) / l2_entries` means, which is otherwise three
/// divisions the reader has to interpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClusterAddress {
    /// Index of the virtual cluster, counting from the start of the disk.
    virt_cluster: u64,
    /// Which L1 entry — i.e. which L2 table — covers that cluster.
    l1_index: u32,
    /// Which entry within that L2 table.
    l2_index: u32,
    /// How far into the cluster the original byte offset falls.
    offset_in_cluster: u64,
}

/// L2 entry flags.
const L2_FLAG_COMPRESSED: u64 = 1 << 62;
/// v3 only: when bit 62 is clear, bit 0 means "this cluster reads as zeros".
const L2_FLAG_ZERO: u64 = 1 << 0;
/// COPIED: bit 63 of an L1 or L2 entry. Set means "this cluster's refcount
/// is exactly 1, so it can be written through without copy-on-write."
/// Always set when the writer allocates a fresh cluster.
const L2_FLAG_COPIED: u64 = 1 << 63;

/// Bits an L1 entry must leave clear: 0..=8 below the offset field and
/// 56..=62 above it. Bit 63 is COPIED and bits 9..=55 are the offset.
///
/// An L1 entry has no compressed flag and no zero flag — bit 62 means
/// "compressed" one level down and nothing here — so the reserved span
/// above the offset is one bit wider than an L2 entry's.
const L1_RESERVED_MASK: u64 = 0x7f00_0000_0000_01ff;

/// Bits a standard (uncompressed) L2 entry must leave clear: 1..=8 and
/// 56..=61. Bit 0 is the v3 zero flag, 9..=55 the host offset, 62 the
/// compressed flag and 63 COPIED.
///
/// ON A VERSION 2 IMAGE BIT 0 IS RESERVED TOO ("With version 2 ... this
/// is always 0"), so it is added to this mask there rather than being
/// ignored; see `lookup_cluster`.
///
/// The disk-image validator reports a violation as "found l2 entry with
/// reserved bits set" and refuses to convert the image; the bits are
/// how a producer says something this reader would not understand, so
/// following the entry anyway is guessing.
const L2_RESERVED_MASK: u64 = 0x3f00_0000_0000_01fe;

/// Maximum backing-file recursion depth. A pathological chain (or a cycle)
/// is rejected rather than blowing the stack or hanging.
const MAX_BACKING_DEPTH: u32 = 16;

/// What an L2 lookup tells us about a virtual cluster.
#[derive(Debug, Clone, Copy)]
enum ClusterMap {
    /// L1 or L2 entry is zero — defer to backing if present, else zeros.
    Unallocated,
    /// v3 zero flag — always reads zeros, never defers to backing.
    Zero,
    /// Uncompressed allocated cluster. `copied` reflects bit 63 of the L2
    /// entry — when set, the spec guarantees refcount == 1 and writes may
    /// go through in place without consulting the refcount table.
    Plain { host_off: u64, copied: bool },
    /// Compressed cluster — `host_off` is byte-granular (not cluster-aligned),
    /// `byte_len` is the on-disk span in bytes (sector-rounded).
    Compressed { host_off: u64, byte_len: usize },
}

/// What a freshly allocated cluster has to be filled with *before* the
/// caller's payload is spliced into it — that is, whatever a reader sees
/// at that virtual offset today.
///
/// This is the copy-up step, and it is the only thing that differs
/// between the allocating cases in [`Qcow2Reader::write_at`]. It has to
/// be exhaustive: repointing L2 stops the old source from ever being
/// consulted again, so anything the seed fails to reproduce is lost.
#[derive(Debug, Clone, Copy)]
enum ClusterSeed {
    /// The v3 zero flag: the cluster reads as zeros on its own account
    /// and never defers to the backing chain, so zeros are the whole
    /// truth about what it holds.
    Zeros,
    /// An unallocated cluster reads *through* to the backing chain, so
    /// the parent's cluster has to be copied up. With no parent this
    /// still yields zeros — but for a different reason than [`Zeros`],
    /// which is why the two are not one case.
    ///
    /// [`Zeros`]: ClusterSeed::Zeros
    BackingChain,
    /// A plain host cluster being cloned because it is shared
    /// (copy-on-write).
    HostCluster(u64),
    /// A compressed cluster being rewritten as an uncompressed one.
    Compressed { host_off: u64, byte_len: usize },
}

/// How [`Qcow2Reader::write_at`] lands one chunk into the cluster that
/// holds it. Exactly one case writes through; every other case is the
/// same allocate-seed-splice-publish sequence with a different
/// [`ClusterSeed`].
#[derive(Debug, Clone, Copy)]
enum WritePlan {
    /// The cluster is allocated, uncompressed and ours alone — write
    /// straight into it, no allocation and no seed.
    InPlace { host_off: u64 },
    /// The cluster cannot be written through, so a fresh one is
    /// allocated and seeded. `release` names the host cluster whose
    /// refcount is dropped once L2 points at the replacement.
    Reallocate {
        seed: ClusterSeed,
        release: Option<u64>,
    },
}

/// Public-facing status of a virtual cluster. Coarser than the
/// internal `ClusterMap` (which carries host offsets and compressed-
/// span sizes): the consumer of `extents()` only needs to know whether
/// to read, zero-fill, or defer to a backing reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterStatus {
    /// Cluster holds user data — uncompressed or compressed. Consumer
    /// should call [`Qcow2Reader::read_at`] to materialise the bytes.
    Allocated,
    /// v3 zero flag — guaranteed zeros, never defers to backing.
    /// Consumer can zero-fill its buffer without invoking the reader.
    Zero,
    /// L1 or L2 entry is zero. With no backing image this also reads
    /// as zeros; with a backing image the backing's data would be
    /// returned. Consumer that wants to honour backing chains must
    /// still call `read_at` for this kind; consumer that doesn't care
    /// (e.g. burns from a single-image source) can treat it like Zero.
    Unallocated,
}

/// A run of contiguous virtual clusters with the same status. Yielded
/// by [`Qcow2Reader::extents`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    /// Byte offset from the start of the virtual disk.
    pub virt_offset: u64,
    /// Length in bytes. Always cluster-aligned except possibly the
    /// final extent, which may be shorter when virtual_size is not a
    /// multiple of cluster_size.
    pub length: u64,
    pub status: ClusterStatus,
}

/// Iterator over [`Extent`]s for the whole virtual disk. See
/// [`Qcow2Reader::extents`] for the high-level intent.
pub struct ExtentIter<'a> {
    reader: &'a Qcow2Reader,
    cursor: u64,
}

impl<'a> ExtentIter<'a> {
    fn new(reader: &'a Qcow2Reader) -> Self {
        Self { reader, cursor: 0 }
    }
}

impl<'a> Iterator for ExtentIter<'a> {
    type Item = Result<Extent>;

    fn next(&mut self) -> Option<Self::Item> {
        let total = self.reader.virtual_size();
        if self.cursor >= total {
            return None;
        }
        let cluster_size = self.reader.cluster_size();
        let start = self.cursor;

        let status = match self.reader.cluster_status_at(start) {
            Ok(s) => s,
            Err(e) => {
                self.cursor = total;
                return Some(Err(e));
            }
        };

        let mut end = (start + cluster_size).min(total);
        while end < total {
            match self.reader.cluster_status_at(end) {
                Ok(s) if s == status => {
                    end = (end + cluster_size).min(total);
                }
                Ok(_) => break,
                Err(e) => {
                    self.cursor = total;
                    return Some(Err(e));
                }
            }
        }

        self.cursor = end;
        Some(Ok(Extent {
            virt_offset: start,
            length: end - start,
            status,
        }))
    }
}

/// QCOW2 reader backed by a generic [`fs_core::BlockDevice`]. May own a
/// recursive backing-file reader if the image references a parent.
///
/// Implements [`fs_core::BlockRead`] / [`fs_core::BlockDevice`] so the
/// reader can be handed straight to any consumer that takes those traits
/// — partition probes, filesystem drivers, slice decorators. The
/// inherent [`Qcow2Reader::read_at`] / [`Qcow2Reader::write_at`] keep the
/// rich [`Error`] type for callers that want to match on specific failure
/// modes.
pub struct Qcow2Reader {
    /// Backing block device. All host-offset reads/writes go through here.
    /// `Arc<dyn BlockDevice>` because `BlockDevice` is `Send + Sync` and
    /// the reader may live behind an `Arc` itself (FFI handles).
    dev: Arc<dyn BlockDevice>,
    header: Header,
    /// Cached L1 table. Always small (l1_size * 8 bytes). Mutex-wrapped
    /// because the writer mutates entries in place when allocating L2
    /// tables.
    l1: Mutex<Vec<u64>>,
    /// Single-slot L2 cache: (l1_index, cluster bytes).
    l2_cache: Mutex<Option<(u32, Vec<u8>)>>,
    /// Single-slot decompressed-cluster cache: (virt_cluster_index, bytes).
    /// Catches sequential access within one compressed cluster.
    decompress_cache: Mutex<Option<(u64, Vec<u8>)>>,
    /// Optional parent in the backing chain. Always opened read-only,
    /// regardless of the child's mode — you don't write through backing.
    backing: Option<Box<Qcow2Reader>>,
    /// True when the image was opened read-write. Read-only images reject
    /// every `write_at` call up front.
    writable: bool,
    /// How far the image reaches, in bytes.
    ///
    /// Seeded from `dev.size_bytes()` and raised by [`Qcow2Reader::dev_write`]
    /// whenever a write lands past it. `fs_core::FileDevice` records its
    /// length when the file is opened and never re-stats, so the device's
    /// own answer goes stale the moment the allocator hands out a cluster
    /// beyond the current tail — and every bound checked against it would
    /// then reject a cluster this reader had just written. This is the
    /// same number, kept honest.
    image_len: Mutex<u64>,
    /// The refcount table, read on the first allocation and kept (#44).
    ///
    /// Every allocation used to read it whole. Only this reader writes it
    /// -- pass 2 of [`Qcow2Reader::allocate_cluster`] publishing a new
    /// block -- and that write updates the copy here too.
    refcount_table: Mutex<Option<Vec<u8>>>,
    /// The host cluster index the allocator's scan resumes from (#44).
    ///
    /// Every present refcount entry below it was non-zero when last
    /// looked at. The scan used to restart at block 0, entry 0, so each
    /// allocation walked every full block before it; it now starts here,
    /// and a refcount dropped to zero moves the cursor back down.
    alloc_cursor: Mutex<u64>,
    /// Held for the whole of every `write_at`.
    ///
    /// Allocation, copy-on-write and the L2/L1 updates are read-modify-
    /// write sequences over whole metadata clusters: read a refcount block
    /// or an L2 table, change a few bytes, write the cluster back. The
    /// type is `Sync` and the C ABI hands one handle to any thread, so two
    /// writers interleaving those sequences each wrote back a cluster read
    /// before the other's change landed. Measured (#39): 48 writes to
    /// distinct clusters from two threads left `qemu-img check` reporting
    /// corruption and leaked clusters, where one thread left it clean.
    /// Serialising writers makes each sequence atomic against the others.
    write_lock: Mutex<()>,
}

/// The length of a table, once it is known to be inside the image.
///
/// The L1 table and the refcount table are each read whole into a
/// buffer sized from header fields. `l1_size` is a `u32` with only a
/// lower bound (it must cover the virtual size); `refcount_table_clusters`
/// is a `u32` multiplied by the cluster size, which at the largest
/// cluster this reader accepts is nine petabytes. That allocation does
/// not fail politely: `handle_alloc_error` aborts, and an abort is not
/// something the FFI boundary's `catch_unwind` can turn into an error
/// code -- the host process dies.
///
/// A table that does not fit inside the image is not a table.
///
/// `outside` is the message for one that does not fit. It is static
/// because `Error::Corrupt` carries a `&'static str`, and formatting
/// one per failure would mean leaking a string for every hostile
/// image.
fn span_inside_the_image(
    dev_size: u64,
    offset: u64,
    length: u64,
    outside: &'static str,
) -> Result<usize> {
    let end = offset.checked_add(length).ok_or(Error::Corrupt(outside))?;
    if end > dev_size {
        return Err(Error::Corrupt(outside));
    }
    // Checked, not `as`: on a 32-bit target `as` keeps the low bits, and a
    // truncated buffer beside a walk bounded by the full length indexes
    // past its end (#62).
    usize::try_from(length).map_err(|_| Error::Corrupt(outside))
}

impl Qcow2Reader {
    /// Open `path` read-only and parse the header + L1 table. If the image
    /// references a backing file, the parent is opened recursively (capped at
    /// `MAX_BACKING_DEPTH`).
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let p = path.as_ref();
        let dev = FileDevice::open(p).map_err(fs_core_to_qcow2_error)?;
        Self::open_inner(
            Arc::new(dev),
            false,
            MAX_BACKING_DEPTH,
            Some(p.to_path_buf()),
        )
    }

    /// Open `path` read-write. Backing parents (if any) are still opened
    /// read-only — writes only ever land in the leaf image.
    ///
    /// Writes allocate clusters, copy shared clusters and shared L2
    /// tables before changing them, replace compressed clusters with
    /// uncompressed ones, and copy up from the backing chain. See
    /// [`Qcow2Reader::write_at`] for what is still refused.
    pub fn open_rw<P: AsRef<Path>>(path: P) -> Result<Self> {
        let p = path.as_ref();
        let dev = FileDevice::open_rw(p).map_err(fs_core_to_qcow2_error)?;
        Self::open_inner(
            Arc::new(dev),
            true,
            MAX_BACKING_DEPTH,
            Some(p.to_path_buf()),
        )
    }

    /// Open read-write if possible, fall back to read-only otherwise.
    /// Useful for inspectors that prefer RW but tolerate locked paths.
    pub fn open_best_effort<P: AsRef<Path>>(path: P) -> Result<Self> {
        let p = path.as_ref();
        match Self::open_rw(p) {
            Ok(r) => Ok(r),
            Err(_) => Self::open(p),
        }
    }

    /// Open read-only on top of an arbitrary `BlockDevice`. The on-device
    /// path has no filesystem context, so an image that references a
    /// backing file is rejected with `Error::Unsupported` — backing-chain
    /// resolution requires the path-based entry points.
    pub fn open_on_device(dev: Arc<dyn BlockDevice>) -> Result<Self> {
        Self::open_inner(dev, false, MAX_BACKING_DEPTH, None)
    }

    /// Open read-write on top of an arbitrary `BlockDevice`. The device
    /// must report `is_writable()`; otherwise the call returns
    /// `Error::ReadOnly`.
    pub fn open_rw_on_device(dev: Arc<dyn BlockDevice>) -> Result<Self> {
        if !dev.is_writable() {
            return Err(Error::ReadOnly);
        }
        Self::open_inner(dev, true, MAX_BACKING_DEPTH, None)
    }

    fn open_inner(
        dev: Arc<dyn BlockDevice>,
        writable: bool,
        depth_remaining: u32,
        parent_path: Option<PathBuf>,
    ) -> Result<Self> {
        if depth_remaining == 0 {
            return Err(Error::BackingTooDeep);
        }

        // Header: v2 stops at 72, v3 fixed fields at 104, v3 additional
        // fields (compression_type at 104, then padding) extend further.
        // Read 112 bytes so we always capture the compression_type byte
        // for any v3 image that declared it.
        let mut head_bytes = [0u8; 112];
        match dev.read_at(0, &mut head_bytes) {
            Ok(()) => {}
            Err(fs_core::Error::ShortRead { got, .. }) if got >= 72 => {
                // Tolerate device shorter than 112 bytes as long as we got
                // the v2 minimum — Header::parse will reject if needed.
                head_bytes[got..].fill(0);
            }
            Err(e) => return Err(fs_core_to_qcow2_error(e)),
        }

        let header = Header::parse(&head_bytes[..])?;
        header.check_supported()?;
        // The advisory bits are advisory to a reader and not to a
        // writer, so the gate is asked which one this is. `open_inner`
        // is the only place that knows.
        if writable {
            header.check_writable()?;
        }

        let mut l1_bytes = vec![
            0u8;
            span_inside_the_image(
                dev.size_bytes(),
                header.l1_table_offset,
                u64::from(header.l1_size) * 8,
                "L1 table reaches past the end of the image",
            )?
        ];
        dev.read_at(header.l1_table_offset, &mut l1_bytes)
            .map_err(fs_core_to_qcow2_error)?;
        let mut l1 = Vec::with_capacity(header.l1_size as usize);
        for chunk in l1_bytes.chunks_exact(8) {
            l1.push(u64::from_be_bytes(chunk.try_into().unwrap()));
        }

        // A CHAIN NEEDS BOTH FIELDS, and this keyed on the size alone.
        //
        // The reference tool wants an offset to know where the name is
        // and a length to know how much of it there is, and treats
        // either being zero as "no backing file". Both halves were
        // measured, and we disagreed with it in both directions:
        //
        //   offset = 0, size = 12
        //     qemu-img info: opens the image, reports no backing file
        //     ours:          read 12 bytes from offset 0 — the header's
        //                    own magic — and refused with BadBackingPath
        //
        //   offset = 0x50, size = 0
        //     qemu-img info: opens the image, reports no backing file
        //     ours:          refused, once the offset gate was added on
        //                    its own
        //
        // The second is why this tests both rather than only the offset:
        // an offset with no length is not a path, and refusing it would
        // reject an image the reference opens — which is the same
        // mistake, made while fixing the first.
        let backing = if header.backing_file_offset != 0 && header.backing_file_size != 0 {
            // Backing-chain resolution needs a real path so the parent can
            // be opened by name. Reject when caller went through the
            // on-device entry point.
            let child_path = match &parent_path {
                Some(p) => p.clone(),
                None => {
                    return Err(Error::Unsupported(
                        "image references a backing file; open by path to resolve the chain",
                    ));
                }
            };
            let backing_path = read_backing_path(&dev, &header, &child_path)?;
            // Backing parents always open read-only — writes only land in
            // the leaf image's own cluster store.
            let parent_dev = FileDevice::open(&backing_path).map_err(fs_core_to_qcow2_error)?;
            Some(Box::new(Self::open_inner(
                Arc::new(parent_dev),
                false,
                depth_remaining - 1,
                Some(backing_path),
            )?))
        } else {
            None
        };

        let image_len = dev.size_bytes();
        Ok(Self {
            dev,
            header,
            l1: Mutex::new(l1),
            l2_cache: Mutex::new(None),
            decompress_cache: Mutex::new(None),
            backing,
            writable,
            image_len: Mutex::new(image_len),
            refcount_table: Mutex::new(None),
            alloc_cursor: Mutex::new(0),
            write_lock: Mutex::new(()),
        })
    }

    /// Virtual disk size in bytes — the addressable range for `read_at`.
    pub fn virtual_size(&self) -> u64 {
        self.header.virtual_size
    }

    /// Cluster size (1 << cluster_bits).
    pub fn cluster_size(&self) -> u64 {
        self.header.cluster_size
    }

    /// QCOW2 version (2 or 3).
    pub fn version(&self) -> u32 {
        self.header.version
    }

    /// Parsed header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Whether this image has a backing parent.
    pub fn has_backing(&self) -> bool {
        self.backing.is_some()
    }

    /// Whether the image was opened read-write.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Public-facing status of the cluster containing byte `virt`.
    /// Walks L1/L2 only — does not read cluster data. Use this to
    /// decide whether a consumer needs to invoke [`Qcow2Reader::read_at`] (for
    /// [`ClusterStatus::Allocated`]) or can skip-fill with zeros
    /// (for [`ClusterStatus::Zero`] / [`ClusterStatus::Unallocated`]
    /// when no backing is present).
    ///
    /// Returns [`Error::OutOfBounds`] if `virt >= virtual_size()`.
    pub fn cluster_status_at(&self, virt: u64) -> Result<ClusterStatus> {
        if virt >= self.header.virtual_size {
            return Err(Error::OutOfBounds {
                offset: virt,
                len: 1,
                size: self.header.virtual_size,
            });
        }
        match self.lookup_cluster(virt)? {
            ClusterMap::Plain { .. } | ClusterMap::Compressed { .. } => {
                Ok(ClusterStatus::Allocated)
            }
            ClusterMap::Zero => Ok(ClusterStatus::Zero),
            ClusterMap::Unallocated => Ok(ClusterStatus::Unallocated),
        }
    }

    /// Iterate the virtual disk as a sequence of contiguous extents,
    /// merging adjacent clusters of the same status. The iterator
    /// stops at [`Qcow2Reader::virtual_size`]. Each yielded extent is
    /// cluster-aligned (except possibly the last, which is clamped
    /// to virtual_size).
    ///
    /// This is the cheap way to drive a sparse-aware copy, and the
    /// saving is on the CONSUMER's side: every [`ClusterStatus::Zero`] /
    /// [`ClusterStatus::Unallocated`] extent can be written as zeros
    /// directly on the destination without reading any data through the
    /// qcow2 read path. For a 100 GiB image with 12 GiB allocated, a
    /// consumer handles ~13 extents rather than ~1.6M clusters.
    ///
    /// THE WALK ITSELF IS NOT CHEAPER. The iterator looks up the status
    /// of every cluster of the virtual disk, and looks up the first
    /// cluster of each extent after the first twice (once to end the
    /// previous extent, once to start its own): clusters + extents - 1
    /// lookups, ~1.6M for that 100 GiB image at a 64 KiB cluster. Each
    /// takes the L1 lock and, where the L1 entry is present, the
    /// L2-cache lock, and each L2 table reached is read. No data cluster
    /// is read. Budget a full walk as time proportional to the virtual
    /// size, not to the number of extents.
    pub fn extents(&self) -> ExtentIter<'_> {
        ExtentIter::new(self)
    }

    // -- internal device adapters ------------------------------------------

    fn dev_read(&self, off: u64, buf: &mut [u8]) -> Result<()> {
        self.dev.read_at(off, buf).map_err(fs_core_to_qcow2_error)
    }

    /// Make room for a cluster at `host_off` before anything writes into it.
    ///
    /// `write_at` used to extend a `FileDevice` implicitly, and that is how
    /// this format allocated: write the new cluster past the old end, then
    /// record where it went. `rust-fs-core` withdrew that in its #75, and
    /// correctly — the file grew while `size_bytes()` went on reporting the
    /// length taken at open, so a caching device could hold bytes no bounded
    /// read could reach (rust-fs-core#70). `set_len` is the replacement:
    /// growth that says so, and moves the reported length with it.
    ///
    /// # IT ONLY EVER GROWS
    ///
    /// `BlockDevice::set_len` *sets*: a smaller `new_len` truncates, exactly
    /// as `std::fs::File::set_len` does. Nothing here wants that — an
    /// allocation appends — so the current length is read first and a device
    /// already long enough is left alone. That also keeps a writable but
    /// fixed-length device (a block-device node, a pre-sized image) working
    /// for every allocation that fits inside it, instead of failing on a
    /// `set_len` it never needed.
    ///
    /// # WHY NOT IN `dev_write`
    ///
    /// Because `dev_write` is also the in-place and metadata path. Growing
    /// there would extend the file on any stray offset, which dissolves the
    /// bounds check rust-fs-core#70 exists to provide. Only the allocator
    /// knows a cluster is *supposed* to be new.
    fn dev_grow_for_cluster(&self, host_off: u64) -> Result<()> {
        let end = host_off.saturating_add(self.header.cluster_size);
        if self.dev.size_bytes() >= end {
            return Ok(());
        }
        self.dev.set_len(end).map_err(fs_core_to_qcow2_error)
    }

    fn dev_write(&self, off: u64, buf: &[u8]) -> Result<()> {
        self.dev
            .write_at(off, buf)
            .map_err(fs_core_to_qcow2_error)?;
        // The image may be longer than the device reported at open, because
        // the allocator grew it with `dev_grow_for_cluster`. Recording the
        // end here keeps one place that knows how far the image reaches,
        // whichever path wrote last.
        //
        // This used to say a write past the tail extends the file. It does
        // not any more — `write_at` refuses that (rust-fs-core#75) — and the
        // growth is explicit in the allocator now.
        let end = off.saturating_add(buf.len() as u64);
        let mut len = self.image_len.lock().unwrap();
        if end > *len {
            *len = end;
        }
        Ok(())
    }

    fn dev_flush(&self) -> Result<()> {
        self.dev.flush().map_err(fs_core_to_qcow2_error)
    }

    /// Write to the image. Every cluster state is handled by the same
    /// two-case shape, decided by `Qcow2Reader::plan_write`:
    ///
    /// - **Allocated, uncompressed, single-ref**: direct write to the
    ///   existing host cluster. Nothing is allocated.
    /// - **Everything else** — shared, compressed, zero-flagged or
    ///   unallocated: allocate a fresh host cluster, *seed* it with the
    ///   bytes a reader sees at this offset today, splice the caller's
    ///   payload in, repoint L2 (with COPIED set), then drop the old
    ///   cluster's refcount if it had one. If the L1 entry was
    ///   unallocated, an L2 table is allocated first.
    ///
    /// The seed is the only step that differs between those cases; see
    /// `ClusterSeed` for what each state has to be seeded with and why
    /// zero-flagged and unallocated clusters are not the same case.
    ///
    /// Crash-safety ordering: refcount → data → metadata, with
    /// `dev.flush()` between each step. A crash mid-allocation may leak a
    /// cluster but never corrupts the image.
    ///
    /// Internal snapshots are not refused: a cluster or L2 table they
    /// share is copied before it is written (see `plan_write` and
    /// `update_l2_entry`).
    ///
    /// Refused with [`Error::Unsupported`] when the write has to allocate
    /// a cluster or read a refcount (a cluster whose L2 entry lacks
    /// COPIED); a plain in-place write reaches none of these:
    ///
    /// - Image with `refcount_order != 4` (only u16 refcounts handled).
    /// - Image with no refcount table.
    /// - Every refcount block full AND no empty refcount-table slot to
    ///   grow a new block into (the refcount table itself is never grown).
    ///
    /// Refused with [`Error::ReadOnly`]: image opened via `open()`.
    /// Refused with [`Error::OutOfBounds`]: range past virtual size.
    pub fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(Error::ReadOnly);
        }
        let len = buf.len() as u64;
        if len == 0 {
            return Ok(());
        }
        let end = offset
            .checked_add(len)
            .ok_or(Error::Corrupt("offset+len overflow"))?;
        if end > self.header.virtual_size {
            return Err(Error::OutOfBounds {
                offset,
                len,
                size: self.header.virtual_size,
            });
        }

        // One writer at a time: see `write_lock`.
        let _writer = self.write_lock.lock().unwrap();

        // Snapshot safety is handled per-cluster below: a shared cluster
        // (host refcount > 1) is CoW'd before the user payload lands. The
        // image-wide `nb_snapshots > 0` check that Phase A used is gone —
        // shared clusters can exist for reasons other than internal
        // snapshots (e.g. external tooling), and the per-cluster check is
        // what actually keeps snapshots intact.

        let cluster_size = self.header.cluster_size;

        let mut cursor = offset;
        let mut written = 0usize;

        while cursor < end {
            let in_cluster = self.split_virtual(cursor).offset_in_cluster as usize;
            let chunk = std::cmp::min(cluster_size - in_cluster as u64, end - cursor) as usize;
            let src = &buf[written..written + chunk];

            match self.plan_write(self.lookup_cluster(cursor)?)? {
                WritePlan::InPlace { host_off } => {
                    self.dev_write(host_off + in_cluster as u64, src)?;
                }
                WritePlan::Reallocate { seed, release } => {
                    // Seed the new cluster with what a reader sees at
                    // `cursor` today, splice the caller's bytes into it,
                    // publish it, then drop the old cluster's share.
                    //
                    // Crash-safety order:
                    //   data → refcount(new=1) → L2 → refcount(old-1).
                    // allocate_cluster already wrote refcount=1 for the
                    // new cluster; the data write follows; only once L2
                    // is repointed do we drop the old refcount. Failing
                    // that last step leaks one refcount but never
                    // corrupts data — fsck-recoverable.
                    let mut full = self.seed_cluster(cursor, seed)?;
                    full[in_cluster..in_cluster + chunk].copy_from_slice(src);

                    let new_host = self.allocate_cluster()?;
                    self.dev_write(new_host, &full)?;
                    self.dev_flush()?;
                    self.repoint_l2(cursor, new_host)?;

                    if let Some(old_host) = release {
                        let _ = self.decrement_refcount(old_host);
                    }
                }
            }

            cursor += chunk as u64;
            written += chunk;
        }
        Ok(())
    }

    /// Decide how a chunk lands in a cluster whose L2 lookup gave `map`.
    ///
    /// Every case but the first allocates, and they differ in exactly one
    /// thing — the seed. Making that decision here, once, rather than
    /// inline in a branch per cluster state, is what keeps it visible
    /// that [`ClusterMap::Zero`] and [`ClusterMap::Unallocated`] do *not*
    /// seed the same way.
    fn plan_write(&self, map: ClusterMap) -> Result<WritePlan> {
        Ok(match map {
            ClusterMap::Plain { host_off, copied } => {
                // COPIED (L2 bit 63) is the spec's promise that
                // refcount == 1, so we can write in place without
                // touching the refcount table. When it's clear we have to
                // consult the actual refcount: a value > 1 means the
                // cluster is shared (with an internal snapshot, with
                // another L2 entry, etc.) and writing through would
                // mutate the sharer's view.
                let shared = !copied && self.read_refcount(host_off)? > 1;
                if shared {
                    WritePlan::Reallocate {
                        seed: ClusterSeed::HostCluster(host_off),
                        release: Some(host_off),
                    }
                } else {
                    WritePlan::InPlace { host_off }
                }
            }
            // The replacement is uncompressed, so the old compressed
            // cluster loses its only L2 reference and can be released.
            //
            // WHAT THIS RELEASE DOES AND DOES NOT COVER, stated because
            // the offset being bounded now makes it easy to assume the
            // rest is settled. `decrement_refcount` treats the value as
            // a cluster identity (`host_off / cluster_size`), and
            // `lookup_cluster` now guarantees the whole payload lies
            // inside the image -- so the decrement can no longer land on
            // a cluster outside it. It still names only the cluster
            // holding the payload's FIRST byte, and a compressed payload
            // is byte-granular and up to about 4 MiB long, so it can end
            // in a later cluster whose reference is not dropped here.
            //
            // NOT ESTABLISHED, and deliberately not guessed at: whether
            // the format's accounting gives every spanned host cluster a
            // reference at all. This crate never writes compressed
            // clusters, so those refcounts came from an external
            // producer, and the question is about what that producer
            // wrote rather than about this code.
            // `tests/qemu_validation.rs` is where it can be settled --
            // build an image whose compressed payload straddles a
            // cluster boundary, overwrite that virtual cluster, and ask
            // the external validator whether the leftover clusters are
            // still accounted for.
            ClusterMap::Compressed { host_off, byte_len } => WritePlan::Reallocate {
                seed: ClusterSeed::Compressed { host_off, byte_len },
                release: Some(host_off),
            },
            ClusterMap::Zero => WritePlan::Reallocate {
                seed: ClusterSeed::Zeros,
                release: None,
            },
            ClusterMap::Unallocated => WritePlan::Reallocate {
                seed: ClusterSeed::BackingChain,
                release: None,
            },
        })
    }

    /// Materialise a whole cluster holding exactly what a reader sees at
    /// virtual offset `virt` right now, ready for the caller's payload to
    /// be spliced into it. See [`ClusterSeed`] for why each state seeds
    /// the way it does.
    fn seed_cluster(&self, virt: u64, seed: ClusterSeed) -> Result<Vec<u8>> {
        let cluster_size = self.header.cluster_size;
        let mut full = vec![0u8; cluster_size as usize];
        match seed {
            ClusterSeed::Zeros => {}
            ClusterSeed::HostCluster(host_off) => self.dev_read(host_off, &mut full)?,
            ClusterSeed::Compressed { host_off, byte_len } => {
                self.read_decompressed_slice(virt / cluster_size, host_off, byte_len, 0, &mut full)?
            }
            // Cluster-aligned on purpose: we are reproducing the entire
            // cluster the backing chain would have served, not just the
            // slice the caller happens to be overwriting.
            ClusterSeed::BackingChain => {
                self.read_unallocated(virt & !(cluster_size - 1), &mut full)?
            }
        }
        Ok(full)
    }

    /// How many refcount entries one refcount block holds — and the
    /// single place that refuses a refcount width this crate cannot
    /// walk.
    ///
    /// Everything that touches a refcount block treats it as an array
    /// of big-endian `u16`. That is right only when
    /// `refcount_order == 4`; at any other order the entries are a
    /// different width, and walking them at this stride would read two
    /// neighbouring entries as one — handing out a cluster that is
    /// actually in use, and overwriting live data with it. So the guard
    /// is not a politeness, and it is not optional at any call site.
    ///
    /// v2 images have no `refcount_order` field at all; the format
    /// fixes them at 16 bits, which is why the version check comes
    /// first rather than reading a field that is not there.
    ///
    /// It was written out twice before, thirty lines apart, and neither
    /// copy was covered — deleting either failed no tests. See
    /// `a_non_u16_refcount_width_is_refused_rather_than_mis_walked`.
    fn refcount_entries_per_block(&self) -> Result<u64> {
        let refcount_bits = if self.header.version >= 3 {
            1u32 << self.header.refcount_order
        } else {
            16
        };
        if refcount_bits != REFCOUNT_ENTRY_BYTES as u32 * 8 {
            return Err(Error::Unsupported(
                "non-16-bit refcount entries (refcount_order != 4)",
            ));
        }
        Ok(self.header.cluster_size / REFCOUNT_ENTRY_BYTES)
    }

    /// Split a virtual byte offset into the address the format uses.
    ///
    /// The one place the two-level addressing rule is written down. It
    /// was previously re-derived at each site that needed it, in two
    /// different spellings (`l1_idx` and `l1_index`), which made the
    /// write path's three branches read as three unrelated paragraphs
    /// rather than one operation with one differing step.
    fn split_virtual(&self, virt: u64) -> ClusterAddress {
        let cluster_size = self.header.cluster_size;
        let l2_entries = self.header.l2_entries();
        let virt_cluster = virt / cluster_size;
        ClusterAddress {
            virt_cluster,
            l1_index: (virt_cluster / l2_entries) as u32,
            l2_index: (virt_cluster % l2_entries) as u32,
            offset_in_cluster: virt % cluster_size,
        }
    }

    /// Point the L2 entry covering virtual offset `virt` at a host
    /// cluster we have just allocated. Such a cluster has refcount 1 by
    /// construction, which is exactly what COPIED asserts.
    fn repoint_l2(&self, virt: u64, new_host: u64) -> Result<()> {
        let addr = self.split_virtual(virt);
        self.update_l2_entry(addr.l1_index, addr.l2_index, l2_entry_for(new_host))
    }

    /// Find a host cluster with refcount = 0, increment it to 1, and
    /// return the cluster's host byte offset.
    ///
    /// **Two** passes, then a refusal — the doc here previously
    /// described three paths, and the third was a garbled account of
    /// what is really the failure case.
    ///
    /// 1. Walk every refcount block the table already points at, and
    ///    claim the first entry whose count is zero. Note the first
    ///    empty table *slot* on the way past, in case pass 1 finds
    ///    nothing.
    /// 2. Every present block was full. Take that first empty slot and
    ///    allocate a fresh refcount block for it, placed at the host
    ///    cluster the slot's own range begins at. The new block can then
    ///    record itself (entry 0) and the caller's cluster (entry 1),
    ///    which is why it needs no allocation of its own.
    ///
    /// If pass 1 found nothing free **and** there was no empty slot
    /// either, the refcount table itself is full. Growing it is
    /// deliberately out of scope, so this surfaces as `Unsupported`
    /// rather than being worked around. In practice a table sized for
    /// its device always has spare slots; an image that lands here has
    /// a table too small for the disk it describes.
    ///
    /// Crash-safety order for pass 2:
    ///   data (zeroed block) → block-with-self-ref=1 → refcount-table-entry
    /// with `dev_flush` between each step. A crash mid-sequence may leak
    /// one host cluster but never corrupts the image.
    ///
    /// Errors with `Unsupported` when the image has no refcount table or
    /// uses a non-u16 refcount width — see
    /// [`Qcow2Reader::refcount_entries_per_block`] for why the second
    /// one is not negotiable.
    fn allocate_cluster(&self) -> Result<u64> {
        let cluster_size = self.header.cluster_size;
        let entries_per_block = self.refcount_entries_per_block()?;

        let rt_off = self.header.refcount_table_offset;
        let rt_clusters = self.header.refcount_table_clusters as u64;
        if rt_off == 0 || rt_clusters == 0 {
            return Err(Error::Unsupported(
                "no refcount table (cannot allocate clusters)",
            ));
        }
        let rt_size = rt_clusters
            .checked_mul(cluster_size)
            .ok_or(Error::Corrupt("refcount table size overflows"))?;
        let mut rt_guard = self.refcount_table.lock().expect("refcount table lock");
        if rt_guard.is_none() {
            let mut rt_bytes = vec![
                0u8;
                span_inside_the_image(
                    self.dev.size_bytes(),
                    rt_off,
                    rt_size,
                    "refcount table reaches past the end of the image",
                )?
            ];
            self.dev_read(rt_off, &mut rt_bytes)?;
            *rt_guard = Some(rt_bytes);
        }
        let rt_bytes = rt_guard.as_mut().expect("loaded above");
        let mut cursor = self.alloc_cursor.lock().expect("alloc cursor lock");

        let rt_entries_total = rt_bytes.len() / TABLE_ENTRY_BYTES as usize;
        let start_block = usize::try_from(*cursor / entries_per_block).unwrap_or(usize::MAX);
        let start_entry = (*cursor % entries_per_block) as usize;

        // The first absent slot, for pass 2, over the whole table: it is
        // resident, so this costs no device reads, and a slot below the
        // cursor may still be absent.
        let first_empty_block_slot = (0..rt_entries_total).find(|&i| {
            let at = i * TABLE_ENTRY_BYTES as usize;
            rt_bytes[at..at + TABLE_ENTRY_BYTES as usize] == [0u8; 8]
        });

        // Pass 1: walk the table from the cursor. For each present block
        // try to claim a free slot.
        for block_idx in start_block..rt_entries_total {
            let entry_off = block_idx * TABLE_ENTRY_BYTES as usize;
            let block_off = u64::from_be_bytes(
                rt_bytes[entry_off..entry_off + TABLE_ENTRY_BYTES as usize]
                    .try_into()
                    .unwrap(),
            );
            if block_off == 0 {
                continue;
            }

            let mut block_bytes = vec![0u8; cluster_size as usize];
            self.dev_read(block_off, &mut block_bytes)?;

            let first_entry = if block_idx == start_block {
                start_entry
            } else {
                0
            };
            for entry_idx in first_entry..entries_per_block as usize {
                let off = entry_idx * 2;
                let refcount = u16::from_be_bytes([block_bytes[off], block_bytes[off + 1]]);
                if refcount == 0 {
                    let host_cluster_idx =
                        (block_idx as u64) * entries_per_block + (entry_idx as u64);

                    // Host cluster 0 holds the header. A well-formed
                    // image always marks it in use, so the scan never
                    // reaches here — but this crate reads images it does
                    // not trust, and a malformed one that leaves its
                    // refcount at zero would be handed cluster 0, which
                    // the caller immediately zero-fills. That wipes the
                    // header, and the L2 entry written afterwards is
                    // `0 | COPIED`, which `lookup_cluster` reads straight
                    // back as Unallocated.
                    if host_cluster_idx == 0 {
                        return Err(Error::Corrupt(
                            "refcount table marks host cluster 0 (the header) as free",
                        ));
                    }

                    let host_off = host_cluster_idx * cluster_size;

                    // ROOM FIRST. This cluster is free in the refcount block
                    // but may still be past the device's end — a refcount
                    // block governs a whole range, and the image only reaches
                    // as far as it has been grown.
                    self.dev_grow_for_cluster(host_off)?;

                    // Only the two bytes that changed go back to the
                    // device, not the whole block (#44).
                    self.dev_write(block_off + off as u64, &1u16.to_be_bytes())?;
                    self.dev_flush()?;
                    *cursor = host_cluster_idx + 1;
                    return Ok(host_off);
                }
            }
        }

        // Pass 2: every present block is full. Grow into the first absent
        // refcount-table slot by allocating a fresh refcount block.
        let block_idx = first_empty_block_slot.ok_or(Error::Unsupported(
            "refcount table is full and every block is full (refcount-table grow not implemented)",
        ))?;

        // Place the new refcount block at the first host cluster of the
        // range this slot governs: host_cluster_idx =
        // block_idx * entries_per_block. That lets the new block point at
        // *itself* with refcount = 1 and claim a fresh host cluster from
        // its own range for the caller.
        //
        // This is NOT the device's tail. `block_idx` is the FIRST absent
        // slot pass 1 saw scanning up from 0, so with a hole in the table
        // (slot 0 full, slot 1 absent, slot 2 present) the block lands at
        // the start of slot 1's range, in the middle of the image. The two
        // coincide only for a table populated densely from slot 0.
        //
        // ASSUMPTION, not checked: an absent slot means no cluster in its
        // range is in use. True of a well-formed image. On a malformed one
        // with live data at these offsets, the new block written below
        // overwrites `new_block_off`, and the caller later overwrites
        // `caller_off` -- unlike pass 1, which refuses the one malformed
        // case it can see (a free header cluster).
        //
        // Concretely: the new refcount block lives at host cluster idx
        // `new_block_cluster`, which is the first entry inside the slot it
        // governs. That cluster's self-entry within the new block is at
        // entry_idx 0; mark it refcount=1. Then pick entry_idx 1 for the
        // caller's data cluster, mark refcount=1, and return its offset.
        let new_block_cluster = (block_idx as u64) * entries_per_block;
        let new_block_off = new_block_cluster * cluster_size;
        let caller_cluster_idx = new_block_cluster + 1;
        let caller_off = caller_cluster_idx * cluster_size;

        // Step 1: zero-init the new refcount block on disk, then mark
        // its first two entries (self + caller) as refcount=1.
        // ROOM FOR BOTH, IN ONE CALL. The new refcount block lands at
        // `new_block_off` and the cluster handed back at
        // `caller_off = new_block_off + cluster_size`, so growing for the
        // caller's cluster covers the block in front of it too.
        self.dev_grow_for_cluster(caller_off)?;

        let mut new_block_bytes = vec![0u8; cluster_size as usize];
        new_block_bytes[0..2].copy_from_slice(&1u16.to_be_bytes());
        new_block_bytes[2..4].copy_from_slice(&1u16.to_be_bytes());
        self.dev_write(new_block_off, &new_block_bytes)?;
        self.dev_flush()?;

        // Step 2: publish the new block's address into the refcount-table
        // entry. After this flush the block is reachable; if we crash
        // before it the only loss is two host clusters at a known offset.
        let entry_off_in_rt = (block_idx as u64) * TABLE_ENTRY_BYTES;
        self.dev_write(rt_off + entry_off_in_rt, &new_block_off.to_be_bytes())?;
        // The resident copy follows the device before the flush can fail,
        // so a failed flush cannot leave it offering this slot again.
        let at = entry_off_in_rt as usize;
        rt_bytes[at..at + TABLE_ENTRY_BYTES as usize].copy_from_slice(&new_block_off.to_be_bytes());
        self.dev_flush()?;
        // The rest of the new block is free; blocks before it were full
        // when the scan passed them.
        *cursor = caller_cluster_idx + 1;

        Ok(caller_off)
    }

    /// Decrement the refcount of the host cluster containing `host_off`.
    /// Mirror image of [`Qcow2Reader::allocate_cluster`]: locates the
    /// matching refcount block, drops the entry by 1, syncs.
    ///
    /// Used by the compressed-cluster rewrite path to free the old
    /// compressed cluster after a fresh uncompressed replacement is in
    /// place. Multi-host-cluster compressed data: this only decrements
    /// the starting cluster — additional spanned clusters stay marked
    /// in-use, becoming a "clean leak" recoverable by `qemu-img check`.
    /// Real compressed payloads almost always fit in one host cluster,
    /// so the leak window is rare in practice.
    /// Where the refcount entry for the cluster containing `host_off`
    /// lives, or why it does not.
    ///
    /// The two callers disagree about what absence means, and that
    /// disagreement is correct rather than accidental — which is why
    /// this returns it rather than deciding:
    ///
    /// - [`Qcow2Reader::read_refcount`] treats an uncovered or
    ///   unallocated range as **zero references**. The cluster is not
    ///   live, so there is no share to copy away from.
    /// - [`Qcow2Reader::decrement_refcount`] treats it as **corruption**.
    ///   A caller releasing a cluster the table does not cover is
    ///   releasing something that was never recorded as taken.
    ///
    /// Sharing the arithmetic while keeping that split is the point.
    /// The two functions were thirty near-identical lines apart, and a
    /// reader had to diff them to discover that the divergence was
    /// deliberate.
    fn locate_refcount_entry(&self, host_off: u64) -> Result<RefcountEntryLocation> {
        let cluster_size = self.header.cluster_size;
        let entries_per_block = self.refcount_entries_per_block()?;

        let host_cluster_idx = host_off / cluster_size;
        let block_idx = host_cluster_idx / entries_per_block;
        let entry_idx = host_cluster_idx % entries_per_block;

        let rt_off = self.header.refcount_table_offset;
        let rt_clusters = self.header.refcount_table_clusters as u64;
        if rt_off == 0 || rt_clusters == 0 {
            return Err(Error::Unsupported("no refcount table"));
        }
        if block_idx * TABLE_ENTRY_BYTES >= rt_clusters * cluster_size {
            return Ok(RefcountEntryLocation::PastTableCoverage);
        }

        let mut rt_entry_bytes = [0u8; 8];
        self.dev_read(rt_off + block_idx * TABLE_ENTRY_BYTES, &mut rt_entry_bytes)?;
        let block_off = u64::from_be_bytes(rt_entry_bytes);
        if block_off == 0 {
            return Ok(RefcountEntryLocation::BlockNotAllocated);
        }

        Ok(RefcountEntryLocation::At {
            block_off,
            byte_in_block: (entry_idx as usize) * 2,
        })
    }

    fn decrement_refcount(&self, host_off: u64) -> Result<()> {
        let (block_off, off) = match self.locate_refcount_entry(host_off)? {
            RefcountEntryLocation::At {
                block_off,
                byte_in_block,
            } => (block_off, byte_in_block),
            // Releasing a cluster the table does not cover means the
            // caller believes it took something that was never recorded
            // as taken.
            RefcountEntryLocation::PastTableCoverage => {
                return Err(Error::Corrupt(
                    "decrement: host cluster past refcount table coverage",
                ))
            }
            RefcountEntryLocation::BlockNotAllocated => {
                return Err(Error::Corrupt(
                    "decrement: refcount block not allocated for this range",
                ))
            }
        };

        // The two bytes of this entry, not the whole block (#44).
        let mut entry = [0u8; 2];
        self.dev_read(block_off + off as u64, &mut entry)?;

        let cur = u16::from_be_bytes(entry);
        if cur == 0 {
            return Err(Error::Corrupt(
                "decrement: refcount already zero (double free?)",
            ));
        }
        let new_refcount = cur - 1;

        // Only the entry that changed, not the whole block (#44).
        self.dev_write(block_off + off as u64, &new_refcount.to_be_bytes())?;
        self.dev_flush()?;
        if new_refcount == 0 {
            // Free again: the allocator's scan must be able to reach it.
            let freed = host_off / self.header.cluster_size;
            let mut cursor = self.alloc_cursor.lock().expect("alloc cursor lock");
            *cursor = (*cursor).min(freed);
        }
        Ok(())
    }

    /// Read the refcount of the host cluster containing `host_off`.
    /// Mirror of [`Qcow2Reader::decrement_refcount`] but read-only — used
    /// to decide whether a write needs copy-on-write before touching a
    /// shared cluster.
    ///
    /// Returns 0 when the refcount block isn't allocated for this range
    /// (treats absence as "no share to worry about" — the cluster also
    /// isn't really live, and the caller will alloc fresh either way).
    fn read_refcount(&self, host_off: u64) -> Result<u16> {
        let (block_off, off) = match self.locate_refcount_entry(host_off)? {
            RefcountEntryLocation::At {
                block_off,
                byte_in_block,
            } => (block_off, byte_in_block),
            // No entry means no references — the cluster is not live, so
            // there is no share to copy away from.
            RefcountEntryLocation::PastTableCoverage | RefcountEntryLocation::BlockNotAllocated => {
                return Ok(0)
            }
        };

        let mut entry_bytes = [0u8; 2];
        self.dev_read(block_off + off as u64, &mut entry_bytes)?;
        Ok(u16::from_be_bytes(entry_bytes))
    }

    /// Whether the L2 table at `l2_table_off` is reached from more than
    /// one L1 table, so that writing into it would change somebody
    /// else's view of the disk.
    ///
    /// The same shape as the data-cluster test in
    /// [`Qcow2Reader::plan_write`], and for the same reason: COPIED is
    /// the format's promise that the refcount is exactly 1, so it is
    /// only when the bit is clear that the refcount has to be read.
    fn l2_table_is_shared(&self, l1_entry: u64, l2_table_off: u64) -> Result<bool> {
        if l1_entry & L2_FLAG_COPIED != 0 {
            return Ok(false);
        }
        Ok(self.read_refcount(l2_table_off)? > 1)
    }

    /// Write `cluster_size` zero bytes to `host_off` and flush.
    /// Used to initialise a freshly-allocated cluster before the user's
    /// payload lands inside it.
    fn zero_cluster(&self, host_off: u64) -> Result<()> {
        let cluster_size = self.header.cluster_size as usize;
        let zeros = vec![0u8; cluster_size];
        self.dev_write(host_off, &zeros)?;
        self.dev_flush()?;
        Ok(())
    }

    /// Overwrite a single L2 entry on disk and invalidate the in-memory
    /// L2 cache so subsequent lookups re-read. Allocates a fresh L2 table
    /// (and updates L1) on demand if `l1[l1_idx]` is unallocated.
    fn update_l2_entry(&self, l1_idx: u32, l2_idx: u32, new_entry: u64) -> Result<()> {
        let l1_entry = {
            let l1 = self.l1.lock().unwrap();
            if (l1_idx as usize) >= l1.len() {
                return Err(Error::Corrupt("l1_idx out of range"));
            }
            l1[l1_idx as usize]
        };
        // The same check the read path makes, and it matters more here:
        // this writes a whole cluster to the offset the entry names, so
        // an unchecked one turns a corrupt table into writes at an
        // address the image chose.
        let l2_table_off = match self.l2_table_offset(l1_entry)? {
            Some(off) => off,
            None => self.allocate_l2_table(l1_idx)?,
        };

        // An L2 table can be shared, and when an internal snapshot
        // exists it is exactly what is shared: the active L1 and the
        // snapshot's L1 point at the same table, and the format says so
        // by clearing COPIED on the L1 entry. Writing into it changes
        // the snapshot's view of the disk, which is the one thing a
        // snapshot is for. So copy it first — allocate, copy verbatim,
        // repoint L1 with COPIED set, and only then release the old
        // table's share of it.
        //
        // The copy leaves the entries alone. Every data cluster the
        // table names was already reachable from two L1 tables and is
        // still reachable from two afterwards, so no refcount changes:
        // the number of paths to each cluster is what the refcount
        // counts, and copying the table does not add or remove one.
        //
        // Crash-safety order: data, then L1, then the release. A crash
        // between the first two leaks a cluster and changes nothing a
        // reader can see.
        let l2_table_off = if self.l2_table_is_shared(l1_entry, l2_table_off)? {
            // Read whole only when it has to be copied whole (#44).
            let mut l2_bytes = vec![0u8; self.header.cluster_size as usize];
            self.dev_read(l2_table_off, &mut l2_bytes)?;
            let copy_off = self.allocate_cluster()?;
            self.dev_write(copy_off, &l2_bytes)?;
            self.dev_flush()?;
            self.update_l1_entry(l1_idx, l2_entry_for(copy_off))?;
            let _ = self.decrement_refcount(l2_table_off);
            copy_off
        } else {
            l2_table_off
        };

        // Only the eight bytes of this entry go back, not the whole table
        // (#44). A copied table was written whole just above, so the entry
        // lands on top of the copy.
        let off = l2_idx as u64 * TABLE_ENTRY_BYTES;
        self.dev_write(l2_table_off + off, &new_entry.to_be_bytes())?;
        self.dev_flush()?;

        // Invalidate caches keyed off this L2 cluster.
        *self.l2_cache.lock().unwrap() = None;
        *self.decompress_cache.lock().unwrap() = None;
        Ok(())
    }

    /// Flush writes to stable storage. No-op for read-only images.
    pub fn flush(&self) -> Result<()> {
        if !self.writable {
            return Ok(());
        }
        self.dev_flush()
    }

    /// Allocate a host cluster, zero-initialise it, and point L1 at it as
    /// a brand-new L2 table. Returns the host offset of the new L2.
    fn allocate_l2_table(&self, l1_idx: u32) -> Result<u64> {
        let l2_host = self.allocate_cluster()?;
        self.zero_cluster(l2_host)?;
        let new_l1_entry = l2_entry_for(l2_host);
        self.update_l1_entry(l1_idx, new_l1_entry)?;
        Ok(l2_host)
    }

    /// Overwrite a single L1 entry on disk and update the in-memory cache.
    /// Holds the l1 mutex across the disk write so other threads never
    /// observe a memory/disk mismatch.
    fn update_l1_entry(&self, l1_idx: u32, new_entry: u64) -> Result<()> {
        let mut l1 = self.l1.lock().unwrap();
        if (l1_idx as usize) >= l1.len() {
            return Err(Error::Corrupt("l1_idx out of range"));
        }
        let l1_offset_on_disk = self.header.l1_table_offset + (l1_idx as u64) * TABLE_ENTRY_BYTES;
        self.dev_write(l1_offset_on_disk, &new_entry.to_be_bytes())?;
        self.dev_flush()?;
        l1[l1_idx as usize] = new_entry;
        Ok(())
    }

    /// Read exactly `buf.len()` bytes starting at virtual `offset`.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let len = buf.len() as u64;
        if len == 0 {
            return Ok(());
        }
        let end = offset
            .checked_add(len)
            .ok_or(Error::Corrupt("offset+len overflow"))?;
        if end > self.header.virtual_size {
            return Err(Error::OutOfBounds {
                offset,
                len,
                size: self.header.virtual_size,
            });
        }

        let cluster_size = self.header.cluster_size;

        let mut cursor = offset;
        let mut written = 0usize;

        while cursor < end {
            let addr = self.split_virtual(cursor);
            let in_cluster = addr.offset_in_cluster;
            let chunk = std::cmp::min(cluster_size - in_cluster, end - cursor) as usize;

            let map = self.lookup_cluster(cursor)?;
            let dst = &mut buf[written..written + chunk];

            match map {
                ClusterMap::Plain { host_off, .. } => {
                    self.dev_read(host_off + in_cluster, dst)?;
                }
                ClusterMap::Compressed { host_off, byte_len } => {
                    self.read_decompressed_slice(
                        addr.virt_cluster,
                        host_off,
                        byte_len,
                        in_cluster as usize,
                        dst,
                    )?;
                }
                ClusterMap::Zero => {
                    dst.fill(0);
                }
                ClusterMap::Unallocated => {
                    self.read_unallocated(cursor, dst)?;
                }
            }

            cursor += chunk as u64;
            written += chunk;
        }

        Ok(())
    }

    /// Defer to the backing chain if present, otherwise zero-fill. Backing
    /// reads past the backing's virtual_size are zero-filled per the spec.
    fn read_unallocated(&self, virt: u64, dst: &mut [u8]) -> Result<()> {
        match &self.backing {
            None => {
                dst.fill(0);
                Ok(())
            }
            Some(b) => {
                let bsz = b.virtual_size();
                let len = dst.len() as u64;
                if virt >= bsz {
                    dst.fill(0);
                    Ok(())
                } else if virt + len > bsz {
                    let n = (bsz - virt) as usize;
                    b.read_at(virt, &mut dst[..n])?;
                    dst[n..].fill(0);
                    Ok(())
                } else {
                    b.read_at(virt, dst)
                }
            }
        }
    }

    /// How far the image reaches. See [`Qcow2Reader::image_len`].
    fn image_len(&self) -> u64 {
        *self.image_len.lock().unwrap()
    }

    /// A host cluster offset taken out of a table entry, checked before
    /// it is used as an address.
    ///
    /// The format says such an offset is cluster-aligned and inside the
    /// image, and every other producer enforces both: the disk-image
    /// validator refuses to open an image whose L1 entry is unaligned
    /// ("Table is not cluster aligned; L1 entry corrupted") and refuses
    /// to convert one whose L2 entry is ("Cluster allocation offset
    /// 0x200 unaligned"). This reader used to follow either — and the
    /// masks are the problem, not the arithmetic: `OFFSET_MASK` covers
    /// bits 9..55, so an offset of 0x200 survives it intact and names
    /// byte 512, which is inside the header. The read succeeded and
    /// handed the header's own bytes back as the guest's data, which is
    /// the one failure a caller cannot detect.
    fn checked_host_offset(
        &self,
        off: u64,
        unaligned: &'static str,
        outside: &'static str,
    ) -> Result<u64> {
        let cluster_size = self.header.cluster_size;
        if !off.is_multiple_of(cluster_size) {
            return Err(Error::Corrupt(unaligned));
        }
        let end = off
            .checked_add(cluster_size)
            .ok_or(Error::Corrupt(outside))?;
        if end > self.image_len() {
            return Err(Error::Corrupt(outside));
        }
        Ok(off)
    }

    /// The L2 table an L1 entry names, checked.
    ///
    /// Returns `None` for an entry of zero, which is the format's way of
    /// saying the whole range that entry covers is unallocated.
    fn l2_table_offset(&self, l1_entry: u64) -> Result<Option<u64>> {
        if l1_entry & L1_RESERVED_MASK != 0 {
            return Err(Error::Corrupt("L1 entry has reserved bits set"));
        }
        let off = l1_entry & OFFSET_MASK;
        if off == 0 {
            return Ok(None);
        }
        self.checked_host_offset(
            off,
            "L1 entry names an L2 table that is not cluster-aligned",
            "L1 entry names an L2 table outside the image",
        )
        .map(Some)
    }

    fn lookup_cluster(&self, virt: u64) -> Result<ClusterMap> {
        let ClusterAddress {
            l1_index, l2_index, ..
        } = self.split_virtual(virt);

        let l1_entry = {
            let l1 = self.l1.lock().unwrap();
            if (l1_index as u64) >= l1.len() as u64 {
                return Err(Error::Corrupt("l1_index past l1 table"));
            }
            l1[l1_index as usize]
        };
        let Some(l2_table_off) = self.l2_table_offset(l1_entry)? else {
            return Ok(ClusterMap::Unallocated);
        };
        // COPIED on the L1 entry says the L2 table's refcount is 1. When
        // it is clear the table may be reached from a snapshot's L1 as
        // well, and then every cluster the table names is reached twice
        // too — so a COPIED bit on an entry inside it cannot be taken at
        // face value, whatever it says.
        let l1_copied = l1_entry & L2_FLAG_COPIED != 0;

        let l2_entry = self.read_l2_entry(l1_index, l2_table_off, l2_index)?;

        if l2_entry & L2_FLAG_COMPRESSED != 0 {
            let (host_off, byte_len) =
                decode_compressed_descriptor(l2_entry, self.header.cluster_bits);
            // BOUNDED HERE, BECAUSE THIS ARM RETURNS BEFORE THE ONE
            // BELOW REACHES `checked_host_offset`. Until this, a
            // compressed descriptor was the only span in this file that
            // reached the device unchecked: `read_decompressed_slice`
            // adds no bound of its own, so an offset the image chose
            // named a read address, and `plan_write` put the same value
            // in `release` where `decrement_refcount` treats it as a
            // cluster identity.
            //
            // NOT `checked_host_offset`, AND THAT IS THE WHOLE POINT.
            // That guard also demands cluster alignment, and a
            // compressed descriptor is byte-granular by design --
            // `decode_compressed_descriptor` splits the 62-bit word at
            // `62 - (cluster_bits - 8)`, so a legal offset can name any
            // byte. Reusing it would reject valid compressed images.
            // What is needed is the bound without the alignment demand,
            // which is the shape the L1 and refcount tables already use.
            //
            // The END is checked too, not just the start: the payload
            // runs `[host_off, host_off + byte_len)` and a descriptor
            // can put the start inside the file and the end past it.
            span_inside_the_image(
                self.image_len(),
                host_off,
                byte_len as u64,
                "L2 entry names a compressed cluster outside the image",
            )?;
            return Ok(ClusterMap::Compressed { host_off, byte_len });
        }
        // From here the entry describes a standard cluster, so the
        // reserved spans either side of the offset field must be clear
        // before anything in it is believed — including the zero flag,
        // which is bit 0 of the same word.
        let reserved = if self.header.version >= 3 {
            L2_RESERVED_MASK
        } else {
            // Bit 0 is the zero flag only from v3. Ignoring it on v2
            // served the host cluster where the validator refuses the
            // image ("Zero cluster entry found in pre-v3 image").
            L2_RESERVED_MASK | L2_FLAG_ZERO
        };
        if l2_entry & reserved != 0 {
            return Err(Error::Corrupt("L2 entry has reserved bits set"));
        }
        if self.header.version >= 3 && (l2_entry & L2_FLAG_ZERO) != 0 {
            return Ok(ClusterMap::Zero);
        }
        let host = l2_entry & OFFSET_MASK;
        if host == 0 {
            return Ok(ClusterMap::Unallocated);
        }
        let host = self.checked_host_offset(
            host,
            "L2 entry names a data cluster that is not cluster-aligned",
            "L2 entry names a data cluster outside the image",
        )?;
        let copied = l1_copied && (l2_entry & L2_FLAG_COPIED != 0);
        Ok(ClusterMap::Plain {
            host_off: host,
            copied,
        })
    }

    fn read_l2_entry(&self, l1_index: u32, l2_table_off: u64, l2_index: u32) -> Result<u64> {
        let cluster_size = self.header.cluster_size as usize;
        let mut cache = self.l2_cache.lock().unwrap();

        let need_load = match &*cache {
            Some((cached_idx, _)) => *cached_idx != l1_index,
            None => true,
        };

        if need_load {
            let mut buf = vec![0u8; cluster_size];
            self.dev_read(l2_table_off, &mut buf)?;
            *cache = Some((l1_index, buf));
        }

        let bytes = &cache.as_ref().unwrap().1;
        let off = l2_index as usize * TABLE_ENTRY_BYTES as usize;
        let entry = u64::from_be_bytes(
            bytes[off..off + TABLE_ENTRY_BYTES as usize]
                .try_into()
                .unwrap(),
        );
        Ok(entry)
    }

    /// Decompress (or hit the cache for) a compressed cluster, then copy out
    /// `[skip, skip+dst.len())` of the decompressed bytes.
    fn read_decompressed_slice(
        &self,
        virt_cluster: u64,
        host_off: u64,
        byte_len: usize,
        skip: usize,
        dst: &mut [u8],
    ) -> Result<()> {
        let cluster_size = self.header.cluster_size as usize;

        // Cache lookup.
        {
            let cache = self.decompress_cache.lock().unwrap();
            if let Some((c, bytes)) = &*cache {
                if *c == virt_cluster {
                    dst.copy_from_slice(&bytes[skip..skip + dst.len()]);
                    return Ok(());
                }
            }
        }

        // Read compressed bytes.
        let mut compressed = vec![0u8; byte_len];
        self.dev_read(host_off, &mut compressed)?;

        // Dispatch on the v3 compression_type field. 0 = zlib (raw
        // deflate per the qcow2 spec — no zlib header), 1 = zstd. v2
        // images leave the field at the default 0.
        let mut decoded = vec![0u8; cluster_size];
        match self.header.compression_type {
            0 => {
                let mut decoder = DeflateDecoder::new(&compressed[..]);
                decoder
                    .read_exact(&mut decoded)
                    .map_err(|e| Error::Decompress(e.to_string()))?;
            }
            1 => {
                // ruzstd's StreamingDecoder reads one or more zstd frames
                // and yields the decoded byte stream. read_to_end is
                // used (not read_exact) because zstd frames know their
                // own decompressed length; we then check it matches the
                // cluster size we expected.
                //
                // ONE BYTE MORE THAN A CLUSTER, and no further. The
                // descriptor bounds how much compressed payload there
                // is -- at most about 4 MiB -- and says nothing about
                // how much it decodes to. Zeros compress to almost
                // nothing, so a payload inside a single 512-byte sector
                // decoded to 64 MiB and reached 2.35 GB resident before
                // the length was compared to the cluster size, which
                // happened after the whole frame was in memory. The
                // extra byte is what makes "too long" still detectable
                // once the read stops early.
                let decoder = ruzstd::decoding::StreamingDecoder::new(&compressed[..])
                    .map_err(|e| Error::Decompress(e.to_string()))?;
                let mut out = Vec::with_capacity(cluster_size);
                decoder
                    .take(cluster_size as u64 + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| Error::Decompress(e.to_string()))?;
                if out.len() != cluster_size {
                    return Err(Error::Decompress(format!(
                        "zstd frame produced {} bytes, expected {}",
                        out.len(),
                        cluster_size
                    )));
                }
                decoded.copy_from_slice(&out);
            }
            _ => {
                return Err(Error::Unsupported("unknown compression_type"));
            }
        }

        dst.copy_from_slice(&decoded[skip..skip + dst.len()]);

        // Populate cache.
        let mut cache = self.decompress_cache.lock().unwrap();
        *cache = Some((virt_cluster, decoded));
        Ok(())
    }
}

/// The L1 or L2 entry that points at a host cluster we have just
/// allocated.
///
/// COPIED (bit 63) is the format's promise that the cluster's refcount
/// is exactly 1, so a writer may go through it in place without
/// copy-on-write. A freshly allocated cluster has refcount 1 by
/// construction, which is why setting the flag here is not an
/// assumption — it is the thing that just became true.
///
/// The mask is not defensive tidying either: an offset with low bits
/// set would silently shift the pointer once it reaches an L2 entry,
/// because those bits belong to the flags rather than to the address.
fn l2_entry_for(host_off: u64) -> u64 {
    (host_off & OFFSET_MASK) | L2_FLAG_COPIED
}

/// Decode the compressed-cluster descriptor in an L2 entry.
///
/// Returns `(host offset, span)`: where the compressed payload starts,
/// byte-granular, and how many bytes to read from there.
///
/// The descriptor packs two fields whose boundary **moves with the
/// cluster size**, which is the part worth reading slowly. The low
/// `62 - (cluster_bits - 8)` bits are a byte offset; everything above
/// them is a count of *additional* 512-byte sectors. A bigger cluster
/// needs more bits for the sector count (a compressed cluster can be
/// longer), so it leaves fewer for the offset — the two fields trade
/// against each other inside one 62-bit word.
///
/// Three things are easy to get wrong here, and each has a name below:
///
/// * the sector count is of sectors **beyond the first**, so the span
///   is at least one sector even when the count is zero;
/// * it is measured from the start of the sector containing
///   `host_off`, not from `host_off` — so an unaligned offset yields a
///   span that is *not* a whole number of sectors;
/// * 512 is the compressed-sector size from the specification. It is
///   deliberately neither the cluster size nor the device's block size,
///   and does not follow either of them.
///
/// The encode side lives in this crate's test fixtures rather than
/// here, since nothing in the reader writes compressed clusters. What
/// keeps the two honest is `tests/qemu_validation.rs`, which hands our
/// compressed images to an external tool and compares its decode with
/// ours.
fn decode_compressed_descriptor(entry: u64, cluster_bits: u32) -> (u64, usize) {
    // Bits 62 and 63 are the COMPRESSED and COPIED flags; the
    // descriptor is everything below them.
    let descriptor = entry & ((1u64 << 62) - 1);

    let offset_bits = (62 - (cluster_bits - 8)) as u64;
    let host_off = descriptor & ((1u64 << offset_bits) - 1);
    let additional_sectors = descriptor >> offset_bits;

    let first_sector_start = (host_off / COMPRESSED_SECTOR_SIZE) * COMPRESSED_SECTOR_SIZE;
    let end = first_sector_start + (additional_sectors + 1) * COMPRESSED_SECTOR_SIZE;
    let span = (end - host_off) as usize;
    (host_off, span)
}

/// Read the backing-file path from the header and resolve it relative to the
/// child image's directory if it isn't absolute.
fn read_backing_path(
    dev: &Arc<dyn BlockDevice>,
    header: &Header,
    child_path: &Path,
) -> Result<PathBuf> {
    let len = header.backing_file_size as usize;
    if len > 1024 {
        return Err(Error::Corrupt("backing_file_size > 1024 bytes"));
    }
    // A zero length never reaches here: `open_inner` treats it as "no
    // backing file", the way the reference tool does. The assertion
    // records that this function's contract starts after that decision,
    // so a caller cannot reintroduce the empty-path join by calling it
    // directly.
    debug_assert_ne!(len, 0, "a chain with no path length is not a chain");
    let mut bytes = vec![0u8; len];
    dev.read_at(header.backing_file_offset, &mut bytes)
        .map_err(fs_core_to_qcow2_error)?;
    if bytes.contains(&0) {
        // A NUL cannot be in a path, and a string carrying one is how a
        // caller that measures with `strlen` and a caller that uses the
        // declared length come to disagree about which file was opened.
        return Err(Error::Corrupt("backing-file path contains a NUL byte"));
    }
    let s = std::str::from_utf8(&bytes).map_err(|_| Error::BadBackingPath)?;
    let p = Path::new(s);
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        let parent = child_path.parent().unwrap_or_else(|| Path::new("."));
        Ok(parent.join(p))
    }
}

// ---------------------------------------------------------------------------
// fs_core::BlockRead bridge — lifts qcow2's rich Error to fs_core::Error so
// any consumer that takes a `BlockRead` (partition probe, fs driver, slice
// adapter) can drive a Qcow2Reader directly.
// ---------------------------------------------------------------------------

impl fs_core::BlockRead for Qcow2Reader {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        Qcow2Reader::read_at(self, offset, buf).map_err(qcow2_to_fs_core_error)
    }

    fn size_bytes(&self) -> u64 {
        self.virtual_size()
    }
}

/// Write support: forwards to inherent `write_at` / `flush` /
/// `is_writable`. Read-only images keep returning `ReadOnly` from the
/// inherent `write_at`, which maps cleanly to `fs_core::Error::ReadOnly`.
impl fs_core::BlockDevice for Qcow2Reader {
    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()> {
        Qcow2Reader::write_at(self, offset, buf).map_err(qcow2_to_fs_core_error)
    }

    fn flush(&self) -> fs_core::Result<()> {
        Qcow2Reader::flush(self).map_err(qcow2_to_fs_core_error)
    }

    fn is_writable(&self) -> bool {
        Qcow2Reader::is_writable(self)
    }

    /// A QCOW2 IMAGE PRESENTED AS A DEVICE DOES NOT GROW, AND THAT IS A
    /// DECISION RATHER THAN AN OMISSION.
    ///
    /// The guest-visible length here is the header's `size` field: it is what
    /// the image declares to whatever is layered inside it. Growing that is
    /// resizing the virtual disk — a header rewrite with its own rules — not
    /// the same operation as letting the *host* file get longer, which is what
    /// `dev_grow_for_cluster` does when the allocator needs a new cluster.
    ///
    /// So this stays `false`, and `set_len` keeps the trait's refusal. It is
    /// stated here because a defaulted method left unmentioned reads exactly
    /// like one nobody considered.
    fn can_grow(&self) -> bool {
        false
    }
}

fn qcow2_to_fs_core_error(e: Error) -> fs_core::Error {
    match e {
        Error::Io(io) => fs_core::Error::Io(io),
        Error::OutOfBounds { offset, len, size } => {
            fs_core::Error::OutOfBounds { offset, len, size }
        }
        Error::ReadOnly => fs_core::Error::ReadOnly,
        other => fs_core::Error::Custom(other.to_string()),
    }
}

fn fs_core_to_qcow2_error(e: fs_core::Error) -> Error {
    match e {
        fs_core::Error::Io(io) => Error::Io(io),
        fs_core::Error::ShortRead { offset, want, got } => Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("short read at {offset}: wanted {want} got {got}"),
        )),
        fs_core::Error::ReadOnly => Error::ReadOnly,
        fs_core::Error::OutOfBounds { offset, len, size } => {
            Error::OutOfBounds { offset, len, size }
        }
        fs_core::Error::Custom(s) => Error::Decompress(s),
    }
}

#[cfg(test)]
mod span_tests {
    use super::*;

    /// A span that fits the image is its length, in every width.
    #[test]
    fn a_span_inside_the_image_is_its_length() {
        assert_eq!(
            span_inside_the_image(8192, 4096, 4096, "outside").unwrap(),
            4096
        );
        assert!(span_inside_the_image(8192, 4097, 4096, "outside").is_err());
    }

    /// #62: A LENGTH `usize` CANNOT HOLD IS REFUSED, NOT TRUNCATED.
    ///
    /// On a 32-bit target `length as usize` kept the low 32 bits, so a
    /// 5 GiB refcount table inside a 6 GiB image came back as a 1 GiB
    /// buffer while the walk over it was bounded by the full 5 GiB, and
    /// the slice index ran past the buffer. Only a 32-bit build can
    /// observe it, because on 64 bits every `u64` fits.
    #[cfg(target_pointer_width = "32")]
    #[test]
    fn a_span_longer_than_usize_is_refused_rather_than_truncated() {
        let gib = 1u64 << 30;
        let got = span_inside_the_image(6 * gib, 0, 5 * gib, "outside");
        assert!(
            matches!(got, Err(Error::Corrupt("outside"))),
            "a 5 GiB span does not fit a 32-bit usize; got {got:?}"
        );
    }
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;

    /// The compressed descriptor, encoded the way the format specifies.
    ///
    /// Only the tests need this direction, so it lives here rather than
    /// in the crate proper. It is written from the specification rather
    /// than derived from `decode_compressed_descriptor`, so a
    /// round-trip through the two is a real check and not a tautology.
    fn encode_compressed_descriptor(host_off: u64, compressed_len: u64, cluster_bits: u32) -> u64 {
        let offset_bits = 62 - (cluster_bits - 8);
        // The count is of *additional* sectors beyond the one containing
        // host_off — so it is measured from the start of that sector,
        // not from host_off itself.
        let first_sector_start = (host_off / COMPRESSED_SECTOR_SIZE) * COMPRESSED_SECTOR_SIZE;
        let end = host_off + compressed_len;
        let sectors = (end - first_sector_start).div_ceil(COMPRESSED_SECTOR_SIZE);
        let additional = sectors - 1;
        (additional << offset_bits) | host_off | L2_FLAG_COMPRESSED
    }

    /// A payload wholly inside one sector spans exactly that sector's
    /// remainder.
    #[test]
    fn a_single_sector_payload_decodes_to_one_sector() {
        let entry = encode_compressed_descriptor(4096 * 3, 400, 12);
        assert_eq!(decode_compressed_descriptor(entry, 12), (4096 * 3, 512));
    }

    /// A payload crossing sector boundaries.
    ///
    /// **This is the case no test reached before.** Every compressed
    /// fixture in the suite is a cluster of one repeated byte, which
    /// deflates to well under one sector, so the sector count was always
    /// zero and the whole `additional sectors` half of the descriptor
    /// was dead as far as the tests were concerned: widening the offset
    /// field by a bit — which moves where that count is read from —
    /// failed **no** tests at all.
    #[test]
    fn a_multi_sector_payload_decodes_to_every_sector_it_covers() {
        for (len, expected_span) in [(512u64, 512usize), (513, 1024), (1024, 1024), (1025, 1536)] {
            let entry = encode_compressed_descriptor(4096 * 3, len, 12);
            let (host, span) = decode_compressed_descriptor(entry, 12);
            assert_eq!(host, 4096 * 3, "host offset for len {len}");
            assert_eq!(span, expected_span, "span for len {len}");
        }
    }

    /// A host offset that is not sector-aligned.
    ///
    /// The span runs from `host_off` to the end of the last sector the
    /// payload touches, so it is *not* a whole number of sectors when
    /// the offset is unaligned. The report flagged this term as never
    /// exercised, and it was right — every fixture is cluster-aligned.
    #[test]
    fn an_unaligned_host_offset_shortens_the_span_by_its_remainder() {
        // 100 bytes into a sector, 300 bytes long: still one sector, but
        // only 412 bytes of it lie at or after host_off.
        let entry = encode_compressed_descriptor(4096 * 3 + 100, 300, 12);
        assert_eq!(
            decode_compressed_descriptor(entry, 12),
            (4096 * 3 + 100, 412)
        );

        // Same start, long enough to cross into the next sector.
        let entry = encode_compressed_descriptor(4096 * 3 + 100, 500, 12);
        assert_eq!(
            decode_compressed_descriptor(entry, 12),
            (4096 * 3 + 100, 924)
        );
    }

    /// The offset field's width depends on the cluster size, so the same
    /// host offset encodes differently at different `cluster_bits`.
    ///
    /// Every existing test runs at `cluster_bits = 12`, which left the
    /// `- (cluster_bits - 8)` term unexercised.
    #[test]
    fn the_offset_field_width_tracks_the_cluster_size() {
        for cluster_bits in [9u32, 12, 16, 21] {
            let host = 1u64 << 20;
            let entry = encode_compressed_descriptor(host, 1000, cluster_bits);
            let (decoded_host, span) = decode_compressed_descriptor(entry, cluster_bits);
            assert_eq!(decoded_host, host, "host at cluster_bits {cluster_bits}");
            assert_eq!(span, 1024, "span at cluster_bits {cluster_bits}");
        }
    }

    /// The host-offset mask covers bits 9..55 inclusive, and nothing else.
    ///
    /// Derived here from the bit range the specification names, rather
    /// than copied from the constant — the constant written out as a hex
    /// literal is exactly the kind of thing that can be one nibble wrong
    /// and still look plausible. Nothing else in the suite would notice:
    /// widening the mask to strip bit 9 as well failed **no** tests,
    /// because every fixture offset is cluster-aligned and so has zeroes
    /// there anyway.
    /// The mask in `l2_entry_for` is load-bearing, not tidying.
    ///
    /// Every host offset the allocator hands out is cluster-aligned, so
    /// the mask is a no-op in practice and dropping it failed **no**
    /// tests. It matters for the case the allocator does not produce: an
    /// offset with low bits set would land those bits on top of the
    /// flags field, and the entry would then read back as a *different*
    /// host offset — or, with bit 0 set on a v3 image, as the "reads as
    /// zeros" flag.
    #[test]
    fn an_unaligned_host_offset_cannot_bleed_into_the_flag_bits() {
        let aligned = 4096u64 * 7;
        assert_eq!(l2_entry_for(aligned), aligned | L2_FLAG_COPIED);

        // Low bits set: they must be dropped, not carried into flags.
        let entry = l2_entry_for(aligned | 0x1FF);
        assert_eq!(entry & OFFSET_MASK, aligned, "offset is unchanged");
        assert_eq!(
            entry & L2_FLAG_ZERO,
            0,
            "bit 0 did not become the zero flag"
        );
        assert_eq!(entry & L2_FLAG_COMPRESSED, 0, "bit 62 is clear");
        assert_ne!(entry & L2_FLAG_COPIED, 0, "COPIED is set");
    }

    #[test]
    fn the_offset_mask_is_exactly_bits_9_through_55() {
        let all_bits_below_56 = (1u64 << 56) - 1;
        let bits_below_9 = (1u64 << 9) - 1;
        assert_eq!(OFFSET_MASK, all_bits_below_56 & !bits_below_9);

        // Stated the other way round, so a single edit cannot satisfy
        // both: the mask keeps bit 9 and bit 55, and drops bit 8 and 56.
        assert_ne!(OFFSET_MASK & (1 << 9), 0, "bit 9 is part of the offset");
        assert_ne!(OFFSET_MASK & (1 << 55), 0, "bit 55 is part of the offset");
        assert_eq!(OFFSET_MASK & (1 << 8), 0, "bit 8 is not");
        assert_eq!(OFFSET_MASK & (1 << 56), 0, "bit 56 is not");
    }
}
