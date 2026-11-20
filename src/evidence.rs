//! Journal evidence from a read-only device (#1318, #1307).
//!
//! The change journal and `$LogFile` readers were written for a volume this
//! crate may write, so they take [`BlockIo`], whose `write_all_at` a caller
//! inspecting a damaged disk cannot honestly provide. Everything here takes
//! [`fs_core::BlockRead`] instead, which has no write at all, and reads
//! through the same decoders:
//!
//! - [`usn_journal_state`] and [`read_usn_records`]: `$Extend\$UsnJrnl`'s
//!   bounds and its `USN_RECORD_V2` records, in bounded batches from a
//!   cursor ([`crate::usn_image`]).
//! - [`logfile_state`], [`read_logfile`] and [`walk_logfile`]: whether the
//!   log holds work, its bytes, and its records in LSN order
//!   ([`crate::logfile_replay::walk_records`]).
//! - [`replay_logfile_in_memory`]: the Windows-compatible replay `fsck` runs
//!   ([`crate::fsck::replay_logfile_io`]), with every write it makes held in
//!   memory. The returned [`ReplayedVolume`] is a `BlockRead` that shows the
//!   device as the replay leaves it, so it can be opened with
//!   [`crate::volume::Volume::open`] and read like the recovered volume,
//!   while the device itself is never written.
//!
//! What a replay reconstructs is evidence of what the log held, not proof
//! that it reached the disk; a caller presents it beside the unreplayed
//! volume, never in its place.

use crate::block_io::BlockIo;
use crate::error::Error;
use crate::fsck::{self, ReplayReport};
use crate::logfile::LogfileState;
use crate::logfile_replay::{self, RecordWalk};
use crate::usn_cursor::UsnCursor;
use crate::usn_image::{self, ImageJournal, JournalReadError, JournalState, RecordBatch};
use fs_core::BlockRead;
use std::collections::BTreeMap;

/// The largest `$LogFile` read whole. Windows sizes the log from the
/// volume and caps it far below this; a larger declared size is damage.
pub const MAX_LOGFILE_BYTES: u64 = 1024 * 1024 * 1024;

/// A [`BlockIo`] over a device that can only be read: every write is refused.
struct ReadOnly<'a, D: ?Sized>(&'a D);

impl<D: BlockRead + ?Sized> BlockIo for ReadOnly<'_, D> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.0
            .read_at(offset, buf)
            .map_err(|error| error.to_string())
    }
    fn write_all_at(&mut self, offset: u64, _: &[u8]) -> Result<(), String> {
        Err(format!(
            "journal evidence is read from a read-only device and never written (byte {offset:#x})"
        ))
    }
    fn size(&self) -> u64 {
        self.0.size_bytes()
    }
}

/// The change journal's state: absent, being deleted, or active with the
/// bounds its `$Max` and `$J` streams record.
pub fn usn_journal_state<D: BlockRead + ?Sized>(device: &D) -> Result<JournalState, Error> {
    if !READY {
        return Err(not_ready());
    }
    usn_image::journal_state_io(&mut ReadOnly(device))
}

/// The change journal records from `cursor` on, examining at most
/// `max_bytes` of `$J`. See [`usn_image::read_records_io`].
pub fn read_usn_records<D: BlockRead + ?Sized>(
    device: &D,
    journal: &ImageJournal,
    cursor: UsnCursor,
    max_bytes: usize,
) -> Result<RecordBatch, JournalReadError> {
    if !READY {
        return Err(JournalReadError::Io(not_ready()));
    }
    usn_image::read_records_io(&mut ReadOnly(device), journal, cursor, max_bytes)
}

/// Whether `$LogFile` is empty, clean, or holds work a replay would redo.
pub fn logfile_state<D: BlockRead + ?Sized>(device: &D) -> Result<LogfileState, Error> {
    if !READY {
        return Err(not_ready());
    }
    fsck::logfile_state_io(&mut ReadOnly(device))
}

/// `$LogFile`'s bytes and the volume offset they start at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogfileBytes {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

/// Reads `$LogFile` whole, up to [`MAX_LOGFILE_BYTES`].
pub fn read_logfile<D: BlockRead + ?Sized>(device: &D) -> Result<LogfileBytes, Error> {
    if !READY {
        return Err(not_ready());
    }
    let (offset, size) = fsck::locate_logfile_data_io(&mut ReadOnly(device))?;
    if size > MAX_LOGFILE_BYTES {
        return Err(Error::invalid(format!(
            "$LogFile says it is {size} bytes, over the {MAX_LOGFILE_BYTES}-byte bound"
        )));
    }
    let mut bytes = vec![0; size as usize];
    device
        .read_at(offset, &mut bytes)
        .map_err(|error| Error::io(format!("read $LogFile: {error}")))?;
    Ok(LogfileBytes { offset, bytes })
}

