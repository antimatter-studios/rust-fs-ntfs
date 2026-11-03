//! Repair plans: every write a repair would make, worked out without
//! making any of them (#589, #709).
//!
//! A plan is made by running the repair itself, [`crate::fsck::fsck_io_with`],
//! over an overlay of the volume that records each write instead of making
//! it: reads see the writes recorded so far over the volume's own bytes,
//! so the repair decides exactly as it would on the volume. There is one
//! repair, and a plan cannot drift from it.
//!
//! A plan is a list of [`PlanUnit`]s in the order the repair makes them,
//! one per durable boundary: the writes the repair makes between two
//! flushes. A unit's writes may land in any order, so a unit is what is
//! applied, and flushed, as a whole. Each write carries the bytes the
//! volume holds there at its turn -- after the writes before it, including
//! any earlier write to the same bytes -- and the bytes it leaves.
//!
//! Nothing is written to the volume, and nothing is written into its
//! `$LogFile`: an external plan is not a native transaction.

use crate::block_io::BlockIo;
use crate::error::Error;
use crate::fsck::{fsck_io_with, FsckOptions, FsckReport};

/// One write a repair would make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWrite {
    /// Byte offset on the volume.
    pub offset: u64,
    /// What the volume holds there at this write's turn.
    pub before: Vec<u8>,
    /// What this write leaves there: as many bytes as `before`.
    pub after: Vec<u8>,
}

/// The writes a repair makes between two flushes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanUnit {
    pub writes: Vec<PlannedWrite>,
}

/// Every write a repair would make, and what it would report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairPlan {
    /// In the order the repair makes them; empty when it writes nothing.
    pub units: Vec<PlanUnit>,
    /// What the repair would report once every unit is applied.
    pub report: FsckReport,
}

/// Plan what [`fsck_io_with`] with `options` would write to `io`, writing
/// nothing. A repair the library refuses -- a log it cannot replay in full,
/// a hibernated volume, a device that cannot order its writes -- is
/// refused here in the same words, with no plan.
pub fn plan_fsck_io<T: BlockIo + ?Sized>(
    io: &mut T,
    options: FsckOptions,
) -> Result<RepairPlan, Error> {
    let mut planning = Planning {
        io,
        units: Vec::new(),
        open: PlanUnit::default(),
    };
    let report = fsck_io_with(&mut planning, options, None)?;
    planning.close_unit();
    Ok(RepairPlan {
        units: planning.units,
        report,
    })
}

/// The volume as the repair would leave it so far, recording each write
/// instead of making it.
struct Planning<'a, T: ?Sized> {
    io: &'a mut T,
    /// The units flushed so far, then the one still open: every write
    /// recorded, in order, laid over the volume's bytes on read.
    units: Vec<PlanUnit>,
    open: PlanUnit,
}

impl<T: ?Sized> Planning<'_, T> {
    fn close_unit(&mut self) {
        if !self.open.writes.is_empty() {
            self.units.push(std::mem::take(&mut self.open));
        }
    }
}

impl<T: BlockIo + ?Sized> BlockIo for Planning<'_, T> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.io.read_exact_at(offset, buf)?;
        let end = offset + buf.len() as u64;
        // In order, so a later write to the same bytes wins.
        let recorded = self.units.iter().chain([&self.open]);
        for w in recorded.flat_map(|unit| &unit.writes) {
            let (at, bytes) = (w.offset, &w.after);
            let (from, to) = (at.max(offset), (at + bytes.len() as u64).min(end));
            if from < to {
                buf[(from - offset) as usize..(to - offset) as usize]
                    .copy_from_slice(&bytes[(from - at) as usize..(to - at) as usize]);
            }
        }
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let mut before = vec![0u8; buf.len()];
        self.read_exact_at(offset, &mut before)?;
        self.open.writes.push(PlannedWrite {
            offset,
            before,
            after: buf.to_vec(),
        });
        Ok(())
    }

    fn size(&self) -> u64 {
        self.io.size()
    }

    fn sync(&mut self) -> Result<(), String> {
        self.close_unit();
        Ok(())
    }

    /// The repair refuses a device that cannot order its writes (#518);
    /// so does its plan, which would have to be applied to that device.
    fn can_sync(&self) -> bool {
        self.io.can_sync()
    }
}

