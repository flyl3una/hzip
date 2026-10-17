//! On-disk header parser. Layout per the QCOW2 spec:
//!
//! ```text
//!   0   u32  magic = "QFI\xfb" (0x51 0x46 0x49 0xfb)
//!   4   u32  version (2 or 3)
//!   8   u64  backing_file_offset
//!  16   u32  backing_file_size
//!  20   u32  cluster_bits        (cluster_size = 1 << cluster_bits)
//!  24   u64  size                (virtual disk size in bytes)
//!  32   u32  crypt_method        (0 = none, 1 = AES, 2 = LUKS)
//!  36   u32  l1_size             (number of u64 entries in L1)
//!  40   u64  l1_table_offset
//!  48   u64  refcount_table_offset
//!  56   u32  refcount_table_clusters
//!  60   u32  nb_snapshots
//!  64   u64  snapshots_offset
//!  --- v3 only ---
//!  72   u64  incompatible_features
//!  80   u64  compatible_features
//!  88   u64  autoclear_features
//!  96   u32  refcount_order      (refcount_bits = 1 << refcount_order)
//! 100   u32  header_length
//!  --- v3 additional fields (only present when header_length > 104) ---
//! 104   u8   compression_type    (0 = zlib, 1 = zstd)
//! 105   7B   padding
//! ```
//!
//! All multi-byte fields are big-endian.

use crate::error::{Error, Result};

pub const QCOW2_MAGIC: u32 = 0x5146_49fb; // "QFI\xfb"

/// Byte offsets of the header's fields, per the layout above.
///
/// The parser reads through these; the *fixtures* deliberately do not,
/// and write literal offsets instead. That is what lets a wrong
/// constant here be caught rather than agreed with: moving
/// `CLUSTER_BITS` by one byte fails 52 tests, `L1_TABLE_OFFSET` 35.
/// Rewriting the fixtures against this table would take both to zero.
/// `tests::offsets_match_the_published_specification` is the same
/// intent written down once, so it survives a later tidy-up.
pub mod offsets {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 4;
    pub const BACKING_FILE_OFFSET: usize = 8;
    pub const BACKING_FILE_SIZE: usize = 16;
    pub const CLUSTER_BITS: usize = 20;
    pub const SIZE: usize = 24;
    pub const CRYPT_METHOD: usize = 32;
    pub const L1_SIZE: usize = 36;
    pub const L1_TABLE_OFFSET: usize = 40;
    pub const REFCOUNT_TABLE_OFFSET: usize = 48;
    pub const REFCOUNT_TABLE_CLUSTERS: usize = 56;
    pub const NB_SNAPSHOTS: usize = 60;
    pub const SNAPSHOTS_OFFSET: usize = 64;

    // v3 only.
    pub const INCOMPATIBLE_FEATURES: usize = 72;
    pub const COMPATIBLE_FEATURES: usize = 80;
    pub const AUTOCLEAR_FEATURES: usize = 88;
    pub const REFCOUNT_ORDER: usize = 96;
    pub const HEADER_LENGTH: usize = 100;

    /// Where the v3 header ends if it carries no additional fields.
    /// A `header_length` above this means `COMPRESSION_TYPE` is present.
    pub const V3_BASE_HEADER_LENGTH: u32 = 104;
    /// First byte of the v3 additional-fields region.
    pub const COMPRESSION_TYPE: usize = 104;

    /// v2 has no feature words, no `refcount_order` and no
    /// `header_length` on disk. The specification fixes their values,
    /// and these are them — not defaults this crate chose.
    pub mod v2_implicit {
        pub const REFCOUNT_ORDER: u32 = 4;
        pub const HEADER_LENGTH: u32 = 72;
    }
}

/// Bits set in `incompatible_features` we know how to refuse cleanly.
pub mod incompat {
    pub const DIRTY: u64 = 1 << 0;
    pub const CORRUPT: u64 = 1 << 1;
    pub const DATA_FILE: u64 = 1 << 2; // external data file
    pub const COMPRESSION_TYPE: u64 = 1 << 3; // non-zlib compression declared
    pub const EXTENDED_L2: u64 = 1 << 4;
}

#[derive(Debug, Clone)]
pub struct Header {
    pub version: u32,
    pub cluster_bits: u32,
    pub cluster_size: u64,
    pub virtual_size: u64,
    pub crypt_method: u32,

    pub l1_size: u32,
    pub l1_table_offset: u64,

    pub refcount_table_offset: u64,
    pub refcount_table_clusters: u32,

    pub nb_snapshots: u32,
    pub snapshots_offset: u64,

    pub backing_file_offset: u64,
    pub backing_file_size: u32,

