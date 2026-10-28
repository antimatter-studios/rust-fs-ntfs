//! Writing `$LogFile`: recording a metadata change the way NTFS records it,
//! so that a restart can redo it, or undo it, whatever reached the disk
//! (#581).
//!
//! The first slice of a native journal writer: **one change to a resident
//! value in an MFT record**, logged as Windows logs one -- an
//! `UpdateResidentValue` record holding the new bytes (redo) and the old
//! (undo), then a `ForgetTransaction` that ends its transaction -- and
//! appended, in memory, after the last record of an LFS 2.x log.
//! [`append_resident_update`] returns the LSN the changed record has to
//! carry once the change itself is written home.
//!
//! # What is established, and how
//!
//! * Both records are byte for byte what Windows writes. Every
//!   `UpdateResidentValue` Windows wrote whole, and every
//!   `ForgetTransaction`, on the record pages of
//!   `test-disks/windows-interrupted-logfile-1` is encoded again from its
//!   fields and compared with Windows' bytes (`tests` below).
//! * Appended to the log of `test-disks/windows-interrupted-1`, the change
//!   is redone by this crate's replay ([`crate::logfile_replay::plan`]):
//!   the record it names is written with the new bytes and the update's
//!   LSN, and every other write that replay makes is unchanged
//!   (`tests/logfile_write.rs`). That is this crate's reader agreeing with
//!   this crate's writer. Windows' restart recovering a log this module
//!   wrote is not shown yet (#581).
//!
//! # The layout, as the Windows records show it
//!
//! * The client data starts with a 0x20-byte header, then one LCN per
//!   cluster of the page the record changes. A record that names no page
//!   still has room for one, filled with ones.
//! * The redo data follows the LCNs; the undo data follows the redo data,
//!   8-aligned; the client data ends 8-aligned after the undo data.
//! * An `UpdateResidentValue` record's undo-next LSN is its previous LSN:
//!   undoing it leads to the record before it in its transaction.
//! * A `ForgetTransaction` has `CompensationLogRecord` for its undo, no
//!   undo-next LSN, flag `0x2` in its record header, and `0x18` and `2` in
//!   the target-attribute and block-size fields, which replay does not
//!   read for it. Windows leaves the record and attribute offsets of a few
//!   of them uninitialised; this writes zeros there, as it does in most.
//! * A transaction's id is the offset of its entry in the transaction
//!   table: `0x18` for the first, then one 0x28-byte entry after another.
//!
//! # What is not written yet, and refused (#581)
//!
//! * A log that is not LFS 2.x. Windows leaves a cleanly dismounted
//!   volume's log at LFS 1.x, flagged clean, and writes a 2.x log with a
//!   fresh checkpoint when it mounts the volume. That handoff is not
//!   written yet, so this cannot yet journal a change to a volume Windows
//!   detached cleanly.
//! * A log whose restart area marks the volume clean: a restart would not
//!   read the records after it.
//! * A log in which `$MFT`'s `$DATA` is not an open attribute: the
//!   `OpenNonresidentAttribute` record that opens it is not written yet.
//! * A record that does not fit in what is left of its log page, and a
//!   log that would wrap past its end.
//! * Device I/O. The log is changed in memory only: writing its pages in
//!   order, through the tail copies that protect a page being rewritten,
//!   and flushing them before the change itself is written, are later
//!   slices.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::logfile::{CleanBecause, LogfileState};
use crate::logfile_replay::{
    read_log, redo_data, u16_at, u32_at, u64_at, Log, COMPENSATION, ENTRY_ALLOCATED,
    FORGET_TRANSACTION, LFS_CLIENT_RECORD, LFS_CLIENT_RESTART, LFS_STRIDE, OPEN_ATTRIBUTE_ENTRY,
    OPEN_ATTRIBUTE_TABLE_DUMP, OPEN_NONRESIDENT_ATTRIBUTE, PAGE_RECORD_END, RECORD_HEADER,
    TRANSACTION_ENTRY, TRANSACTION_TABLE_DUMP, UPDATE_RESIDENT_VALUE,
};
use crate::mft_io::apply_fixup_on_write_magic;

