//! Salvage reads that follow a file record out of itself (#1379): where the
//! file records are ([`MftMap`]), the attributes an `$ATTRIBUTE_LIST` keeps in
//! extension records ([`ResolvedRecord`]), the streams those attributes make
//! up ([`SalvageStream`]), and their bytes, LZNT1-compressed or not.
//!
//! Everything here is read-only and reports damage rather than working round
//! it: an extension record that is missing, fails its fixups, or names
//! another base record leaves the streams it held a part of marked
//! incomplete, and an incomplete stream is refused, never read short.

use super::{
    decode_file_name, inspect_file_record, AttributeEvidence, AttributeValue, FileNameEvidence,
    PrimitiveAPI, RecordEvidence, MAX_RECORD_BYTES,
};
use crate::attr_io::{attr_flags, AttrType};
use crate::compression;
use crate::data_runs::DataRun;
use crate::error::Error;
use crate::mft_io::BootParams;
use std::collections::BTreeMap;

/// NTFS compresses in units of 16 clusters, `compression_unit = 4`, and
/// defines no larger unit (the same bound the mounted reader keeps).
const MAX_COMPRESSION_UNIT: u8 = 4;
const RECORD_NUMBER_MASK: u64 = 0x0000_ffff_ffff_ffff;
/// An attribute list entry's fixed part; the name follows it.
const LIST_ENTRY_NAME_OFFSET: usize = 0x1a;
/// The most extension records one resolution reads, so a damaged list
/// cannot send it across the whole MFT.
const MAX_EXTENSION_RECORDS: usize = 4096;
/// The largest `$ATTRIBUTE_LIST` value read. Windows' lists stay far below
/// this; a larger declared size is damage.
const MAX_ATTRIBUTE_LIST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CLUSTER_SIZE: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
enum Layout {
    /// A hypothesis: the records follow each other from `start`.
    Contiguous { start: u64 },
    /// `$MFT`'s own run list; `records` is how many records it maps.
    Runs { runs: Vec<DataRun>, records: u64 },
}

/// Where a volume's file records are, so a reference from one record to
/// another (an attribute list entry, a base record reference) can be read.
#[derive(Clone, Debug)]
pub struct MftMap {
    layout: Layout,
    pub record_size: usize,
    pub sector_size: u16,
    pub cluster_size: u64,
    /// What this map could not establish, such as the later extents of a
    /// `$MFT` whose own attribute list did not resolve. A record outside
    /// what is mapped reads as an error.
    pub limitations: Vec<String>,
}

fn check_geometry(record_size: usize, sector_size: u16, cluster_size: u64) -> Result<(), Error> {
    if !record_size.is_power_of_two() || !(512..=MAX_RECORD_BYTES).contains(&record_size) {
        return Err(Error::invalid("invalid file record size"));
    }
    if !sector_size.is_power_of_two() || !(512..=4096).contains(&sector_size) {
        return Err(Error::invalid("invalid sector size"));
    }
    if !cluster_size.is_power_of_two() || !(512..=MAX_CLUSTER_SIZE).contains(&cluster_size) {
        return Err(Error::invalid("invalid cluster size"));
    }
    Ok(())
}

impl MftMap {
    /// A map that takes the records to follow each other from
    /// `first_record_offset`, for when `$MFT`'s own record is lost. It is a
    /// hypothesis: a fragmented `$MFT` breaks it after its first extent, and
    /// a record read through it is only what lies at that offset.
    pub fn contiguous(
        first_record_offset: u64,
        record_size: usize,
        sector_size: u16,
        cluster_size: u64,
    ) -> Result<Self, Error> {
        check_geometry(record_size, sector_size, cluster_size)?;
        Ok(Self {
            layout: Layout::Contiguous {
                start: first_record_offset,
            },
            record_size,
            sector_size,
            cluster_size,
            limitations: vec!["contiguous hypothesis: $MFT's own run list was not read".into()],
        })
    }

    /// A map from `$MFT`'s run list, which must start at VCN 0 and run on
    /// without a gap. `data_size` is `$MFT`'s data size; records past it, or
    /// past what the runs map, are outside the map.
    pub fn from_runs(
        runs: Vec<DataRun>,
        data_size: u64,
        record_size: usize,
        sector_size: u16,
        cluster_size: u64,
    ) -> Result<Self, Error> {
        check_geometry(record_size, sector_size, cluster_size)?;
        let mut next = 0u64;
        for run in &runs {
            if run.starting_vcn != next || run.length == 0 {
                return Err(Error::invalid("$MFT run list is discontinuous"));
            }
            next = next
                .checked_add(run.length)
                .ok_or_else(|| Error::invalid("$MFT run list overflows"))?;
        }
        let mapped = next
            .checked_mul(cluster_size)
            .ok_or_else(|| Error::invalid("$MFT run list overflows"))?;
        let records = data_size.min(mapped) / record_size as u64;
        let mut limitations = Vec::new();
        if mapped < data_size {
            limitations.push(format!(
                "$MFT's run list maps {mapped} of its {data_size} bytes; later records are \
                 outside this map"
            ));
        }
        Ok(Self {
            layout: Layout::Runs { runs, records },
            record_size,
            sector_size,
            cluster_size,
            limitations,
        })
    }

