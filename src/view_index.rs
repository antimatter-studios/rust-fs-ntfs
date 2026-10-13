//! Entries of the `$Extend` view indexes that follow a file's attributes:
//! `$Extend\$Reparse:$R`, one per reparse point, and `$Extend\$ObjId:$O`,
//! one per object id (#478).
//!
//! A view index is an index whose entries carry a key and data of their
//! own rather than a `$FILE_NAME` (MS-FSCC §2.4). Both of these collate
//! `COLLATION_NTOFS_ULONGS`: the key compared as an array of little-endian
//! 32-bit words, word by word. Their entries, as Windows writes them
//! (measured on `test-disks/windows-interrupted-quota.recovered.img`):
//!
//! - `$R`: key = reparse tag (u32) + the file's MFT reference (u64),
//!   12 bytes; no data. `data_offset` 0x1C, `entry_length` 0x20.
//! - `$O`: key = the 16-byte object id; data = the file's MFT reference
//!   (u64) + birth volume id + birth object id + domain id (16 bytes
//!   each), 56 bytes. `data_offset` 0x20, `entry_length` 0x58. A file
//!   with the 16-byte form of `$OBJECT_ID` has a birth volume and domain
//!   id of zero and a birth object id equal to its object id.
//!
//! An entry is put only into the resident `$INDEX_ROOT`: an index that has
//! spilled into INDX blocks, or a root with no room for the entry, refuses
//! an insert before anything is written (#571). An entry is taken out
//! wherever it is, the root or an INDX block ([`remove_io`]): a leaf's
//! entry is cut from its node, and an interior one is replaced by its
//! neighbour as `$I30`'s are, by the same code.

use crate::attr_io::{self, AttrType};
use crate::block_io::BlockIo;
use crate::error::Error;
use crate::index_io::{NodeEntry, NodeKeys};
use std::cmp::Ordering;

/// `$Extend\$Reparse`'s index.
pub const REPARSE_PATH: &str = "/$Extend/$Reparse";
pub const REPARSE_INDEX: &str = "$R";
/// `$Extend\$ObjId`'s index.
pub const OBJID_PATH: &str = "/$Extend/$ObjId";
pub const OBJID_INDEX: &str = "$O";

const IR_INDEX_HEADER_OFFSET: usize = 0x10;
const INDEX_HEADER_SIZE: usize = 0x10;
const IH_FIRST_ENTRY: usize = 0x00;
const IH_TOTAL_SIZE: usize = 0x04;
const IH_ALLOCATED_SIZE: usize = 0x08;
const IH_FLAGS: usize = 0x0C;
const IH_FLAG_HAS_SUBNODES: u8 = 0x01;
const IE_LENGTH: usize = 0x08;
const IE_KEY_LENGTH: usize = 0x0A;
const IE_FLAGS: usize = 0x0C;
const IE_KEY_START: usize = 0x10;
const IE_FLAG_LAST: u16 = 0x02;

/// `$R`'s key for a reparse point of `tag` on the file `file_reference`.
pub fn reparse_key(tag: u32, file_reference: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(12);
    key.extend_from_slice(&tag.to_le_bytes());
    key.extend_from_slice(&file_reference.to_le_bytes());
    key
}

/// `$O`'s data for the file `file_reference` carrying `object_id`, with
/// the birth ids its `$OBJECT_ID` holds, or those Windows gives the
/// 16-byte form when it holds none.
pub fn objid_data(file_reference: u64, object_id: &[u8; 16], births: Option<&[u8; 48]>) -> Vec<u8> {
    let mut data = Vec::with_capacity(56);
    data.extend_from_slice(&file_reference.to_le_bytes());
    match births {
        Some(b) => data.extend_from_slice(b),
        None => {
            data.extend_from_slice(&[0u8; 16]);
            data.extend_from_slice(object_id);
            data.extend_from_slice(&[0u8; 16]);
        }
    }
    data
}

