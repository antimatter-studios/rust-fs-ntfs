//! Cluster allocation rebuilt from the file records (#1388), for a volume
//! whose `$Bitmap` cannot be read: every cluster some in-use record's
//! non-resident attribute maps is in use, as chkdsk rebuilds it.
//!
//! The walk reads every record the [`MftMap`] covers, base and extension
//! records alike, so the metafiles' own clusters (`$MFT`, `$MFTMirr`,
//! `$LogFile`, `$Bitmap`, `$Boot`) and the bad clusters `$BadClus:$Bad`
//! maps are counted like any file's. What it could not read is reported,
//! never guessed: a cluster no readable record claims is not proven free.

use super::extents::{read_stream_at, SalvageStream};
use super::{AttributeValue, MftMap, NtfsPrimitiveAPI};
use crate::attr_io::AttrType;
use crate::error::Error;

/// One record that was read but whose attributes could not all be decoded,
/// such as a run list that does not decode: the attributes before the
/// damage are counted, the rest are not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordDamage {
    pub record: u64,
    pub reason: String,
}

/// A run that reaches past the volume's last cluster; the part inside the
/// volume is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunOutsideVolume {
    pub record: u64,
    pub lcn: u64,
    pub length: u64,
}

/// The allocation the readable records describe.
#[derive(Clone, Debug)]
pub struct DerivedAllocation {
    /// The clusters the volume holds; nothing at or past it is counted.
    pub cluster_count: u64,
    /// In-use clusters as `(first cluster, cluster count)`, sorted and merged.
    pub ranges: Vec<(u64, u64)>,
    /// Records the walk tried.
    pub records_walked: u64,
    /// In-use records whose attributes were counted, base and extension.
    pub records_in_use: u64,
    /// Records `$MFT`'s own `$BITMAP` marks free, which were not read.
    pub records_free: u64,
    /// Records of nothing but zeros, taken as never written. Counted only
    /// when `$MFT`'s `$BITMAP` could not be read, and named in the
    /// limitations then: a zeroed record and an unwritten one look alike.
    pub records_blank: u64,
    /// Whether `$MFT`'s `$BITMAP` said which records are in use.
    pub mft_bitmap_read: bool,
    /// Records that could not be read or failed their fixups, as
    /// `(first record, record count)`, sorted and merged.
    pub unreadable: Vec<(u64, u64)>,
    pub unreadable_count: u64,
    pub damaged: Vec<RecordDamage>,
    pub outside: Vec<RunOutsideVolume>,
    /// Everything that keeps this from being the whole allocation.
    pub limitations: Vec<String>,
    /// No record was unreadable or damaged, no run fell outside the volume,
    /// and the map itself was read from `$MFT` with no limitation.
    pub complete: bool,
}

impl DerivedAllocation {
    /// Whether some readable record claims `cluster`.
    pub fn is_claimed(&self, cluster: u64) -> bool {
        let index = self.ranges.partition_point(|(first, _)| *first <= cluster);
        index > 0 && {
            let (first, count) = self.ranges[index - 1];
            cluster - first < count
        }
    }

    pub fn claimed_clusters(&self) -> u64 {
        self.ranges.iter().map(|(_, count)| count).sum()
    }
}

/// Sorts and merges `(start, end)` half-open ranges into `(start, count)`.
fn merged(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        match out.last_mut() {
            Some((first, count)) if start <= *first + *count => {
                *count = (*count).max(end - *first);
            }
            _ => out.push((start, end - start)),
        }
    }
    out
}

