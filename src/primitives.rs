use crate::attr_io::{self, AttrType};
use crate::data_runs::{decode_runs, DataRun};
use crate::error::Error;
use crate::mft_io::{self, BootParams, MFT_FLAG_DIRECTORY, MFT_FLAG_IN_USE};
use crate::volume::{RawVolume, Volume, VolumeAPI};
use fs_core::BlockRead;

mod extents;
pub use extents::{
    MftMap, MissingExtent, ResolvedAttribute, ResolvedRecord, SalvageStream, StreamExtent,
};

pub const MAX_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub enum AttributeValue {
    Resident(Vec<u8>),
    NonResident {
        runs: Vec<DataRun>,
        first_vcn: u64,
        data_size: u64,
        initialized_size: u64,
    },
}

#[derive(Clone, Debug)]
pub struct AttributeEvidence {
    pub type_code: u32,
    pub name: String,
    pub flags: u16,
    pub offset: usize,
    /// The attribute's instance number in its record (header offset 0x0E),
    /// which an `$ATTRIBUTE_LIST` entry names it by.
    pub attribute_id: u16,
    /// For a non-resident attribute, the last VCN its run list maps (header
    /// offset 0x18); `None` for a resident one.
    pub last_vcn: Option<u64>,
    /// For a non-resident attribute, the base-2 log of its compression unit in
    /// clusters (header offset 0x22); 0 when uncompressed or resident.
    pub compression_unit: u8,
    pub value: AttributeValue,
}

/// `$STANDARD_INFORMATION` as decoded from a resident value. Times are raw
/// NTFS FILETIMEs (100 ns since 1601-01-01 UTC); convert with
/// [`crate::read::nt_to_unix`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandardInformationEvidence {
    pub created_nt: u64,
    pub modified_nt: u64,
    pub mft_modified_nt: u64,
    pub accessed_nt: u64,
    pub file_attributes: u32,
}

/// `$VOLUME_INFORMATION` as decoded from the `$Volume` record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeInformationEvidence {
    pub major: u8,
    pub minor: u8,
    /// The raw flags word; bit 0 is the dirty flag.
    pub flags: u16,
}

impl VolumeInformationEvidence {
    pub const DIRTY: u16 = 0x0001;

    pub fn is_dirty(&self) -> bool {
        self.flags & Self::DIRTY != 0
    }
}

/// One resident `$FILE_NAME` attribute as decoded from a record. A record
/// can hold several: a long Win32 name, its DOS 8.3 shadow, and one per hard
/// link. The parent reference is what a directory index would be consulted
/// for; it is evidence of where the name was last linked, not proof that the
/// parent record still exists or still has that sequence number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileNameEvidence {
    /// The attribute's index in [`RecordEvidence::attributes`], or in
    /// [`ResolvedRecord::attributes`] for a name from a resolved record.
    pub attribute: usize,
    /// Low 48 bits of the parent reference.
    pub parent_record: u64,
    /// High 16 bits of the parent reference.
    pub parent_sequence: u16,
    /// 0 POSIX, 1 Win32, 2 DOS, 3 Win32 and DOS.
    pub namespace: u8,
    pub name: String,
    /// False when the UTF-16 name held an unpaired surrogate and `name` was
    /// decoded with replacement characters.
    pub name_valid: bool,
    /// The attribute-flag bits stored in the name (`0x1000_0000` for a directory).
    pub flags: u32,
    pub created_nt: u64,
    pub modified_nt: u64,
    pub mft_modified_nt: u64,
    pub accessed_nt: u64,
    pub allocated_size: u64,
    pub real_size: u64,
}

impl FileNameEvidence {
    /// Whether this is the 8.3 shadow of another name.
    pub fn is_dos_only(&self) -> bool {
        self.namespace == 2
    }
}