/// `COLLATION_NTOFS_ULONGS`: the keys as little-endian 32-bit words,
/// compared word by word; a key that is a prefix of the other sorts first.
pub fn ulongs_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let words = |k: &[u8]| -> Vec<u32> {
        k.chunks(4)
            .map(|c| {
                let mut w = [0u8; 4];
                w[..c.len()].copy_from_slice(c);
                u32::from_le_bytes(w)
            })
            .collect()
    };
    words(a).cmp(&words(b))
}

/// A view-index entry of `key` and `data`: the data straight after the key,
/// the entry rounded up to 8 bytes.
pub fn entry(key: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    let data_offset = IE_KEY_START + key.len();
    let length = (data_offset + data.len()).div_ceil(8) * 8;
    let (Ok(data_offset16), Ok(data_length16), Ok(length16), Ok(key_length16)) = (
        u16::try_from(data_offset),
        u16::try_from(data.len()),
        u16::try_from(length),
        u16::try_from(key.len()),
    ) else {
        return Err(Error::invalid("a view-index entry larger than 64 KiB"));
    };
    let mut e = vec![0u8; length];
    e[0..2].copy_from_slice(&data_offset16.to_le_bytes());
    e[2..4].copy_from_slice(&data_length16.to_le_bytes());
    e[IE_LENGTH..IE_LENGTH + 2].copy_from_slice(&length16.to_le_bytes());
    e[IE_KEY_LENGTH..IE_KEY_LENGTH + 2].copy_from_slice(&key_length16.to_le_bytes());
    e[IE_KEY_START..data_offset].copy_from_slice(key);
    e[data_offset..data_offset + data.len()].copy_from_slice(data);
    Ok(e)
}

/// Where the index root's entries are: the attribute's offset, its value
/// start and length, the index header's start, and the entries' start and
/// end. Refuses a root that has spilled into INDX blocks.
struct Root {
    attr_offset: usize,
    value_start: usize,
    value_len: usize,
    header: usize,
    entries: usize,
    end: usize,
}

fn root(record: &[u8], index: &str) -> Result<Root, Error> {
    let ir = attr_io::find_attribute(record, AttrType::IndexRoot, Some(index))
        .ok_or_else(|| Error::io(format!("$INDEX_ROOT:{index} not found")))?;
    let (Some(value_offset), Some(value_len), true) = (
        ir.resident_value_offset,
        ir.resident_value_length,
        ir.is_resident,
    ) else {
        return Err(Error::io(format!("$INDEX_ROOT:{index} is not resident")));
    };
    let value_start = ir.attr_offset + value_offset as usize;
    let value_len = value_len as usize;
    if value_len < IR_INDEX_HEADER_OFFSET + INDEX_HEADER_SIZE
        || value_start + value_len > record.len()
    {
        return Err(Error::io(format!(
            "$INDEX_ROOT:{index} is {value_len} bytes, too short for its headers"
        )));
    }
    let header = value_start + IR_INDEX_HEADER_OFFSET;
    let u32_at =
        |o: usize| u32::from_le_bytes(record[o..o + 4].try_into().expect("4 bytes")) as usize;
    let first = u32_at(header + IH_FIRST_ENTRY);
    let total = u32_at(header + IH_TOTAL_SIZE);
    if record[header + IH_FLAGS] & IH_FLAG_HAS_SUBNODES != 0 {
        return Err(Error::refused(format!(
            "$INDEX_ROOT:{index} has spilled into INDX blocks, and changing a view index \
             there is not implemented (#478)"
        )));
    }
    if first < INDEX_HEADER_SIZE
        || !first.is_multiple_of(8)
        || first > total
        || header + total > value_start + value_len
    {
        return Err(Error::io(format!(
            "$INDEX_ROOT:{index}'s header names entries outside its value"
        )));
    }
    Ok(Root {
        attr_offset: ir.attr_offset,
        value_start,
        value_len,
        header,
        entries: header + first,
        end: header + total,
    })
}

/// One entry of a root: its offset, its length and its key.
type Entry = (usize, usize, Vec<u8>);

