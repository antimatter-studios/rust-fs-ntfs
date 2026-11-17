use crate::attr_io::{self, AttrType};
use crate::data_runs::{decode_runs, DataRun};
use crate::error::Error;
use crate::mft_io::{self, MFT_FLAG_DIRECTORY, MFT_FLAG_IN_USE};
use crate::volume::{RawVolume, Volume, VolumeAPI};
use fs_core::BlockRead;

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
    pub value: AttributeValue,
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
    pub attributes: Vec<AttributeEvidence>,
    pub issues: Vec<String>,
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
        if attribute.flags
            & (attr_io::attr_flags::COMPRESSION_MASK | attr_io::attr_flags::ENCRYPTED)
            != 0
        {
            return Err(Error::refused(
                "compressed or encrypted salvage streams require a format-specific decoder",
            ));
        }
        match &attribute.value {
            AttributeValue::Resident(data) => {
                let count = (data.len() as u64)
                    .saturating_sub(offset)
                    .min(output.len() as u64) as usize;
                if count != 0 {
                    output[..count]
                        .copy_from_slice(&data[offset as usize..offset as usize + count]);
                }
                Ok(count)
            }
            AttributeValue::NonResident {
                runs,
                first_vcn,
                data_size,
                initialized_size,
            } => {
                if !cluster_size.is_power_of_two()
                    || !(512..=2 * 1024 * 1024).contains(&cluster_size)
                {
                    return Err(Error::invalid("invalid cluster size"));
                }
                if *first_vcn != 0
                    || *initialized_size > *data_size
                    || record
                        .attributes
                        .iter()
                        .any(|entry| entry.type_code == AttrType::AttributeList as u32)
                {
                    return Err(Error::refused(
                        "stream requires unresolved attribute extents",
                    ));
                }
                let mut next_vcn = 0u64;
                for run in runs {
                    if run.starting_vcn != next_vcn || run.length == 0 {
                        return Err(Error::invalid("stream run list is discontinuous"));
                    }
                    next_vcn = next_vcn
                        .checked_add(run.length)
                        .ok_or_else(|| Error::invalid("run list overflow"))?;
                    if let Some(lcn) = run.lcn {
                        lcn.checked_add(run.length)
                            .and_then(|end| end.checked_mul(cluster_size))
                            .filter(|end| *end <= self.size_bytes())
                            .ok_or_else(|| Error::invalid("data run exceeds volume"))?;
                    }
                }
                if next_vcn
                    .checked_mul(cluster_size)
                    .filter(|bytes| *bytes >= *data_size)
                    .is_none()
                {
                    return Err(Error::invalid("run list ends before stream size"));
                }
                let count = data_size.saturating_sub(offset).min(output.len() as u64) as usize;
                let mut copied = 0;
                while copied < count {
                    let position = offset + copied as u64;
                    if position >= *initialized_size {
                        output[copied..count].fill(0);
                        break;
                    }
                    let vcn = position / cluster_size;
                    let run = runs
                        .iter()
                        .find(|run| vcn >= run.starting_vcn && vcn - run.starting_vcn < run.length)
                        .ok_or_else(|| Error::invalid("stream has an unmapped range"))?;
                    let within = position % cluster_size;
                    let chunk = (cluster_size - within)
                        .min((count - copied) as u64)
                        .min(initialized_size - position) as usize;
                    let target = &mut output[copied..copied + chunk];
                    match run.lcn {
                        Some(lcn) => {
                            let physical = (lcn + vcn - run.starting_vcn)
                                .checked_mul(cluster_size)
                                .and_then(|start| start.checked_add(within))
                                .ok_or_else(|| Error::invalid("stream offset overflow"))?;
                            self.read_volume_bytes(physical, target)?;
                        }
                        None => target.fill(0),
                    }
                    copied += chunk;
                }
                Ok(count)
            }
        }
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
        || !evidence.raw.len().is_multiple_of(usize::from(sector_size))
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
        let mut next = first;
        for location in attr_io::iter_attributes(&fixed) {
            if location.attr_offset != next {
                return Err(Error::invalid("attribute chain is discontinuous"));
            }
            next = location.attr_offset + location.attr_length;
            let attribute = &fixed[location.attr_offset..next];
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
