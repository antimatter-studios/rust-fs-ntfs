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
//! Format evidence: the Windows-authored restart areas and record pages
//! exercised in `tests/windows_interrupted_fixture.rs`. The external
//! filesystem programs used by tests run as subprocess oracles.

use crate::error::Error;
use crate::mft_io::{apply_fixup_on_read_magic, apply_fixup_on_write_magic};

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
fn restart_page(read: &mut ReadLog<'_>, off: u64, page_size: usize) -> Result<RestartArea, Error> {
    let mut page = read(off, page_size).ok_or_else(|| {
        Error::io(format!(
            "no restart page at {off:#x}: the log ends before it"
        ))
    })?;
    apply_fixup_on_read_magic(&mut page, LFS_STRIDE, b"RSTR")
        .map_err(|e| e.context(format!("restart page at {off:#x}")))?;
    let bad = |what: &str| Error::io(format!("restart page at {off:#x}: {what}"));
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

/// Restart pages to write back: byte offset in the log, bytes.
type RestartPages = Vec<(u64, Vec<u8>)>;

/// The log's readable restart pages with `RESTART_VOLUME_IS_CLEAN` set,
/// each ready to be written back over itself (fixups applied), and the
/// system page size, which is where the second one starts.
///
/// The first returned is the one [`state`] reads when both are readable:
/// the higher current LSN, the first page on a tie. Written in this order,
/// the page [`state`] reads is marked clean by the first write, so a stop
/// between the two leaves a log that reads as clean. The flag is what
/// Windows sets on a clean dismount; ntfs-3g and the Linux kernel driver
/// read it as clean too.
pub(crate) fn restart_pages_marked_clean(
    read: &mut ReadLog<'_>,
) -> Result<(usize, RestartPages), Error> {
    let header = read(0, 0x18).ok_or(Error::io("the log is shorter than a restart page header"))?;
    let system_page_size =
        u32_at(&header, 0x10).ok_or(Error::io("the log is shorter than a restart page header"))?;
    if !(512..=65536).contains(&system_page_size) || !system_page_size.is_power_of_two() {
        return Err(Error::io(format!(
            "restart page names a system page size of {system_page_size}"
        )));
    }
    let sps = system_page_size as usize;
    let mut pages = Vec::new();
    for off in [0, sps as u64] {
        let Ok(ra) = restart_page(read, off, sps) else {
            continue;
        };
        let Some(mut page) = read(off, sps) else {
            continue;
        };
        apply_fixup_on_read_magic(&mut page, LFS_STRIDE, b"RSTR")?;
        let at = u16_at(&page, RESTART_AREA_OFFSET_FIELD)
            .ok_or(Error::io("restart page: truncated header"))? as usize
            + RESTART_AREA_FLAGS_OFFSET;
        let flags = (ra.flags | RESTART_VOLUME_IS_CLEAN).to_le_bytes();
        page.get_mut(at..at + 2)
            .ok_or(Error::io("restart page: restart area outside the page"))?
            .copy_from_slice(&flags);
        apply_fixup_on_write_magic(&mut page, LFS_STRIDE, b"RSTR")?;
        pages.push((ra.current_lsn, off, page));
    }
    if pages.is_empty() {
        return Err(Error::io("$LogFile has no readable restart page"));
    }
    pages.sort_by_key(|(lsn, off, _)| (std::cmp::Reverse(*lsn), *off));
    Ok((
        sps,
        pages
            .into_iter()
            .map(|(_, off, page)| (off, page))
            .collect(),
    ))
}

/// Byte offset in the log of `lsn`, or `None` if the geometry is nonsense.
pub(crate) fn lsn_to_offset(lsn: u64, seq_number_bits: u32) -> Option<u64> {
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
        Err(why) if why.contains("$LogFile") => LogfileState::Pending(why.into()),
        Err(why) => LogfileState::Pending(format!(
            "$LogFile cannot be shown to hold nothing to replay ({why}), so it may hold \
             transactions this library cannot replay (rust-fs-ntfs#137)"
        )),
    }
}

