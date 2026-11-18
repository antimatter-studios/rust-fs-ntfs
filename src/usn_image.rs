//! Read-only change-journal inspection of an NTFS image (#609).
//!
//! A volume's change journal is the file `$Extend\$UsnJrnl` with two named
//! data streams: `$Max`, 32 bytes holding `MaximumSize`, `AllocationDelta`,
//! `UsnJournalID` and `LowestValidUsn`, and `$J`, the records themselves.
//! A record's USN is its byte offset in `$J`, so `$J`'s data size is the
//! journal's `NextUsn`. Windows deallocates the front of `$J` as the journal
//! grows past its maximum size, so the first allocated cluster is where the
//! readable records begin: `FirstUsn`.
//!
//! Nothing here writes, and nothing here needs Windows: the journal is read
//! through this crate's own `$MFT`, attribute and run-list parsing. The
//! journal's state on disk is what is reported; a volume Windows has
//! mounted since the image was taken may hold more.
//!
//! [`journal_state_io`] says whether a journal is there and what its bounds
//! are. [`read_records_io`] reads a bounded batch of `USN_RECORD_V2`
//! records from a saved cursor and returns the cursor of the first record
//! it did not read. A cursor the journal can no longer answer -- another
//! journal's, one past `NextUsn`, one whose records are gone -- is refused
//! with the reason [`admit_cursor`] gives, never answered with an empty
//! batch.

use crate::attr_io::AttrType;
use crate::block_io::BlockIo;
use crate::error::{Error, Kind};
use crate::read;
use crate::usn::{decode_usn_record_v2, UsnDecodeError, UsnRecordV2};
use crate::usn_cursor::{admit_cursor, CursorAdmission, UsnCursor, UsnJournalData};
use std::fmt;

const JOURNAL_PATH: &str = "/$Extend/$UsnJrnl";
/// `$Max` is `MaximumSize`, `AllocationDelta`, `UsnJournalID` and
/// `LowestValidUsn`, eight bytes each.
pub const MAX_STREAM_LEN: usize = 32;
/// `$VOLUME_INFORMATION`'s flag for a journal Windows is deleting; its next
/// mount finishes the deletion.
const VOLUME_DELETE_USN_UNDERWAY: u16 = 0x0010;
/// Records are 8-byte aligned; the space between them is zero.
const RECORD_ALIGNMENT: usize = 8;

/// What an image's change journal is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalState {
    /// The volume has no `$Extend\$UsnJrnl`.
    Absent,
    /// Windows was deleting the journal (`$Volume` flag `0x0010`); its next
    /// mount completes that, and no record in it is to be relied on.
    Deleting,
    /// An active journal, with the bounds its streams record.
    Active(ImageJournal),
}

/// The bounds of an image's change journal, as its `$Max` and `$J` streams
/// record them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageJournal {
    /// MFT record number of `$Extend\$UsnJrnl`.
    pub record: u64,
    /// `UsnJournalID`, from `$Max`.
    pub journal_id: u64,
    /// `FirstUsn`: the first allocated byte of `$J`, and never below
    /// `LowestValidUsn` or above `NextUsn`.
    pub first_usn: i64,
    /// `NextUsn`: the data size of `$J`.
    pub next_usn: i64,
    /// `LowestValidUsn`, from `$Max`.
    pub lowest_valid_usn: i64,
    /// `MaximumSize`, from `$Max`.
    pub maximum_size: u64,
    /// `AllocationDelta`, from `$Max`.
    pub allocation_delta: u64,
}

impl ImageJournal {
    /// These bounds as `USN_JOURNAL_DATA_V0`, for [`admit_cursor`].
    ///
    /// The disk records no `MaxUsn`: Windows works one out from the volume
    /// when it mounts it. `max_usn` is therefore `i64::MAX`, the largest
    /// USN a record can carry, and bounds nothing the other fields do not.
    pub fn journal_data(&self) -> UsnJournalData {
        UsnJournalData {
            journal_id: self.journal_id,
            first_usn: self.first_usn,
            next_usn: self.next_usn,
            lowest_valid_usn: self.lowest_valid_usn,
            max_usn: i64::MAX,
            maximum_size: self.maximum_size,
            allocation_delta: self.allocation_delta,
        }
    }

