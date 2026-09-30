//! Reading `$LogFile`'s restart area far enough to answer one question:
//! **does the log hold anything a replay would have to redo or undo?**
//!
//! This is the read half of replay (#137), not replay. It exists so that
//! `fsck` can clear the dirty flag on a volume whose log records nothing
//! outstanding, instead of refusing every log that is not all `0xFF`
//! (#375). Anything it cannot establish is answered "may hold work", and
//! that is what keeps a real transaction from being discarded.
//!
//! # What a clean log looks like
//!
//! Taken from a volume Windows formatted, wrote to and cleanly detached
//! (`test-disks/windows-clean-logfile.bin.gz`), and from ntfs-3g's check,
//! which agrees with it:
//!
//! * two restart pages (`RSTR`) at offsets 0 and the system page size; the
//!   one with the higher `current_lsn` is authoritative;
//! * a restart area whose `flags` carry `RESTART_VOLUME_IS_CLEAN` (`0x0002`)
//!   -- what Windows writes on a clean dismount -- or whose in-use client
//!   list is empty (`0xFFFF`): nobody has the log open;
//! * otherwise, the one in-use client's restart LSN names the last record
//!   written (`current_lsn`), and that record, found where the LSN says
//!   it is, is an NTFS checkpoint (a client restart record) whose
//!   transaction table and dirty page table are both empty. Nothing
//!   follows it, no transaction is open and no page is dirty: nothing to
//!   redo, nothing to undo.
//!
//! An LSN is turned into a byte offset in the log as
//! `(lsn << seq_number_bits) >> (seq_number_bits - 3)`: the low bits
//! are the offset in 8-byte units, the high bits a wrap count. On
//! Windows' log above, restart LSN `0x184415` resolves to `0x220a8`, and
//! the record found there carries exactly that LSN.
//!
//! References: MS-FSCC and Windows Internals 7th ed., "NTFS Logging" (the
//! `RSTR` / `RCRD` taxonomy and the restart area). No GPL implementation
//! was consulted; ntfs-3g is used only as a program, in the tests.

use crate::mft_io::apply_fixup_on_read_magic;

/// Log pages are protected by an update sequence array with a 512-byte
/// stride, whatever the device's sector size.
const LFS_STRIDE: u16 = 512;
/// `client_in_use_list` / `client_free_list` value meaning "none".
const NO_CLIENT: u16 = 0xFFFF;
/// Restart-area flag Windows sets when the volume was dismounted cleanly.
pub const RESTART_VOLUME_IS_CLEAN: u16 = 0x0002;
/// LFS record type of a client restart record: NTFS's checkpoint.
const LFS_CLIENT_RESTART: u32 = 2;
/// Size of one client record in the restart area's client array.
const CLIENT_RECORD_BYTES: usize = 0xA0;
/// Offset, within a restart page, of the restart-area offset field.
const RESTART_AREA_OFFSET_FIELD: usize = 0x18;
/// Offset, within the restart area, of the `flags` field.
pub const RESTART_AREA_FLAGS_OFFSET: usize = 0x0E;

/// Why a log was judged to hold nothing to replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanBecause {
    /// The restart area's in-use client list is empty.
    NoClientOpen,
    /// The restart area carries `RESTART_VOLUME_IS_CLEAN`.
    MarkedClean,
    /// The last record is a checkpoint with no open transaction and no
    /// dirty page.
    CheckpointOnly,
}

/// What `$LogFile` holds, as far as replay is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogfileState {
    /// Every byte is `0xFF`: an initialised-empty log.
    Empty,
    /// A restart area that records nothing to redo or undo.
    Clean(CleanBecause),
    /// Anything else, with the reason: the log may hold transactions.
    Pending(String),
}

impl LogfileState {
    /// Whether discarding the log, or clearing the dirty flag over it,
    /// could lose a transaction.
    pub fn needs_replay(&self) -> bool {
        matches!(self, LogfileState::Pending(_))
    }

    /// One line for a report.
    pub fn describe(&self) -> String {
        match self {
            LogfileState::Empty => "$LogFile is empty (all 0xFF)".to_string(),
            LogfileState::Clean(CleanBecause::NoClientOpen) => {
                "$LogFile's restart area has no client open".to_string()
            }
            LogfileState::Clean(CleanBecause::MarkedClean) => {
                "$LogFile's restart area marks the volume clean".to_string()
            }
            LogfileState::Clean(CleanBecause::CheckpointOnly) => {
                "$LogFile ends in a checkpoint with no open transaction and no dirty page"
                    .to_string()
            }
            LogfileState::Pending(why) => why.clone(),
        }
    }
}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

/// The fields of one restart area this module reads.
#[derive(Debug, Clone, Copy)]
struct RestartArea {
    current_lsn: u64,
    in_use: u16,
    flags: u16,
    seq_number_bits: u32,
    log_page_size: u32,
    record_header_length: u16,
    /// `client_restart_lsn` of the in-use client, when there is one.
    client_restart_lsn: Option<u64>,
}

