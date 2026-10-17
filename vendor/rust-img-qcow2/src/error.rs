//! Error type. Kept narrow on purpose — one variant per failure shape so
//! callers can match without parsing strings.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    /// Underlying I/O failed (read, seek, open).
    Io(io::Error),
    /// The file is not a QCOW2 image (bad magic).
    NotQcow2,
    /// Header version is outside the supported range (2 or 3).
    UnsupportedVersion(u32),
    /// `cluster_bits` is outside the spec's [9, 21] range.
    InvalidClusterBits(u32),
    /// Header field combination is internally inconsistent.
    Corrupt(&'static str),
    /// A feature is set that this reader does not yet implement
    /// (compression, encryption, backing chain, external data file, etc.).
    Unsupported(&'static str),
    /// Read past the end of the virtual disk.
    OutOfBounds { offset: u64, len: u64, size: u64 },
    /// Deflate decoding failed for a compressed cluster.
    Decompress(String),
    /// Backing-file path stored in the header is not valid UTF-8.
    BadBackingPath,
    /// Backing chain exceeded the configured recursion depth — likely a cycle.
    BackingTooDeep,
    /// `write_at` called on an image opened read-only.
    ReadOnly,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::NotQcow2 => write!(f, "not a QCOW2 image (bad magic)"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported qcow2 version: {v}"),
            Error::InvalidClusterBits(b) => write!(f, "invalid cluster_bits: {b} (must be 9..=21)"),
            Error::Corrupt(s) => write!(f, "corrupt qcow2: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported qcow2 feature: {s}"),
            Error::OutOfBounds { offset, len, size } => {
                write!(
                    f,
                    "read [{offset}, {offset}+{len}) past virtual size {size}"
                )
            }
            Error::Decompress(s) => write!(f, "decompress: {s}"),
            Error::BadBackingPath => write!(f, "backing-file path is not valid UTF-8"),
            Error::BackingTooDeep => write!(f, "backing chain too deep (cycle?)"),
            Error::ReadOnly => write!(f, "image was opened read-only"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