    /// The cursor that reads every record still in the journal.
    pub fn first_cursor(&self) -> UsnCursor {
        UsnCursor {
            journal_id: self.journal_id,
            next_usn: self.first_usn,
        }
    }
}

/// Reads the change journal's state from `io`, which is never written.
pub fn journal_state_io<T: BlockIo + ?Sized>(io: &mut T) -> Result<JournalState, Error> {
    let record = match read::resolve_path(io, JOURNAL_PATH) {
        Ok(record) => record,
        Err(e) if e.kind() == Kind::NotFound => return Ok(JournalState::Absent),
        Err(e) => return Err(e.context("look up $Extend\\$UsnJrnl")),
    };
    if read::read_volume_info(io)?.flags & VOLUME_DELETE_USN_UNDERWAY != 0 {
        return Ok(JournalState::Deleting);
    }
    let max = read::read_attribute_value(io, record, AttrType::Data, Some("$Max"))
        .map_err(|e| e.context("read $UsnJrnl:$Max"))?;
    if max.len() < MAX_STREAM_LEN {
        return Err(Error::io(format!(
            "$UsnJrnl:$Max is {} bytes, short of the {MAX_STREAM_LEN} it holds",
            max.len()
        )));
    }
    let q = |i: usize| u64::from_le_bytes(max[i * 8..i * 8 + 8].try_into().unwrap());
    let (maximum_size, allocation_delta, journal_id, lowest_valid) = (q(0), q(1), q(2), q(3));
    let span = read::attribute_allocation_io(io, record, AttrType::Data, Some("$J"))?
        .ok_or_else(|| Error::not_found("$UsnJrnl has no $J stream"))?;
    let usn = |v: u64, what: &str| {
        i64::try_from(v).map_err(|_| Error::io(format!("$UsnJrnl {what} {v:#x} is not a USN")))
    };
    let next_usn = usn(span.data_size, "$J size")?;
    let lowest_valid_usn = usn(lowest_valid, "LowestValidUsn")?;
    let first_allocated = match span.first_allocated {
        Some(at) => usn(at, "first allocated byte of $J")?,
        None => next_usn,
    };
    Ok(JournalState::Active(ImageJournal {
        record,
        journal_id,
        first_usn: first_allocated.max(lowest_valid_usn).min(next_usn),
        next_usn,
        lowest_valid_usn,
        maximum_size,
        allocation_delta,
    }))
}

/// One bounded read of `$J`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBatch {
    pub records: Vec<UsnRecordV2>,
    /// The cursor of the first record not read: where the next batch
    /// starts.
    pub next: UsnCursor,
    /// `next` is the journal's `NextUsn`: every record was read.
    pub finished: bool,
}

/// Why a batch could not be read.
#[derive(Debug)]
pub enum JournalReadError {
    /// The journal cannot answer the cursor: what [`admit_cursor`] said.
    NotAdmitted(CursorAdmission),
    /// `max_bytes` is too small to hold the record at `usn`.
    BoundTooSmall {
        usn: i64,
        record_length: u32,
        max_bytes: usize,
    },
    /// The record at `usn` could not be decoded.
    Record { usn: i64, error: UsnDecodeError },
    /// The record at byte `usn` of `$J` carries another USN.
    Misplaced { usn: i64, recorded: i64 },
    /// The record at `usn` runs past the journal's `NextUsn`.
    PastNextUsn {
        usn: i64,
        record_length: u32,
        next_usn: i64,
    },
    /// `$J` could not be read.
    Io(Error),
}