#[derive(Clone, Debug)]
pub struct RecordEvidence {
    pub offset: u64,
    pub sector_size: u16,
    pub raw: Vec<u8>,
    pub fixed: Option<Vec<u8>>,
    pub in_use: bool,
    pub is_directory: bool,
    pub sequence: u16,
    pub base_reference: u64,
    /// The record's own MFT number, from header offset 0x2C. Only records
    /// whose update sequence array starts at or after 0x30 (NTFS 3.1) carry
    /// one; older layouts, such as the one the Linux ntfs3 driver writes, and
    /// records whose header could not be read, report `None`.
    /// It is the record's own claim, not proof of where it was found.
    pub record_number: Option<u64>,
    pub attributes: Vec<AttributeEvidence>,
    pub issues: Vec<String>,
}

const RECORD_NUMBER_OFFSET: usize = 0x2c;
const FIRST_ATTRIBUTE_WITH_NUMBER: usize = 0x30;
const FILE_NAME_FIXED_BYTES: usize = 66;
const STANDARD_INFORMATION_BYTES: usize = 48;

fn le_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn le_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// One resident `$FILE_NAME`, decoded; `None` when it is not one, or is too
/// short to hold the name its length byte declares.
pub(crate) fn decode_file_name(
    index: usize,
    attribute: &AttributeEvidence,
) -> Option<FileNameEvidence> {
    if attribute.type_code != AttrType::FileName as u32 {
        return None;
    }
    let AttributeValue::Resident(value) = &attribute.value else {
        return None;
    };
    if value.len() < FILE_NAME_FIXED_BYTES {
        return None;
    }
    let characters = usize::from(value[64]);
    let name_bytes = value.get(FILE_NAME_FIXED_BYTES..FILE_NAME_FIXED_BYTES + characters * 2)?;
    let units: Vec<u16> = name_bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let mut name_valid = true;
    let name: String = char::decode_utf16(units)
        .map(|unit| {
            unit.unwrap_or_else(|_| {
                name_valid = false;
                char::REPLACEMENT_CHARACTER
            })
        })
        .collect();
    let parent = le_u64(value, 0);
    Some(FileNameEvidence {
        attribute: index,
        parent_record: parent & 0x0000_ffff_ffff_ffff,
        parent_sequence: (parent >> 48) as u16,
        namespace: value[65],
        name,
        name_valid,
        flags: le_u32(value, 56),
        created_nt: le_u64(value, 8),
        modified_nt: le_u64(value, 16),
        mft_modified_nt: le_u64(value, 24),
        accessed_nt: le_u64(value, 32),
        allocated_size: le_u64(value, 40),
        real_size: le_u64(value, 48),
    })
}

impl RecordEvidence {
    /// The unnamed resident `$STANDARD_INFORMATION`, when the record has one
    /// long enough to decode.
    pub fn standard_information(&self) -> Option<StandardInformationEvidence> {
        self.attributes.iter().find_map(|attribute| {
            if attribute.type_code != AttrType::StandardInformation as u32
                || !attribute.name.is_empty()
            {
                return None;
            }
            let AttributeValue::Resident(value) = &attribute.value else {
                return None;
            };
            (value.len() >= STANDARD_INFORMATION_BYTES).then(|| StandardInformationEvidence {
                created_nt: le_u64(value, 0),
                modified_nt: le_u64(value, 8),
                mft_modified_nt: le_u64(value, 16),
                accessed_nt: le_u64(value, 24),
                file_attributes: le_u32(value, 32),
            })
        })
    }

    /// Every resident `$FILE_NAME` long enough to decode, in attribute order.
    /// A name whose declared length runs past its value is skipped, not guessed at.
    pub fn file_names(&self) -> Vec<FileNameEvidence> {
        self.attributes
            .iter()
            .enumerate()
            .filter_map(|(index, attribute)| decode_file_name(index, attribute))
            .collect()
    }

    /// The resident `$VOLUME_INFORMATION` of a `$Volume` record, when long enough.
    pub fn volume_information(&self) -> Option<VolumeInformationEvidence> {
        self.attributes.iter().find_map(|attribute| {
            if attribute.type_code != AttrType::VolumeInformation as u32 {
                return None;
            }
            let AttributeValue::Resident(value) = &attribute.value else {
                return None;
            };
            (value.len() >= 12).then(|| VolumeInformationEvidence {
                major: value[8],
                minor: value[9],
                flags: u16::from_le_bytes([value[10], value[11]]),
            })
        })
    }