/// Each entry of the root, up to and not including the LAST sentinel,
/// whose offset is returned beside them.
fn walk(record: &[u8], r: &Root) -> Result<(Vec<Entry>, usize), Error> {
    let mut out = Vec::new();
    let mut at = r.entries;
    loop {
        if at + IE_KEY_START > r.end {
            return Err(Error::io("a view index root with no LAST entry"));
        }
        let length =
            u16::from_le_bytes([record[at + IE_LENGTH], record[at + IE_LENGTH + 1]]) as usize;
        let flags = u16::from_le_bytes([record[at + IE_FLAGS], record[at + IE_FLAGS + 1]]);
        if flags & IE_FLAG_LAST != 0 {
            return Ok((out, at));
        }
        let key_length =
            u16::from_le_bytes([record[at + IE_KEY_LENGTH], record[at + IE_KEY_LENGTH + 1]])
                as usize;
        if length < IE_KEY_START + key_length || at + length > r.end {
            return Err(Error::io(format!("a malformed view index entry at {at}")));
        }
        out.push((
            at,
            length,
            record[at + IE_KEY_START..at + IE_KEY_START + key_length].to_vec(),
        ));
        at += length;
    }
}

/// Write the size of the root's entries into the index header at `header`,
/// as both its total and its allocated size: a resident root has no slack.
fn set_sizes(record: &mut [u8], header: usize, total: usize) -> Result<(), Error> {
    let total = u32::try_from(total).map_err(|_| Error::io("index root size overflows"))?;
    record[header + IH_TOTAL_SIZE..header + IH_TOTAL_SIZE + 4]
        .copy_from_slice(&total.to_le_bytes());
    record[header + IH_ALLOCATED_SIZE..header + IH_ALLOCATED_SIZE + 4]
        .copy_from_slice(&total.to_le_bytes());
    Ok(())
}

/// Put the entry for `key` with `data` in the resident root of `index` in
/// `record`, in collation order, replacing one already under that key.
/// Refused, with `record` unchanged, when the root has spilled into INDX
/// blocks or the record has no room for the entry.
pub fn put(record: &mut [u8], index: &str, key: &[u8], data: &[u8]) -> Result<(), Error> {
    let new = entry(key, data)?;
    let mut scratch = record.to_vec();
    remove(&mut scratch, index, key)?;
    let r = root(&scratch, index)?;
    let (entries, last) = walk(&scratch, &r)?;
    let at = entries
        .iter()
        .find(|(_, _, k)| ulongs_cmp(key, k) == Ordering::Less)
        .map_or(last, |(o, _, _)| *o);
    let in_value = at - r.value_start;
    let old_len = r.value_len;
    let header_rel = r.header - r.value_start;
    let total = r.end - r.header;
    crate::attr_resize::resize_resident_value(
        &mut scratch,
        r.attr_offset,
        u32::try_from(old_len + new.len()).map_err(|_| Error::io("index root size overflows"))?,
    )
    .map_err(|e| e.context(format!("no room in the record for a {index} entry")))?;
    let r2 = root(&scratch, index)?;
    let at = r2.value_start + in_value;
    scratch.copy_within(at..r2.value_start + old_len, at + new.len());
    scratch[at..at + new.len()].copy_from_slice(&new);
    debug_assert_eq!(r2.header - r2.value_start, header_rel);
    set_sizes(&mut scratch, r2.header, total + new.len())?;
    root(&scratch, index)?;
    record.copy_from_slice(&scratch);
    Ok(())
}

/// Take the entry for `key` out of the resident root of `index` in
/// `record`. Returns whether there was one. Refused, with `record`
/// unchanged, when the root has spilled into INDX blocks.
pub fn remove(record: &mut [u8], index: &str, key: &[u8]) -> Result<bool, Error> {
    let r = root(record, index)?;
    let (entries, _) = walk(record, &r)?;
    let Some(&(at, length, _)) = entries.iter().find(|(_, _, k)| k.as_slice() == key) else {
        return Ok(false);
    };
    let old_len = r.value_len;
    let total = r.end - r.header;
    let value_end = r.value_start + old_len;
    // On a copy, so a refusal leaves `record` unchanged. The header's sizes
    // shrink before the value does, so the root never names entries past
    // its own end.
    let mut scratch = record.to_vec();
    scratch.copy_within(at + length..value_end, at);
    scratch[value_end - length..value_end].fill(0);
    set_sizes(&mut scratch, r.header, total - length)?;
    crate::attr_resize::resize_resident_value(
        &mut scratch,
        r.attr_offset,
        u32::try_from(old_len - length).map_err(|_| Error::io("index root size overflows"))?,
    )?;
    root(&scratch, index)?;
    record.copy_from_slice(&scratch);
    Ok(true)
}

