//! Bounded, resumable, read-only inventory of the MFT.
//!
//! Every call examines at most `max_slots` MFT slots, counting the slots
//! that hold no record: a volume whose live records are sparse cannot turn
//! one nominally bounded batch into a scan of the whole table. The batch
//! returns the first slot it did not read as an explicit cursor, so a
//! caller can stop between batches and resume where it left off.
//!
//! Records are located through `$MFT`'s own mapping pairs
//! ([`mft_io::mft_record_offset_io`]) and read with the update-sequence
//! fixups applied ([`mft_io::apply_fixup_on_read`]), the same path every
//! other reader in this crate uses. Nothing here writes to the volume.
//!
//! A file reference is the 48-bit record number with the record's 16-bit
//! sequence number in the top 16 bits, the layout Windows reports through
//! `FSCTL_GET_NTFS_FILE_RECORD` and `fsutil file queryfileid`. A table
//! whose slot count does not fit in 48 bits is refused rather than
//! truncated.

use crate::attr_io::{find_attribute, AttrType};
use crate::block_io::BlockIo;
use crate::error::Error;
use crate::mft_io::{
    self, apply_fixup_on_read, record_flags, record_sequence, BootParams, MFT_FLAG_DIRECTORY,
    MFT_FLAG_IN_USE,
};

/// Largest record number a file reference can carry.
pub const MAX_RECORD_NUMBER: u64 = (1 << 48) - 1;

/// Offset of the base-record file reference in a file record header.
const OFF_BASE_REFERENCE: usize = 0x20;

/// Where the next batch starts: the first MFT slot not yet examined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryCursor {
    pub next_record: u64,
}

impl InventoryCursor {
    /// The cursor for a fresh inventory, at record zero.
    pub const START: InventoryCursor = InventoryCursor { next_record: 0 };
}

/// One in-use MFT record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryEntry {
    pub record_number: u64,
    /// The record header's sequence number, unmodified.
    pub sequence: u16,
    /// `record_number | (sequence << 48)`.
    pub file_reference: u64,
    pub is_directory: bool,
    /// The base record's file reference; zero for a base record.
    pub base_reference: u64,
}

/// What one bounded call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryBatch {
    pub entries: Vec<InventoryEntry>,
    /// Slots examined by this call, used or not. Never more than the bound.
    pub slots_visited: u64,
    /// Slots examined that hold no in-use record.
    pub unused_slots: u64,
    /// Slots whose record failed its fixup check, by record number.
    pub damaged_records: Vec<u64>,
    /// The first slot this call did not examine; `None` once the table is
    /// exhausted.
    pub next: Option<InventoryCursor>,
    /// Slots in the table, from `$MFT`'s `$DATA` length.
    pub total_slots: u64,
}

/// Compose a file reference, refusing a record number that does not fit.
pub fn file_reference(record_number: u64, sequence: u16) -> Result<u64, Error> {
    if record_number > MAX_RECORD_NUMBER {
        return Err(Error::invalid(format!(
            "record number {record_number} does not fit in a 48-bit file reference"
        )));
    }
    Ok(record_number | (u64::from(sequence) << 48))
}

/// How many slots `$MFT` holds, from its unnamed `$DATA` length.
pub fn mft_slot_count_io<T: BlockIo + ?Sized>(io: &mut T) -> Result<(BootParams, u64), Error> {
    let (params, record0) = mft_io::read_mft_record_io(io, 0)?;
    if params.file_record_size == 0 {
        return Err(Error::invalid("file record size is zero"));
    }
    let data = find_attribute(&record0, AttrType::Data, None)
        .ok_or(Error::invalid("$MFT has no unnamed $DATA"))?;
    let length = data
        .non_resident_value_length
        .ok_or(Error::invalid("$MFT's unnamed $DATA is resident"))?;
    if length % params.file_record_size != 0 {
        return Err(Error::invalid(format!(
            "$MFT length {length} is not a multiple of the {}-byte record size",
            params.file_record_size
        )));
    }
    let slots = length / params.file_record_size;
    if slots > MAX_RECORD_NUMBER + 1 {
        return Err(Error::invalid(format!(
            "$MFT holds {slots} slots, more than a file reference can name"
        )));
    }
    Ok((params, slots))
}