/// The NTFS client header that starts a client record's data, before its
/// LCNs.
const CLIENT_HEADER: usize = 0x20;
/// The first transaction table entry's offset, the first transaction id.
const FIRST_TRANSACTION: u32 = 0x18;
/// Record header flag Windows sets on every `ForgetTransaction`.
const FORGET_FLAGS: u16 = 0x2;
/// What Windows writes in a `ForgetTransaction`'s target-attribute and
/// block-size fields, on every one the captures hold.
const FORGET_TARGET_ATTRIBUTE: u16 = 0x18;
const FORGET_BLOCK_SECTORS: u16 = 2;
/// `$MFT`'s `$DATA`: attribute type and file record number.
const MFT_DATA: (u32, u64) = (0x80, 0);

/// A change to a resident value in an MFT record: the fields of the
/// `UpdateResidentValue` record that logs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentUpdate {
    /// The VCN, in `$MFT`'s `$DATA`, of the first cluster of the page
    /// that holds the record.
    pub target_vcn: u64,
    /// The LCN of each cluster of that page from `target_vcn`, as far as
    /// the record reaches.
    pub lcns: Vec<u64>,
    /// Where the record starts in the page, in 512-byte units.
    pub cluster_block_offset: u16,
    /// The MFT record's size in bytes.
    pub record_size: u32,
    /// The attribute's offset in the record.
    pub record_offset: u16,
    /// The change's offset in the attribute (not in its value).
    pub attribute_offset: u16,
    /// The bytes written there.
    pub redo: Vec<u8>,
    /// The bytes they replace.
    pub undo: Vec<u8>,
}

/// What [`append_resident_update`] appended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journaled {
    /// The transaction's id: its entry's offset in the transaction table.
    pub transaction: u32,
    /// The `UpdateResidentValue` record's LSN: the LSN the changed MFT
    /// record carries once the change is written home.
    pub update: u64,
    /// The `ForgetTransaction` record's LSN, now the log's last.
    pub forget: u64,
    /// The byte offset in the log of each page written.
    pub pages: Vec<usize>,
}

fn unwritten(why: impl std::fmt::Display) -> Error {
    Error::io(format!(
        "$LogFile cannot record this change ({why}), so nothing was written to it \
         (rust-fs-ntfs#581)"
    ))
}

fn put(b: &mut [u8], at: usize, bytes: &[u8]) {
    b[at..at + bytes.len()].copy_from_slice(bytes);
}

/// One client record: the LFS record header and the NTFS client data.
struct Client<'a> {
    redo_op: u16,
    undo_op: u16,
    target_attribute: u16,
    lcns: &'a [u64],
    record_offset: u16,
    attribute_offset: u16,
    cluster_block_offset: u16,
    block_sectors: u16,
    target_vcn: u64,
    redo: &'a [u8],
    undo: &'a [u8],
}

/// The client data of `c`, laid out as Windows lays it out.
fn client_data(c: &Client<'_>) -> Result<Vec<u8>, Error> {
    let redo_offset = CLIENT_HEADER + 8 * c.lcns.len().max(1);
    let undo_offset = redo_offset + c.redo.len().div_ceil(8) * 8;
    let len = (undo_offset + c.undo.len()).div_ceil(8) * 8;
    let field = |n: usize, what: &str| {
        u16::try_from(n).map_err(|_| unwritten(format!("{what} of {n} bytes")))
    };
    let mut d = vec![0u8; len];
    put(&mut d, 0x00, &c.redo_op.to_le_bytes());
    put(&mut d, 0x02, &c.undo_op.to_le_bytes());
    put(
        &mut d,
        0x04,
        &field(redo_offset, "a redo offset")?.to_le_bytes(),
    );
    put(
        &mut d,
        0x06,
        &field(c.redo.len(), "redo data")?.to_le_bytes(),
    );
    put(
        &mut d,
        0x08,
        &field(undo_offset, "an undo offset")?.to_le_bytes(),
    );
    put(
        &mut d,
        0x0A,
        &field(c.undo.len(), "undo data")?.to_le_bytes(),
    );
    put(&mut d, 0x0C, &c.target_attribute.to_le_bytes());
    put(&mut d, 0x0E, &field(c.lcns.len(), "a page")?.to_le_bytes());
    put(&mut d, 0x10, &c.record_offset.to_le_bytes());
    put(&mut d, 0x12, &c.attribute_offset.to_le_bytes());
    put(&mut d, 0x14, &c.cluster_block_offset.to_le_bytes());
    put(&mut d, 0x16, &c.block_sectors.to_le_bytes());
    put(&mut d, 0x18, &c.target_vcn.to_le_bytes());
    if c.lcns.is_empty() {
        d[CLIENT_HEADER..CLIENT_HEADER + 8].fill(0xFF);
    }
    for (i, lcn) in c.lcns.iter().enumerate() {
        put(&mut d, CLIENT_HEADER + 8 * i, &lcn.to_le_bytes());
    }
    put(&mut d, redo_offset, c.redo);
    put(&mut d, undo_offset, c.undo);
    Ok(d)
}

