//! A file's `$ATTRIBUTE_LIST`, kept true while its attributes change, and
//! created when its base record is full (#480).
//!
//! # What Windows does, measured
//!
//! `test-disks/ntfs-attrlist.img` is a file Windows gave 96 small named
//! streams, one at a time. Its records show the whole method:
//!
//! - The base record keeps `$STANDARD_INFORMATION` and the
//!   `$ATTRIBUTE_LIST`. The first time the base record filled, Windows
//!   moved every other attribute into one new extension record, renumbered
//!   there from 0, and went on putting new streams in the base record.
//! - Each time the base record filled again, a batch of the streams in it
//!   moved out to a new extension record.
//! - Once the list itself no longer fitted, it went non-resident: 4704
//!   bytes in two clusters.
//! - An extension record is a base record's header with the hard-link
//!   count 0, the flags IN_USE alone, `base_file_record_reference` naming
//!   the base record and its sequence, and the attribute ids counted from 0.
//! - The list has one entry per attribute, wherever it is, and none for
//!   itself; each entry is `align8(0x1A + 2 * name length)` bytes with the
//!   name at 0x1A, and the entries are in type order, then name order,
//!   then starting VCN.
//!
//! # What this does
//!
//! The same, with one difference. The attributes this moves are the named
//! `$DATA` streams and, once the list has no room even in clusters, the
//! `$FILE_NAME`s, which a file's hard links can fill its base record with
//! (#779). This driver's other write paths -- the unnamed `$DATA` writes,
//! reparse points, object ids -- edit the base record, so the unnamed
//! `$DATA` and the rest stay there, where those paths find them; names are
//! added and removed through the list. Windows' own layout allows either;
//! chkdsk grades both. EAs are written through the list too
//! (#660): `$EA` and `$EA_INFORMATION` are added and replaced like a
//! stream, but never moved to make room for one.
//!
//! - A file with no list whose base record has no room for a stream or
//!   an EA gets one: an entry for every attribute, and the named streams
//!   moved to an extension record.
//! - A new attribute goes in the base record while it and its entry fit
//!   there, else in an extension record with room, else in a new one.
//! - When the list no longer fits the base record, the named streams in
//!   the base record move out to extension records; when that is not
//!   enough, the list goes non-resident, and moves to a larger run of
//!   clusters when it outgrows the one it has; when the base record has
//!   no room even for the list's non-resident header, its names move out.
//! - An attribute that no longer fits its record moves, and an extension
//!   record left with no attributes is freed.
//!
//! Everything is planned in memory first, so a change refused for any
//! reason writes nothing. The writes then go in the order that keeps the
//! list from naming what is not there: the list's clusters, the extension
//! records, then the base record that holds or maps the list; records and
//! clusters it no longer names are freed last.

use crate::attr_io::{self, attr_off, AttrLocation, AttrType};
use crate::block_io::BlockIo;
use crate::data_runs::{self, DataRun};
use crate::error::Error;
use crate::mft_io::{self, BootParams};
use crate::record_build::{align8, encode_file_reference};
use crate::upcase::UpcaseTable;

const REC_BYTES_USED: usize = 0x18;
const REC_BYTES_ALLOCATED: usize = 0x1C;
const REC_BASE_REFERENCE: usize = 0x20;
/// Where a record's update sequence array starts, in every record this
/// crate writes and every one Windows does.
const REC_USA_OFFSET: usize = 0x30;
/// An attribute list entry's fixed part; the name follows it.
const ENTRY_NAME_OFFSET: usize = 0x1A;
/// A resident attribute's header, which an unnamed one's value follows.
const RESIDENT_HEADER: usize = 0x18;
const RECORD_NUMBER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// The record an extension record is looked for from: the one after the
/// base record, as Windows chose.
const EXTENSION_HINT_STEP: u64 = 1;

/// One entry of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    type_code: u32,
    /// The name as stored, in UTF-16 code units; empty for an unnamed
    /// attribute.
    name: Vec<u16>,
    starting_vcn: u64,
    /// The file reference of the record holding the attribute: its number
    /// and its sequence.
    reference: u64,
    attribute_id: u16,
}

impl Entry {
    fn len(&self) -> usize {
        entry_len(self.name.len())
    }