    /// Whether this map is the contiguous hypothesis rather than `$MFT`'s runs.
    pub fn is_hypothesis(&self) -> bool {
        matches!(self.layout, Layout::Contiguous { .. })
    }

    /// How many records the map covers, when it is read from `$MFT`.
    pub fn records(&self) -> Option<u64> {
        match &self.layout {
            Layout::Contiguous { .. } => None,
            Layout::Runs { records, .. } => Some(*records),
        }
    }

    /// The volume offset of record `number`'s first byte.
    pub fn record_offset(&self, number: u64) -> Option<u64> {
        self.pieces(number)?.first().map(|(offset, _)| *offset)
    }

    /// The record that starts at volume offset `offset`, if one does.
    pub fn record_number_at(&self, offset: u64) -> Option<u64> {
        let size = self.record_size as u64;
        match &self.layout {
            Layout::Contiguous { start } => {
                let byte = offset.checked_sub(*start)?;
                byte.is_multiple_of(size).then_some(byte / size)
            }
            Layout::Runs { runs, records } => runs.iter().find_map(|run| {
                let start = run.lcn?.checked_mul(self.cluster_size)?;
                let length = run.length.checked_mul(self.cluster_size)?;
                let within = offset.checked_sub(start).filter(|at| *at < length)?;
                let byte = run.starting_vcn.checked_mul(self.cluster_size)? + within;
                let number = byte / size;
                (byte.is_multiple_of(size)
                    && number < *records
                    && self.record_offset(number) == Some(offset))
                .then_some(number)
            }),
        }
    }

    /// The volume ranges holding record `number`, in order.
    fn pieces(&self, number: u64) -> Option<Vec<(u64, usize)>> {
        let size = self.record_size as u64;
        match &self.layout {
            Layout::Contiguous { start } => Some(vec![(
                start.checked_add(number.checked_mul(size)?)?,
                self.record_size,
            )]),
            Layout::Runs { runs, records } => {
                if number >= *records {
                    return None;
                }
                let start = number.checked_mul(size)?;
                let end = start.checked_add(size)?;
                let mut pieces: Vec<(u64, usize)> = Vec::new();
                let mut position = start;
                while position < end {
                    let vcn = position / self.cluster_size;
                    let run = find_run(runs, vcn)?;
                    let run_end = (run.starting_vcn + run.length).checked_mul(self.cluster_size)?;
                    let take = end.min(run_end) - position;
                    let physical = (run.lcn? + (vcn - run.starting_vcn))
                        .checked_mul(self.cluster_size)?
                        .checked_add(position % self.cluster_size)?;
                    match pieces.last_mut() {
                        Some((at, length)) if *at + *length as u64 == physical => {
                            *length += take as usize
                        }
                        _ => pieces.push((physical, take as usize)),
                    }
                    position += take;
                }
                Some(pieces)
            }
        }
    }
}

/// The run holding `vcn`, by binary search over runs sorted by VCN.
fn find_run(runs: &[DataRun], vcn: u64) -> Option<&DataRun> {
    let index = runs.partition_point(|run| run.starting_vcn <= vcn);
    let run = runs.get(index.checked_sub(1)?)?;
    (vcn - run.starting_vcn < run.length).then_some(run)
}

/// One attribute of a resolved record, and where it was found.
#[derive(Clone, Debug)]
pub struct ResolvedAttribute {
    /// The number of the record holding it, when known.
    pub record: Option<u64>,
    /// Whether it is held in the base record.
    pub in_base: bool,
    pub attribute: AttributeEvidence,
}

/// An `$ATTRIBUTE_LIST` entry whose attribute could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingExtent {
    pub type_code: u32,
    pub name: String,
    pub starting_vcn: u64,
    /// The record the list says holds it.
    pub record: u64,
    pub reason: String,
}

/// A base record with every attribute its `$ATTRIBUTE_LIST` names, from
/// whichever record holds it.
#[derive(Clone, Debug)]
pub struct ResolvedRecord {
    /// The base record's number: its own claim, else where the map puts it.
    pub record_number: Option<u64>,
    pub base: RecordEvidence,
    /// The extension records read, by number.
    pub extensions: Vec<(u64, RecordEvidence)>,
    /// The base record's attributes, then the extension records' in list order.
    pub attributes: Vec<ResolvedAttribute>,
    /// List entries whose attribute could not be read.
    pub missing: Vec<MissingExtent>,
    /// False when the record has a list that could not be read whole, so it
    /// may name attributes nobody has seen.
    pub attribute_list_complete: bool,
    pub limitations: Vec<String>,
}