/// The log's records in LSN order from `from` (by default where replay's
/// analysis starts), at most `limit` of them, each at most
/// `max_record_bytes` of client data. See [`logfile_replay::walk_records`].
pub fn walk_logfile<D: BlockRead + ?Sized>(
    device: &D,
    from: Option<u64>,
    limit: usize,
    max_record_bytes: usize,
) -> Result<RecordWalk, Error> {
    let log = read_logfile(device)?;
    logfile_replay::walk_records(&log.bytes, from, limit, max_record_bytes)
}

const PAGE: u64 = 4096;

/// The read-only entry points are declared but not wired to the decoders yet.
const READY: bool = false;

fn not_ready() -> Error {
    Error::refused("journal evidence from a read-only device is not wired in yet")
}

/// A device as a replay left it: its own bytes, with the replay's writes
/// over them, held in memory.
pub struct ReplayedVolume<D: BlockRead> {
    device: D,
    pages: BTreeMap<u64, Vec<u8>>,
    written: Vec<(u64, u64)>,
    report: Option<ReplayReport>,
}

impl<D: BlockRead> ReplayedVolume<D> {
    fn new(device: D) -> Self {
        Self {
            device,
            pages: BTreeMap::new(),
            written: Vec::new(),
            report: None,
        }
    }

    /// What the replay did, or `None` when the log held no work to replay.
    pub fn report(&self) -> Option<&ReplayReport> {
        self.report.as_ref()
    }

    /// The byte ranges `(offset, length)` the replay wrote, merged and in
    /// order: the evidence of what it changed.
    pub fn changed_ranges(&self) -> Vec<(u64, u64)> {
        let mut ranges = self.written.clone();
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (offset, length) in ranges {
            match merged.last_mut() {
                Some((at, len)) if offset <= *at + *len => {
                    *len = (*len).max(offset + length - *at);
                }
                _ => merged.push((offset, length)),
            }
        }
        merged
    }

    /// The device, unchanged.
    pub fn into_inner(self) -> D {
        self.device
    }

    fn read_overlaid(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        self.device.read_at(offset, buf)?;
        let end = offset + buf.len() as u64;
        for (&page, bytes) in self.pages.range(offset / PAGE * PAGE..end) {
            let from = page.max(offset);
            let to = (page + PAGE).min(end);
            if from < to {
                buf[(from - offset) as usize..(to - offset) as usize]
                    .copy_from_slice(&bytes[(from - page) as usize..(to - page) as usize]);
            }
        }
        Ok(())
    }
}

impl<D: BlockRead> BlockRead for ReplayedVolume<D> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        self.read_overlaid(offset, buf)
    }
    fn size_bytes(&self) -> u64 {
        self.device.size_bytes()
    }
}

/// The replay writes here; the device behind it is only read.
impl<D: BlockRead> BlockIo for ReplayedVolume<D> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.read_overlaid(offset, buf)
            .map_err(|error| error.to_string())
    }
    fn write_all_at(&mut self, offset: u64, data: &[u8]) -> Result<(), String> {
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|end| *end <= self.device.size_bytes())
            .ok_or_else(|| format!("replay write at byte {offset:#x} runs past the device"))?;
        let mut page = offset / PAGE * PAGE;
        while page < end {
            if !self.pages.contains_key(&page) {
                let length = PAGE.min(self.device.size_bytes() - page) as usize;
                let mut bytes = vec![0; length];
                self.device
                    .read_at(page, &mut bytes)
                    .map_err(|error| error.to_string())?;
                self.pages.insert(page, bytes);
            }
            let bytes = self.pages.get_mut(&page).expect("inserted above");
            let from = page.max(offset);
            let to = (page + bytes.len() as u64).min(end);
            bytes[(from - page) as usize..(to - page) as usize]
                .copy_from_slice(&data[(from - offset) as usize..(to - offset) as usize]);
            page += PAGE;
        }
        if !data.is_empty() {
            self.written.push((offset, data.len() as u64));
        }
        Ok(())
    }
    fn size(&self) -> u64 {
        self.device.size_bytes()
    }
    fn sync(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// Writes held in memory are ordered by construction.
    fn can_sync(&self) -> bool {
        true
    }
}

/// Runs the Windows-compatible `$LogFile` replay with every write held in
/// memory, and returns the device as the replay leaves it. A log with no
/// work returns the device unchanged with no report. A log the replay
/// refuses is an error, and nothing is kept.
pub fn replay_logfile_in_memory<D: BlockRead>(device: D) -> Result<ReplayedVolume<D>, Error> {
    if !READY {
        return Err(not_ready());
    }
    let mut replayed = ReplayedVolume::new(device);
    if !logfile_state(&replayed.device)?.needs_replay() {
        return Ok(replayed);
    }
    replayed.report = Some(fsck::replay_logfile_io(&mut replayed)?);
    Ok(replayed)
}