/// Reads `len` bytes of the log at `offset`, or `None` past its end.
pub type ReadLog<'a> = dyn FnMut(u64, usize) -> Option<Vec<u8>> + 'a;

/// Parse the restart page at `off`, or say why it is not one.
fn restart_page(read: &mut ReadLog<'_>, off: u64, page_size: usize) -> Result<RestartArea, String> {
    let mut page = read(off, page_size)
        .ok_or_else(|| format!("no restart page at {off:#x}: the log ends before it"))?;
    apply_fixup_on_read_magic(&mut page, LFS_STRIDE, b"RSTR")
        .map_err(|e| format!("restart page at {off:#x}: {e}"))?;
    let bad = |what: &str| format!("restart page at {off:#x}: {what}");
    let log_page_size = u32_at(&page, 0x14).ok_or_else(|| bad("truncated header"))?;
    let ra =
        u16_at(&page, RESTART_AREA_OFFSET_FIELD).ok_or_else(|| bad("truncated header"))? as usize;
    let current_lsn = u64_at(&page, ra).ok_or_else(|| bad("restart area outside the page"))?;
    let log_clients =
        u16_at(&page, ra + 0x08).ok_or_else(|| bad("restart area outside the page"))?;
    let in_use = u16_at(&page, ra + 0x0C).ok_or_else(|| bad("restart area outside the page"))?;
    let flags = u16_at(&page, ra + RESTART_AREA_FLAGS_OFFSET)
        .ok_or_else(|| bad("restart area outside the page"))?;
    let seq_number_bits =
        u32_at(&page, ra + 0x10).ok_or_else(|| bad("restart area outside the page"))?;
    let client_array =
        u16_at(&page, ra + 0x16).ok_or_else(|| bad("restart area outside the page"))? as usize;
    let record_header_length =
        u16_at(&page, ra + 0x24).ok_or_else(|| bad("restart area outside the page"))?;

    let client_restart_lsn = if in_use == NO_CLIENT {
        None
    } else {
        if in_use >= log_clients {
            return Err(bad(&format!(
                "in-use client {in_use} is outside the {log_clients}-client array"
            )));
        }
        let client = ra + client_array + in_use as usize * CLIENT_RECORD_BYTES;
        if client + CLIENT_RECORD_BYTES > page.len() {
            return Err(bad("client record outside the page"));
        }
        Some(u64_at(&page, client + 0x08).ok_or_else(|| bad("client record outside the page"))?)
    };
    Ok(RestartArea {
        current_lsn,
        in_use,
        flags,
        seq_number_bits,
        log_page_size,
        record_header_length,
        client_restart_lsn,
    })
}

/// Byte offset in the log of `lsn`, or `None` if the geometry is nonsense.
fn lsn_to_offset(lsn: u64, seq_number_bits: u32) -> Option<u64> {
    if !(3..64).contains(&seq_number_bits) {
        return None;
    }
    Some((lsn << seq_number_bits) >> (seq_number_bits - 3))
}

/// Decide what `log` -- the whole of `$LogFile`'s `$DATA` -- holds.
pub fn state(log: &[u8]) -> LogfileState {
    if log.iter().all(|&b| b == 0xFF) {
        return LogfileState::Empty;
    }
    state_of_nonempty(&mut |off, len| {
        let start = usize::try_from(off).ok()?;
        log.get(start..start.checked_add(len)?).map(<[u8]>::to_vec)
    })
}

/// [`state`] for a log already known not to be all `0xFF`, read a page at
/// a time through `read`: the two restart pages and, at most, the one
/// page holding the last checkpoint.
pub fn state_of_nonempty(read: &mut ReadLog<'_>) -> LogfileState {
    match analyse(read) {
        Ok(state) => state,
        // Every reason names the log and the replay it would take, so a
        // refusal says what it is protecting whichever check tripped.
        Err(why) if why.contains("$LogFile") => LogfileState::Pending(why),
        Err(why) => LogfileState::Pending(format!(
            "$LogFile cannot be shown to hold nothing to replay ({why}), so it may hold \
             transactions this library cannot replay (rust-fs-ntfs#137)"
        )),
    }
}