/// Whether the index named `index` in `record` has spilled into INDX
/// blocks: its root's header says it has sub-nodes.
pub fn spilled(record: &[u8], index: &str) -> Result<bool, Error> {
    let ir = attr_io::find_attribute(record, AttrType::IndexRoot, Some(index))
        .ok_or_else(|| Error::io(format!("$INDEX_ROOT:{index} not found")))?;
    let value = ir.attr_offset
        + ir.resident_value_offset
            .ok_or_else(|| Error::io(format!("$INDEX_ROOT:{index} is not resident")))?
            as usize;
    record
        .get(value + IR_INDEX_HEADER_OFFSET + IH_FLAGS)
        .map(|f| f & IH_FLAG_HAS_SUBNODES != 0)
        .ok_or_else(|| Error::io(format!("$INDEX_ROOT:{index} is too short for its header")))
}

/// The key a node's entry holds.
fn node_key(entry: &NodeEntry) -> &[u8] {
    let len = usize::from(u16::from_le_bytes([
        entry.key[IE_KEY_LENGTH],
        entry.key[IE_KEY_LENGTH + 1],
    ]));
    entry
        .key
        .get(IE_KEY_START..IE_KEY_START + len)
        .unwrap_or(&[])
}

/// Take the entry for `key` out of the index named `index` of the record
/// `rec`, wherever it is (#571). Returns whether there was one.
///
/// While the index fits its root, this is [`remove`]. Once it has spilled
/// into INDX blocks, the entry is found in the root or in a block. A leaf's
/// entry is cut from its node and the block written back; a leaf left empty
/// stays, as Windows leaves it. An interior entry, which routes to the
/// subtree below it, is replaced by its in-order neighbour, as a directory's
/// `$I30` entries are (`write::remove_interior_entry_of_io`).
pub(crate) fn remove_io<T: BlockIo + ?Sized>(
    io: &mut T,
    rec: u64,
    index: &str,
    key: &[u8],
) -> Result<bool, Error> {
    let (_, record) = crate::mft_io::read_mft_record_io(io, rec)?;
    if !spilled(&record, index)? {
        if !remove(&mut record.clone(), index, key)? {
            return Ok(false);
        }
        crate::mft_io::update_mft_record_io(io, rec, |r| remove(r, index, key).map(|_| ()))?;
        return Ok(true);
    }
    let what = format!("the {index} entry of record {rec}");
    let is_wanted = |entry: &NodeEntry| -> Result<bool, Error> { Ok(node_key(entry) == key) };
    let root = crate::index_io::parse_index_root_node_of(&record, index, NodeKeys::View)?;
    if root.entries.iter().any(|e| node_key(e) == key) {
        crate::write::remove_interior_entry_of_io(
            io,
            rec,
            index,
            NodeKeys::View,
            None,
            &what,
            &is_wanted,
        )?;
        return Ok(true);
    }
    let ia = crate::idx_block::load_index_io(io, rec, index)?;
    for vcn in ia.allocated_block_vcns() {
        let block = crate::idx_block::read_indx_block_io(io, &ia, vcn)?;
        let mut node = crate::index_io::parse_indx_node_of(&block, NodeKeys::View)?;
        let Some(at) = node.entries.iter().position(|e| node_key(e) == key) else {
            continue;
        };
        if node.entries[at].child.is_some() {
            crate::write::remove_interior_entry_of_io(
                io,
                rec,
                index,
                NodeKeys::View,
                Some(vcn),
                &what,
                &is_wanted,
            )?;
        } else {
            node.entries.remove(at);
            let bytes = crate::index_io::write_indx_node(&block, &node, vcn)?;
            crate::idx_block::write_indx_block_io(io, &ia, vcn, &bytes)?;
        }
        return Ok(true);
    }
    Ok(false)
}