    /// The resident `$VOLUME_NAME` label of a `$Volume` record. An odd byte
    /// count, or an unpaired surrogate, is replaced with the replacement
    /// character rather than hidden.
    pub fn volume_name(&self) -> Option<String> {
        self.attributes.iter().find_map(|attribute| {
            if attribute.type_code != AttrType::VolumeName as u32 {
                return None;
            }
            let AttributeValue::Resident(value) = &attribute.value else {
                return None;
            };
            let units: Vec<u16> = value
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        })
    }

    /// Whether the record carries an `$ATTRIBUTE_LIST`, which means some of
    /// its attributes may live in extension records this value does not include.
    pub fn has_attribute_list(&self) -> bool {
        self.attributes
            .iter()
            .any(|attribute| attribute.type_code == AttrType::AttributeList as u32)
    }
}

pub trait PrimitiveAPI: VolumeAPI {
    fn read_file_record_at(
        &self,
        offset: u64,
        length: usize,
        sector_size: u16,
    ) -> Result<RecordEvidence, Error> {
        if !(48..=MAX_RECORD_BYTES).contains(&length) {
            return Err(Error::invalid(
                "file record length outside inspection limit",
            ));
        }
        let mut raw = vec![0; length];
        self.read_volume_bytes(offset, &mut raw)?;
        Ok(inspect_file_record(offset, raw, sector_size))
    }

    /// Reads one `$DATA` attribute of a single record, as a stream.
    ///
    /// The attribute has to hold its whole stream: a record with an
    /// `$ATTRIBUTE_LIST`, or an extent that does not start at VCN 0, is
    /// refused here; [`PrimitiveAPI::resolve_salvage_record`] follows the list
    /// and [`PrimitiveAPI::read_resolved_stream_at`] reads what it joins.
    /// LZNT1-compressed streams are decoded; encrypted ones are refused.
    fn read_salvage_stream_at(
        &self,
        record: &RecordEvidence,
        attribute: usize,
        cluster_size: u64,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        if record.fixed.is_none() || record.base_reference != 0 {
            return Err(Error::invalid(
                "record has no validated base-record interpretation",
            ));
        }
        let attribute = record
            .attributes
            .get(attribute)
            .ok_or_else(|| Error::invalid("attribute index outside record"))?;
        if attribute.type_code != AttrType::Data as u32 {
            return Err(Error::invalid("attribute is not a data stream"));
        }
        if record.has_attribute_list() {
            return Err(Error::refused(
                "stream requires unresolved attribute extents: the record has an \
                 $ATTRIBUTE_LIST, which resolve_salvage_record follows",
            ));
        }
        if let AttributeValue::NonResident { first_vcn, .. } = &attribute.value {
            if *first_vcn != 0 {
                return Err(Error::refused(
                    "stream requires unresolved attribute extents: this extent starts at a \
                     later VCN",
                ));
            }
        }
        let stream = SalvageStream::from_single_attribute(record.record_number, attribute);
        self.read_resolved_stream_at(&stream, cluster_size, offset, output)
    }

    /// Where the file records of a volume are, read from `$MFT`'s own record
    /// (record 0) at `mft_offset`. When record 0 has an `$ATTRIBUTE_LIST`, the
    /// extents it names are followed through the part of `$MFT` its first
    /// extent maps. A part of `$MFT` this cannot map is a limitation of the
    /// returned map, not an error.
    fn read_mft_map(
        &self,
        mft_offset: u64,
        record_size: usize,
        sector_size: u16,
        cluster_size: u64,
    ) -> Result<MftMap, Error> {
        extents::read_mft_map(self, mft_offset, record_size, sector_size, cluster_size)
    }