/// One extent of a stream: a `$DATA` attribute and the VCNs it maps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamExtent {
    pub record: Option<u64>,
    pub attribute_id: u16,
    pub first_vcn: u64,
    /// `None` for a resident stream.
    pub last_vcn: Option<u64>,
}

/// A `$DATA` stream joined from its extents.
#[derive(Clone, Debug)]
pub struct SalvageStream {
    /// Empty for the unnamed stream.
    pub name: String,
    /// The first extent's attribute flags: compression, encryption, sparse.
    pub flags: u16,
    /// The value of a resident stream.
    pub resident: Option<Vec<u8>>,
    /// The joined run list of a non-resident stream, from VCN 0.
    pub runs: Vec<DataRun>,
    pub data_size: u64,
    pub initialized_size: u64,
    pub compression_unit: u8,
    pub extents: Vec<StreamExtent>,
    /// Why the stream cannot be read whole. A stream with any is refused.
    pub limitations: Vec<String>,
}

impl SalvageStream {
    pub fn len(&self) -> u64 {
        self.data_size
    }

    pub fn is_empty(&self) -> bool {
        self.data_size == 0
    }

    pub fn is_complete(&self) -> bool {
        self.limitations.is_empty()
    }

    pub fn is_compressed(&self) -> bool {
        self.flags & attr_flags::COMPRESSION_MASK != 0
    }

    pub fn is_encrypted(&self) -> bool {
        self.flags & attr_flags::ENCRYPTED != 0
    }

    pub(super) fn from_single_attribute(
        record: Option<u64>,
        attribute: &AttributeEvidence,
    ) -> Self {
        join_stream(&attribute.name, &[(record, attribute)], &[], true)
    }
}

/// Joins one stream's extents, sorted by first VCN, and says what is wrong
/// with the result rather than guessing past it.
fn join_stream(
    name: &str,
    extents: &[(Option<u64>, &AttributeEvidence)],
    missing: &[&MissingExtent],
    list_complete: bool,
) -> SalvageStream {
    let mut stream = SalvageStream {
        name: name.to_owned(),
        flags: 0,
        resident: None,
        runs: Vec::new(),
        data_size: 0,
        initialized_size: 0,
        compression_unit: 0,
        extents: Vec::new(),
        limitations: Vec::new(),
    };
    for gone in missing {
        stream.limitations.push(format!(
            "the extent at VCN {} in record {} is missing: {}",
            gone.starting_vcn, gone.record, gone.reason
        ));
    }
    if !list_complete {
        stream
            .limitations
            .push("the attribute list was not read whole, so extents may be missing".into());
    }
    let mut sorted: Vec<_> = extents.to_vec();
    sorted.sort_by_key(|(_, attribute)| match &attribute.value {
        AttributeValue::Resident(_) => 0,
        AttributeValue::NonResident { first_vcn, .. } => *first_vcn,
    });
    for (record, attribute) in &sorted {
        let (first_vcn, last_vcn) = match &attribute.value {
            AttributeValue::Resident(_) => (0, None),
            AttributeValue::NonResident { first_vcn, .. } => (*first_vcn, attribute.last_vcn),
        };
        stream.extents.push(StreamExtent {
            record: *record,
            attribute_id: attribute.attribute_id,
            first_vcn,
            last_vcn,
        });
    }
    let Some((_, first)) = sorted.first() else {
        stream.limitations.push("the stream has no extent".into());
        return stream;
    };
    stream.flags = first.flags;
    stream.compression_unit = first.compression_unit;
    if let AttributeValue::Resident(value) = &first.value {
        if sorted.len() > 1 {
            stream
                .limitations
                .push("a resident stream has further extents".into());
        }
        stream.data_size = value.len() as u64;
        stream.initialized_size = value.len() as u64;
        stream.resident = Some(value.clone());
        return stream;
    }
    let mut expected = 0u64;
    for (record, attribute) in &sorted {
        let AttributeValue::NonResident {
            runs,
            first_vcn,
            data_size,
            initialized_size,
        } = &attribute.value
        else {
            stream
                .limitations
                .push("a non-resident stream has a resident extent".into());
            return stream;
        };
        let at = record.map_or_else(|| "this record".to_owned(), |n| format!("record {n}"));
        if *first_vcn == 0 {
            stream.data_size = *data_size;
            stream.initialized_size = *initialized_size;
        }
        if *first_vcn != expected {
            stream.limitations.push(if *first_vcn > expected {
                format!("VCNs {expected} to {} are in no extent", first_vcn - 1)
            } else {
                format!("the extent in {at} at VCN {first_vcn} overlaps the one before it")
            });
            return stream;
        }
        if sorted.len() == 1 {
            // A lone extent holds the whole stream; the reader checks its runs.
            stream.runs.extend_from_slice(runs);
            continue;
        }
        // An empty stream maps nothing: its last VCN is -1.
        let end = match attribute.last_vcn {
            Some(u64::MAX) if runs.is_empty() && *first_vcn == 0 => 0,
            Some(last) => match last.checked_add(1) {
                Some(end) => end,
                None => {
                    stream
                        .limitations
                        .push(format!("the extent in {at} has an invalid last VCN"));
                    return stream;
                }
            },
            None => {
                stream
                    .limitations
                    .push(format!("the extent in {at} has no last VCN"));
                return stream;
            }
        };
        let mapped: Option<u64> = runs.iter().try_fold(*first_vcn, |vcn, run| {
            (run.starting_vcn == vcn && run.length != 0)
                .then(|| vcn.checked_add(run.length))
                .flatten()
        });
        if mapped != Some(end) {
            stream.limitations.push(format!(
                "the runs of the extent in {at} do not map VCNs {first_vcn} to {}",
                end.saturating_sub(1)
            ));
            return stream;
        }
        stream.runs.extend_from_slice(runs);
        expected = end;
    }
    if sorted
        .first()
        .is_some_and(|(_, attribute)| matches!(&attribute.value, AttributeValue::NonResident { first_vcn, .. } if *first_vcn != 0))
    {
        stream
            .limitations
            .push("no extent starts at VCN 0, which holds the stream's size".into());
    }
    stream
}