/// A plan loaded back from its file, with the identity of the source it
/// was made from.
#[cfg(feature = "plan-file")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPlan {
    /// The source's size in bytes.
    pub source_size: u64,
    /// The volume serial number from the source's boot sector.
    pub volume_serial: u64,
    pub units: Vec<PlanUnit>,
}

/// Why a plan file was refused.
#[cfg(feature = "plan-file")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanFileError {
    /// The file ends before what it says it holds.
    Truncated { length: usize, expected: usize },
    /// The file holds bytes past what it says it holds.
    TrailingBytes { length: usize, expected: usize },
    /// Not a plan file.
    NotAPlan,
    /// A format version this reader does not know.
    UnsupportedVersion(u32),
    /// The checksum over the file does not match it.
    ChecksumMismatch,
    /// A unit's before-image is not the one its hash names.
    BeforeImageMismatch { unit: usize },
    /// The structure is inconsistent with itself.
    Malformed(&'static str),
}

#[cfg(feature = "plan-file")]
impl std::fmt::Display for PlanFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { length, expected } => {
                write!(f, "plan file truncated: {length} bytes of {expected}")
            }
            Self::TrailingBytes { length, expected } => {
                write!(f, "plan file has trailing bytes: {length} bytes, {expected} expected")
            }
            Self::NotAPlan => write!(f, "not a repair plan file"),
            Self::UnsupportedVersion(v) => write!(
                f,
                "plan file format version {v} is not supported (this reader knows version {PLAN_FILE_VERSION})"
            ),
            Self::ChecksumMismatch => write!(f, "plan file checksum does not match its contents"),
            Self::BeforeImageMismatch { unit } => {
                write!(f, "plan file unit {unit}: before-image does not match its hash")
            }
            Self::Malformed(what) => write!(f, "plan file malformed: {what}"),
        }
    }
}

#[cfg(feature = "plan-file")]
impl std::error::Error for PlanFileError {}

/// The plan file format this reader writes and reads.
#[cfg(feature = "plan-file")]
pub const PLAN_FILE_VERSION: u32 = 1;

/// The first bytes of every plan file.
#[cfg(feature = "plan-file")]
const PLAN_FILE_MAGIC: &[u8; 8] = b"NTFSPLAN";

/// Magic, version, unit count and file length, then the source's size and
/// volume serial number. The layout is docs/repair-plan-file.md.
#[cfg(feature = "plan-file")]
const PLAN_FILE_PREFIX: usize = 8 + 4 + 4 + 8 + 8 + 8;

#[cfg(feature = "plan-file")]
const SHA256_LEN: usize = 32;

/// Where the NTFS boot sector holds the volume serial number.
#[cfg(feature = "plan-file")]
const VOLUME_SERIAL_OFFSET: u64 = 0x48;

/// Save `plan`, made from `source`, as the bytes of a plan file bound to
/// `source`'s size and volume serial number. The file goes wherever the
/// caller puts it; nothing is written to `source`.
#[cfg(feature = "plan-file")]
pub fn save_plan<T: BlockIo + ?Sized>(
    plan: &RepairPlan,
    source: &mut T,
) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    let mut serial = [0u8; 8];
    source.read_exact_at(VOLUME_SERIAL_OFFSET, &mut serial)?;
    let units = u32::try_from(plan.units.len()).map_err(|_| "too many units for a plan file")?;
    let mut out = Vec::new();
    out.extend_from_slice(PLAN_FILE_MAGIC);
    out.extend_from_slice(&PLAN_FILE_VERSION.to_le_bytes());
    out.extend_from_slice(&units.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // the file length, below
    out.extend_from_slice(&source.size().to_le_bytes());
    out.extend_from_slice(&serial);
    for unit in &plan.units {
        let writes =
            u32::try_from(unit.writes.len()).map_err(|_| "too many writes in a plan unit")?;
        out.extend_from_slice(&writes.to_le_bytes());
        out.extend_from_slice(&before_image_sha256(unit));
        for w in &unit.writes {
            let length = u32::try_from(w.before.len()).map_err(|_| "a write too long")?;
            if w.after.len() != w.before.len() {
                return Err(format!(
                    "the write at {} has {} bytes before and {} after",
                    w.offset,
                    w.before.len(),
                    w.after.len()
                ));
            }
            out.extend_from_slice(&w.offset.to_le_bytes());
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&w.before);
            out.extend_from_slice(&w.after);
        }
    }
    let length = (out.len() + SHA256_LEN) as u64;
    out[16..24].copy_from_slice(&length.to_le_bytes());
    let sum = Sha256::digest(&out);
    out.extend_from_slice(&sum);
    Ok(out)
}