/// Examine at most `max_slots` MFT slots from `cursor` and return the
/// in-use records among them.
pub fn inventory_batch_io<T: BlockIo + ?Sized>(
    io: &mut T,
    cursor: InventoryCursor,
    max_slots: u64,
) -> Result<InventoryBatch, Error> {
    if max_slots == 0 {
        return Err(Error::invalid("inventory bound must be at least one slot"));
    }
    let (params, total_slots) = mft_slot_count_io(io)?;
    if cursor.next_record > total_slots {
        return Err(Error::invalid(format!(
            "inventory cursor {} is past the table's {total_slots} slots",
            cursor.next_record
        )));
    }
    let end = cursor
        .next_record
        .saturating_add(max_slots)
        .min(total_slots);
    let size = usize::try_from(params.file_record_size)
        .map_err(|_| Error::invalid("file record size does not fit in memory"))?;
    let mut batch = InventoryBatch {
        entries: Vec::new(),
        slots_visited: 0,
        unused_slots: 0,
        damaged_records: Vec::new(),
        next: None,
        total_slots,
    };
    let mut buf = vec![0u8; size];
    for record_number in cursor.next_record..end {
        batch.slots_visited += 1;
        let offset = mft_io::mft_record_offset_io(io, &params, record_number)?;
        io.read_exact_at(offset, &mut buf)
            .map_err(|e| format!("read record {record_number}: {e}"))?;
        if &buf[0..4] != b"FILE" {
            batch.unused_slots += 1;
            continue;
        }
        if apply_fixup_on_read(&mut buf, params.bytes_per_sector).is_err() {
            batch.damaged_records.push(record_number);
            continue;
        }
        let flags = record_flags(&buf);
        if flags & MFT_FLAG_IN_USE == 0 {
            batch.unused_slots += 1;
            continue;
        }
        let sequence = record_sequence(&buf);
        let base_reference = u64::from_le_bytes(
            buf[OFF_BASE_REFERENCE..OFF_BASE_REFERENCE + 8]
                .try_into()
                .expect("8 bytes"),
        );
        batch.entries.push(InventoryEntry {
            record_number,
            sequence,
            file_reference: file_reference(record_number, sequence)?,
            is_directory: flags & MFT_FLAG_DIRECTORY != 0,
            base_reference,
        });
    }
    if end < total_slots {
        batch.next = Some(InventoryCursor { next_record: end });
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_io::PathIo;
    use std::path::{Path, PathBuf};

    const SIZE: u64 = 16 * 1024 * 1024;

    fn image(stem: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("mft-inventory-{stem}-{}.img", std::process::id()));
        let file = std::fs::File::create(&path).expect("create image");
        file.set_len(SIZE).expect("size image");
        drop(file);
        let mut io = PathIo::open_rw(&path).expect("open image");
        crate::mkfs::format_filesystem(&mut io, SIZE, 4096, 1024, None, Some(0x609))
            .expect("format");
        path
    }

    fn full_scan(path: &Path, bound: u64) -> (Vec<InventoryEntry>, Vec<InventoryBatch>) {
        let mut io = PathIo::open_ro(path).expect("open");
        let mut cursor = InventoryCursor::START;
        let mut entries = Vec::new();
        let mut batches = Vec::new();
        loop {
            let batch = inventory_batch_io(&mut io, cursor, bound).expect("batch");
            entries.extend(batch.entries.iter().copied());
            let next = batch.next;
            batches.push(batch);
            match next {
                Some(c) => cursor = c,
                None => break,
            }
        }
        (entries, batches)
    }

    #[test]
    fn zero_bound_is_refused() {
        let path = image("zero");
        let mut io = PathIo::open_ro(&path).expect("open");
        assert!(inventory_batch_io(&mut io, InventoryCursor::START, 0).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cursor_past_the_table_is_refused() {
        let path = image("past");
        let mut io = PathIo::open_ro(&path).expect("open");
        let (_, total) = mft_slot_count_io(&mut io).expect("count");
        let past = InventoryCursor {
            next_record: total + 1,
        };
        assert!(inventory_batch_io(&mut io, past, 1).is_err());
        let at_end = InventoryCursor { next_record: total };
        let batch = inventory_batch_io(&mut io, at_end, 1).expect("end batch");
        assert_eq!(batch.slots_visited, 0);
        assert_eq!(batch.next, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_batch_stays_inside_its_bound_including_unused_slots() {
        let path = image("bound");
        let (_, batches) = full_scan(&path, 3);
        let total = batches[0].total_slots;
        assert!(total > 16, "fresh volume has the reserved records");
        let mut visited = 0;
        for batch in &batches {
            assert!(batch.slots_visited <= 3);
            assert_eq!(
                batch.slots_visited,
                batch.entries.len() as u64
                    + batch.unused_slots
                    + batch.damaged_records.len() as u64
            );
            visited += batch.slots_visited;
        }
        assert_eq!(visited, total, "batches cover every slot exactly once");
        // Records 12..16 are reserved and unused on a fresh volume; a
        // batch over only them returns nothing but still advances.
        let mut io = PathIo::open_ro(&path).expect("open");
        let batch = inventory_batch_io(&mut io, InventoryCursor { next_record: 12 }, 4)
            .expect("reserved batch");
        assert_eq!(batch.slots_visited, 4);
        assert_eq!(batch.next, Some(InventoryCursor { next_record: 16 }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resumed_batches_match_one_unbounded_batch() {
        let path = image("resume");
        let (small, _) = full_scan(&path, 1);
        let (large, batches) = full_scan(&path, u64::MAX);
        assert_eq!(batches.len(), 1);
        assert_eq!(small, large);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_references_carry_the_record_sequence() {
        let path = image("sequence");
        let (entries, _) = full_scan(&path, 7);
        let mut io = PathIo::open_ro(&path).expect("open");
        for entry in &entries {
            let (_, record) =
                mft_io::read_mft_record_io(&mut io, entry.record_number).expect("read");
            assert_eq!(entry.sequence, record_sequence(&record));
            assert_eq!(
                entry.file_reference & MAX_RECORD_NUMBER,
                entry.record_number
            );
            assert_eq!(entry.file_reference >> 48, u64::from(entry.sequence));
        }
        // $MFT is record 0 and the root directory record 5.
        assert!(entries.iter().any(|e| e.record_number == 0));
        assert!(entries
            .iter()
            .any(|e| e.record_number == 5 && e.is_directory));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_record_number_past_48_bits_is_refused_not_truncated() {
        assert!(file_reference(MAX_RECORD_NUMBER + 1, 1).is_err());
        assert_eq!(
            file_reference(MAX_RECORD_NUMBER, 0xFFFF).expect("fits"),
            u64::MAX
        );
    }

    #[test]
    fn inventory_leaves_the_volume_bytes_unchanged() {
        let path = image("readonly");
        let before = std::fs::read(&path).expect("read");
        let _ = full_scan(&path, 5);
        assert_eq!(before, std::fs::read(&path).expect("read"));
        let _ = std::fs::remove_file(&path);
    }
}