impl ResolvedRecord {
    /// Every `$DATA` stream, the unnamed one first, then by name in the order
    /// they were found.
    pub fn streams(&self) -> Vec<SalvageStream> {
        let mut names: Vec<&str> = Vec::new();
        for entry in &self.attributes {
            if entry.attribute.type_code == AttrType::Data as u32
                && !names.contains(&entry.attribute.name.as_str())
            {
                names.push(&entry.attribute.name);
            }
        }
        for gone in &self.missing {
            if gone.type_code == AttrType::Data as u32 && !names.contains(&gone.name.as_str()) {
                names.push(&gone.name);
            }
        }
        names.sort_by_key(|name| !name.is_empty());
        names.into_iter().map(|name| self.join(name)).collect()
    }

    /// The `$DATA` stream called `name`, empty for the unnamed one.
    pub fn stream(&self, name: &str) -> Option<SalvageStream> {
        let known = self.attributes.iter().any(|entry| {
            entry.attribute.type_code == AttrType::Data as u32 && entry.attribute.name == name
        }) || self
            .missing
            .iter()
            .any(|gone| gone.type_code == AttrType::Data as u32 && gone.name == name);
        known.then(|| self.join(name))
    }

    fn join(&self, name: &str) -> SalvageStream {
        let extents: Vec<(Option<u64>, &AttributeEvidence)> = self
            .attributes
            .iter()
            .filter(|entry| {
                entry.attribute.type_code == AttrType::Data as u32 && entry.attribute.name == name
            })
            .map(|entry| (entry.record, &entry.attribute))
            .collect();
        let missing: Vec<&MissingExtent> = self
            .missing
            .iter()
            .filter(|gone| gone.type_code == AttrType::Data as u32 && gone.name == name)
            .collect();
        join_stream(name, &extents, &missing, self.attribute_list_complete)
    }

    /// Every resident `$FILE_NAME`, from whichever record holds it: one per
    /// hard link, and the DOS shadows. `attribute` indexes [`Self::attributes`].
    pub fn file_names(&self) -> Vec<FileNameEvidence> {
        self.attributes
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| decode_file_name(index, &entry.attribute))
            .collect()
    }
}

struct ListEntry {
    type_code: u32,
    name: String,
    starting_vcn: u64,
    reference: u64,
    attribute_id: u16,
}