/// Every key of the index named `index` of the record `rec`, in the
/// index's order, each with whether it sits in an interior node: a walk of
/// the whole tree, its root and its INDX blocks, child before entry.
pub fn keys_io<T: BlockIo + ?Sized>(
    io: &mut T,
    rec: u64,
    index: &str,
) -> Result<Vec<(Vec<u8>, bool)>, Error> {
    let (_, record) = crate::mft_io::read_mft_record_io(io, rec)?;
    let root = crate::index_io::parse_index_root_node_of(&record, index, NodeKeys::View)?;
    if !root.is_interior() {
        return Ok(root
            .entries
            .iter()
            .map(|e| (node_key(e).to_vec(), false))
            .collect());
    }
    let ia = crate::idx_block::load_index_io(io, rec, index)?;
    let mut out = Vec::new();
    // Recursive, with a depth bound, so a cycle is an error and not a hang.
    fn visit<T: BlockIo + ?Sized>(
        io: &mut T,
        ia: &crate::idx_block::IndexAllocation,
        node: &crate::index_io::IndexNode,
        depth: usize,
        out: &mut Vec<(Vec<u8>, bool)>,
    ) -> Result<(), Error> {
        if depth > 64 {
            return Err(Error::io(
                "a view index deeper than 64 levels, or with a cycle",
            ));
        }
        let interior = node.is_interior();
        for entry in &node.entries {
            if let Some(child) = entry.child {
                let block = crate::idx_block::read_indx_block_io(io, ia, child)?;
                let below = crate::index_io::parse_indx_node_of(&block, NodeKeys::View)?;
                visit(io, ia, &below, depth + 1, out)?;
            }
            out.push((node_key(entry).to_vec(), interior));
        }
        if let Some(child) = node.last_child {
            let block = crate::idx_block::read_indx_block_io(io, ia, child)?;
            let below = crate::index_io::parse_indx_node_of(&block, NodeKeys::View)?;
            visit(io, ia, &below, depth + 1, out)?;
        }
        Ok(())
    }
    visit(io, &ia, &root, 0, &mut out)?;
    Ok(out)
}

/// The keys in the resident root of `index` in `record`, in order.
pub fn keys(record: &[u8], index: &str) -> Result<Vec<Vec<u8>>, Error> {
    let r = root(record, index)?;
    Ok(walk(record, &r)?.0.into_iter().map(|(_, _, k)| k).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_match_the_layout_windows_writes() {
        let r = entry(&reparse_key(0xA000_0003, 0x0001_0000_0000_0029), &[]).unwrap();
        assert_eq!(
            r,
            [
                0x1c, 0, 0, 0, 0, 0, 0, 0, 0x20, 0, 0x0c, 0, 0, 0, 0, 0, 0x03, 0, 0, 0xa0, 0x29, 0,
                0, 0, 0, 0, 1, 0, 0, 0, 0, 0
            ]
        );
        let id = [7u8; 16];
        let o = entry(&id, &objid_data(0x0001_0000_0000_0202, &id, None)).unwrap();
        assert_eq!(o.len(), 0x58);
        assert_eq!(&o[0..4], &[0x20, 0, 0x38, 0]);
        assert_eq!(&o[8..12], &[0x58, 0, 0x10, 0]);
        assert_eq!(&o[0x20..0x28], &0x0001_0000_0000_0202u64.to_le_bytes());
        assert_eq!(&o[0x28..0x38], &[0u8; 16]);
        assert_eq!(&o[0x38..0x48], &id);
        assert_eq!(&o[0x48..0x58], &[0u8; 16]);
    }

    #[test]
    fn ulongs_compare_word_by_word_not_byte_by_byte() {
        // Byte order would put the second first; word order does not.
        let a = [0x00, 0x01, 0, 0];
        let b = [0x01, 0x00, 0, 0];
        assert_eq!(ulongs_cmp(&a, &b), Ordering::Greater);
        assert_eq!(ulongs_cmp(&a[..], &a[..]), Ordering::Equal);
        assert_eq!(
            ulongs_cmp(&[1, 0, 0, 0], &[1, 0, 0, 0, 0, 0, 0, 0]),
            Ordering::Less
        );
    }
}