/// The LFS record header for client data `data`, then the data.
fn record(
    lsn: u64,
    previous: u64,
    undo_next: u64,
    client: [u8; 4],
    transaction: u32,
    flags: u16,
    data: &[u8],
) -> Vec<u8> {
    let mut r = vec![0u8; RECORD_HEADER];
    put(&mut r, 0x00, &lsn.to_le_bytes());
    put(&mut r, 0x08, &previous.to_le_bytes());
    put(&mut r, 0x10, &undo_next.to_le_bytes());
    put(&mut r, 0x18, &(data.len() as u32).to_le_bytes());
    put(&mut r, 0x1C, &client);
    put(&mut r, 0x20, &LFS_CLIENT_RECORD.to_le_bytes());
    put(&mut r, 0x24, &transaction.to_le_bytes());
    put(&mut r, 0x28, &flags.to_le_bytes());
    r.extend_from_slice(data);
    r
}

/// The `UpdateResidentValue` record logging `u` at `lsn`, after `previous`
/// in its transaction (0 for its first record).
fn update_resident_value(
    lsn: u64,
    previous: u64,
    client: [u8; 4],
    transaction: u32,
    target_attribute: u16,
    u: &ResidentUpdate,
) -> Result<Vec<u8>, Error> {
    if u.lcns.is_empty() {
        return Err(unwritten("a resident update that names no cluster"));
    }
    if u.record_size == 0 || !u.record_size.is_multiple_of(512) {
        return Err(unwritten(format!(
            "an MFT record of {} bytes",
            u.record_size
        )));
    }
    let block_sectors = u16::try_from(u.record_size / 512)
        .map_err(|_| unwritten(format!("an MFT record of {} bytes", u.record_size)))?;
    let data = client_data(&Client {
        redo_op: UPDATE_RESIDENT_VALUE,
        undo_op: UPDATE_RESIDENT_VALUE,
        target_attribute,
        lcns: &u.lcns,
        record_offset: u.record_offset,
        attribute_offset: u.attribute_offset,
        cluster_block_offset: u.cluster_block_offset,
        block_sectors,
        target_vcn: u.target_vcn,
        redo: &u.redo,
        undo: &u.undo,
    })?;
    Ok(record(
        lsn,
        previous,
        previous,
        client,
        transaction,
        0,
        &data,
    ))
}

/// The `ForgetTransaction` record ending `transaction` at `lsn`, after
/// its last record, `previous`.
fn forget_transaction(lsn: u64, previous: u64, client: [u8; 4], transaction: u32) -> Vec<u8> {
    let data = client_data(&Client {
        redo_op: FORGET_TRANSACTION,
        undo_op: COMPENSATION,
        target_attribute: FORGET_TARGET_ATTRIBUTE,
        lcns: &[],
        record_offset: 0,
        attribute_offset: 0,
        cluster_block_offset: 0,
        block_sectors: FORGET_BLOCK_SECTORS,
        target_vcn: 0,
        redo: &[],
        undo: &[],
    })
    .expect("a ForgetTransaction holds no data to overflow a field");
    record(lsn, previous, 0, client, transaction, FORGET_FLAGS, &data)
}