/// The entries of an `$ATTRIBUTE_LIST` value up to the first one that is
/// damaged, and what was wrong with it.
fn decode_list(value: &[u8]) -> (Vec<ListEntry>, Option<String>) {
    let mut entries = Vec::new();
    let mut at = 0usize;
    while at + LIST_ENTRY_NAME_OFFSET <= value.len() {
        let length = usize::from(u16::from_le_bytes([value[at + 4], value[at + 5]]));
        if length == 0 {
            break;
        }
        if length < LIST_ENTRY_NAME_OFFSET || at + length > value.len() {
            return (
                entries,
                Some(format!(
                    "attribute list entry at {at} has an invalid length {length}"
                )),
            );
        }
        let units = usize::from(value[at + 6]);
        let name_offset = usize::from(value[at + 7]);
        let Some(name_bytes) = value
            .get(at + name_offset..at + name_offset + 2 * units)
            .filter(|_| units == 0 || name_offset + 2 * units <= length)
        else {
            return (
                entries,
                Some(format!(
                    "attribute list entry at {at} has a name past its end"
                )),
            );
        };
        let name: Vec<u16> = name_bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let field = |offset: usize| {
            u64::from_le_bytes(value[at + offset..at + offset + 8].try_into().unwrap())
        };
        entries.push(ListEntry {
            type_code: u32::from_le_bytes(value[at..at + 4].try_into().unwrap()),
            name: String::from_utf16_lossy(&name),
            starting_vcn: field(8),
            reference: field(16),
            attribute_id: u16::from_le_bytes([value[at + 24], value[at + 25]]),
        });
        at += length;
    }
    (entries, None)
}

fn attribute_vcn(attribute: &AttributeEvidence) -> u64 {
    match &attribute.value {
        AttributeValue::Resident(_) => 0,
        AttributeValue::NonResident { first_vcn, .. } => *first_vcn,
    }
}

/// Whether `attribute` is the one `entry` names.
fn entry_names(entry: &ListEntry, attribute: &AttributeEvidence) -> bool {
    attribute.type_code == entry.type_code
        && attribute.attribute_id == entry.attribute_id
        && attribute.name == entry.name
        && attribute_vcn(attribute) == entry.starting_vcn
}

pub(super) fn read_mapped_record<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    map: &MftMap,
    number: u64,
) -> Result<RecordEvidence, Error> {
    let pieces = map
        .pieces(number)
        .ok_or_else(|| Error::invalid(format!("record {number} is outside the MFT map")))?;
    let mut raw = vec![0; map.record_size];
    let mut filled = 0;
    for (offset, length) in &pieces {
        volume.read_volume_bytes(*offset, &mut raw[filled..filled + length])?;
        filled += length;
    }
    Ok(inspect_file_record(pieces[0].0, raw, map.sector_size))
}

pub(super) fn read_mft_map<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    mft_offset: u64,
    record_size: usize,
    sector_size: u16,
    cluster_size: u64,
) -> Result<MftMap, Error> {
    check_geometry(record_size, sector_size, cluster_size)?;
    let mut raw = vec![0; record_size];
    volume.read_volume_bytes(mft_offset, &mut raw)?;
    let record = inspect_file_record(mft_offset, raw, sector_size);
    let map = map_from_record_zero(volume, &record, record_size, sector_size, cluster_size)?;
    if map.record_offset(0) != Some(mft_offset) {
        return Err(Error::invalid(
            "$MFT's run list does not start where its record was read",
        ));
    }
    Ok(map)
}

pub(super) fn read_mft_map_from_mirror<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    boot: &BootParams,
) -> Result<MftMap, Error> {
    let record = read_mirror_record(volume, boot, 0)?;
    let mut map = map_from_record_zero(
        volume,
        &record,
        boot.file_record_size as usize,
        boot.bytes_per_sector,
        boot.cluster_size,
    )?;
    map.limitations.insert(
        0,
        "built from $MFTMirr's copy of $MFT's record 0, which is only as current as the \
         mirror"
            .into(),
    );
    let expected = boot.mft_lcn.checked_mul(boot.cluster_size);
    if map.record_offset(0) != expected {
        map.limitations.push(format!(
            "$MFTMirr's record 0 puts $MFT at byte {:?}, and the boot sector at cluster {}",
            map.record_offset(0),
            boot.mft_lcn
        ));
    }
    Ok(map)
}

pub(super) fn read_mirror_record<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    boot: &BootParams,
    number: u64,
) -> Result<RecordEvidence, Error> {
    if number >= boot.mft_mirror_records() {
        return Err(Error::invalid(format!(
            "$MFTMirr holds $MFT's first {} records, not record {number}",
            boot.mft_mirror_records()
        )));
    }
    let offset = boot
        .mft_mirror_offset()
        .and_then(|start| start.checked_add(number.checked_mul(boot.file_record_size)?))
        .ok_or_else(|| Error::invalid("the boot sector's $MFTMirr cluster overflows"))?;
    volume.read_file_record_at(
        offset,
        boot.file_record_size as usize,
        boot.bytes_per_sector,
    )
}