pub(super) fn derive_allocation<V: NtfsPrimitiveAPI + ?Sized>(
    volume: &V,
    map: &MftMap,
    cluster_count: u64,
) -> Result<DerivedAllocation, Error> {
    let records = map.records().ok_or_else(|| {
        Error::refused(
            "the map is the contiguous hypothesis, which does not say how many records there \
             are; build it from $MFT or $MFTMirr",
        )
    })?;
    let mut claimed: Vec<(u64, u64)> = Vec::new();
    let mut unreadable: Vec<(u64, u64)> = Vec::new();
    let mut result = DerivedAllocation {
        cluster_count,
        ranges: Vec::new(),
        records_walked: 0,
        records_in_use: 0,
        records_free: 0,
        records_blank: 0,
        mft_bitmap_read: false,
        unreadable: Vec::new(),
        unreadable_count: 0,
        damaged: Vec::new(),
        outside: Vec::new(),
        limitations: map.limitations.clone(),
        complete: false,
    };
    let mut claim = |record: u64, lcn: u64, length: u64, result: &mut DerivedAllocation| {
        let end = lcn.saturating_add(length);
        if end > cluster_count {
            result.outside.push(RunOutsideVolume {
                record,
                lcn,
                length,
            });
        }
        let end = end.min(cluster_count);
        if lcn < end {
            claimed.push((lcn, end));
        }
    };
    // $MFT's own run list, as the map holds it: the clusters of $MFT stay
    // claimed even when the record that maps them is the one lost.
    for run in map.runs() {
        if let Some(lcn) = run.lcn {
            claim(0, lcn, run.length, &mut result);
        }
    }
    let in_use = mft_bitmap(volume, map, records);
    result.mft_bitmap_read = in_use.is_some();
    if in_use.is_none() {
        result.limitations.push(
            "$MFT's $BITMAP could not be read, so records of nothing but zeros are taken as \
             never written"
                .into(),
        );
    }
    for number in 0..records {
        if let Some(bits) = &in_use {
            let used = bits
                .get((number / 8) as usize)
                .is_some_and(|byte| byte & (1 << (number % 8)) != 0);
            if !used {
                result.records_free += 1;
                continue;
            }
        }
        result.records_walked += 1;
        let record = match volume.read_mapped_record(map, number) {
            Ok(record) if record.fixed.is_some() => record,
            Ok(record) if in_use.is_none() && record.raw.iter().all(|byte| *byte == 0) => {
                result.records_blank += 1;
                continue;
            }
            _ => {
                unreadable.push((number, number + 1));
                continue;
            }
        };
        if !record.in_use {
            continue;
        }
        result.records_in_use += 1;
        if !record.issues.is_empty() {
            result.damaged.push(RecordDamage {
                record: number,
                reason: record.issues.join("; "),
            });
        }
        for attribute in &record.attributes {
            if let AttributeValue::NonResident { runs, .. } = &attribute.value {
                for run in runs {
                    if let Some(lcn) = run.lcn {
                        claim(number, lcn, run.length, &mut result);
                    }
                }
            }
        }
    }
    result.ranges = merged(claimed);
    result.unreadable = merged(unreadable);
    result.unreadable_count = result.unreadable.iter().map(|(_, count)| count).sum();
    if result.unreadable_count != 0 {
        let first = result.unreadable.iter().take(8).map(|(first, count)| {
            if *count == 1 {
                format!("{first}")
            } else {
                format!("{first}-{}", first + count - 1)
            }
        });
        result.limitations.push(format!(
            "{} records could not be read ({}{}); clusters only they map are not claimed",
            result.unreadable_count,
            first.collect::<Vec<_>>().join(", "),
            if result.unreadable.len() > 8 {
                ", ..."
            } else {
                ""
            }
        ));
    }
    for damage in &result.damaged {
        result.limitations.push(format!(
            "record {} was read only in part: {}",
            damage.record, damage.reason
        ));
    }
    for run in &result.outside {
        result.limitations.push(format!(
            "record {} maps clusters {} to {}, past the volume's {} clusters",
            run.record,
            run.lcn,
            run.lcn.saturating_add(run.length).saturating_sub(1),
            cluster_count
        ));
    }
    result.complete = result.limitations.is_empty();
    Ok(result)
}

/// `$MFT`'s `$BITMAP`, one bit per record, from record 0 as the map reads
/// it; `None` when it cannot be read whole.
fn mft_bitmap<V: NtfsPrimitiveAPI + ?Sized>(
    volume: &V,
    map: &MftMap,
    records: u64,
) -> Option<Vec<u8>> {
    let record = volume.read_mapped_record(map, 0).ok()?;
    let mut base = record;
    base.fixed.as_ref()?;
    base.record_number.get_or_insert(0);
    let resolved = volume.resolve_salvage_record(&base, map).ok()?;
    let attribute = resolved.attributes.iter().find(|entry| {
        entry.attribute.type_code == AttrType::Bitmap as u32 && entry.attribute.name.is_empty()
    })?;
    let stream = SalvageStream::from_single_attribute(Some(0), &attribute.attribute);
    let wanted = records.div_ceil(8);
    if !stream.is_complete() || stream.len() < wanted || stream.len() > wanted + 4096 {
        return None;
    }
    let mut bits = vec![0; stream.len() as usize];
    let count = read_stream_at(volume, &stream, map.cluster_size, 0, &mut bits).ok()?;
    (count == bits.len()).then_some(bits)
}