/// The allocated entries of the restart table the record at `lsn` dumps,
/// by their offset in the table.
fn table(l: &Log<'_>, lsn: u64, op: u16) -> Result<Vec<(usize, Vec<u8>)>, Error> {
    let (_, data, _) = l.record(lsn)?;
    if u16_at(&data, 0)? != op {
        return Err(unwritten(format!(
            "the checkpoint's table at LSN {lsn:#x} is not of operation {op:#x}"
        )));
    }
    let (off, len) = (u16_at(&data, 4)? as usize, u16_at(&data, 6)? as usize);
    let t = data
        .get(off..off + len)
        .ok_or_else(|| unwritten(format!("the table at LSN {lsn:#x} is truncated")))?;
    let (size, count) = (u16_at(t, 0)? as usize, u16_at(t, 2)? as usize);
    let mut out = Vec::new();
    for i in 0..count {
        let at = 0x18 + i * size;
        let e = t
            .get(at..at + size)
            .ok_or_else(|| unwritten(format!("an entry past the table at LSN {lsn:#x}")))?;
        if u32_at(e, 0)? == ENTRY_ALLOCATED {
            out.push((at, e.to_vec()));
        }
    }
    Ok(out)
}

/// What a restart would know at the end of the log: the open attributes,
/// by their offset in the open attribute table, as (type, file record);
/// the transactions not forgotten; the client id the log's records carry;
/// and the LSN after the last record.
struct Analysis {
    open: BTreeMap<u16, (u32, u64)>,
    transactions: BTreeSet<u32>,
    client: [u8; 4],
    after: u64,
}

/// Analysis, from the last checkpoint's start to the last record, as
/// [`crate::logfile_replay::plan`] performs it.
fn analyse(l: &Log<'_>, checkpoint: u64) -> Result<Analysis, Error> {
    let (header, body, _) = l.record(checkpoint)?;
    if u32_at(&header, 0x20)? != LFS_CLIENT_RESTART || body.len() < 0x30 {
        return Err(unwritten(format!(
            "the checkpoint at LSN {checkpoint:#x} is not one"
        )));
    }
    let mut client = [0u8; 4];
    client.copy_from_slice(&header[0x1C..0x20]);
    let start = match u64_at(&body, 0x08)? {
        0 => checkpoint,
        s => s,
    };
    let mut open = BTreeMap::new();
    let dump = u64_at(&body, 0x10)?;
    if dump != 0 {
        for (at, e) in table(l, dump, OPEN_ATTRIBUTE_TABLE_DUMP)? {
            if e.len() != OPEN_ATTRIBUTE_ENTRY {
                return Err(unwritten(format!(
                    "open attribute entries of {} bytes",
                    e.len()
                )));
            }
            open.insert(
                at as u16,
                (u32_at(&e, 0x08)?, u64_at(&e, 0x10)? & 0xFFFF_FFFF_FFFF),
            );
        }
    }
    let mut transactions = BTreeSet::new();
    let dump = u64_at(&body, 0x28)?;
    if dump != 0 {
        for (at, _) in table(l, dump, TRANSACTION_TABLE_DUMP)? {
            transactions.insert(at as u32);
        }
    }
    let mut lsn = start;
    let after = loop {
        let (header, data, next) = l.record(lsn)?;
        if u32_at(&header, 0x20)? == LFS_CLIENT_RECORD {
            let transaction = u32_at(&header, 0x24)?;
            let redo = u16_at(&data, 0)?;
            if redo == FORGET_TRANSACTION {
                transactions.remove(&transaction);
            } else {
                transactions.insert(transaction);
            }
            if redo == OPEN_NONRESIDENT_ATTRIBUTE {
                let e = redo_data(&header, &data)?;
                if e.len() < OPEN_ATTRIBUTE_ENTRY {
                    return Err(unwritten(format!(
                        "a short open attribute entry at LSN {lsn:#x}"
                    )));
                }
                open.insert(
                    u16_at(&data, 0x0C)?,
                    (u32_at(&e, 0x08)?, u64_at(&e, 0x10)? & 0xFFFF_FFFF_FFFF),
                );
            }
        }
        if lsn >= l.newest {
            break next;
        }
        if next <= lsn {
            return Err(unwritten(format!(
                "the record after LSN {lsn:#x} goes backwards"
            )));
        }
        lsn = next;
    };
    if lsn != l.newest {
        return Err(unwritten(format!(
            "the records stop at LSN {lsn:#x}, not at the last one, {:#x}",
            l.newest
        )));
    }
    Ok(Analysis {
        open,
        transactions,
        client,
        after,
    })
}