/// The SHA-256 of a unit's before-image: each write's `before`, in order.
#[cfg(feature = "plan-file")]
fn before_image_sha256(unit: &PlanUnit) -> [u8; SHA256_LEN] {
    use sha2::{Digest, Sha256};
    let mut sha = Sha256::new();
    for w in &unit.writes {
        sha.update(&w.before);
    }
    sha.finalize().into()
}

/// Load a plan file, refusing one that is truncated, longer than it says,
/// of a newer format version, or changed anywhere. A refused file yields
/// nothing to apply.
#[cfg(feature = "plan-file")]
pub fn load_plan(bytes: &[u8]) -> Result<SavedPlan, PlanFileError> {
    use sha2::{Digest, Sha256};
    let mut r = Reader { bytes, at: 0 };
    let truncated = |expected: usize| PlanFileError::Truncated {
        length: bytes.len(),
        expected,
    };
    if bytes.len() < PLAN_FILE_MAGIC.len() || &bytes[..8] != PLAN_FILE_MAGIC {
        return Err(if PLAN_FILE_MAGIC.starts_with(bytes) {
            truncated(PLAN_FILE_PREFIX + SHA256_LEN)
        } else {
            PlanFileError::NotAPlan
        });
    }
    r.at = 8;
    let version = r.u32().ok_or(truncated(PLAN_FILE_PREFIX + SHA256_LEN))?;
    if version != PLAN_FILE_VERSION {
        return Err(PlanFileError::UnsupportedVersion(version));
    }
    let units = r.u32().ok_or(truncated(PLAN_FILE_PREFIX + SHA256_LEN))?;
    let length = r.u64().ok_or(truncated(PLAN_FILE_PREFIX + SHA256_LEN))?;
    let expected = usize::try_from(length).map_err(|_| truncated(usize::MAX))?;
    if expected < PLAN_FILE_PREFIX + SHA256_LEN {
        return Err(PlanFileError::Malformed(
            "file length shorter than its header",
        ));
    }
    if bytes.len() < expected {
        return Err(truncated(expected));
    }
    if bytes.len() > expected {
        return Err(PlanFileError::TrailingBytes {
            length: bytes.len(),
            expected,
        });
    }
    let (body, sum) = bytes.split_at(expected - SHA256_LEN);
    if Sha256::digest(body).as_slice() != sum {
        return Err(PlanFileError::ChecksumMismatch);
    }
    let malformed = PlanFileError::Malformed("a unit runs past the end of the file");
    let mut r = Reader {
        bytes: body,
        at: 24,
    };
    let source_size = r.u64().ok_or(malformed.clone())?;
    let volume_serial = r.u64().ok_or(malformed.clone())?;
    let mut loaded = Vec::new();
    for unit in 0..units as usize {
        let writes = r.u32().ok_or(malformed.clone())?;
        let hash = r.take(SHA256_LEN).ok_or(malformed.clone())?;
        let mut planned = PlanUnit::default();
        for _ in 0..writes {
            let offset = r.u64().ok_or(malformed.clone())?;
            let length = r.u32().ok_or(malformed.clone())? as usize;
            let before = r.take(length).ok_or(malformed.clone())?.to_vec();
            let after = r.take(length).ok_or(malformed.clone())?.to_vec();
            planned.writes.push(PlannedWrite {
                offset,
                before,
                after,
            });
        }
        if before_image_sha256(&planned) != hash {
            return Err(PlanFileError::BeforeImageMismatch { unit });
        }
        loaded.push(planned);
    }
    if r.at != body.len() {
        return Err(PlanFileError::Malformed(
            "bytes between the last unit and the checksum",
        ));
    }
    Ok(SavedPlan {
        source_size,
        volume_serial,
        units: loaded,
    })
}

/// Reads little-endian fields in order; `None` past the end.
#[cfg(feature = "plan-file")]
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

#[cfg(feature = "plan-file")]
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let taken = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}