    /// Reads `$MFT` record `number` from `$MFTMirr`, where the boot sector
    /// puts it, with the same fixup checks and damage reports as a record
    /// read from `$MFT`. Only the first [`BootParams::mft_mirror_records`]
    /// records are mirrored; a later one is refused.
    fn read_mirror_record(&self, boot: &BootParams, number: u64) -> Result<RecordEvidence, Error> {
        extents::read_mirror_record(self, boot, number)
    }

    /// An [`MftMap`] built from `$MFTMirr`'s copy of record 0, for a volume
    /// whose own record 0 is damaged. The map says so in its limitations,
    /// and says when the copy disagrees with the boot sector about where
    /// `$MFT` is.
    fn read_mft_map_from_mirror(&self, boot: &BootParams) -> Result<MftMap, Error> {
        extents::read_mft_map_from_mirror(self, boot)
    }

    /// Reads file record `number` through `map`, joining the pieces of a
    /// record that spans clusters `$MFT` does not hold contiguously.
    fn read_mapped_record(&self, map: &MftMap, number: u64) -> Result<RecordEvidence, Error> {
        extents::read_mapped_record(self, map, number)
    }

    /// Follows a base record's `$ATTRIBUTE_LIST` to its extension records and
    /// returns every attribute the record has, wherever it is held. Each
    /// extension record is checked: its fixups, that it is in use, and that it
    /// names this base record and its sequence number. One that is missing or
    /// fails a check is reported in [`ResolvedRecord::limitations`] and the
    /// streams it held a part of are marked incomplete, never read short.
    fn resolve_salvage_record(
        &self,
        base: &RecordEvidence,
        map: &MftMap,
    ) -> Result<ResolvedRecord, Error> {
        extents::resolve_salvage_record(self, base, map)
    }

    /// Reads a stream from a resolved record. A stream with any limitation is
    /// refused, naming it. LZNT1-compressed units are decoded, and a unit that
    /// does not decode is an error, never zeros. Encrypted streams are refused.
    fn read_resolved_stream_at(
        &self,
        stream: &SalvageStream,
        cluster_size: u64,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        extents::read_stream_at(self, stream, cluster_size, offset, output)
    }
}

impl<Device: BlockRead> PrimitiveAPI for RawVolume<Device> {}
impl<Device: BlockRead> PrimitiveAPI for Volume<Device> {}

impl<Device: BlockRead> Volume<Device> {
    pub fn inspect_mft_record(&self, record_number: u64) -> Result<RecordEvidence, Error> {
        let offset = self
            .raw
            .with_io(|io| mft_io::mft_record_offset_io(io, self.boot(), record_number))?;
        self.read_file_record_at(
            offset,
            self.boot().file_record_size as usize,
            self.boot().bytes_per_sector,
        )
    }
}