impl fmt::Display for JournalReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAdmitted(admission) => {
                write!(f, "the journal cannot answer this cursor: {admission:?}")
            }
            Self::BoundTooSmall {
                usn,
                record_length,
                max_bytes,
            } => write!(
                f,
                "the record at USN {usn} is {record_length} bytes, over the {max_bytes}-byte bound"
            ),
            Self::Record { usn, error } => write!(f, "the record at USN {usn}: {error}"),
            Self::Misplaced { usn, recorded } => {
                write!(f, "the record at USN {usn} records USN {recorded}")
            }
            Self::PastNextUsn {
                usn,
                record_length,
                next_usn,
            } => write!(
                f,
                "the {record_length}-byte record at USN {usn} runs past NextUsn {next_usn}"
            ),
            Self::Io(e) => write!(f, "read $UsnJrnl:$J: {e}"),
        }
    }
}

impl std::error::Error for JournalReadError {}

/// Reads the records from `cursor` on, examining at most `max_bytes` of
/// `$J`, from `io`, which is never written.
///
/// The zero padding between records counts against the bound. A batch
/// stops before a record it cannot read whole, and an empty batch is
/// returned only when the cursor is already at `NextUsn`.
pub fn read_records_io<T: BlockIo + ?Sized>(
    io: &mut T,
    journal: &ImageJournal,
    cursor: UsnCursor,
    max_bytes: usize,
) -> Result<RecordBatch, JournalReadError> {
    let from = match admit_cursor(&cursor, &journal.journal_data()) {
        CursorAdmission::Admitted { from, .. } => from,
        CursorAdmission::UpToDate { next_usn } => {
            return Ok(RecordBatch {
                records: Vec::new(),
                next: UsnCursor {
                    journal_id: journal.journal_id,
                    next_usn,
                },
                finished: true,
            })
        }
        refused => return Err(JournalReadError::NotAdmitted(refused)),
    };
    let window = (journal.next_usn - from).min(max_bytes as i64) as usize;
    let bytes = read::read_attribute_range(
        io,
        journal.record,
        AttrType::Data,
        Some("$J"),
        from as u64,
        window,
    )
    .map_err(JournalReadError::Io)?;
    let (records, read) = decode_window(&bytes, from, journal.next_usn, max_bytes)?;
    let next_usn = from + read as i64;
    Ok(RecordBatch {
        records,
        next: UsnCursor {
            journal_id: journal.journal_id,
            next_usn,
        },
        finished: next_usn == journal.next_usn,
    })
}