    /// v3 only — zero for v2 images.
    pub incompatible_features: u64,
    pub compatible_features: u64,
    pub autoclear_features: u64,
    pub refcount_order: u32,
    pub header_length: u32,

    /// v3 additional field at byte 104. Present only when
    /// `header_length > 104`; defaults to 0 (zlib) otherwise.
    /// Spec values: 0 = zlib (deflate), 1 = zstd.
    pub compression_type: u8,
}

impl Header {
    /// Parse from the first 104 bytes of the file. `bytes` may be longer; only
    /// the prefix is read.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 72 {
            return Err(Error::Corrupt("header shorter than 72 bytes"));
        }

        let magic = read_u32(bytes, offsets::MAGIC);
        if magic != QCOW2_MAGIC {
            return Err(Error::NotQcow2);
        }

        let version = read_u32(bytes, offsets::VERSION);
        if version != 2 && version != 3 {
            return Err(Error::UnsupportedVersion(version));
        }

        let backing_file_offset = read_u64(bytes, offsets::BACKING_FILE_OFFSET);
        let backing_file_size = read_u32(bytes, offsets::BACKING_FILE_SIZE);
        let cluster_bits = read_u32(bytes, offsets::CLUSTER_BITS);
        if !(9..=21).contains(&cluster_bits) {
            return Err(Error::InvalidClusterBits(cluster_bits));
        }
        let cluster_size = 1u64 << cluster_bits;

        let virtual_size = read_u64(bytes, offsets::SIZE);
        let crypt_method = read_u32(bytes, offsets::CRYPT_METHOD);
        let l1_size = read_u32(bytes, offsets::L1_SIZE);
        let l1_table_offset = read_u64(bytes, offsets::L1_TABLE_OFFSET);
        let refcount_table_offset = read_u64(bytes, offsets::REFCOUNT_TABLE_OFFSET);
        let refcount_table_clusters = read_u32(bytes, offsets::REFCOUNT_TABLE_CLUSTERS);
        let nb_snapshots = read_u32(bytes, offsets::NB_SNAPSHOTS);
        let snapshots_offset = read_u64(bytes, offsets::SNAPSHOTS_OFFSET);

        let (
            incompatible_features,
            compatible_features,
            autoclear_features,
            refcount_order,
            header_length,
        ) = if version >= 3 {
            if bytes.len() < offsets::V3_BASE_HEADER_LENGTH as usize {
                return Err(Error::Corrupt("v3 header shorter than 104 bytes"));
            }
            (
                read_u64(bytes, offsets::INCOMPATIBLE_FEATURES),
                read_u64(bytes, offsets::COMPATIBLE_FEATURES),
                read_u64(bytes, offsets::AUTOCLEAR_FEATURES),
                read_u32(bytes, offsets::REFCOUNT_ORDER),
                read_u32(bytes, offsets::HEADER_LENGTH),
            )
        } else {
            // v2 implicit defaults per spec.
            (
                0,
                0,
                0,
                offsets::v2_implicit::REFCOUNT_ORDER,
                offsets::v2_implicit::HEADER_LENGTH,
            )
        };

        // v3 additional-fields region starts at byte 104. The first byte is
        // `compression_type` (0 = zlib, 1 = zstd). Older v3 images set
        // header_length to 104 and omit this byte; treat as zlib.
        let compression_type = if version >= 3
            && header_length > offsets::V3_BASE_HEADER_LENGTH
            && bytes.len() > offsets::COMPRESSION_TYPE
        {
            bytes[offsets::COMPRESSION_TYPE]
        } else {
            0
        };

        // Sanity: l1 must be large enough to address the whole virtual disk.
        let l2_entries_per_cluster = cluster_size / crate::reader::TABLE_ENTRY_BYTES;
        let bytes_per_l1_entry = cluster_size * l2_entries_per_cluster;
        let needed_l1 = virtual_size.div_ceil(bytes_per_l1_entry);
        if (l1_size as u64) < needed_l1 {
            return Err(Error::Corrupt("l1_size too small for virtual_size"));
        }

        // WHERE THE TABLES ARE, checked before anything follows them.
        //
        // Every table in a qcow2 image occupies whole clusters, so its
        // offset is cluster-aligned by construction. These three were
        // read out of the header and used raw: the L1 table was read
        // from wherever `l1_table_offset` pointed and its bytes taken
        // as L1 entries, and `allocate_cluster` did the same with the
        // refcount table — and then *wrote* a refcount block back to an
        // address decoded from those bytes.
        //
        // The reference validator refuses both by name, and the two
        // failures we had are different from each other and both bad.
        // Measured on a 3 MiB pattern converted to qcow2, with each
        // offset moved half a cluster (0x200) past its real value, so
        // the table is still inside the image and only its alignment is
        // wrong:
        //
        //   l1_table_offset  0x30000 -> 0x30200
        //     qemu-img info: "Active L1 table offset invalid"
        //     ours: open OK; read_at(0, 16) = 00 x16, where the guest
        //           really holds 00 01 02 03 ... — a populated image
        //           reading as sparse, with no error
        //     ours: open_rw + write_at succeeded, grew the file by
        //           131072 bytes and published an L2 pointer into bytes
        //           that are not the L1 table
        //
        //   refcount_table_offset  0x10000 -> 0x10200
        //     qemu-img info: "Reference count table offset invalid"
        //     ours: open OK, and a write allocated a cluster from
        //           refcount entries decoded out of the wrong bytes
        //
        // `span_inside_the_image` already keeps both tables inside the
        // file; alignment is the half nothing checked, and it needs no
        // device, so it belongs here beside the other header rules.
        for (offset, unaligned) in [
            (l1_table_offset, "l1_table_offset is not cluster-aligned"),
            (
                refcount_table_offset,
                "refcount_table_offset is not cluster-aligned",
            ),
            // Zero means "no snapshot table", which is ordinary; a
            // non-zero one is a table like any other.
            (snapshots_offset, "snapshots_offset is not cluster-aligned"),
        ] {
            if !offset.is_multiple_of(cluster_size) {
                return Err(Error::Corrupt(unaligned));
            }
        }

        // The backing-file name lives in the header area, ahead of the
        // L1 table, so its offset is inside the first cluster. The
        // reference tool caps it there and refuses anything past it.
        //
        // Nothing bounded it, and the chain was keyed off
        // `backing_file_size` rather than the offset. Measured:
        //
        //   size = 12, offset = 0
        //     qemu-img info: reports the image, no backing file
        //     ours: read 12 bytes from offset 0 — the header's own
        //           magic — and refused with BadBackingPath, on an
        //           image the validator opens without complaint
        //
        //   offset = 0x50000, a real parent's name written there
        //     qemu-img info: "Invalid backing file offset"
        //     ours: opened it, and opened the parent
        //
        // The second is the one that matters. 0x50000 is inside a data
        // cluster — bytes the guest itself wrote — so a guest that can
        // write its own disk could choose which host file this reader
        // opens as its parent.
        if backing_file_offset > cluster_size {
            return Err(Error::Corrupt(
                "backing_file_offset is past the first cluster, where the name cannot be",
            ));
        }
        // AND THE NAME MUST END THERE TOO. The offset gate alone let
        // `offset + size` run into cluster 1, which can be a guest data
        // cluster, so the guest could still supply the path. The
        // reference refuses "Backing file name too long" when
        // `size > cluster_size - offset`. The subtraction cannot wrap
        // because of the check above, which is why the two are ordered.
        let room = cluster_size
            .checked_sub(backing_file_offset)
            .ok_or(Error::Corrupt(
                "backing_file_offset is past the first cluster",
            ))?;
        if u64::from(backing_file_size) > room {
            return Err(Error::Corrupt(
                "backing-file name runs past the first cluster, where it cannot be",
            ));
        }

        Ok(Header {
            version,
            cluster_bits,
            cluster_size,
            virtual_size,
            crypt_method,
            l1_size,
            l1_table_offset,
            refcount_table_offset,
            refcount_table_clusters,
            nb_snapshots,
            snapshots_offset,
            backing_file_offset,
            backing_file_size,
            incompatible_features,
            compatible_features,
            autoclear_features,
            refcount_order,
            header_length,
            compression_type,
        })
    }

    /// Number of u64 entries in one L2 table.
    pub fn l2_entries(&self) -> u64 {
        self.cluster_size / 8
    }

    /// Reject feature combinations the reader does not yet handle. Currently
    /// supported: uncompressed clusters, zlib- and zstd-compressed clusters,
    /// backing files. Refused: encryption, external data file, extended L2
    /// entries, unknown compression types.
    pub fn check_supported(&self) -> Result<()> {
        if self.crypt_method != 0 {
            return Err(Error::Unsupported("encryption (AES or LUKS)"));
        }
        if self.version >= 3 {
            // COMPRESSION_TYPE in incompatible_features means "compression
            // is something other than the default (zlib)". We honour it as
            // long as compression_type itself is a value we implement.
            let known = incompat::DIRTY | incompat::CORRUPT | incompat::COMPRESSION_TYPE;
            let unknown = self.incompatible_features & !known;
            if unknown != 0 {
                if self.incompatible_features & incompat::DATA_FILE != 0 {
                    return Err(Error::Unsupported("external data file"));
                }
                if self.incompatible_features & incompat::EXTENDED_L2 != 0 {
                    return Err(Error::Unsupported("extended L2 entries"));
                }
                return Err(Error::Unsupported("unknown incompatible_features bit"));
            }
            // 0 = zlib, 1 = zstd. Anything else is something we don't know
            // how to decode.
            if self.compression_type > 1 {
                return Err(Error::Unsupported("unknown compression_type"));
            }
        }
        Ok(())
    }

    /// The image was flagged corrupt: somebody has already established
    /// that its metadata is inconsistent.
    ///
    /// Tolerated on the read path deliberately — the flag is how a
    /// damaged image gets looked at — and refused on the write path by
    /// [`Header::check_writable`].
    pub fn is_corrupt(&self) -> bool {
        self.version >= 3 && self.incompatible_features & incompat::CORRUPT != 0
    }

    /// The refcounts were not written back before the last shutdown, so
    /// the refcount blocks on disk are not to be trusted.
    pub fn has_dirty_refcounts(&self) -> bool {
        self.version >= 3 && self.incompatible_features & incompat::DIRTY != 0
    }

    /// Reject an image that may be read but must not be written.
    ///
    /// `check_supported` puts DIRTY and CORRUPT in the tolerated set,
    /// and for reading that is right: both are advisory, and refusing
    /// them would take away the one thing somebody with a damaged image
    /// wants. Neither is advisory for a writer, and the reference tool
    /// treats them as hard gates on the read/write open — measured:
    ///
    /// ```text
    /// CORRUPT set:  qemu-img info      -> reports the image, "corrupt: true"
    ///               qemu-img snapshot  -> "Image is corrupt; cannot be
    ///                                      opened read/write"
    ///
    /// DIRTY set:    qemu-img snapshot  -> succeeds, and the bit is 0x0
    ///                                      afterwards: it repaired the
    ///                                      refcounts first
    /// ```
    ///
    /// CORRUPT says the metadata is already inconsistent, and writing
    /// turns a recoverable inconsistency into a different one. DIRTY is
    /// the sharper of the two here, because this crate does not repair:
    /// `allocate_cluster` finds a free cluster by trusting exactly the
    /// blocks the bit says are stale, so a cluster that is really in use
    /// can read as refcount 0, and the write path then hands it out,
    /// fills it and points an L2 entry at it. The data destroyed
    /// belongs to some other part of the guest's disk and nothing
    /// reports an error.
    ///
    /// Repairing the refcounts, which is what the reference tool does,
    /// would be the better answer and is a larger piece of work.
    /// Refusing is what this crate can honestly do today.
    pub fn check_writable(&self) -> Result<()> {
        if self.is_corrupt() {
            return Err(Error::Unsupported(
                "image is flagged corrupt; it can be read but not written",
            ));
        }
        if self.has_dirty_refcounts() {
            return Err(Error::Unsupported(
                "image is flagged dirty; its refcounts need repair before it can be written",
            ));
        }
        Ok(())
    }
}

fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn read_u64(b: &[u8], off: usize) -> u64 {
    u64::from_be_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

#[cfg(test)]
mod tests {
    /// The offset table, checked against the specification's own table.
    ///
    /// A deliberate second copy: the literals here come from the QCOW2
    /// specification, reproduced in this module's doc comment. Every
    /// other reader in the crate now goes through the constants, so
    /// this is the only place that can disagree with them.
    ///
    /// If this test and the constants diverge, re-read the spec before
    /// touching either — this is the half with independent provenance.
    #[test]
    fn offsets_match_the_published_specification() {
        use super::offsets as at;
        assert_eq!(at::MAGIC, 0);
        assert_eq!(at::VERSION, 4);
        assert_eq!(at::BACKING_FILE_OFFSET, 8);
        assert_eq!(at::BACKING_FILE_SIZE, 16);
        assert_eq!(at::CLUSTER_BITS, 20);
        assert_eq!(at::SIZE, 24);
        assert_eq!(at::CRYPT_METHOD, 32);
        assert_eq!(at::L1_SIZE, 36);
        assert_eq!(offsets::L1_TABLE_OFFSET, 40);
        assert_eq!(offsets::REFCOUNT_TABLE_OFFSET, 48);
        assert_eq!(at::REFCOUNT_TABLE_CLUSTERS, 56);
        assert_eq!(at::NB_SNAPSHOTS, 60);
        assert_eq!(offsets::SNAPSHOTS_OFFSET, 64);
        assert_eq!(at::INCOMPATIBLE_FEATURES, 72);
        assert_eq!(at::COMPATIBLE_FEATURES, 80);
        assert_eq!(at::AUTOCLEAR_FEATURES, 88);
        assert_eq!(at::REFCOUNT_ORDER, 96);
        assert_eq!(at::HEADER_LENGTH, 100);
        assert_eq!(at::V3_BASE_HEADER_LENGTH, 104);
        assert_eq!(at::COMPRESSION_TYPE, 104);
        assert_eq!(at::v2_implicit::REFCOUNT_ORDER, 4);
        assert_eq!(at::v2_implicit::HEADER_LENGTH, 72);
    }

    /// No header field overlaps the next.
    ///
    /// The table above pins each offset on its own; this checks they
    /// still describe one layout. A field widened without its
    /// neighbour moving satisfies every assertion above and fails here.
    #[test]
    fn no_header_field_overlaps_its_neighbour() {
        use super::offsets as at;
        let fields = [
            (at::MAGIC, 4),
            (at::VERSION, 4),
            (at::BACKING_FILE_OFFSET, 8),
            (at::BACKING_FILE_SIZE, 4),
            (at::CLUSTER_BITS, 4),
            (at::SIZE, 8),
            (at::CRYPT_METHOD, 4),
            (at::L1_SIZE, 4),
            (offsets::L1_TABLE_OFFSET, 8),
            (offsets::REFCOUNT_TABLE_OFFSET, 8),
            (at::REFCOUNT_TABLE_CLUSTERS, 4),
            (at::NB_SNAPSHOTS, 4),
            (offsets::SNAPSHOTS_OFFSET, 8),
            (at::INCOMPATIBLE_FEATURES, 8),
            (at::COMPATIBLE_FEATURES, 8),
            (at::AUTOCLEAR_FEATURES, 8),
            (at::REFCOUNT_ORDER, 4),
            (at::HEADER_LENGTH, 4),
            (at::COMPRESSION_TYPE, 1),
        ];
        let mut reached = 0usize;
        for (start, width) in fields {
            assert!(
                start >= reached,
                "field at {start} overlaps the one ending at {reached}"
            );
            reached = start + width;
        }
        // The v3 base header ends exactly where the additional-fields
        // region begins.
        assert_eq!(at::V3_BASE_HEADER_LENGTH as usize, at::COMPRESSION_TYPE);
    }

    use super::*;

    #[test]
    fn rejects_bad_magic() {
        let bytes = [0u8; 104];
        assert!(matches!(Header::parse(&bytes), Err(Error::NotQcow2)));
    }

    #[test]
    fn rejects_bad_version() {
        let mut bytes = [0u8; 104];
        bytes[0..4].copy_from_slice(&QCOW2_MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&7u32.to_be_bytes());
        assert!(matches!(
            Header::parse(&bytes),
            Err(Error::UnsupportedVersion(7))
        ));
    }

    /// Build a parseable v3 header (112 bytes so the compression_type
    /// byte at offset 104 is read). cluster_bits = 16 (64 KiB clusters),
    /// virtual_size = 1 MiB, l1_size = 1 (the minimum that addresses it).
    ///
    /// The table offsets are left at zero, which is what the reference
    /// validator accepts: patching either to 0 in a real image and
    /// asking `qemu-img info` reports the image normally, so zero is
    /// not a value this parser gets to refuse. The tests that care
    /// about those fields set them.
    fn valid_v3_header() -> Vec<u8> {
        let mut b = vec![0u8; 112];
        b[0..4].copy_from_slice(&QCOW2_MAGIC.to_be_bytes());
        b[4..8].copy_from_slice(&3u32.to_be_bytes()); // version 3
        b[20..24].copy_from_slice(&16u32.to_be_bytes()); // cluster_bits
        b[24..32].copy_from_slice(&(1u64 << 20).to_be_bytes()); // virtual_size = 1 MiB
        b[36..40].copy_from_slice(&1u32.to_be_bytes()); // l1_size
        b[96..100].copy_from_slice(&4u32.to_be_bytes()); // refcount_order
        b[100..104].copy_from_slice(&112u32.to_be_bytes()); // header_length
        b
    }

    /// A table's offset names whole clusters, or the header is refused.
    ///
    /// Each of the three is moved half a cluster past a legal value, so
    /// only its alignment is wrong — the offset is otherwise a place a
    /// table could be. The reference validator refuses the first two by
    /// name ("Active L1 table offset invalid", "Reference count table
    /// offset invalid"), and before this we opened both.
    #[test]
    fn a_table_offset_that_is_not_cluster_aligned_is_refused() {
        for (field, off, expect) in [
            (
                "l1",
                offsets::L1_TABLE_OFFSET,
                "l1_table_offset is not cluster-aligned",
            ),
            (
                "refcount",
                offsets::REFCOUNT_TABLE_OFFSET,
                "refcount_table_offset is not cluster-aligned",
            ),
            (
                "snapshots",
                offsets::SNAPSHOTS_OFFSET,
                "snapshots_offset is not cluster-aligned",
            ),
        ] {
            let mut b = valid_v3_header();
            set_u64(&mut b, off, 0x0003_0200);
            match Header::parse(&b) {
                Err(Error::Corrupt(msg)) => assert_eq!(msg, expect, "{field}"),
                other => panic!("{field}: expected Corrupt({expect:?}), got {other:?}"),
            }
        }
    }

    /// The last aligned value each field must still accept.
    ///
    /// Without this, a check written as "must be a multiple of the
    /// cluster size and greater than one cluster", or any other stricter
    /// reading, would pass the test above while refusing ordinary
    /// images.
    #[test]
    fn cluster_aligned_table_offsets_are_accepted() {
        for off in [
            offsets::L1_TABLE_OFFSET,
            offsets::REFCOUNT_TABLE_OFFSET,
            offsets::SNAPSHOTS_OFFSET,
        ] {
            let mut b = valid_v3_header();
            set_u64(&mut b, off, 1 << 16); // exactly one cluster in
            Header::parse(&b).expect("a cluster-aligned offset is legal");
        }
        // Zero is aligned, and the reference validator accepts it in
        // all three fields — measured by patching each to 0 in a real
        // image and asking `qemu-img info`, which reports the image
        // normally. So this parser does not get to refuse it either;
        // the fields it names are checked where they are used.
        Header::parse(&valid_v3_header()).expect("zero offsets are what the validator accepts");
    }

    fn set_u32(b: &mut [u8], off: usize, v: u32) {
        b[off..off + 4].copy_from_slice(&v.to_be_bytes());
    }
    fn set_u64(b: &mut [u8], off: usize, v: u64) {
        b[off..off + 8].copy_from_slice(&v.to_be_bytes());
    }

    #[test]
    fn parses_valid_v3_header_fields() {
        let h = Header::parse(&valid_v3_header()).unwrap();
        assert_eq!(h.version, 3);
        assert_eq!(h.cluster_bits, 16);
        assert_eq!(h.cluster_size, 1 << 16);
        assert_eq!(h.virtual_size, 1 << 20);
        assert_eq!(h.refcount_order, 4);
        assert_eq!(h.l2_entries(), (1 << 16) / 8);
        h.check_supported().unwrap();
    }

    #[test]
    fn v2_header_uses_implicit_defaults() {
        let mut b = valid_v3_header();
        set_u32(&mut b, 4, 2); // version 2
        let h = Header::parse(&b).unwrap();
        // v2 has no feature fields; spec defaults apply.
        assert_eq!(h.incompatible_features, 0);
        assert_eq!(h.refcount_order, 4);
        assert_eq!(h.header_length, 72);
        assert_eq!(h.compression_type, 0);
        h.check_supported().unwrap();
    }

    #[test]
    fn rejects_cluster_bits_outside_9_to_21() {
        for bad in [0u32, 8, 22, 31] {
            let mut b = valid_v3_header();
            set_u32(&mut b, 20, bad);
            match Header::parse(&b) {
                Err(Error::InvalidClusterBits(v)) => assert_eq!(v, bad),
                other => panic!("cluster_bits {bad}: expected InvalidClusterBits, got {other:?}"),
            }
        }
        // Boundaries 9 and 21 are accepted. Use a small virtual_size so
        // l1_size = 1 addresses it even at cluster_bits = 9 (where one L1
        // entry only maps 32 KiB).
        for ok in [9u32, 21] {
            let mut b = valid_v3_header();
            set_u32(&mut b, 20, ok);
            set_u64(&mut b, 24, 16 * 1024); // 16 KiB virtual disk
            Header::parse(&b).unwrap_or_else(|e| panic!("cluster_bits {ok} should parse: {e:?}"));
        }
    }

    #[test]
    fn rejects_l1_size_too_small_for_virtual_size() {
        let mut b = valid_v3_header();
        // Keep l1_size = 1 but blow up virtual_size so one L1 entry can't
        // cover it. At cluster_bits=16 one L1 entry maps 512 MiB, so ask
        // for 2 GiB.
        set_u64(&mut b, 24, 2u64 << 30);
        match Header::parse(&b) {
            Err(Error::Corrupt(_)) => {}
            other => panic!("expected Corrupt(l1_size too small), got {other:?}"),
        }
    }

    #[test]
    fn rejects_v3_header_shorter_than_104_bytes() {
        let mut b = valid_v3_header();
        b.truncate(96);
        match Header::parse(&b) {
            Err(Error::Corrupt(_)) => {}
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn compression_type_byte_only_read_when_header_is_long_enough() {
        // header_length = 104 → the compression_type byte is omitted and
        // defaults to zlib (0) even if byte 104 happens to be non-zero.
        let mut b = valid_v3_header();
        set_u32(&mut b, 100, 104); // header_length = 104
        b[104] = 1; // would be zstd if it were read
        let h = Header::parse(&b).unwrap();
        assert_eq!(
            h.compression_type, 0,
            "byte past header_length must be ignored"
        );
    }

    #[test]
    fn check_supported_rejects_encryption() {
        let mut b = valid_v3_header();
        set_u32(&mut b, 32, 1); // crypt_method = AES
        let h = Header::parse(&b).unwrap();
        match h.check_supported() {
            Err(Error::Unsupported(m)) => assert!(m.contains("encryption")),
            other => panic!("expected Unsupported(encryption), got {other:?}"),
        }
    }

    #[test]
    fn check_supported_rejects_external_data_file() {
        let mut b = valid_v3_header();
        set_u64(&mut b, 72, incompat::DATA_FILE);
        let h = Header::parse(&b).unwrap();
        match h.check_supported() {
            Err(Error::Unsupported(m)) => assert_eq!(m, "external data file"),
            other => panic!("expected Unsupported(external data file), got {other:?}"),
        }
    }

    #[test]
    fn check_supported_rejects_extended_l2() {
        let mut b = valid_v3_header();
        set_u64(&mut b, 72, incompat::EXTENDED_L2);
        let h = Header::parse(&b).unwrap();
        match h.check_supported() {
            Err(Error::Unsupported(m)) => assert_eq!(m, "extended L2 entries"),
            other => panic!("expected Unsupported(extended L2 entries), got {other:?}"),
        }
    }

    #[test]
    fn check_supported_rejects_unknown_incompatible_bit() {
        let mut b = valid_v3_header();
        set_u64(&mut b, 72, 1 << 20); // a bit we don't recognise
        let h = Header::parse(&b).unwrap();
        match h.check_supported() {
            Err(Error::Unsupported(m)) => assert_eq!(m, "unknown incompatible_features bit"),
            other => panic!("expected Unsupported(unknown bit), got {other:?}"),
        }
    }

    #[test]
    fn check_supported_rejects_unknown_compression_type() {
        let mut b = valid_v3_header();
        // Declare COMPRESSION_TYPE and an out-of-range compression byte.
        set_u64(&mut b, 72, incompat::COMPRESSION_TYPE);
        b[104] = 2; // neither zlib(0) nor zstd(1)
        let h = Header::parse(&b).unwrap();
        match h.check_supported() {
            Err(Error::Unsupported(m)) => assert_eq!(m, "unknown compression_type"),
            other => panic!("expected Unsupported(unknown compression_type), got {other:?}"),
        }
    }

    #[test]
    fn check_supported_accepts_zstd_with_compression_type_bit() {
        let mut b = valid_v3_header();
        set_u64(&mut b, 72, incompat::COMPRESSION_TYPE);
        b[104] = 1; // zstd
        let h = Header::parse(&b).unwrap();
        assert_eq!(h.compression_type, 1);
        h.check_supported().expect("zstd is supported");
    }

    #[test]
    fn check_supported_rejects_bad_compression_type_even_without_the_flag() {
        // check_supported gates `compression_type > 1` unconditionally for
        // v3, independent of whether the COMPRESSION_TYPE incompatible bit
        // is declared. Exercise the flag-absent path: an out-of-range byte
        // at offset 104 must still be refused even though
        // incompatible_features is zero.
        let mut b = valid_v3_header();
        // incompatible_features left at 0 (no COMPRESSION_TYPE flag).
        b[104] = 2; // neither zlib(0) nor zstd(1)
        let h = Header::parse(&b).unwrap();
        match h.check_supported() {
            Err(Error::Unsupported(m)) => assert_eq!(m, "unknown compression_type"),
            other => panic!("expected Unsupported(unknown compression_type), got {other:?}"),
        }
    }

    /// The backing-file name lives in the header area, so its offset is
    /// inside the first cluster.
    ///
    /// The reference tool caps it there and refuses anything past it,
    /// because the region ahead of the L1 table is the only place the
    /// name can be. Past it is a data cluster — bytes the guest wrote.
    #[test]
    fn a_backing_file_offset_past_the_first_cluster_is_refused() {
        let mut b = valid_v3_header();
        // cluster_bits is 16 in the fixture, so one cluster is 65536.
        set_u64(&mut b, offsets::BACKING_FILE_OFFSET, 65537);
        match Header::parse(&b) {
            Err(Error::Corrupt(m)) => assert!(
                m.contains("first cluster"),
                "the refusal must name the bound, got {m:?}"
            ),
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    /// The last offset that must still be accepted, and zero.
    ///
    /// An offset of exactly one cluster is the boundary; without this a
    /// bound written as `>=` would refuse it, and zero is how an image
    /// says it has no backing file at all.
    #[test]
    fn backing_file_offsets_inside_the_first_cluster_are_accepted() {
        for off in [0u64, 1, 512, 65536] {
            let mut b = valid_v3_header();
            set_u64(&mut b, offsets::BACKING_FILE_OFFSET, off);
            Header::parse(&b)
                .unwrap_or_else(|e| panic!("offset {off} is inside the first cluster: {e:?}"));
        }
    }

    /// The NAME must END inside the first cluster too, not only start
    /// there.
    ///
    /// Bounding the offset alone let `offset + size` run into cluster 1,
    /// which can be a guest data cluster: the guest then supplies the
    /// parent's path. The reference refuses with "Backing file name too
    /// long" when `size > cluster_size - offset`.
    #[test]
    fn a_backing_file_name_that_runs_past_the_first_cluster_is_refused() {
        // cluster_bits is 16 in the fixture, so one cluster is 65536.
        for (off, size) in [(65536u64, 1u32), (65526, 11), (0, 65537), (1, 65536)] {
            let mut b = valid_v3_header();
            set_u64(&mut b, offsets::BACKING_FILE_OFFSET, off);
            set_u32(&mut b, offsets::BACKING_FILE_SIZE, size);
            match Header::parse(&b) {
                Err(Error::Corrupt(m)) => assert!(
                    m.contains("first cluster"),
                    "offset {off} size {size}: the refusal must name the bound, got {m:?}"
                ),
                other => panic!(
                    "offset {off} size {size} ends past the first cluster; expected Corrupt, \
                     got {other:?}"
                ),
            }
        }
    }

    /// The other side of that bound: a name ending exactly at the end of
    /// the first cluster is accepted, and a zero size at offset 65536 is
    /// how a name-less offset still parses.
    #[test]
    fn a_backing_file_name_ending_inside_the_first_cluster_is_accepted() {
        for (off, size) in [(65526u64, 10u32), (65536, 0), (512, 64), (0, 1024)] {
            let mut b = valid_v3_header();
            set_u64(&mut b, offsets::BACKING_FILE_OFFSET, off);
            set_u32(&mut b, offsets::BACKING_FILE_SIZE, size);
            Header::parse(&b).unwrap_or_else(|e| {
                panic!("offset {off} size {size} ends inside the first cluster: {e:?}")
            });
        }
    }

    #[test]
    fn check_supported_tolerates_dirty_and_corrupt_advisory_bits() {
        // DIRTY and CORRUPT are recognised (and tolerated) incompatible
        // bits — they must not trip the unknown-bit rejection. This is
        // the READ gate; the write gate is `check_writable`, below, and
        // the pair of tests is the whole statement of the decision.
        let mut b = valid_v3_header();
        set_u64(&mut b, 72, incompat::DIRTY | incompat::CORRUPT);
        let h = Header::parse(&b).unwrap();
        h.check_supported()
            .expect("dirty/corrupt are tolerated for reading");
    }

    /// The same two bits are not advisory to a writer.
    ///
    /// Each is set on its own, so a check that only looked at one of
    /// them fails here rather than passing on the other's account.
    #[test]
    fn check_writable_refuses_the_dirty_and_corrupt_bits_separately() {
        for (bit, name, expect) in [
            (incompat::CORRUPT, "CORRUPT", "flagged corrupt"),
            (incompat::DIRTY, "DIRTY", "flagged dirty"),
        ] {
            let mut b = valid_v3_header();
            set_u64(&mut b, 72, bit);
            let h = Header::parse(&b).unwrap();
            h.check_supported()
                .unwrap_or_else(|e| panic!("{name} must still be readable: {e:?}"));
            match h.check_writable() {
                Err(Error::Unsupported(m)) => assert!(
                    m.contains(expect),
                    "{name}: refusal must name the flag, got {m:?}"
                ),
                other => panic!("{name}: expected Unsupported, got {other:?}"),
            }
        }
    }

    /// An image with neither bit is writable, and a v2 image — which has
    /// no `incompatible_features` field at all — is not caught by the
    /// bits happening to be set in bytes it does not use.
    #[test]
    fn check_writable_accepts_an_image_with_neither_bit() {
        let h = Header::parse(&valid_v3_header()).unwrap();
        h.check_writable().expect("an unflagged image is writable");

        let mut b = valid_v3_header();
        set_u32(&mut b, 4, 2); // version 2
        set_u64(&mut b, 72, incompat::DIRTY | incompat::CORRUPT);
        let h = Header::parse(&b).unwrap();
        h.check_writable()
            .expect("v2 has no incompatible_features; those bytes mean nothing");
    }
}