pub fn inspect_file_record(offset: u64, raw: Vec<u8>, sector_size: u16) -> RecordEvidence {
    let mut evidence = RecordEvidence {
        offset,
        sector_size,
        raw,
        fixed: None,
        in_use: false,
        is_directory: false,
        sequence: 0,
        base_reference: 0,
        record_number: None,
        attributes: Vec::new(),
        issues: Vec::new(),
    };
    if evidence.raw.len() > MAX_RECORD_BYTES {
        evidence
            .issues
            .push("record exceeds inspection limit".into());
        return evidence;
    }
    if !sector_size.is_power_of_two()
        || !(512..=4096).contains(&sector_size)
        || evidence.raw.len() < 48
        || !evidence.raw.len().is_multiple_of(512)
    {
        evidence
            .issues
            .push("invalid record length or sector size".into());
        return evidence;
    }
    let mut fixed = evidence.raw.clone();
    if let Err(error) = mft_io::apply_fixup_on_read(&mut fixed, sector_size) {
        evidence.issues.push(error.to_string());
        return evidence;
    }
    let parsed = (|| -> Result<(), Error> {
        let first = attr_io::read_u16_le(&fixed, 20)
            .ok_or_else(|| Error::invalid("short record header"))? as usize;
        let used = attr_io::read_u32_le(&fixed, 24)
            .ok_or_else(|| Error::invalid("short record header"))? as usize;
        if first < 48
            || !first.is_multiple_of(8)
            || used > fixed.len()
            || first.checked_add(4).filter(|end| *end <= used).is_none()
        {
            return Err(Error::invalid("invalid record attribute bounds"));
        }
        evidence.sequence = mft_io::record_sequence(&fixed);
        let flags = mft_io::record_flags(&fixed);
        evidence.in_use = flags & MFT_FLAG_IN_USE != 0;
        evidence.is_directory = flags & MFT_FLAG_DIRECTORY != 0;
        evidence.base_reference = attr_io::read_u64_le(&fixed, 32)
            .ok_or_else(|| Error::invalid("short record header"))?;
        // The number is at 0x2c only when the update sequence array starts after it, at 0x30 or
        // later (NTFS 3.1). Older layouts, such as the Linux ntfs3 driver's, start the array at
        // 0x2a, so those bytes are the array's and name no record.
        let usa_offset = attr_io::read_u16_le(&fixed, 4).map(usize::from);
        if usa_offset.is_some_and(|offset| offset >= FIRST_ATTRIBUTE_WITH_NUMBER) {
            evidence.record_number =
                attr_io::read_u32_le(&fixed, RECORD_NUMBER_OFFSET).map(u64::from);
        }
        let mut next = first;
        for location in attr_io::iter_attributes(&fixed) {
            if location.attr_offset != next {
                return Err(Error::invalid("attribute chain is discontinuous"));
            }
            next = location.attr_offset + location.attr_length;
            let attribute = &fixed[location.attr_offset..next];
            let mut last_vcn = None;
            let mut compression_unit = 0;
            let value = if location.is_resident {
                let start = location.resident_value_offset.unwrap() as usize;
                let length = location.resident_value_length.unwrap() as usize;
                let data = attribute
                    .get(
                        start
                            ..start
                                .checked_add(length)
                                .ok_or_else(|| Error::invalid("resident value overflow"))?,
                    )
                    .filter(|_| start >= 24)
                    .ok_or_else(|| Error::invalid("resident value exceeds attribute"))?;
                AttributeValue::Resident(data.to_vec())
            } else {
                let start = location.non_resident_mapping_pairs_offset.unwrap() as usize;
                if start < 64 || start >= attribute.len() {
                    return Err(Error::invalid("mapping pairs exceed attribute"));
                }
                let first_vcn = attr_io::read_u64_le(attribute, 16).unwrap();
                last_vcn = attr_io::read_u64_le(attribute, 24);
                compression_unit = attribute[0x22];
                let mut runs = decode_runs(&attribute[start..])?;
                for run in &mut runs {
                    run.starting_vcn = run
                        .starting_vcn
                        .checked_add(first_vcn)
                        .ok_or_else(|| Error::invalid("VCN overflow"))?;
                }
                AttributeValue::NonResident {
                    runs,
                    first_vcn,
                    data_size: location.non_resident_value_length.unwrap(),
                    initialized_size: attr_io::read_u64_le(attribute, 56).unwrap(),
                }
            };
            evidence.attributes.push(AttributeEvidence {
                type_code: location.type_code,
                name: attr_io::decode_attr_name(&fixed, &location),
                flags: attr_io::attribute_flags(&fixed, &location).unwrap(),
                offset: location.attr_offset,
                attribute_id: location.attribute_id,
                last_vcn,
                compression_unit,
                value,
            });
        }
        if next.checked_add(4).filter(|end| *end <= used).is_none()
            || attr_io::read_u32_le(&fixed, next) != Some(u32::MAX)
        {
            evidence
                .issues
                .push("attribute chain ended without a valid terminator".into());
        }
        Ok(())
    })();
    match parsed {
        Ok(()) => evidence.fixed = Some(fixed),
        Err(error) => {
            evidence.issues.push(error.to_string());
            if !evidence.attributes.is_empty() {
                evidence.fixed = Some(fixed);
            }
        }
    }
    evidence
}