fn analyse(read: &mut ReadLog<'_>) -> Result<LogfileState, Error> {
    // The first restart page names the system page size, which is where
    // the second one starts.
    let header = read(0, 0x18).ok_or(Error::io("the log is shorter than a restart page header"))?;
    let system_page_size =
        u32_at(&header, 0x10).ok_or(Error::io("the log is shorter than a restart page header"))?;
    if !(512..=65536).contains(&system_page_size) || !system_page_size.is_power_of_two() {
        return Err(Error::io(format!(
            "restart page names a system page size of {system_page_size}"
        )));
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
            return Err(Error::io(format!(
                "$LogFile has no readable restart page ({a}; {b})"
            )));
        }
    };

    if ra.in_use == NO_CLIENT {
        return Ok(LogfileState::Clean(CleanBecause::NoClientOpen));
    }
    if ra.flags & RESTART_VOLUME_IS_CLEAN != 0 {
        return Ok(LogfileState::Clean(CleanBecause::MarkedClean));
    }

    let restart_lsn = ra
        .client_restart_lsn
        .ok_or(Error::io("no in-use client record"))?;
    if restart_lsn != ra.current_lsn {
        return Err(Error::io(format!(
            "$LogFile holds records after its last checkpoint (checkpoint LSN {restart_lsn:#x}, \
             last LSN {:#x}), which may be transactions this library cannot replay \
             (rust-fs-ntfs#137)",
            ra.current_lsn
        )));
    }

    let unreadable = |what: String| {
        Error::io(format!(
            "the checkpoint at LSN {restart_lsn:#x} cannot be read ({what}), so whether \
             $LogFile holds transactions cannot be established (rust-fs-ntfs#137)"
        ))
    };
    let lps = ra.log_page_size;
    if !(512..=65536).contains(&lps) || !lps.is_power_of_two() {
        return Err(unreadable(format!("log page size {lps}")));
    }
    // EVERY RECORD PAGE IS READ, not only the checkpoint's. An LFS 2.0
    // restart area -- what Windows 8 and later run a mounted log at -- is
    // rewritten only at a checkpoint, so `current_lsn` equal to the
    // client's restart LSN says nothing about what was logged since. The
    // record pages do: each names the last LSN ending on it. On a log
    // Windows was writing when it was captured, three pages ended past the
    // checkpoint while the restart area agreed with itself
    // (test-disks/windows-interrupted-logfile-1.bin.gz). A page whose
    // update sequence does not check out may be the page being written
    // when the log stopped, and is not taken on trust either.
    //
    // The two pages after the restart pages are the LFS TAIL COPIES: the
    // newest tail page is written there before its home, so a checkpoint
    // can be there and nowhere else yet (windows-interrupted-logfile-5).
    let lps_u64 = u64::from(lps);
    let first_record_page = 2 * sps as u64;
    let tail_copies_end = first_record_page + 2 * lps_u64;
    let mut tail_copies = Vec::new();
    let mut newest = 0u64;
    let mut at = first_record_page;
    while let Some(mut page) = read(at, lps as usize) {
        if page.get(..4) == Some(b"RCRD".as_slice()) {
            apply_fixup_on_read_magic(&mut page, LFS_STRIDE, b"RCRD").map_err(|e| {
                format!(
                    "$LogFile's record page at {at:#x} is torn ({e}), so it may hold \
                     transactions this library cannot replay (rust-fs-ntfs#137)"
                )
            })?;
            // `last_end_lsn` (0x20), not the field at 0x08: on an LFS 1.x
            // tail copy that one is the file offset of the page's home
            // (0x22000 on Windows' clean log), not an LSN.
            let last_end = u64_at(&page, 0x20).unwrap_or(u64::MAX);
            newest = newest.max(last_end);
            if at < tail_copies_end {
                tail_copies.push(page);
            }
        }
        at += lps_u64;
    }
    if newest > restart_lsn {
        return Err(Error::io(format!(
            "$LogFile holds records after its last checkpoint (checkpoint LSN \
             {restart_lsn:#x}, newest record LSN {newest:#x}), which may be transactions \
             this library cannot replay (rust-fs-ntfs#137)"
        )));
    }

    let offset = lsn_to_offset(restart_lsn, ra.seq_number_bits)
        .ok_or_else(|| unreadable(format!("sequence-number bits {}", ra.seq_number_bits)))?;
    let page_start = offset - offset % lps_u64;
    let in_page = (offset - page_start) as usize;
    let carries = |page: &[u8]| u64_at(page, in_page) == Some(restart_lsn);
    let mut home = read(page_start, lps as usize)
        .ok_or_else(|| unreadable(format!("offset {offset:#x} is past the log's end")))?;
    let home_ok =
        apply_fixup_on_read_magic(&mut home, LFS_STRIDE, b"RCRD").is_ok() && carries(&home);
    let page = if home_ok {
        home
    } else {
        tail_copies
            .into_iter()
            .find(|p| carries(p))
            .ok_or_else(|| {
                unreadable(format!(
                    "neither the record at {offset:#x} nor either tail copy carries it"
                ))
            })?
    };

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
        return Err(Error::io(format!(
            "$LogFile's last checkpoint lists {transactions} bytes of open transactions and \
             {dirty_pages} bytes of dirty pages, which this library cannot replay \
             (rust-fs-ntfs#137)"
        )));
    }
    Ok(LogfileState::Clean(CleanBecause::CheckpointOnly))
}