/// A map from `$MFT`'s own record 0, wherever that copy was read, following
/// its attribute list when it has one.
fn map_from_record_zero<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    record: &RecordEvidence,
    record_size: usize,
    sector_size: u16,
    cluster_size: u64,
) -> Result<MftMap, Error> {
    if record.fixed.is_none() || record.base_reference != 0 {
        return Err(Error::invalid(format!(
            "$MFT's record at {} does not validate: {}",
            record.offset,
            record.issues.join("; ")
        )));
    }
    let first = record
        .attributes
        .iter()
        .find(|attribute| {
            attribute.type_code == AttrType::Data as u32
                && attribute.name.is_empty()
                && matches!(
                    attribute.value,
                    AttributeValue::NonResident { first_vcn: 0, .. }
                )
        })
        .ok_or_else(|| Error::invalid("$MFT's record has no first $DATA extent"))?;
    let AttributeValue::NonResident {
        runs, data_size, ..
    } = &first.value
    else {
        unreachable!("matched as non-resident");
    };
    let mut map = MftMap::from_runs(
        runs.clone(),
        *data_size,
        record_size,
        sector_size,
        cluster_size,
    )?;
    if record.has_attribute_list() {
        // Record 0 is $MFT itself, wherever this copy of it came from.
        let mut base = record.clone();
        base.record_number.get_or_insert(0);
        let resolved = resolve_salvage_record(volume, &base, &map)?;
        match resolved.stream("") {
            Some(stream) if stream.is_complete() => {
                let mut full = MftMap::from_runs(
                    stream.runs,
                    stream.data_size,
                    record_size,
                    sector_size,
                    cluster_size,
                )?;
                full.limitations.extend(resolved.limitations);
                map = full;
            }
            Some(stream) => {
                map.limitations.extend(
                    stream
                        .limitations
                        .into_iter()
                        .map(|limitation| format!("$MFT: {limitation}")),
                );
            }
            None => map
                .limitations
                .push("$MFT's attribute list names no unnamed $DATA".into()),
        }
    }
    Ok(map)
}

pub(super) fn resolve_salvage_record<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    base: &RecordEvidence,
    map: &MftMap,
) -> Result<ResolvedRecord, Error> {
    if base.fixed.is_none() || base.base_reference != 0 {
        return Err(Error::invalid(
            "record has no validated base-record interpretation",
        ));
    }
    let number = base
        .record_number
        .or_else(|| map.record_number_at(base.offset));
    let mut resolved = ResolvedRecord {
        record_number: number,
        base: base.clone(),
        extensions: Vec::new(),
        attributes: base
            .attributes
            .iter()
            .map(|attribute| ResolvedAttribute {
                record: number,
                in_base: true,
                attribute: attribute.clone(),
            })
            .collect(),
        missing: Vec::new(),
        attribute_list_complete: true,
        limitations: base
            .issues
            .iter()
            .map(|issue| format!("base record: {issue}"))
            .collect(),
    };
    let Some(list) = base
        .attributes
        .iter()
        .find(|attribute| attribute.type_code == AttrType::AttributeList as u32)
    else {
        return Ok(resolved);
    };
    let value = match &list.value {
        AttributeValue::Resident(value) => Ok(value.clone()),
        AttributeValue::NonResident {
            data_size,
            first_vcn: 0,
            ..
        } if *data_size <= MAX_ATTRIBUTE_LIST_BYTES => {
            let stream = SalvageStream::from_single_attribute(number, list);
            let mut value = vec![0; *data_size as usize];
            read_stream_at(volume, &stream, map.cluster_size, 0, &mut value).and_then(|count| {
                if count == value.len() {
                    Ok(value)
                } else {
                    Err(Error::invalid("attribute list read short"))
                }
            })
        }
        AttributeValue::NonResident { .. } => Err(Error::invalid(
            "attribute list is not a single extent within the size this reads",
        )),
    };
    let value = match value {
        Ok(value) => value,
        Err(error) => {
            resolved.attribute_list_complete = false;
            resolved
                .limitations
                .push(format!("the attribute list could not be read: {error}"));
            return Ok(resolved);
        }
    };
    let (entries, damage) = decode_list(&value);
    if let Some(damage) = damage {
        resolved.attribute_list_complete = false;
        resolved.limitations.push(damage);
    }
    let missing = |entry: &ListEntry, reason: String| MissingExtent {
        type_code: entry.type_code,
        name: entry.name.clone(),
        starting_vcn: entry.starting_vcn,
        record: entry.reference & RECORD_NUMBER_MASK,
        reason,
    };
    let mut by_record: BTreeMap<u64, Vec<&ListEntry>> = BTreeMap::new();
    for entry in &entries {
        let target = entry.reference & RECORD_NUMBER_MASK;
        let in_base = match number {
            Some(number) => target == number,
            None => base
                .attributes
                .iter()
                .any(|attribute| entry_names(entry, attribute)),
        };
        if in_base {
            if !base
                .attributes
                .iter()
                .any(|attribute| entry_names(entry, attribute))
            {
                resolved.missing.push(missing(
                    entry,
                    "the base record does not hold the attribute its list names".into(),
                ));
            }
        } else {
            by_record.entry(target).or_default().push(entry);
        }
    }
    let Some(base_number) = number else {
        resolved.limitations.push(
            "the base record's number is unknown, so extension records cannot be checked".into(),
        );
        for entries in by_record.values() {
            for entry in entries {
                resolved
                    .missing
                    .push(missing(entry, "the base record's number is unknown".into()));
            }
        }
        return Ok(resolved);
    };
    for (index, (target, entries)) in by_record.into_iter().enumerate() {
        let reason = if index >= MAX_EXTENSION_RECORDS {
            Err(format!(
                "more than {MAX_EXTENSION_RECORDS} extension records"
            ))
        } else {
            read_mapped_record(volume, map, target)
                .map_err(|error| format!("record {target} could not be read: {error}"))
                .and_then(|record| {
                    let expected = base_number | (u64::from(base.sequence) << 48);
                    if record.fixed.is_none() {
                        Err(format!(
                            "record {target} does not validate: {}",
                            record.issues.join("; ")
                        ))
                    } else if !record.in_use {
                        Err(format!("record {target} is not in use"))
                    } else if record.base_reference != expected {
                        Err(format!(
                            "record {target} names base record {} sequence {}, not {base_number} \
                             sequence {}",
                            record.base_reference & RECORD_NUMBER_MASK,
                            record.base_reference >> 48,
                            base.sequence
                        ))
                    } else {
                        Ok(record)
                    }
                })
        };
        let record = match reason {
            Ok(record) => record,
            Err(reason) => {
                for entry in entries {
                    resolved.missing.push(missing(entry, reason.clone()));
                }
                continue;
            }
        };
        for entry in entries {
            let sequence = (entry.reference >> 48) as u16;
            if sequence != record.sequence {
                resolved.missing.push(missing(
                    entry,
                    format!(
                        "the list names record {target} sequence {sequence}, and it has sequence {}",
                        record.sequence
                    ),
                ));
                continue;
            }
            match record
                .attributes
                .iter()
                .find(|attribute| entry_names(entry, attribute))
            {
                Some(attribute) => resolved.attributes.push(ResolvedAttribute {
                    record: Some(target),
                    in_base: false,
                    attribute: attribute.clone(),
                }),
                None => resolved.missing.push(missing(
                    entry,
                    format!(
                        "record {target} holds no attribute {} with id {}",
                        entry.type_code, entry.attribute_id
                    ),
                )),
            }
        }
        resolved.extensions.push((target, record));
    }
    Ok(resolved)
}