fn analyse(read: &mut ReadLog<'_>) -> Result<LogfileState, String> {
    // The first restart page names the system page size, which is where
    // the second one starts.
    let header = read(0, 0x18).ok_or("the log is shorter than a restart page header")?;
    let system_page_size =
        u32_at(&header, 0x10).ok_or("the log is shorter than a restart page header")?;
    if !(512..=65536).contains(&system_page_size) || !system_page_size.is_power_of_two() {
        return Err(format!(
            "restart page names a system page size of {system_page_size}"
        ));
    }
    let sps = system_page_size as usize;
    let first = restart_page(read, 0, sps);
    let second = restart_page(read, sps as u64, sps);
    let ra = match (first, second) {
        (Ok(a), Ok(b)) => {
            if b.current_lsn > a.current_lsn {
                b
            } else {
                a
            }
        }
        (Ok(a), Err(_)) | (Err(_), Ok(a)) => a,
        (Err(a), Err(b)) => {
            return Err(format!("$LogFile has no readable restart page ({a}; {b})"));
        }
    };

    if ra.in_use == NO_CLIENT {
        return Ok(LogfileState::Clean(CleanBecause::NoClientOpen));
    }
    if ra.flags & RESTART_VOLUME_IS_CLEAN != 0 {
        return Ok(LogfileState::Clean(CleanBecause::MarkedClean));
    }

    let restart_lsn = ra.client_restart_lsn.ok_or("no in-use client record")?;
    if restart_lsn != ra.current_lsn {
        return Err(format!(
            "$LogFile holds records after its last checkpoint (checkpoint LSN {restart_lsn:#x}, \
             last LSN {:#x}), which may be transactions this library cannot replay \
             (rust-fs-ntfs#137)",
            ra.current_lsn
        ));
    }

    let unreadable = |what: String| {
        format!(
            "the checkpoint at LSN {restart_lsn:#x} cannot be read ({what}), so whether \
             $LogFile holds transactions cannot be established (rust-fs-ntfs#137)"
        )
    };
    let lps = ra.log_page_size;
    if !(512..=65536).contains(&lps) || !lps.is_power_of_two() {
        return Err(unreadable(format!("log page size {lps}")));
    }
    let offset = lsn_to_offset(restart_lsn, ra.seq_number_bits)
        .ok_or_else(|| unreadable(format!("sequence-number bits {}", ra.seq_number_bits)))?;
    let page_start = offset - offset % u64::from(lps);
    let in_page = (offset - page_start) as usize;
    let mut page = read(page_start, lps as usize)
        .ok_or_else(|| unreadable(format!("offset {offset:#x} is past the log's end")))?;
    apply_fixup_on_read_magic(&mut page, LFS_STRIDE, b"RCRD")
        .map_err(|e| unreadable(format!("record page at {page_start:#x}: {e}")))?;

    let this_lsn =
        u64_at(&page, in_page).ok_or_else(|| unreadable("header past the page".into()))?;
    if this_lsn != restart_lsn {
        return Err(unreadable(format!(
            "the record at {offset:#x} carries LSN {this_lsn:#x}"
        )));
    }
    let data_len = u32_at(&page, in_page + 0x18)
        .ok_or_else(|| unreadable("header past the page".into()))? as usize;
    let record_type =
        u32_at(&page, in_page + 0x20).ok_or_else(|| unreadable("header past the page".into()))?;
    if record_type != LFS_CLIENT_RESTART {
        return Err(unreadable(format!(
            "record type {record_type}, not a checkpoint"
        )));
    }
    let body = in_page + ra.record_header_length as usize;
    // The body is NTFS's restart area: four table LSNs at 0x10..0x30 and
    // four table lengths at 0x30..0x40. A body that does not fit in this
    // page is one this reader will not stitch together.
    if data_len < 0x40 || body + data_len > page.len() {
        return Err(unreadable(format!(
            "a checkpoint body of {data_len} bytes at page offset {body:#x}"
        )));
    }
    let dirty_pages = u32_at(&page, body + 0x38).unwrap_or(u32::MAX);
    let transactions = u32_at(&page, body + 0x3C).unwrap_or(u32::MAX);
    if dirty_pages != 0 || transactions != 0 {
        return Err(format!(
            "$LogFile's last checkpoint lists {transactions} bytes of open transactions and \
             {dirty_pages} bytes of dirty pages, which this library cannot replay \
             (rust-fs-ntfs#137)"
        ));
    }
    Ok(LogfileState::Clean(CleanBecause::CheckpointOnly))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_all_ff_log_is_empty() {
        assert_eq!(state(&[0xFF; 8192]), LogfileState::Empty);
    }

    #[test]
    fn bytes_that_are_not_a_restart_page_may_hold_work() {
        let mut log = vec![0xFF; 16384];
        log[..4].copy_from_slice(b"RSTR");
        assert!(state(&log).needs_replay());
        assert!(state(&[0u8; 8192]).needs_replay());
    }

    #[test]
    fn an_lsn_resolves_as_windows_writes_it() {
        // Windows' clean log: restart LSN 0x184415, 45 sequence bits, and
        // the checkpoint found at 0x220a8 (tests/logfile_state.rs reads it).
        assert_eq!(lsn_to_offset(0x18_4415, 45), Some(0x2_20a8));
        assert_eq!(lsn_to_offset(1, 64), None);
        assert_eq!(lsn_to_offset(1, 2), None);
    }
}