    fn record_number(&self) -> u64 {
        self.reference & RECORD_NUMBER_MASK
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.resize(start + self.len(), 0);
        let e = &mut out[start..];
        e[0..4].copy_from_slice(&self.type_code.to_le_bytes());
        e[4..6].copy_from_slice(&(self.len() as u16).to_le_bytes());
        e[6] = self.name.len() as u8;
        e[7] = ENTRY_NAME_OFFSET as u8;
        e[8..16].copy_from_slice(&self.starting_vcn.to_le_bytes());
        e[16..24].copy_from_slice(&self.reference.to_le_bytes());
        e[24..26].copy_from_slice(&self.attribute_id.to_le_bytes());
        for (i, unit) in self.name.iter().enumerate() {
            let at = ENTRY_NAME_OFFSET + 2 * i;
            e[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
}

/// How long the entry for an attribute whose name is `name_units` long is.
fn entry_len(name_units: usize) -> usize {
    align8(ENTRY_NAME_OFFSET + 2 * name_units)
}

/// The list's value, decoded with each name kept as the code units stored.
fn decode_entries(value: &[u8]) -> Result<Vec<Entry>, Error> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + ENTRY_NAME_OFFSET <= value.len() {
        let u16_at = |o: usize| u16::from_le_bytes([value[o], value[o + 1]]);
        let u64_at = |o: usize| u64::from_le_bytes(value[o..o + 8].try_into().expect("8 bytes"));
        let length = u16_at(at + 4) as usize;
        if length == 0 {
            break;
        }
        if length < ENTRY_NAME_OFFSET || at + length > value.len() {
            return Err(Error::io(format!(
                "$ATTRIBUTE_LIST entry at {at} has length {length}, which the list's {} bytes \
                 cannot hold",
                value.len()
            )));
        }
        let name_units = value[at + 6] as usize;
        let name_offset = value[at + 7] as usize;
        if name_units > 0 && name_offset + 2 * name_units > length {
            return Err(Error::io(format!(
                "$ATTRIBUTE_LIST entry at {at} has a name past its {length} bytes"
            )));
        }
        let name = (0..name_units)
            .map(|i| u16_at(at + name_offset + 2 * i))
            .collect();
        out.push(Entry {
            type_code: u32::from_le_bytes(value[at..at + 4].try_into().expect("4 bytes")),
            name,
            starting_vcn: u64_at(at + 8),
            reference: u64_at(at + 16),
            attribute_id: u16_at(at + 24),
        });
        at += length;
    }
    Ok(out)
}

fn encode_entries(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(entries.iter().map(Entry::len).sum());
    for e in entries {
        e.encode_into(&mut out);
    }
    out
}

/// Type order, then name order -- under the volume's `$UpCase`, then code
/// unit by code unit, as attributes are ordered in a record (#467) -- then
/// starting VCN. Entries equal on all three keep their order.
fn sort_entries(entries: &mut [Entry], upcase: Option<&UpcaseTable>) {
    entries.sort_by(|a, b| {
        a.type_code
            .cmp(&b.type_code)
            .then_with(|| crate::index_io::compare_names(&a.name, &b.name, upcase))
            .then_with(|| crate::index_io::compare_names_ordinal(&a.name, &b.name))
            .then_with(|| a.starting_vcn.cmp(&b.starting_vcn))
    });
}

fn u16_at(record: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([record[at], record[at + 1]])
}

fn u32_at(record: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(record[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(record: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(record[at..at + 8].try_into().expect("8 bytes"))
}

/// Bytes a record has left for attributes.
fn free_space(record: &[u8]) -> usize {
    (u32_at(record, REC_BYTES_ALLOCATED) as usize)
        .saturating_sub(u32_at(record, REC_BYTES_USED) as usize)
}

fn name_units(record: &[u8], loc: &AttrLocation) -> Vec<u16> {
    let start = loc.attr_offset + loc.name_offset as usize;
    (0..loc.name_length as usize)
        .map(|i| u16_at(record, start + 2 * i))
        .collect()
}

/// The first VCN an attribute covers: 0 for a resident one.
fn starting_vcn(record: &[u8], loc: &AttrLocation) -> u64 {
    if loc.is_resident {
        0
    } else {
        u64_at(record, loc.attr_offset + attr_off::NONRES_FIRST_VCN)
    }
}

/// The attribute of `type_code` with id `id` in `record`.
fn find_by_id(record: &[u8], type_code: u32, id: u16) -> Option<AttrLocation> {
    attr_io::iter_attributes(record).find(|l| l.type_code == type_code && l.attribute_id == id)
}

fn set_attribute_id(attr: &mut [u8], id: u16) {
    attr[attr_off::ATTRIBUTE_ID..attr_off::ATTRIBUTE_ID + 2].copy_from_slice(&id.to_le_bytes());
}

/// One MFT record of the file, as it will be written.
struct Segment {
    number: u64,
    /// Fixed up: the form a mutator sees.
    bytes: Vec<u8>,
    /// Allocated by this change; its bytes are written whole.
    fresh: bool,
    dirty: bool,
}

impl Segment {
    fn reference(&self) -> u64 {
        encode_file_reference(self.number, mft_io::record_sequence(&self.bytes))
    }

    fn is_empty(&self) -> bool {
        attr_io::iter_attributes(&self.bytes).next().is_none()
    }
}

/// Where the list's value lives.
#[derive(Debug, Clone, PartialEq, Eq)]
enum List {
    /// The file has none, and has needed none yet.
    Absent,
    /// In the base record, or to be put there.
    Resident,
    /// In clusters: `(lcn, count)` runs holding `allocated` bytes.
    NonResident {
        runs: Vec<(u64, u64)>,
        allocated: u64,
    },
}

/// A file whose attributes may be spread over several records, held in
/// memory while a change to them is planned (#480).
pub(crate) struct ListedFile {
    params: BootParams,
    upcase: Option<UpcaseTable>,
    /// `[0]` is the base record; the rest are its extension records.
    segments: Vec<Segment>,
    entries: Vec<Entry>,
    list: List,
    /// The list's new value, once settled, for a non-resident list.
    list_value: Option<Vec<u8>>,
    /// MFT records and clusters this change allocated: given back if it
    /// is abandoned.
    new_records: Vec<u64>,
    new_clusters: Vec<(u64, u64)>,
    /// Clusters the list held before it moved: freed once it is written.
    released_clusters: Vec<(u64, u64)>,
}

impl ListedFile {
    /// Read the file whose base record is `base`: the base record, its list
    /// and every extension record the list names.
    ///
    /// A list this does not understand is refused before anything is
    /// written: one naming a record that is not in use, belongs to another
    /// file, or does not hold the attribute the entry says it does. So is
    /// a directory, whose index this driver edits in the base record alone.
    pub(crate) fn open<T: BlockIo + ?Sized>(io: &mut T, base: u64) -> Result<Self, Error> {
        let (params, record) = mft_io::read_mft_record_io(io, base)?;
        if u64_at(&record, REC_BASE_REFERENCE) != 0 {
            return Err(Error::io(format!(
                "record {base} is an extension record, not a file's base record"
            )));
        }
        if mft_io::record_flags(&record) & mft_io::MFT_FLAG_DIRECTORY != 0 {
            return Err(Error::refused(format!(
                "record {base} is a directory, whose attributes this driver does not move \
                 between records"
            )));
        }
        let upcase = UpcaseTable::load_io(io).ok();
        let mut file = ListedFile {
            params,
            upcase,
            segments: vec![Segment {
                number: base,
                bytes: record,
                fresh: false,
                dirty: false,
            }],
            entries: Vec::new(),
            list: List::Absent,
            list_value: None,
            new_records: Vec::new(),
            new_clusters: Vec::new(),
            released_clusters: Vec::new(),
        };
        let Some(loc) = file.list_location() else {
            return Ok(file);
        };
        let value = crate::read::read_attribute_value(io, base, AttrType::AttributeList, None)?;
        file.entries = decode_entries(&value)?;
        file.list = if loc.is_resident {
            List::Resident
        } else {
            let record = &file.segments[0].bytes;
            let flags = attr_io::attribute_flags(record, &loc).unwrap_or(0);
            if flags & attr_io::attr_flags::TRANSFORMED != 0 {
                return Err(Error::refused(format!(
                    "record {base}: its $ATTRIBUTE_LIST is {}",
                    attr_io::describe_transform_flags(flags)
                )));
            }
            let mpo = loc
                .non_resident_mapping_pairs_offset
                .ok_or(Error::io("$ATTRIBUTE_LIST has no mapping-pairs offset"))?
                as usize;
            let runs = data_runs::decode_runs(
                &record[loc.attr_offset + mpo..loc.attr_offset + loc.attr_length],
            )?
            .into_iter()
            .map(|r| {
                r.lcn.map(|lcn| (lcn, r.length)).ok_or_else(|| {
                    Error::io(format!("record {base}: its $ATTRIBUTE_LIST has a hole"))
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
            List::NonResident {
                runs,
                allocated: u64_at(record, loc.attr_offset + attr_off::NONRES_ALLOCATED_LENGTH),
            }
        };
        let mut numbers: Vec<u64> = Vec::new();
        for e in &file.entries {
            let n = e.record_number();
            if n != base && !numbers.contains(&n) {
                numbers.push(n);
            }
        }
        for n in numbers {
            let (_, record) = mft_io::read_mft_record_io(io, n)?;
            if mft_io::record_flags(&record) & mft_io::MFT_FLAG_IN_USE == 0 {
                return Err(Error::io(format!(
                    "the $ATTRIBUTE_LIST of record {base} names record {n}, which is not in use"
                )));
            }
            let owner = u64_at(&record, REC_BASE_REFERENCE) & RECORD_NUMBER_MASK;
            if owner != base {
                return Err(Error::io(format!(
                    "the $ATTRIBUTE_LIST of record {base} names record {n}, whose base record \
                     is {owner}"
                )));
            }
            file.segments.push(Segment {
                number: n,
                bytes: record,
                fresh: false,
                dirty: false,
            });
        }
        for e in &file.entries {
            let segment = file.segment_index(e.record_number()).expect("loaded above");
            let record = &file.segments[segment].bytes;
            let found = find_by_id(record, e.type_code, e.attribute_id)
                .filter(|l| name_units(record, l) == e.name);
            if found.is_none() {
                return Err(Error::io(format!(
                    "the $ATTRIBUTE_LIST of record {base} puts attribute {:#x} id {} in record \
                     {}, and it is not there",
                    e.type_code,
                    e.attribute_id,
                    e.record_number()
                )));
            }
        }
        Ok(file)
    }

    /// The bytes the largest attribute an empty extension record takes.
    pub(crate) fn max_attribute_len(&self) -> usize {
        let size = self.params.file_record_size as usize;
        let sectors = size / self.params.bytes_per_sector.max(1) as usize;
        size - align8(REC_USA_OFFSET + 2 + 2 * sectors) - 8
    }

    /// The volume's geometry, as the base record was read with.
    pub(crate) fn params(&self) -> &BootParams {
        &self.params
    }

    /// The bytes of the attribute `found` locates.
    pub(crate) fn attribute_bytes(&self, found: &Found) -> &[u8] {
        let record = &self.segments[found.segment].bytes;
        &record[found.loc.attr_offset..found.loc.attr_offset + found.loc.attr_length]
    }

    /// The clusters the attribute `found` locates maps, as `(lcn, count)`:
    /// none for a resident one, and none for a hole.
    pub(crate) fn runs(&self, found: &Found) -> Result<Vec<(u64, u64)>, Error> {
        if found.loc.is_resident {
            return Ok(Vec::new());
        }
        let attr = self.attribute_bytes(found);
        let mpo = found
            .loc
            .non_resident_mapping_pairs_offset
            .ok_or(Error::io(
                "a non-resident attribute has no mapping-pairs offset",
            ))? as usize;
        if mpo >= attr.len() {
            return Err(Error::io(format!(
                "a non-resident attribute's mapping pairs start at {mpo}, past its {} bytes",
                attr.len()
            )));
        }
        Ok(data_runs::decode_runs(&attr[mpo..])?
            .into_iter()
            .filter_map(|r| r.lcn.filter(|_| r.length > 0).map(|lcn| (lcn, r.length)))
            .collect())
    }

    /// Count clusters the caller allocated for this change among those
    /// [`Self::abandon`] gives back.
    pub(crate) fn adopt_clusters(&mut self, lcn: u64, count: u64) {
        self.new_clusters.push((lcn, count));
    }

    /// Where the attribute of `ty` named `name` is, or `None`. One split
    /// across records is refused: nothing here moves a segment of one.
    pub(crate) fn find(&self, ty: AttrType, name: &str) -> Result<Option<Found>, Error> {
        let units: Vec<u16> = name.encode_utf16().collect();
        if self.list == List::Absent {
            let record = &self.segments[0].bytes;
            return Ok(attr_io::iter_attributes(record)
                .find(|l| l.type_code == ty as u32 && name_units(record, l) == units)
                .map(|loc| Found { segment: 0, loc }));
        }
        let matching: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.type_code == ty as u32 && e.name == units)
            .collect();
        match matching.as_slice() {
            [] => Ok(None),
            [e] => {
                let segment = self
                    .segment_index(e.record_number())
                    .ok_or_else(|| Error::io("an entry names a record that was not read"))?;
                let loc = find_by_id(&self.segments[segment].bytes, e.type_code, e.attribute_id)
                    .ok_or_else(|| Error::io("an entry names an attribute that is not there"))?;
                Ok(Some(Found { segment, loc }))
            }
            _ => Err(Error::refused(format!(
                "{ty:?} {name:?} is split across {} records, and this driver does not move a \
                 segment of one",
                matching.len()
            ))),
        }
    }

    /// Add `attr`, a whole attribute whose id is assigned here.
    pub(crate) fn add<T: BlockIo + ?Sized>(
        &mut self,
        io: &mut T,
        mut attr: Vec<u8>,
    ) -> Result<(), Error> {
        if attr.len() > self.max_attribute_len() {
            return Err(Error::no_space(format!(
                "an attribute of {} bytes does not fit in an MFT record of {}",
                attr.len(),
                self.params.file_record_size
            )));
        }
        if self.list == List::Absent {
            if free_space(&self.segments[0].bytes) >= attr.len() {
                return self.place(0, &mut attr, false);
            }
            self.convert(io)?;
        }
        let name_len = attr[attr_off::NAME_LENGTH] as usize;
        let growth = self.list_growth(entry_len(name_len));
        let target = if free_space(&self.segments[0].bytes) >= attr.len() + growth {
            0
        } else {
            self.segment_with_room(attr.len(), io)?
        };
        self.place(target, &mut attr, true)
    }

    /// Replace the attribute `ty` named `name` with `attr`: where it is,
    /// keeping its id, when its record has the room, else wherever
    /// [`Self::add`] puts it.
    pub(crate) fn replace<T: BlockIo + ?Sized>(
        &mut self,
        io: &mut T,
        ty: AttrType,
        name: &str,
        mut attr: Vec<u8>,
    ) -> Result<(), Error> {
        let found = self
            .find(ty, name)?
            .ok_or_else(|| Error::not_found(format!("{ty:?} {name:?} not found")))?;
        let segment = &mut self.segments[found.segment];
        if free_space(&segment.bytes) + found.loc.attr_length >= attr.len() {
            set_attribute_id(&mut attr, found.loc.attribute_id);
            crate::attr_resize::replace_attribute(
                &mut segment.bytes,
                found.loc.attr_offset,
                &attr,
            )?;
            segment.dirty = true;
            return Ok(());
        }
        self.remove(ty, name)?;
        self.add(io, attr)
    }

    /// Take out the attribute `ty` named `name`, with its entry, and
    /// return its bytes. An extension record it leaves empty is freed.
    pub(crate) fn remove(&mut self, ty: AttrType, name: &str) -> Result<Vec<u8>, Error> {
        let found = self
            .find(ty, name)?
            .ok_or_else(|| Error::not_found(format!("{ty:?} {name:?} not found")))?;
        let bytes = self.attribute_bytes(&found).to_vec();
        let number = self.segments[found.segment].number;
        let segment = &mut self.segments[found.segment];
        crate::write::remove_listed_attribute_at(
            &mut segment.bytes,
            found.loc.attr_offset,
            found.loc.attr_length,
        )?;
        segment.dirty = true;
        self.entries.retain(|e| {
            !(e.record_number() == number
                && e.type_code == found.loc.type_code
                && e.attribute_id == found.loc.attribute_id)
        });
        Ok(bytes)
    }

    /// Add `attr`, a whole `$FILE_NAME` attribute, wherever [`Self::add`]
    /// puts it, and count one hard link more in the base record (#779).
    /// Windows keeps names in extension records too, so one the base
    /// record has no room for goes to an extension record, with its entry.
    pub(crate) fn add_file_name<T: BlockIo + ?Sized>(
        &mut self,
        io: &mut T,
        attr: Vec<u8>,
    ) -> Result<(), Error> {
        self.add(io, attr)?;
        let base = &mut self.segments[0];
        let links = u16_at(&base.bytes, 0x12).saturating_add(1);
        base.bytes[0x12..0x14].copy_from_slice(&links.to_le_bytes());
        base.dirty = true;
        Ok(())
    }

    /// Whether `len` more bytes fit in the record `record`.
    pub(crate) fn record_has_room(record: &[u8], len: usize) -> bool {
        free_space(record) >= len
    }

    /// Take out the `$FILE_NAME` that names the file `basename` in the
    /// directory `parent_reference`, from whichever record holds it, with
    /// its entry, and count one hard link fewer in the base record (#778).
    /// Windows keeps names in extension records too, so every record the
    /// list names is searched, not the base record alone.
    pub(crate) fn remove_file_name(
        &mut self,
        parent_reference: u64,
        basename: &str,
    ) -> Result<(), Error> {
        let found = self
            .segments
            .iter()
            .enumerate()
            .find_map(|(i, s)| {
                crate::write::find_file_name_attr(&s.bytes, parent_reference, basename)
                    .map(|loc| (i, loc))
            })
            .ok_or_else(|| {
                Error::not_found(format!(
                    "unlink: no $FILE_NAME for '{basename}' under parent {parent_reference:#x}"
                ))
            })?;
        let (index, loc) = found;
        let number = self.segments[index].number;
        let segment = &mut self.segments[index];
        crate::write::remove_listed_attribute_at(
            &mut segment.bytes,
            loc.attr_offset,
            loc.attr_length,
        )?;
        segment.dirty = true;
        self.entries.retain(|e| {
            !(e.record_number() == number
                && e.type_code == loc.type_code
                && e.attribute_id == loc.attribute_id)
        });
        let base = &mut self.segments[0];
        let links = u16_at(&base.bytes, 0x12).saturating_sub(1);
        base.bytes[0x12..0x14].copy_from_slice(&links.to_le_bytes());
        base.dirty = true;
        Ok(())
    }

    /// Rename the `$FILE_NAME`s that name the file `old` with `rename`, in
    /// whichever record holds each (#780). Windows keeps names in extension
    /// records too, so every record the list names is searched. A name its
    /// record has no room for once renamed is taken out with its entry and
    /// `moved`, given the old name's namespace, goes wherever
    /// [`Self::add`] puts it.
    pub(crate) fn rename_file_name<T: BlockIo + ?Sized>(
        &mut self,
        io: &mut T,
        old: &str,
        rename: impl Fn(&mut [u8]) -> Result<(), Error>,
        moved: impl Fn(u8) -> Result<Vec<u8>, Error>,
    ) -> Result<(), Error> {
        let mut renamed = false;
        let mut index = 0;
        while index < self.segments.len() {
            let Some((loc, namespace)) =
                crate::write::find_file_name_by_name(&self.segments[index].bytes, old)
            else {
                index += 1;
                continue;
            };
            renamed = true;
            let mut bytes = self.segments[index].bytes.clone();
            if rename(&mut bytes).is_ok() {
                let segment = &mut self.segments[index];
                segment.bytes = bytes;
                segment.dirty = true;
                index += 1;
                continue;
            }
            let number = self.segments[index].number;
            let segment = &mut self.segments[index];
            crate::write::remove_listed_attribute_at(
                &mut segment.bytes,
                loc.attr_offset,
                loc.attr_length,
            )?;
            segment.dirty = true;
            self.entries.retain(|e| {
                !(e.record_number() == number
                    && e.type_code == loc.type_code
                    && e.attribute_id == loc.attribute_id)
            });
            self.add(io, moved(namespace)?)?;
        }
        if !renamed {
            return Err(Error::not_found(format!(
                "rename: no $FILE_NAME for '{old}' in any record of the file"
            )));
        }
        Ok(())
    }

    /// Give a file with no list one: an entry for every attribute in its
    /// base record, and its named streams moved out, as Windows moved
    /// everything but `$STANDARD_INFORMATION` the first time.
    fn convert<T: BlockIo + ?Sized>(&mut self, io: &mut T) -> Result<(), Error> {
        let reference = self.segments[0].reference();
        let record = &self.segments[0].bytes;
        self.entries = attr_io::iter_attributes(record)
            .map(|loc| Entry {
                type_code: loc.type_code,
                name: name_units(record, &loc),
                starting_vcn: starting_vcn(record, &loc),
                reference,
                attribute_id: loc.attribute_id,
            })
            .collect();
        self.list = List::Resident;
        self.evict_named_data(io)?;
        Ok(())
    }

    /// How much the base record's copy of a resident list grows by when
    /// an entry of `len` bytes joins it; nothing for a list in clusters.
    fn list_growth(&self, len: usize) -> usize {
        if self.list != List::Resident {
            return 0;
        }
        let value: usize = self.entries.iter().map(Entry::len).sum();
        let current = self.list_location().map_or(0, |l| l.attr_length);
        align8(RESIDENT_HEADER + value + len).saturating_sub(current)
    }

    fn list_location(&self) -> Option<AttrLocation> {
        attr_io::find_attribute(&self.segments[0].bytes, AttrType::AttributeList, None)
    }

    fn segment_index(&self, number: u64) -> Option<usize> {
        self.segments.iter().position(|s| s.number == number)
    }

    /// An extension record with `len` bytes free: the first that has
    /// them, else a new one.
    fn segment_with_room<T: BlockIo + ?Sized>(
        &mut self,
        len: usize,
        io: &mut T,
    ) -> Result<usize, Error> {
        if let Some(i) =
            (1..self.segments.len()).find(|&i| free_space(&self.segments[i].bytes) >= len)
        {
            return Ok(i);
        }
        self.new_extension(io)
    }

    /// Allocate an extension record: from the record after the base up,
    /// marked in `$MFT:$Bitmap` now so the next one is another, and given
    /// back if the change is abandoned.
    fn new_extension<T: BlockIo + ?Sized>(&mut self, io: &mut T) -> Result<usize, Error> {
        let base = self.segments[0].number;
        let (bitmap, number) =
            crate::mft_bitmap::find_or_grow_free_record_io(io, base + EXTENSION_HINT_STEP)?;
        crate::mft_bitmap::allocate_io(io, &bitmap, number)?;
        self.new_records.push(number);
        let sequence = mft_io::next_sequence_for_allocation_io(io, &self.params, number)?;
        let record_number = u32::try_from(number)
            .map_err(|_| Error::io(format!("MFT record {number} is past 32 bits")))?;
        let bytes = crate::record_build::build_extension_record(
            self.params.file_record_size as usize,
            record_number,
            sequence,
            self.segments[0].reference(),
            self.params.bytes_per_sector,
        )?;
        self.segments.push(Segment {
            number,
            bytes,
            fresh: true,
            dirty: true,
        });
        Ok(self.segments.len() - 1)
    }

    /// Put `attr` in segment `target` with that record's next id, where
    /// attribute order puts it, and enter it in the list when `listed`.
    fn place(&mut self, target: usize, attr: &mut [u8], listed: bool) -> Result<(), Error> {
        let segment = &mut self.segments[target];
        let id = crate::attr_resize::allocate_attribute_id(&mut segment.bytes);
        set_attribute_id(attr, id);
        crate::attr_resize::insert_attribute_listed_typed(
            &mut segment.bytes,
            attr,
            self.upcase.as_ref(),
        )
        .map_err(Error::from)?;
        segment.dirty = true;
        if listed {
            let reference = segment.reference();
            let loc = find_by_id(&segment.bytes, u32_at(attr, 0), id)
                .ok_or(Error::io("an attribute just inserted is not in its record"))?;
            self.entries.push(Entry {
                type_code: loc.type_code,
                name: name_units(&segment.bytes, &loc),
                starting_vcn: starting_vcn(&segment.bytes, &loc),
                reference,
                attribute_id: id,
            });
        }
        Ok(())
    }

    /// Move every named `$DATA` attribute in the base record to extension
    /// records, as Windows moves a batch of streams out when the base
    /// record fills. Returns how many moved.
    fn evict_named_data<T: BlockIo + ?Sized>(&mut self, io: &mut T) -> Result<usize, Error> {
        let record = &self.segments[0].bytes;
        let victims: Vec<(Vec<u8>, u16)> = attr_io::iter_attributes(record)
            .filter(|l| l.type_code == AttrType::Data as u32 && l.name_length > 0)
            .filter(|l| {
                self.entries
                    .iter()
                    .filter(|e| e.type_code == l.type_code && e.name == name_units(record, l))
                    .count()
                    == 1
            })
            .map(|l| {
                (
                    record[l.attr_offset..l.attr_offset + l.attr_length].to_vec(),
                    l.attribute_id,
                )
            })
            .collect();
        self.evict(io, AttrType::Data, victims)
    }

    /// Move every `$FILE_NAME` in the base record to extension records,
    /// once moving the streams out and putting the list in clusters leave
    /// no room for the list (#779): a file's names can fill its base
    /// record, and Windows keeps only `$STANDARD_INFORMATION` and the list
    /// there when it fills (`test-disks/ntfs-attrlist.img`). A name is
    /// matched to its entry by attribute id, as names carry no attribute
    /// name. Returns how many moved.
    fn evict_file_names<T: BlockIo + ?Sized>(&mut self, io: &mut T) -> Result<usize, Error> {
        let base = self.segments[0].number;
        let record = &self.segments[0].bytes;
        let victims: Vec<(Vec<u8>, u16)> = attr_io::iter_attributes(record)
            .filter(|l| l.type_code == AttrType::FileName as u32 && l.is_resident)
            .filter(|l| {
                self.entries.iter().any(|e| {
                    e.record_number() == base
                        && e.type_code == l.type_code
                        && e.attribute_id == l.attribute_id
                })
            })
            .map(|l| {
                (
                    record[l.attr_offset..l.attr_offset + l.attr_length].to_vec(),
                    l.attribute_id,
                )
            })
            .collect();
        self.evict(io, AttrType::FileName, victims)
    }

    /// Move `victims`, attributes of `kind` in the base record given with
    /// their ids there, each to an extension record with room, and point
    /// their list entries at where they now are.
    fn evict<T: BlockIo + ?Sized>(
        &mut self,
        io: &mut T,
        kind: AttrType,
        victims: Vec<(Vec<u8>, u16)>,
    ) -> Result<usize, Error> {
        let base = self.segments[0].number;
        for (mut attr, old_id) in victims.iter().cloned() {
            let loc = find_by_id(&self.segments[0].bytes, kind as u32, old_id)
                .ok_or(Error::io("an attribute to move is not in the base record"))?;
            crate::write::remove_listed_attribute_at(
                &mut self.segments[0].bytes,
                loc.attr_offset,
                loc.attr_length,
            )?;
            self.segments[0].dirty = true;
            let target = self.segment_with_room(attr.len(), io)?;
            let segment = &mut self.segments[target];
            let id = crate::attr_resize::allocate_attribute_id(&mut segment.bytes);
            set_attribute_id(&mut attr, id);
            crate::attr_resize::insert_attribute_listed_typed(
                &mut segment.bytes,
                &attr,
                self.upcase.as_ref(),
            )
            .map_err(Error::from)?;
            segment.dirty = true;
            let reference = segment.reference();
            let entry = self
                .entries
                .iter_mut()
                .find(|e| {
                    e.record_number() == base
                        && e.type_code == kind as u32
                        && e.attribute_id == old_id
                })
                .ok_or(Error::io("an attribute to move has no entry in the list"))?;
            entry.reference = reference;
            entry.attribute_id = id;
        }
        Ok(victims.len())
    }

    /// Settle the list's value where it lives: resized in the base record,
    /// moved out of it, or rewritten in its clusters.
    fn settle_list<T: BlockIo + ?Sized>(&mut self, io: &mut T) -> Result<(), Error> {
        if self.list == List::Absent {
            return Ok(());
        }
        loop {
            sort_entries(&mut self.entries, self.upcase.as_ref());
            let value = encode_entries(&self.entries);
            let current = self.list_location();
            match self.list.clone() {
                List::Absent => return Ok(()),
                List::Resident => {
                    let want = align8(RESIDENT_HEADER + value.len());
                    let have = current.map_or(0, |l| l.attr_length);
                    if free_space(&self.segments[0].bytes) + have >= want {
                        let base = &mut self.segments[0].bytes;
                        match current {
                            Some(loc) => crate::attr_resize::set_resident_value(
                                base,
                                loc.attr_offset,
                                &value,
                            )?,
                            None => {
                                let id = crate::attr_resize::allocate_attribute_id(base);
                                let attr = crate::record_build::build_resident_unnamed_attribute(
                                    AttrType::AttributeList as u32,
                                    id,
                                    &value,
                                )?;
                                crate::attr_resize::insert_attribute_listed_typed(
                                    base,
                                    &attr,
                                    self.upcase.as_ref(),
                                )
                                .map_err(Error::from)?;
                            }
                        }
                        self.segments[0].dirty = true;
                        return Ok(());
                    }
                    if self.evict_named_data(io)? > 0 {
                        continue;
                    }
                    if self.move_list_to_clusters(io, value.len())? {
                        continue;
                    }
                }
                List::NonResident { allocated, .. } => {
                    if value.len() as u64 <= allocated {
                        let loc = current.ok_or(Error::io("the $ATTRIBUTE_LIST went missing"))?;
                        let base = &mut self.segments[0].bytes;
                        for field in [
                            attr_off::NONRES_DATA_LENGTH,
                            attr_off::NONRES_INITIALIZED_LENGTH,
                        ] {
                            base[loc.attr_offset + field..loc.attr_offset + field + 8]
                                .copy_from_slice(&(value.len() as u64).to_le_bytes());
                        }
                        self.segments[0].dirty = true;
                        self.list_value = Some(value);
                        return Ok(());
                    }
                    if self.move_list_to_clusters(io, value.len())? {
                        continue;
                    }
                }
            }
            if self.evict_named_data(io)? == 0 && self.evict_file_names(io)? == 0 {
                return Err(Error::refused(format!(
                    "record {}: its $ATTRIBUTE_LIST does not fit, and nothing left in the record \
                     is an attribute this driver moves",
                    self.segments[0].number
                )));
            }
        }
    }

    /// Give the list a run of clusters `len` bytes hold, and put a
    /// non-resident header in the base record mapping it, in place of
    /// whatever the base record held for it. `false`, with nothing
    /// changed but the clusters given back, when the base record has no
    /// room for the header.
    fn move_list_to_clusters<T: BlockIo + ?Sized>(
        &mut self,
        io: &mut T,
        len: usize,
    ) -> Result<bool, Error> {
        let cluster = self.params.cluster_size;
        let count = (len as u64).div_ceil(cluster).max(1);
        let bitmap = crate::bitmap::locate_bitmap_io(io)?;
        let lcn = crate::bitmap::find_free_run_io(io, &bitmap, count, self.params.mft_lcn)?
            .ok_or_else(|| {
                Error::no_space(format!(
                    "no free run of {count} clusters for an $ATTRIBUTE_LIST"
                ))
            })?;
        let mapping = data_runs::encode_runs(&[DataRun {
            starting_vcn: 0,
            length: count,
            lcn: Some(lcn),
        }])?;
        let current = self.list_location();
        let id = match current {
            Some(loc) => loc.attribute_id,
            None => crate::attr_resize::allocate_attribute_id(&mut self.segments[0].bytes),
        };
        let attr = crate::record_build::build_nonresident_attribute(
            AttrType::AttributeList as u32,
            None,
            id,
            len as u64,
            count * cluster,
            len as u64,
            count as i64 - 1,
            &mapping,
        )?;
        let have = current.map_or(0, |l| l.attr_length);
        if free_space(&self.segments[0].bytes) + have < attr.len() {
            return Ok(false);
        }
        crate::bitmap::allocate_io(io, &bitmap, lcn, count)?;
        self.new_clusters.push((lcn, count));
        let base = &mut self.segments[0].bytes;
        match current {
            Some(loc) => crate::attr_resize::replace_attribute(base, loc.attr_offset, &attr)?,
            None => {
                crate::attr_resize::insert_attribute_listed_typed(base, &attr, self.upcase.as_ref())
                    .map_err(Error::from)?
            }
        }
        self.segments[0].dirty = true;
        if let List::NonResident { runs, .. } = &self.list {
            self.released_clusters.extend(runs.iter().copied());
        }
        self.list = List::NonResident {
            runs: vec![(lcn, count)],
            allocated: count * cluster,
        };
        Ok(true)
    }

    /// Settle the list and write every record and cluster this change
    /// touched, in the order the module documents. Anything refused before
    /// the first write gives back what the change allocated.
    pub(crate) fn commit<T: BlockIo + ?Sized>(mut self, io: &mut T) -> Result<(), Error> {
        if let Err(e) = self.settle_list(io) {
            return Err(self.abandon(io, e));
        }
        // An extension record left empty is freed rather than written; a
        // fresh one never was in use.
        let emptied: Vec<u64> = self.segments[1..]
            .iter()
            .filter(|s| s.is_empty())
            .map(|s| s.number)
            .collect();
        if let Err(e) = self.write_list_value(io) {
            return Err(self.abandon(io, e));
        }
        let params = self.params;
        for segment in self.segments[1..].iter_mut().filter(|s| s.dirty) {
            if emptied.contains(&segment.number) {
                continue;
            }
            if segment.fresh {
                let mut bytes = segment.bytes.clone();
                mft_io::apply_fixup_on_write(&mut bytes, params.bytes_per_sector)?;
                let at = mft_io::mft_record_offset_io(io, &params, segment.number)?;
                io.write_all_at(at, &bytes)
                    .map_err(|e| Error::io(format!("write record {}: {e}", segment.number)))?;
                io.sync()?;
                mft_io::sync_mftmirr_record_with_io(io, &params, segment.number)?;
            } else {
                let bytes = segment.bytes.clone();
                mft_io::update_mft_record_io(io, segment.number, |record| {
                    record.copy_from_slice(&bytes);
                    Ok(())
                })?;
            }
        }
        if self.segments[0].dirty {
            let bytes = self.segments[0].bytes.clone();
            mft_io::update_mft_record_io(io, self.segments[0].number, |record| {
                record.copy_from_slice(&bytes);
                Ok(())
            })?;
        }
        let bitmap = crate::mft_bitmap::locate_io(io)?;
        for segment in &self.segments[1..] {
            if !emptied.contains(&segment.number) {
                continue;
            }
            if !segment.fresh {
                mft_io::update_mft_record_io(io, segment.number, |record| {
                    let flags = mft_io::record_flags(record) & !mft_io::MFT_FLAG_IN_USE;
                    record[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
                    Ok(())
                })?;
            }
            crate::mft_bitmap::free_io(io, &bitmap, segment.number)?;
        }
        if !self.released_clusters.is_empty() {
            let bitmap = crate::bitmap::locate_bitmap_io(io)?;
            for &(lcn, count) in &self.released_clusters {
                crate::bitmap::free_io(io, &bitmap, lcn, count)?;
            }
        }
        Ok(())
    }

    /// Write a non-resident list's value over its clusters, zero-filled
    /// to their end.
    fn write_list_value<T: BlockIo + ?Sized>(&self, io: &mut T) -> Result<(), Error> {
        let (Some(value), List::NonResident { runs, allocated }) = (&self.list_value, &self.list)
        else {
            return Ok(());
        };
        let mut bytes = value.clone();
        bytes.resize(*allocated as usize, 0);
        let cluster = self.params.cluster_size;
        let mut done = 0usize;
        for &(lcn, count) in runs {
            let len = (count * cluster) as usize;
            let chunk = &bytes[done..(done + len).min(bytes.len())];
            let at = mft_io::cluster_span(&self.params, lcn, 0, 0, chunk.len() as u64, io.size())?;
            io.write_all_at(at, chunk)
                .map_err(|e| Error::io(format!("write the $ATTRIBUTE_LIST: {e}")))?;
            done += chunk.len();
        }
        Ok(io.sync()?)
    }

    /// Give back what this change allocated, and return `cause`, noting
    /// what could not be given back.
    pub(crate) fn abandon<T: BlockIo + ?Sized>(self, io: &mut T, cause: Error) -> Error {
        let mut failures = Vec::new();
        if !self.new_records.is_empty() {
            match crate::mft_bitmap::locate_io(io) {
                Ok(bitmap) => {
                    for &n in &self.new_records {
                        if let Err(e) = crate::mft_bitmap::free_io(io, &bitmap, n) {
                            failures.push(format!("MFT record {n}: {e}"));
                        }
                    }
                }
                Err(e) => failures.push(format!("$MFT:$Bitmap: {e}")),
            }
        }
        if !self.new_clusters.is_empty() {
            match crate::bitmap::locate_bitmap_io(io) {
                Ok(bitmap) => {
                    for &(lcn, count) in &self.new_clusters {
                        if let Err(e) = crate::bitmap::free_io(io, &bitmap, lcn, count) {
                            failures.push(format!("{count} clusters at {lcn}: {e}"));
                        }
                    }
                }
                Err(e) => failures.push(format!("$Bitmap: {e}")),
            }
        }
        if failures.is_empty() {
            cause
        } else {
            cause.map_message(|m| {
                format!(
                    "{m} (and what it allocated was not all given back: {})",
                    failures.join("; ")
                )
            })
        }
    }
}

/// An attribute [`ListedFile::find`] located: which of the file's records
/// it is in, and where.
pub(crate) struct Found {
    segment: usize,
    pub(crate) loc: AttrLocation,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(type_code: u32, name: &str, vcn: u64) -> Entry {
        Entry {
            type_code,
            name: name.encode_utf16().collect(),
            starting_vcn: vcn,
            reference: encode_file_reference(39, 1),
            attribute_id: 7,
        }
    }

    #[test]
    fn an_entry_encodes_as_windows_writes_one() {
        // Windows' entry for `stream-000` in ntfs-attrlist.img: 0x30 bytes,
        // the name at 0x1A.
        let e = entry(0x80, "stream-000", 0);
        assert_eq!(e.len(), 0x30);
        let bytes = encode_entries(std::slice::from_ref(&e));
        assert_eq!(bytes.len(), 0x30);
        assert_eq!(&bytes[4..6], &[0x30, 0]);
        assert_eq!(bytes[6], 10);
        assert_eq!(bytes[7], 0x1A);
        assert_eq!(u64_at(&bytes, 16), encode_file_reference(39, 1));
        assert_eq!(decode_entries(&bytes).unwrap(), vec![e]);
        // An unnamed one is 0x20.
        assert_eq!(entry(0x10, "", 0).len(), 0x20);
    }

    #[test]
    fn entries_sort_by_type_then_name_then_vcn() {
        let mut v = vec![
            entry(0x80, "b", 0),
            entry(0x80, "", 0),
            entry(0x30, "", 0),
            entry(0x80, "A", 0),
            entry(0x80, "a", 8),
            entry(0x10, "", 0),
        ];
        sort_entries(&mut v, None);
        let keys: Vec<(u32, String, u64)> = v
            .iter()
            .map(|e| {
                (
                    e.type_code,
                    String::from_utf16_lossy(&e.name),
                    e.starting_vcn,
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (0x10, String::new(), 0),
                (0x30, String::new(), 0),
                (0x80, String::new(), 0),
                (0x80, "A".into(), 0),
                (0x80, "a".into(), 8),
                (0x80, "b".into(), 0),
            ]
        );
    }

    #[test]
    fn a_list_that_runs_past_its_value_is_refused() {
        let mut bytes = encode_entries(&[entry(0x80, "s", 0)]);
        bytes[4] = 0x40;
        assert!(decode_entries(&bytes).is_err());
    }
}