pub(super) fn read_stream_at<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    stream: &SalvageStream,
    cluster_size: u64,
    offset: u64,
    output: &mut [u8],
) -> Result<usize, Error> {
    if let Some(limitation) = stream.limitations.first() {
        return Err(Error::refused(format!(
            "stream requires unresolved attribute extents: {limitation}"
        )));
    }
    if stream.is_encrypted() {
        return Err(Error::refused(
            "encrypted salvage streams are refused: their clusters hold $EFS ciphertext",
        ));
    }
    if let Some(data) = &stream.resident {
        let count = (data.len() as u64)
            .saturating_sub(offset)
            .min(output.len() as u64) as usize;
        if count != 0 {
            output[..count].copy_from_slice(&data[offset as usize..offset as usize + count]);
        }
        return Ok(count);
    }
    if !cluster_size.is_power_of_two() || !(512..=MAX_CLUSTER_SIZE).contains(&cluster_size) {
        return Err(Error::invalid("invalid cluster size"));
    }
    if stream.initialized_size > stream.data_size {
        return Err(Error::invalid("initialized size exceeds stream size"));
    }
    let mut next_vcn = 0u64;
    for run in &stream.runs {
        if run.starting_vcn != next_vcn || run.length == 0 {
            return Err(Error::invalid("stream run list is discontinuous"));
        }
        next_vcn = next_vcn
            .checked_add(run.length)
            .ok_or_else(|| Error::invalid("run list overflow"))?;
        if let Some(lcn) = run.lcn {
            lcn.checked_add(run.length)
                .and_then(|end| end.checked_mul(cluster_size))
                .filter(|end| *end <= volume.size_bytes())
                .ok_or_else(|| Error::invalid("data run exceeds volume"))?;
        }
    }
    if next_vcn
        .checked_mul(cluster_size)
        .filter(|bytes| *bytes >= stream.data_size)
        .is_none()
    {
        return Err(Error::invalid("run list ends before stream size"));
    }
    let count = stream
        .data_size
        .saturating_sub(offset)
        .min(output.len() as u64) as usize;
    let output = &mut output[..count];
    if stream.is_compressed() {
        read_compressed(volume, stream, cluster_size, offset, output)?;
    } else {
        read_plain(volume, stream, cluster_size, offset, output)?;
    }
    Ok(count)
}