/// Where a record of `len` bytes at `lsn` goes -- its page's offset in
/// the log and its own in the page -- and the LSN of the record after it,
/// placed as [`crate::logfile_replay`] reads them.
fn place(l: &Log<'_>, log_len: usize, lsn: u64, len: usize) -> Result<(usize, usize, u64), Error> {
    let (lps, data_offset) = (l.log_page, l.data_offset);
    let at = l.offset(lsn);
    let (home, within) = (at - at % lps, at % lps);
    if within < data_offset || home + lps > log_len {
        return Err(unwritten(format!(
            "LSN {lsn:#x} is outside the log's record pages"
        )));
    }
    if within + len > lps {
        return Err(unwritten(format!(
            "a record of {len} bytes at {within:#x} runs past its {lps}-byte log page, and \
             records across pages are not written yet"
        )));
    }
    let mut next = home + (within + len).div_ceil(8) * 8;
    if lps - next % lps < RECORD_HEADER {
        next = next - next % lps + lps;
    }
    if next >= log_len {
        return Err(unwritten(
            "the log would wrap past its end, which is not written yet",
        ));
    }
    if next.is_multiple_of(lps) {
        next += data_offset;
    }
    Ok((home, within, l.lsn(l.wrap(lsn), next)))
}

/// Record `update` -- a change to a resident value in an MFT record -- in
/// `log`, the whole of `$LogFile`'s `$DATA`, after its last record: an
/// `UpdateResidentValue` in a transaction of its own, ended by a
/// `ForgetTransaction`. In memory: nothing is written to a device.
///
/// Either both records are appended, and read back, or `log` is left as
/// it was and the refusal says why (see the module documentation for what
/// is refused).
pub fn append_resident_update(log: &mut [u8], update: &ResidentUpdate) -> Result<Journaled, Error> {
    if let LogfileState::Clean(CleanBecause::MarkedClean) = crate::logfile::state(log) {
        return Err(unwritten(
            "the restart area marks the volume clean, so a restart would not read the log",
        ));
    }
    let (journaled, pages) = {
        let (l, checkpoint) =
            read_log(log).map_err(|e| unwritten(format!("the log does not read: {e}")))?;
        let checkpoint = checkpoint.ok_or_else(|| unwritten("no client has the log open"))?;
        let a = analyse(&l, checkpoint)?;
        if l.wrap(a.after) != l.wrap(l.newest) {
            return Err(unwritten(
                "the log would wrap past its end, which is not written yet",
            ));
        }
        let target = a
            .open
            .iter()
            .find(|&(_, &open)| open == MFT_DATA)
            .map(|(&at, _)| at)
            .ok_or_else(|| {
                unwritten("$MFT's $DATA is not open in the log, and opening it is not written yet")
            })?;
        let transaction = (0..)
            .map(|k| FIRST_TRANSACTION + k * TRANSACTION_ENTRY as u32)
            .find(|t| !a.transactions.contains(t))
            .expect("a transaction table has a free entry");

        let update_lsn = a.after;
        let first = update_resident_value(update_lsn, 0, a.client, transaction, target, update)?;
        let (_, _, forget_lsn) = place(&l, log.len(), update_lsn, first.len())?;
        let second = forget_transaction(forget_lsn, update_lsn, a.client, transaction);

        // The page the last record starts on: the header every page this
        // writes is modelled on.
        let newest_at = l.offset(l.newest);
        let template = l.page(newest_at - newest_at % l.log_page, l.newest)?;
        let usa = u16_at(template, 0x04)? as usize;
        let mut pages: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for (lsn, bytes) in [(update_lsn, &first), (forget_lsn, &second)] {
            let (home, within, _) = place(&l, log.len(), lsn, bytes.len())?;
            let page = match pages.entry(home) {
                Entry::Occupied(e) => e.into_mut(),
                // The page the last record ends on, records before this
                // one on it already.
                Entry::Vacant(e) if within > l.data_offset => {
                    e.insert(l.page(home, l.newest)?.to_vec())
                }
                Entry::Vacant(e) => {
                    let mut page = vec![0u8; l.log_page];
                    page[..0x08].copy_from_slice(&template[..0x08]);
                    page[0x10..0x14].copy_from_slice(&template[0x10..0x14]);
                    page[usa..usa + 2].copy_from_slice(&template[usa..usa + 2]);
                    e.insert(page)
                }
            };
            put(page, within, bytes);
            // Its last record, the last to end on it, where the next one
            // starts, and that it went out in an I/O of its own.
            let flags = u32_at(page, 0x10)? | PAGE_RECORD_END;
            put(page, 0x08, &lsn.to_le_bytes());
            put(page, 0x10, &flags.to_le_bytes());
            put(page, 0x14, &1u16.to_le_bytes());
            put(page, 0x16, &1u16.to_le_bytes());
            put(page, 0x18, &((within + bytes.len()) as u16).to_le_bytes());
            put(page, 0x20, &lsn.to_le_bytes());
        }
        let journaled = Journaled {
            transaction,
            update: update_lsn,
            forget: forget_lsn,
            pages: pages.keys().copied().collect(),
        };
        (journaled, pages)
    };

    let mut out = log.to_vec();
    for (home, mut page) in pages {
        apply_fixup_on_write_magic(&mut page, LFS_STRIDE, b"RCRD")
            .map_err(|e| unwritten(format!("the log page at {home:#x}: {e}")))?;
        out[home..home + page.len()].copy_from_slice(&page);
    }
    // Read back as a restart reads it: the update follows the old last
    // record, and the forget is the new last.
    let (l, _) = read_log(&out).map_err(|e| unwritten(format!("the log written: {e}")))?;
    let (_, _, next) = l.record(journaled.update)?;
    if next != journaled.forget || l.newest != journaled.forget {
        return Err(unwritten(format!(
            "the log written ends at LSN {:#x}, not at {:#x}",
            l.newest, journaled.forget
        )));
    }
    log.copy_from_slice(&out);
    Ok(journaled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `$LogFile` Windows wrote, from `test-disks`, checked by hash.
    fn windows_log(name: &str, sha: &str) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let path = format!("test-disks/{name}.bin.gz");
        let out = std::process::Command::new("gzip")
            .args(["-dc", &path])
            .output()
            .expect("install gzip to read the Windows captures");
        assert!(out.status.success(), "gzip -dc {path}: {:?}", out.stderr);
        assert_eq!(format!("{:x}", Sha256::digest(&out.stdout)), sha, "{path}");
        out.stdout
    }

    fn interrupted_logfile_1() -> Vec<u8> {
        windows_log(
            "windows-interrupted-logfile-1",
            "e6a36b92ace6fa15378af53d9239b1df0aacdf931e4c721d10ce287259d638eb",
        )
    }

    /// Every client record whose header and data lie whole on a record
    /// page, at its own place in the log: (LSN, header, client data), as
    /// Windows wrote them.
    fn whole_records(log: &[u8]) -> Vec<(u64, Vec<u8>, Vec<u8>)> {
        let (l, _) = read_log(log).unwrap();
        let lps = l.log_page;
        let mut out = Vec::new();
        for (&at, copies) in &l.copies {
            for (_, at_home, page) in copies {
                if !at_home {
                    continue;
                }
                for w in (l.data_offset..lps - RECORD_HEADER).step_by(8) {
                    let lsn = u64_at(page, w).unwrap();
                    if lsn == 0 || l.offset(lsn) != at + w {
                        continue;
                    }
                    let len = u32_at(page, w + 0x18).unwrap() as usize;
                    if u32_at(page, w + 0x20).unwrap() != LFS_CLIENT_RECORD
                        || len < CLIENT_HEADER
                        || w + RECORD_HEADER + len > lps
                    {
                        continue;
                    }
                    out.push((
                        lsn,
                        page[w..w + RECORD_HEADER].to_vec(),
                        page[w + RECORD_HEADER..w + RECORD_HEADER + len].to_vec(),
                    ));
                }
            }
        }
        out
    }

    fn client_of(header: &[u8]) -> [u8; 4] {
        header[0x1C..0x20].try_into().unwrap()
    }

    #[test]
    fn an_update_resident_value_record_is_byte_for_byte_what_windows_writes() {
        let log = interrupted_logfile_1();
        let (mut seen, mut uninitialised) = (0, 0);
        for (lsn, header, data) in whole_records(&log) {
            // Flag 0x4 marks a record Windows left resident file data out
            // of, which this writer never does.
            if u16_at(&data, 0).unwrap() != UPDATE_RESIDENT_VALUE
                || u16_at(&header, 0x28).unwrap() != 0
            {
                continue;
            }
            let field = |at| u16_at(&data, at).unwrap() as usize;
            let (redo, undo) = (
                data[field(0x04)..field(0x04) + field(0x06)].to_vec(),
                data[field(0x08)..field(0x08) + field(0x0A)].to_vec(),
            );
            let mut want = [header.clone(), data.clone()].concat();
            let padding = [
                field(0x04) + field(0x06)..field(0x08),
                field(0x08) + field(0x0A)..data.len(),
            ];
            if padding
                .iter()
                .any(|p| data[p.clone()].iter().any(|&b| b != 0))
            {
                // Alignment after the values Windows left uninitialised:
                // the rest of the record is still compared.
                for p in padding {
                    want[RECORD_HEADER + p.start..RECORD_HEADER + p.end].fill(0);
                }
                uninitialised += 1;
            } else {
                seen += 1;
            }
            let update = ResidentUpdate {
                target_vcn: u64_at(&data, 0x18).unwrap(),
                lcns: (0..field(0x0E))
                    .map(|i| u64_at(&data, CLIENT_HEADER + 8 * i).unwrap())
                    .collect(),
                cluster_block_offset: field(0x14) as u16,
                record_size: field(0x16) as u32 * 512,
                record_offset: field(0x10) as u16,
                attribute_offset: field(0x12) as u16,
                redo,
                undo,
            };
            let ours = update_resident_value(
                lsn,
                u64_at(&header, 0x08).unwrap(),
                client_of(&header),
                u32_at(&header, 0x24).unwrap(),
                field(0x0C) as u16,
                &update,
            )
            .unwrap();
            assert_eq!(ours, want, "LSN {lsn:#x}");
        }
        // Counted independently of this crate, by a Python walk of the
        // same pages with the same rule.
        assert_eq!(
            (seen, uninitialised),
            (656, 33),
            "UpdateResidentValue records compared"
        );
    }

    #[test]
    fn a_forget_transaction_record_is_byte_for_byte_what_windows_writes() {
        let log = interrupted_logfile_1();
        let (mut seen, mut uninitialised) = (0, 0);
        for (lsn, header, data) in whole_records(&log) {
            if u16_at(&data, 0).unwrap() != FORGET_TRANSACTION {
                continue;
            }
            let mut want = [header.clone(), data.clone()].concat();
            if data[0x10..0x14] != [0; 4] {
                // Record and attribute offsets Windows left uninitialised:
                // the rest of the record is still compared.
                want[RECORD_HEADER + 0x10..RECORD_HEADER + 0x14].fill(0);
                uninitialised += 1;
            } else {
                seen += 1;
            }
            let ours = forget_transaction(
                lsn,
                u64_at(&header, 0x08).unwrap(),
                client_of(&header),
                u32_at(&header, 0x24).unwrap(),
            );
            assert_eq!(ours, want, "LSN {lsn:#x}");
        }
        assert_eq!(
            (seen, uninitialised),
            (2689, 2),
            "ForgetTransaction records compared"
        );
    }

    fn update() -> ResidentUpdate {
        // A `$STANDARD_INFORMATION` modification time, where the first
        // `UpdateResidentValue` in the capture changes one.
        ResidentUpdate {
            target_vcn: 0x12,
            lcns: vec![0x2a62],
            cluster_block_offset: 4,
            record_size: 1024,
            record_offset: 0x38,
            attribute_offset: 0x20,
            redo: vec![0x11; 8],
            undo: vec![0x22; 8],
        }
    }

    #[test]
    fn an_update_is_appended_after_a_windows_log_s_last_record_and_reads_back() {
        let before = interrupted_logfile_1();
        let mut log = before.clone();
        let j = append_resident_update(&mut log, &update()).unwrap();

        let (old, _) = read_log(&before).unwrap();
        let (l, _) = read_log(&log).unwrap();
        assert_eq!(l.newest, j.forget);
        // The old last record is as it was, and the update follows it.
        let (h0, d0, next0) = old.record(old.newest).unwrap();
        let (h1, d1, next1) = l.record(old.newest).unwrap();
        assert_eq!((h1, d1), (h0, d0));
        assert_eq!((next1, next0), (j.update, j.update));
        // Transaction 0x40 is still open at the end of this log; 0x18 is
        // the first free entry.
        assert_eq!(j.transaction, 0x18);

        let (h, d, next) = l.record(j.update).unwrap();
        assert_eq!(next, j.forget);
        assert_eq!(u32_at(&h, 0x20).unwrap(), LFS_CLIENT_RECORD);
        assert_eq!(u32_at(&h, 0x24).unwrap(), j.transaction);
        assert_eq!(
            (u64_at(&h, 0x08).unwrap(), u64_at(&h, 0x10).unwrap()),
            (0, 0)
        );
        assert_eq!(
            (u16_at(&d, 0).unwrap(), u16_at(&d, 2).unwrap()),
            (UPDATE_RESIDENT_VALUE, UPDATE_RESIDENT_VALUE)
        );
        // `$MFT`'s `$DATA`, opened after the checkpoint, at 0x18.
        assert_eq!(u16_at(&d, 0x0C).unwrap(), 0x18);
        assert_eq!(redo_data(&h, &d).unwrap(), vec![0x11; 8]);

        let (h, d, _) = l.record(j.forget).unwrap();
        assert_eq!(u32_at(&h, 0x24).unwrap(), j.transaction);
        assert_eq!(
            (u64_at(&h, 0x08).unwrap(), u64_at(&h, 0x10).unwrap()),
            (j.update, 0)
        );
        assert_eq!(u16_at(&d, 0).unwrap(), FORGET_TRANSACTION);

        // Nothing but the pages it names changed; the restart area did not.
        let lps = l.log_page;
        for at in (0..log.len()).step_by(lps) {
            if !j.pages.contains(&at) {
                assert_eq!(log[at..at + lps], before[at..at + lps], "page {at:#x}");
            }
        }
        assert_eq!(j.pages.len(), 1, "both records fit the last record's page");
        assert!(crate::logfile::state(&log).needs_replay());
    }

    #[test]
    fn what_is_not_written_yet_is_refused_and_the_log_left_as_it_was() {
        let before = interrupted_logfile_1();

        // A record larger than a log page.
        let mut log = before.clone();
        let big = ResidentUpdate {
            redo: vec![0x11; 4000],
            undo: vec![0x22; 4000],
            ..update()
        };
        let err = append_resident_update(&mut log, &big).unwrap_err();
        assert!(
            err.contains("records across pages") && err.ends_with("(rust-fs-ntfs#581)"),
            "{err}"
        );
        assert!(log == before);

        // A record named by no cluster, and one of no whole sector.
        for bad in [
            ResidentUpdate {
                lcns: vec![],
                ..update()
            },
            ResidentUpdate {
                record_size: 1000,
                ..update()
            },
        ] {
            let err = append_resident_update(&mut log, &bad).unwrap_err();
            assert!(err.ends_with("(rust-fs-ntfs#581)"), "{err}");
            assert!(log == before);
        }

        // An LFS 1.x log, as Windows leaves a volume it detached cleanly.
        let clean = windows_log(
            "windows-clean-logfile",
            "b5b8d7ed7dacde1c0a6a7304d8d13038706efedd78a066fc67f5c3c624ba8a54",
        );
        let mut log = clean.clone();
        let err = append_resident_update(&mut log, &update()).unwrap_err();
        assert!(err.ends_with("(rust-fs-ntfs#581)"), "{err}");
        assert!(log == clean);

        // An empty log.
        let mut log = vec![0xFF; 65536];
        assert!(append_resident_update(&mut log, &update()).is_err());
        assert!(log == vec![0xFF; 65536]);
    }
}