/// Decodes the records in `bytes`, the window of `$J` starting at USN
/// `from`. Returns them and the number of bytes consumed: up to the first
/// record that does not fit, or all of `bytes`.
fn decode_window(
    bytes: &[u8],
    from: i64,
    next_usn: i64,
    max_bytes: usize,
) -> Result<(Vec<UsnRecordV2>, usize), JournalReadError> {
    if bytes.is_empty() {
        return Err(JournalReadError::BoundTooSmall {
            usn: from,
            record_length: 0,
            max_bytes,
        });
    }
    let mut records = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let usn = from + pos as i64;
        let rest = &bytes[pos..];
        if rest.len() < RECORD_ALIGNMENT {
            // Not even one aligned slot left in the window: the next batch
            // reads it.
            if pos == 0 {
                return Err(JournalReadError::BoundTooSmall {
                    usn,
                    record_length: 0,
                    max_bytes,
                });
            }
            break;
        }
        let length = u32::from_le_bytes(rest[..4].try_into().unwrap());
        if length == 0 {
            // The zero space between records, which a record never starts
            // in: all of the slot is zero, or it is not padding.
            if rest[..RECORD_ALIGNMENT].iter().any(|&b| b != 0) {
                return Err(JournalReadError::Record {
                    usn,
                    error: UsnDecodeError::RecordLengthTooSmall { record_length: 0 },
                });
            }
            pos += RECORD_ALIGNMENT;
            continue;
        }
        if usn + i64::from(length) > next_usn {
            return Err(JournalReadError::PastNextUsn {
                usn,
                record_length: length,
                next_usn,
            });
        }
        if rest.len() < length as usize {
            if pos == 0 {
                return Err(JournalReadError::BoundTooSmall {
                    usn,
                    record_length: length,
                    max_bytes,
                });
            }
            break;
        }
        let record =
            decode_usn_record_v2(rest).map_err(|error| JournalReadError::Record { usn, error })?;
        if record.usn != usn {
            return Err(JournalReadError::Misplaced {
                usn,
                recorded: record.usn,
            });
        }
        pos += record.record_length as usize;
        records.push(record);
    }
    Ok((records, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `USN_RECORD_V2` naming `name`, recording `usn` as its own.
    fn record(name: &str, usn: i64) -> Vec<u8> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let len = (60 + name.len() * 2).next_multiple_of(8);
        let mut b = vec![0u8; len];
        b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        b[8..16].copy_from_slice(&0x0001_0000_0000_0029u64.to_le_bytes());
        b[24..32].copy_from_slice(&usn.to_le_bytes());
        b[56..58].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        b[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            b[60 + i * 2..62 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        b
    }

    /// `$J` from `from`: records at their own USNs, zero between them.
    fn journal(from: i64, parts: &[(i64, &str)], end: i64) -> Vec<u8> {
        let mut j = vec![0u8; (end - from) as usize];
        for &(usn, name) in parts {
            let r = record(name, usn);
            let at = (usn - from) as usize;
            j[at..at + r.len()].copy_from_slice(&r);
        }
        j
    }

    #[test]
    fn padding_between_records_is_skipped_and_counted() {
        // a.txt fills 72 bytes; the rest of the 128-byte gap is zero.
        let j = journal(4000, &[(4000, "a.txt"), (4128, "b.txt")], 4200);
        let (records, read) = decode_window(&j, 4000, 4200, j.len()).unwrap();
        let usns: Vec<i64> = records.iter().map(|r| r.usn).collect();
        assert_eq!(usns, [4000, 4128]);
        assert_eq!(read, j.len());
    }

    #[test]
    fn a_record_cut_by_the_window_is_left_for_the_next_batch() {
        let j = journal(0, &[(0, "a.txt"), (72, "b.txt")], 144);
        let (records, read) = decode_window(&j[..100], 0, 144, 100).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(read, 72, "the cursor stops where b.txt starts");
        let (records, read) = decode_window(&j[..76], 0, 144, 76).unwrap();
        assert_eq!((records.len(), read), (1, 72), "a slot under 8 bytes");
    }

    #[test]
    fn a_window_that_cannot_hold_its_first_record_is_refused() {
        let j = journal(0, &[(0, "a.txt")], 72);
        for bound in [0, 4, 40] {
            match decode_window(&j[..bound], 0, 72, bound) {
                Err(JournalReadError::BoundTooSmall { usn: 0, .. }) => {}
                other => panic!("a {bound}-byte window: {other:?}"),
            }
        }
    }

    #[test]
    fn a_record_at_another_usn_is_misplaced() {
        let mut j = journal(0, &[(0, "a.txt")], 72);
        j[24..32].copy_from_slice(&8i64.to_le_bytes());
        match decode_window(&j, 0, 72, 72) {
            Err(JournalReadError::Misplaced {
                usn: 0,
                recorded: 8,
            }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_record_past_next_usn_is_refused() {
        let j = journal(0, &[(0, "a.txt")], 72);
        match decode_window(&j[..64], 0, 64, 4096) {
            Err(JournalReadError::PastNextUsn {
                usn: 0,
                record_length: 72,
                next_usn: 64,
            }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_zero_length_over_nonzero_bytes_is_not_padding() {
        let mut j = journal(0, &[(0, "a.txt")], 80);
        j[72 + 4] = 2;
        match decode_window(&j, 0, 80, 80) {
            Err(JournalReadError::Record { usn: 72, .. }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn another_record_version_is_refused_by_name() {
        let mut j = journal(0, &[(0, "a.txt")], 72);
        j[4..6].copy_from_slice(&3u16.to_le_bytes());
        match decode_window(&j, 0, 72, 72) {
            Err(JournalReadError::Record {
                usn: 0,
                error: UsnDecodeError::UnsupportedVersion { major: 3, minor: 0 },
            }) => {}
            other => panic!("{other:?}"),
        }
    }
}