fn read_plain<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    stream: &SalvageStream,
    cluster_size: u64,
    offset: u64,
    output: &mut [u8],
) -> Result<(), Error> {
    let count = output.len();
    let mut copied = 0;
    while copied < count {
        let position = offset + copied as u64;
        if position >= stream.initialized_size {
            output[copied..].fill(0);
            break;
        }
        let vcn = position / cluster_size;
        let run = find_run(&stream.runs, vcn)
            .ok_or_else(|| Error::invalid("stream has an unmapped range"))?;
        let within = position - run.starting_vcn * cluster_size;
        let chunk = (run.length * cluster_size - within)
            .min((count - copied) as u64)
            .min(stream.initialized_size - position) as usize;
        let target = &mut output[copied..copied + chunk];
        match run.lcn {
            Some(lcn) => {
                let physical = lcn
                    .checked_mul(cluster_size)
                    .and_then(|start| start.checked_add(within))
                    .ok_or_else(|| Error::invalid("stream offset overflow"))?;
                volume.read_volume_bytes(physical, target)?;
            }
            None => target.fill(0),
        }
        copied += chunk;
    }
    Ok(())
}

/// Decodes the compression units `output` overlaps. A unit is `2^cu`
/// clusters, stored one of three ways: every cluster allocated, stored as is;
/// some leading clusters allocated and the rest a hole, LZNT1-compressed into
/// the allocated ones; no cluster allocated, all zeros. Bytes past the
/// initialized size read as zeros, as they do uncompressed.
fn read_compressed<V: PrimitiveAPI + ?Sized>(
    volume: &V,
    stream: &SalvageStream,
    cluster_size: u64,
    offset: u64,
    output: &mut [u8],
) -> Result<(), Error> {
    if stream.flags & attr_flags::COMPRESSION_MASK != attr_flags::COMPRESSED {
        return Err(Error::refused(
            "only LZNT1 is decoded; the stream names another compression format",
        ));
    }
    let exponent = stream.compression_unit;
    if exponent == 0 || exponent > MAX_COMPRESSION_UNIT {
        return Err(Error::invalid(format!(
            "compression unit is {exponent}, where {MAX_COMPRESSION_UNIT} is the largest \
             unit NTFS compresses in"
        )));
    }
    output.fill(0);
    let unit_clusters = 1u64 << exponent;
    let unit_size = unit_clusters * cluster_size;
    let end = offset + output.len() as u64;
    let readable = end.min(stream.initialized_size);
    let mut unit_start = offset / unit_size * unit_size;
    while unit_start < readable {
        let first_vcn = unit_start / cluster_size;
        let unit_bytes = unit_size.min(stream.data_size - unit_start) as usize;
        let mut lcns = Vec::new();
        for vcn in first_vcn..first_vcn + unit_clusters {
            if let Some(lcn) = find_run(&stream.runs, vcn)
                .and_then(|run| run.lcn.map(|lcn| lcn + vcn - run.starting_vcn))
            {
                lcns.push(lcn);
            }
        }
        if !lcns.is_empty() {
            let mut stored = vec![0; lcns.len() * cluster_size as usize];
            let mut at = 0usize;
            let mut index = 0usize;
            while index < lcns.len() {
                let mut length = 1usize;
                while index + length < lcns.len()
                    && lcns[index + length] == lcns[index] + length as u64
                {
                    length += 1;
                }
                let bytes = length * cluster_size as usize;
                let physical = lcns[index]
                    .checked_mul(cluster_size)
                    .ok_or_else(|| Error::invalid("stream offset overflow"))?;
                volume.read_volume_bytes(physical, &mut stored[at..at + bytes])?;
                at += bytes;
                index += length;
            }
            let plain = if lcns.len() as u64 == unit_clusters {
                stored
            } else {
                compression::decompress_unit(&stored, unit_bytes).map_err(|error| {
                    Error::invalid(format!(
                        "the compression unit at VCN {first_vcn} does not decode: {error}"
                    ))
                })?
            };
            let from = offset.max(unit_start);
            let to = readable.min(unit_start + plain.len() as u64);
            if from < to {
                let source = (from - unit_start) as usize..(to - unit_start) as usize;
                let target = (from - offset) as usize;
                output[target..target + source.len()].copy_from_slice(&plain[source]);
            }
        }
        unit_start += unit_size;
    }
    Ok(())
}