/// The page holding the checkpoint record of a clean log: where format.com
/// put it (`src/logfile-canonical-12k.bin`) and where Windows put it on the
/// clean dismount of `test-disks/windows-clean-logfile.bin.gz`.
const CHECKPOINT_HOME: u64 = 0x22000;
/// An LFS record header: this LSN, the client's previous and undo-next
/// LSNs, the client data length, the client id, the record type, the
/// transaction id, the flags and padding.
const RECORD_HEADER_BYTES: usize = 0x30;
/// The NTFS checkpoint (client restart record) body format.com writes:
/// version 1.0, every table LSN and length 0 (no transaction open, no page
/// dirty), `0x100000` at `0x48` and the cluster size at `0x50`. Windows'
/// own checkpoints carry the same two values there, the cluster size
/// following the volume's (`0x400` on `windows-interrupted-small-cluster`).
const CHECKPOINT_BODY_BYTES: usize = 0x70;
const CHECKPOINT_BODY_CONSTANT_AT: usize = 0x48;
const CHECKPOINT_BODY_CONSTANT: u64 = 0x100000;
const CHECKPOINT_BODY_CLUSTER_SIZE_AT: usize = 0x50;
/// Restart-page header offsets of the LFS version, minor then major.
const RESTART_PAGE_MINOR_VERSION: usize = 0x1A;
const RESTART_PAGE_MAJOR_VERSION: usize = 0x1C;

/// Pages to write into a log, each at its byte offset in the log.
pub(crate) type LogPages = Vec<(u64, Vec<u8>)>;

/// A log as Windows leaves it on a clean dismount, for a log whose replay
/// has reached the volume (#762, part of #649).
pub(crate) struct CleanLog {
    /// The checkpoint record page at its home, then its two tail copies.
    pub records: LogPages,
    /// The two restart pages, the second first: a stop between them leaves
    /// the second, which names the higher current LSN, as the one read.
    pub restart: LogPages,
    /// The current LSN both restart pages name, the checkpoint's own.
    pub current_lsn: u64,
}

/// The highest LSN `log` records anywhere: in its restart areas, and in its
/// record pages' last LSN and last end LSN fields. A tail copy's last-LSN
/// field holds a page offset instead, which is smaller than any LSN of the
/// same log, so it never decides the answer.
pub(crate) fn highest_lsn(log: &[u8]) -> u64 {
    let (Some(sps), Some(lps)) = (u32_at(log, 0x10), u32_at(log, 0x14)) else {
        return 0;
    };
    let valid = |n: u32| (512..=65536).contains(&n) && n.is_power_of_two();
    if !valid(sps) || !valid(lps) {
        return 0;
    }
    let (sps, lps) = (sps as usize, lps as usize);
    let mut highest = 0;
    for at in [0, sps] {
        let Some(page) = log.get(at..) else { continue };
        if page.starts_with(b"RSTR") {
            if let Some(ra) = u16_at(page, RESTART_AREA_OFFSET_FIELD) {
                highest = highest.max(u64_at(page, ra as usize).unwrap_or(0));
            }
        }
    }
    for page in log.get(2 * sps..).unwrap_or(&[]).chunks(lps) {
        if page.starts_with(b"RCRD") {
            for at in [0x08, 0x20] {
                highest = highest.max(u64_at(page, at).unwrap_or(0));
            }
        }
    }
    highest
}

/// What Windows leaves in `$LogFile` on a clean dismount, built over this
/// log's own geometry: two LFS 1.1 restart pages flagged clean, both naming
/// a current LSN that the in-use client's restart LSN repeats, and at that
/// LSN's home an NTFS checkpoint whose tables are empty, with the two tail
/// copies an LFS 1.1 log keeps of it (`test-disks/windows-clean-logfile.bin.gz`,
/// and what `mkfs` writes from format.com's log).
///
/// The current LSN is one wrap past `past`: the LSN's high bits count wraps
/// of the log, so every LSN Windows writes after it, while it fills the
/// rest of the log, is newer than any LSN the replay left on the volume.
/// Redo trusts a block's LSN; a log that numbered below them would have its
/// own records skipped for those blocks after the next unclean shutdown.
pub(crate) fn clean_log_past(
    read: &mut ReadLog<'_>,
    past: u64,
    bytes_per_cluster: u64,
) -> Result<CleanLog, Error> {
    let header = read(0, 0x18).ok_or(Error::io("the log is shorter than a restart page header"))?;
    let sps =
        u32_at(&header, 0x10).ok_or(Error::io("the log is shorter than a restart page header"))?;
    if !(512..=65536).contains(&sps) || !sps.is_power_of_two() {
        return Err(Error::io(format!(
            "restart page names a system page size of {sps}"
        )));
    }
    let sps = sps as usize;
    // The restart page the log is read by: the higher current LSN, the
    // first on a tie, as `state` reads it.
    let mut template: Option<(u64, Vec<u8>)> = None;
    for off in [0, sps as u64] {
        let Some(mut page) = read(off, sps) else {
            continue;
        };
        if apply_fixup_on_read_magic(&mut page, LFS_STRIDE, b"RSTR").is_err() {
            continue;
        }
        let Some(current) =
            u16_at(&page, RESTART_AREA_OFFSET_FIELD).and_then(|ra| u64_at(&page, ra as usize))
        else {
            continue;
        };
        if template.as_ref().is_none_or(|(best, _)| current > *best) {
            template = Some((current, page));
        }
    }
    let (_, mut page) = template.ok_or(Error::io("$LogFile has no readable restart page"))?;
    let bad = |what: String| Error::io(format!("restart page: {what}"));
    let short = || bad("restart area outside the page".to_string());
    let lps = u32_at(&page, 0x14).ok_or_else(short)?;
    if !(512..=65536).contains(&lps) || !lps.is_power_of_two() {
        return Err(bad(format!("log page size {lps}")));
    }
    let lps = lps as usize;
    let ra = u16_at(&page, RESTART_AREA_OFFSET_FIELD).ok_or_else(short)? as usize;
    let log_clients = u16_at(&page, ra + 0x08).ok_or_else(short)?;
    let in_use = u16_at(&page, ra + 0x0C).ok_or_else(short)?;
    let bits = u32_at(&page, ra + 0x10).ok_or_else(short)?;
    let client_array = u16_at(&page, ra + 0x16).ok_or_else(short)? as usize;
    let file_size = u64_at(&page, ra + 0x18).ok_or_else(short)?;
    let header_len = u16_at(&page, ra + 0x24).ok_or_else(short)? as usize;
    let data_offset = u16_at(&page, ra + 0x26).ok_or_else(short)? as usize;
    if in_use == NO_CLIENT || in_use >= log_clients {
        return Err(bad(format!(
            "in-use client {in_use:#x} of {log_clients}: a clean Windows log keeps NTFS's client in use"
        )));
    }
    if !(8..=60).contains(&bits) {
        return Err(bad(format!("{bits} sequence number bits")));
    }
    if header_len != RECORD_HEADER_BYTES
        || !data_offset.is_multiple_of(8)
        || data_offset + RECORD_HEADER_BYTES + CHECKPOINT_BODY_BYTES > lps
    {
        return Err(bad(format!(
            "record header of {header_len:#x} bytes at page offset {data_offset:#x} in \
             {lps:#x}-byte pages, which the checkpoint does not fit"
        )));
    }
    let usa_count = lps / LFS_STRIDE as usize + 1;
    if 0x28 + 2 * usa_count > data_offset {
        return Err(bad(format!(
            "page data offset {data_offset:#x} leaves no room for {usa_count} update sequence entries"
        )));
    }
    let first_record_page = 2 * sps as u64 + 2 * lps as u64;
    let home = if CHECKPOINT_HOME.is_multiple_of(lps as u64)
        && CHECKPOINT_HOME >= first_record_page
        && CHECKPOINT_HOME + lps as u64 <= file_size
    {
        CHECKPOINT_HOME
    } else {
        first_record_page
    };
    if home + lps as u64 > file_size {
        return Err(bad(format!(
            "a {file_size:#x}-byte log has no record page past its restart and tail pages"
        )));
    }
    // An LSN is the wrap count above the record's offset in 8-byte units.
    let low_bits = 64 - bits;
    let wrap = (past >> low_bits) + 1;
    if wrap >> bits != 0 {
        return Err(bad(format!(
            "LSN {past:#x} is in the last wrap {bits} sequence number bits can count"
        )));
    }
    let record_at = home + data_offset as u64;
    let lsn = (wrap << low_bits) | (record_at >> 3);
    debug_assert_eq!(lsn_to_offset(lsn, bits), Some(record_at));

    let mut record = vec![0u8; lps];
    record[..4].copy_from_slice(b"RCRD");
    record[4..6].copy_from_slice(&0x28u16.to_le_bytes());
    record[6..8].copy_from_slice(&(usa_count as u16).to_le_bytes());
    record[0x08..0x10].copy_from_slice(&lsn.to_le_bytes());
    // Both clean logs carry 3 here; format.com's and Windows'.
    record[0x10..0x14].copy_from_slice(&3u32.to_le_bytes());
    record[0x14..0x16].copy_from_slice(&1u16.to_le_bytes());
    record[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());
    let next_record = data_offset + RECORD_HEADER_BYTES + CHECKPOINT_BODY_BYTES;
    record[0x18..0x1A].copy_from_slice(&(next_record as u16).to_le_bytes());
    record[0x20..0x28].copy_from_slice(&lsn.to_le_bytes());
    let r = data_offset;
    record[r..r + 8].copy_from_slice(&lsn.to_le_bytes());
    record[r + 0x18..r + 0x1C].copy_from_slice(&(CHECKPOINT_BODY_BYTES as u32).to_le_bytes());
    record[r + 0x20..r + 0x24].copy_from_slice(&LFS_CLIENT_RESTART.to_le_bytes());
    let body = r + RECORD_HEADER_BYTES;
    record[body..body + 4].copy_from_slice(&1u32.to_le_bytes());
    let at = body + CHECKPOINT_BODY_CONSTANT_AT;
    record[at..at + 8].copy_from_slice(&CHECKPOINT_BODY_CONSTANT.to_le_bytes());
    let at = body + CHECKPOINT_BODY_CLUSTER_SIZE_AT;
    record[at..at + 8].copy_from_slice(&bytes_per_cluster.to_le_bytes());
    apply_fixup_on_write_magic(&mut record, LFS_STRIDE, b"RCRD")?;
    let mut records = vec![(home, record.clone())];
    for tail in [2 * sps as u64, 2 * sps as u64 + lps as u64] {
        // A tail copy names its home in the last-LSN field, which sits in
        // the first sector clear of its last two bytes, outside the update
        // sequence array's reach.
        let mut copy = record.clone();
        copy[0x08..0x10].copy_from_slice(&home.to_le_bytes());
        records.push((tail, copy));
    }

    page[RESTART_PAGE_MINOR_VERSION..RESTART_PAGE_MINOR_VERSION + 2]
        .copy_from_slice(&1u16.to_le_bytes());
    page[RESTART_PAGE_MAJOR_VERSION..RESTART_PAGE_MAJOR_VERSION + 2]
        .copy_from_slice(&1u16.to_le_bytes());
    page[ra..ra + 8].copy_from_slice(&lsn.to_le_bytes());
    let flags =
        u16_at(&page, ra + RESTART_AREA_FLAGS_OFFSET).ok_or_else(short)? | RESTART_VOLUME_IS_CLEAN;
    page[ra + RESTART_AREA_FLAGS_OFFSET..ra + RESTART_AREA_FLAGS_OFFSET + 2]
        .copy_from_slice(&flags.to_le_bytes());
    page[ra + 0x20..ra + 0x24].copy_from_slice(&(CHECKPOINT_BODY_BYTES as u32).to_le_bytes());
    let client = ra + client_array + in_use as usize * CLIENT_RECORD_BYTES;
    if client + 0x10 > sps {
        return Err(short());
    }
    // format.com's oldest LSN is the start of its checkpoint's wrap.
    page[client..client + 8].copy_from_slice(&(wrap << low_bits).to_le_bytes());
    page[client + 8..client + 0x10].copy_from_slice(&lsn.to_le_bytes());
    apply_fixup_on_write_magic(&mut page, LFS_STRIDE, b"RSTR")?;
    // The second page: the same restart area one update sequence number
    // behind, as Windows and mkfs write the pair.
    let mut second = page.clone();
    let usa = u16_at(&page, 4).ok_or_else(short)? as usize;
    let usn = u16_at(&page, usa).ok_or_else(short)?;
    let behind = if usn > 1 { usn - 1 } else { 0xFFFF };
    second[usa..usa + 2].copy_from_slice(&behind.to_le_bytes());
    for end in (LFS_STRIDE as usize..=sps).step_by(LFS_STRIDE as usize) {
        second[end - 2..end].copy_from_slice(&behind.to_le_bytes());
    }
    Ok(CleanLog {
        records,
        restart: vec![(sps as u64, second), (0, page)],
        current_lsn: lsn,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_all_ff_log_is_empty() {
        assert_eq!(state(&[0xFF; 8192]), LogfileState::Empty);
    }

    /// Over format.com's own log geometry, one wrap below its checkpoint,
    /// the clean log rebuilds format.com's checkpoint page byte for byte
    /// once fixups are read, at its LSN and its home (#762).
    #[test]
    fn a_clean_log_over_format_coms_geometry_is_format_coms_checkpoint() {
        const CANONICAL: &[u8] = include_bytes!("logfile-canonical-12k.bin");
        let mut log = vec![0xFFu8; 0x3b_0000];
        log[..CANONICAL.len()].copy_from_slice(CANONICAL);
        let clean = clean_log_past(
            &mut |off, len| {
                let start = usize::try_from(off).ok()?;
                log.get(start..start + len).map(<[u8]>::to_vec)
            },
            0x0F_FFFF,
            0x1000,
        )
        .expect("a clean log over format.com's geometry");
        assert_eq!(clean.current_lsn, 0x10_4408, "format.com's checkpoint LSN");
        let (home, page) = &clean.records[0];
        assert_eq!(*home, 0x22000, "format.com's checkpoint home");
        let mut ours = page.clone();
        apply_fixup_on_read_magic(&mut ours, LFS_STRIDE, b"RCRD").unwrap();
        let mut theirs = CANONICAL[0x2000..0x3000].to_vec();
        apply_fixup_on_read_magic(&mut theirs, LFS_STRIDE, b"RCRD").unwrap();
        // The update sequence numbers may differ. So may 0x3C..0x40, where
        // format.com's LFS 2.0 page also names its home offset; the LFS 1.1
        // page Windows leaves on a clean dismount has zero there, as here.
        for at in [0x28, 0x29, 0x3C, 0x3D, 0x3E, 0x3F] {
            ours[at] = 0;
            theirs[at] = 0;
        }
        let differ: Vec<usize> = (0..0x1000).filter(|&i| ours[i] != theirs[i]).collect();
        assert!(differ.is_empty(), "bytes differ at {differ:x?}");
        for (tail, (at, copy)) in [0x2000u64, 0x3000].iter().zip(&clean.records[1..]) {
            assert_eq!(at, tail);
            assert_eq!(
                u64_at(copy, 0x08),
                Some(0x22000),
                "a tail copy names its home"
            );
        }
        // Both restart pages name it, flagged clean, LFS 1.1.
        for (at, page) in &clean.restart {
            let mut page = page.clone();
            apply_fixup_on_read_magic(&mut page, LFS_STRIDE, b"RSTR").unwrap();
            let ra = u16_at(&page, RESTART_AREA_OFFSET_FIELD).unwrap() as usize;
            assert_eq!(u64_at(&page, ra), Some(0x10_4408), "restart page {at:#x}");
            assert_ne!(
                u16_at(&page, ra + 0x0E).unwrap() & RESTART_VOLUME_IS_CLEAN,
                0
            );
            assert_eq!(
                (u16_at(&page, 0x1A), u16_at(&page, 0x1C)),
                (Some(1), Some(1))
            );
        }
        // And this crate's reader agrees the log holds nothing to replay.
        for (at, page) in clean.records.iter().chain(&clean.restart) {
            log[*at as usize..*at as usize + page.len()].copy_from_slice(page);
        }
        assert!(!state(&log).needs_replay(), "{:?}", state(&log));
    }

    #[test]
    fn the_highest_lsn_is_the_one_windows_last_logged() {
        let out = std::process::Command::new("gzip")
            .args(["-dc", "test-disks/windows-clean-logfile.bin.gz"])
            .output()
            .expect("run gzip");
        assert!(out.status.success());
        assert_eq!(highest_lsn(&out.stdout), 0x18_4415);
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
