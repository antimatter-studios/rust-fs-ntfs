//! Retiring clusters into `$BadClus` (#1006, part of #726).
//!
//! `$BadClus` (MFT record 8) keeps the volume's bad clusters in its named
//! `$Bad` data stream: a sparse stream as long as the volume, in which a
//! bad cluster is mapped at its own LCN (its VCN equals its LCN) and every
//! other cluster is a hole. A retired cluster is also marked in use in
//! `$Bitmap`, so nothing later allocates it.
//!
//! Retiring is a repair like any other, so it is planned with
//! [`crate::repair_plan::plan_retire_clusters_io`] and applied by
//! copy-repair to a new target: the failing source is never written.

use crate::attr_io::{self, iter_attributes, resident_value_start, AttrType};
use crate::attr_resize::{refuse_if_attribute_list, replace_attribute};
use crate::bitmap::{allocate_io, is_allocated_io, locate_bitmap_io};
use crate::block_io::BlockIo;
use crate::data_runs::{decode_runs, encode_runs, vcn_to_lcn, DataRun};
use crate::error::Error;
use crate::mft_inventory::{
    inventory_batch_io, mft_slot_count_io, InventoryCursor, MAX_RECORD_NUMBER,
};
use crate::mft_io::{read_mft_record_io, update_mft_record_io};

const BADCLUS_RECORD: u64 = 8;
const ROOT_RECORD: u64 = 5;
/// `$FILE_NAME`'s namespace for a DOS 8.3 alias of a long name.
const NAMESPACE_DOS: u8 = 2;
/// Deeper than any real tree: a parent chain this long is a loop.
const MAX_PATH_DEPTH: usize = 1024;
const BAD_STREAM: &str = "$Bad";

/// Non-resident attribute header fields.
const ATTR_LENGTH: usize = 0x04;
const ATTR_FLAGS: usize = 0x0C;
const MAPPING_PAIRS_OFFSET: usize = 0x20;
/// Present only in the extended header a sparse or compressed attribute
/// carries: the bytes of the clusters actually mapped.
const TOTAL_ALLOCATED: usize = 0x40;
const FLAGS_SPARSE_OR_COMPRESSED: u16 = 0x8000 | 0x00FF;

/// A cluster asked to be retired that `$Bitmap` marks in use, left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InUseCluster {
    pub lcn: u64,
    /// The file whose attribute maps it; `None` when no in-use record does.
    pub owner: Option<ClusterOwner>,
}

/// The file an attribute that maps a cluster belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterOwner {
    /// The base record's number.
    pub record: u64,
    /// From the root, through each `$FILE_NAME`'s parent.
    pub path: String,
}

/// What [`retire_clusters_io`] did with the clusters it was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retirement {
    /// Mapped in `$Bad` and marked in use, sorted.
    pub retired: Vec<u64>,
    /// In use, so left alone, sorted, each with its owner.
    pub in_use: Vec<InUseCluster>,
}

/// Retire each free cluster in `clusters` into `$BadClus:$Bad` and mark it
/// in use in `$Bitmap`, returning the clusters retired, sorted. A cluster
/// `$Bad` already maps is left as it is and not returned.
///
/// A cluster `$Bitmap` marks in use is left alone (#1007): another
/// attribute owns it, and mapping it in `$Bad` too would cross-link the
/// two. It is returned in [`Retirement::in_use`] with the file whose
/// attribute maps it, so the caller can say which file holds the
/// unreadable sector. A cluster past the span `$Bad` covers is refused
/// before anything is written.
///
/// `$Bitmap` is written before `$BadClus`: a stop between the two leaves a
/// cluster in use that nothing maps, which `chkdsk` reclaims, never one
/// `$Bad` maps that an allocation could still hand out.
pub fn retire_clusters_io<T: BlockIo + ?Sized>(
    io: &mut T,
    clusters: &[u64],
) -> Result<Retirement, Error> {
    let (params, record) = read_mft_record_io(io, BADCLUS_RECORD)?;
    let (_, runs) = bad_stream(&record)?;
    let covered = runs.last().map_or(0, |r| r.starting_vcn + r.length);
    let mut mapped = Vec::new();
    for run in &runs {
        if let Some(lcn) = run.lcn {
            if lcn != run.starting_vcn {
                return Err(Error::refused(format!(
                    "$BadClus:$Bad maps VCN {} at LCN {lcn}; it maps each bad cluster at its own LCN",
                    run.starting_vcn
                )));
            }
            mapped.push((lcn, run.length));
        }
    }

    let bm = locate_bitmap_io(io)?;
    let mut wanted = clusters.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    let mut retired = Vec::new();
    let mut in_use = Vec::new();
    for lcn in wanted {
        if lcn >= covered {
            return Err(Error::refused(format!(
                "cluster {lcn} is past the {covered} clusters $BadClus:$Bad covers"
            )));
        }
        if vcn_to_lcn(&runs, lcn).is_some() {
            continue;
        }
        if is_allocated_io(io, &bm, lcn)? {
            in_use.push(lcn);
            continue;
        }
        retired.push(lcn);
    }
    let in_use = owners_io(io, &in_use)?;
    if retired.is_empty() {
        return Ok(Retirement { retired, in_use });
    }

    for &lcn in &retired {
        allocate_io(io, &bm, lcn, 1)?;
    }
    mapped.extend(retired.iter().map(|&lcn| (lcn, 1)));
    let new_runs = bad_runs(mapped, covered);
    let mapped_bytes: u64 = new_runs
        .iter()
        .filter(|r| r.lcn.is_some())
        .map(|r| r.length)
        .sum::<u64>()
        * params.cluster_size;
    let mapping_pairs = encode_runs(&new_runs)?;
    update_mft_record_io(io, BADCLUS_RECORD, |record| {
        let (loc, _) = bad_stream(record)?;
        let attr = &record[loc.attr_offset..loc.attr_offset + loc.attr_length];
        let mapping =
            u16::from_le_bytes([attr[MAPPING_PAIRS_OFFSET], attr[MAPPING_PAIRS_OFFSET + 1]])
                as usize;
        let mut new_attr = attr[..mapping].to_vec();
        new_attr.extend_from_slice(&mapping_pairs);
        new_attr.resize(new_attr.len().next_multiple_of(8), 0);
        let length = new_attr.len() as u32;
        new_attr[ATTR_LENGTH..ATTR_LENGTH + 4].copy_from_slice(&length.to_le_bytes());
        let flags = u16::from_le_bytes([new_attr[ATTR_FLAGS], new_attr[ATTR_FLAGS + 1]]);
        if flags & FLAGS_SPARSE_OR_COMPRESSED != 0 && mapping >= TOTAL_ALLOCATED + 8 {
            new_attr[TOTAL_ALLOCATED..TOTAL_ALLOCATED + 8]
                .copy_from_slice(&mapped_bytes.to_le_bytes());
        }
        replace_attribute(record, loc.attr_offset, &new_attr)
    })?;
    Ok(Retirement { retired, in_use })
}

/// Each of `clusters` with the file whose non-resident attribute maps it,
/// found by reading every in-use MFT record once. An extension record's
/// attribute is its base record's.
fn owners_io<T: BlockIo + ?Sized>(
    io: &mut T,
    clusters: &[u64],
) -> Result<Vec<InUseCluster>, Error> {
    let mut found: Vec<InUseCluster> = clusters
        .iter()
        .map(|&lcn| InUseCluster { lcn, owner: None })
        .collect();
    if found.is_empty() {
        return Ok(found);
    }
    let (_, slots) = mft_slot_count_io(io)?;
    let inventory = inventory_batch_io(io, InventoryCursor::START, slots.max(1))?;
    for entry in inventory.entries {
        if found.iter().all(|c| c.owner.is_some()) {
            break;
        }
        let (_, record) = read_mft_record_io(io, entry.record_number)?;
        let base = entry.base_reference & MAX_RECORD_NUMBER;
        let owner = if base == 0 { entry.record_number } else { base };
        for loc in iter_attributes(&record) {
            let Some(mapping) = loc.non_resident_mapping_pairs_offset else {
                continue;
            };
            let attr = &record[loc.attr_offset..loc.attr_offset + loc.attr_length];
            let Ok(runs) = decode_runs(attr.get(mapping as usize..).unwrap_or_default()) else {
                continue;
            };
            for cluster in found.iter_mut().filter(|c| c.owner.is_none()) {
                let maps = runs.iter().any(|r| {
                    r.lcn
                        .is_some_and(|lcn| (lcn..lcn + r.length).contains(&cluster.lcn))
                });
                if maps {
                    cluster.owner = Some(ClusterOwner {
                        record: owner,
                        path: path_of_io(io, owner)?,
                    });
                }
            }
        }
    }
    Ok(found)
}

/// The path of record `record_number` from the root, through each
/// `$FILE_NAME`'s parent.
fn path_of_io<T: BlockIo + ?Sized>(io: &mut T, record_number: u64) -> Result<String, Error> {
    let mut names = Vec::new();
    let mut current = record_number;
    while current != ROOT_RECORD {
        if names.len() == MAX_PATH_DEPTH {
            return Err(Error::invalid(format!(
                "record {record_number}'s parent chain is deeper than {MAX_PATH_DEPTH}"
            )));
        }
        let (_, record) = read_mft_record_io(io, current)?;
        let (parent, name) = file_name(&record)
            .ok_or_else(|| Error::invalid(format!("record {current} has no $FILE_NAME")))?;
        names.push(name);
        current = parent;
    }
    names.reverse();
    Ok(format!("/{}", names.join("/")))
}

/// The parent record number and name in `record`'s `$FILE_NAME`,
/// preferring a long name to its DOS alias.
fn file_name(record: &[u8]) -> Option<(u64, String)> {
    let mut alias = None;
    for loc in iter_attributes(record).filter(|l| l.type_code == AttrType::FileName as u32) {
        let start = resident_value_start(&loc)?;
        let value = record.get(start..start + loc.resident_value_length? as usize)?;
        let parent = u64::from_le_bytes(value.get(0..8)?.try_into().ok()?) & MAX_RECORD_NUMBER;
        let length = usize::from(*value.get(0x40)?);
        let units: Vec<u16> = value
            .get(0x42..0x42 + 2 * length)?
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let name = String::from_utf16_lossy(&units);
        if *value.get(0x41)? != NAMESPACE_DOS {
            return Some((parent, name));
        }
        alias.get_or_insert((parent, name));
    }
    alias
}

/// `$BadClus:$Bad` in `record`, and its runs.
fn bad_stream(record: &[u8]) -> Result<(attr_io::AttrLocation, Vec<DataRun>), Error> {
    refuse_if_attribute_list(record, "retire clusters into $BadClus")?;
    let loc = attr_io::find_attribute(record, AttrType::Data, Some(BAD_STREAM))
        .ok_or_else(|| Error::io("$BadClus has no $Bad stream"))?;
    if loc.is_resident {
        return Err(Error::refused("$BadClus:$Bad is resident"));
    }
    let attr = &record[loc.attr_offset..loc.attr_offset + loc.attr_length];
    let mapping =
        u16::from_le_bytes([attr[MAPPING_PAIRS_OFFSET], attr[MAPPING_PAIRS_OFFSET + 1]]) as usize;
    let runs = decode_runs(attr.get(mapping..).unwrap_or_default())?;
    Ok((loc, runs))
}

/// The runs of a `$Bad` stream `covered` clusters long that maps each
/// `(lcn, length)` span at its own LCN, merged where they touch, and holds
/// holes everywhere else.
fn bad_runs(mut mapped: Vec<(u64, u64)>, covered: u64) -> Vec<DataRun> {
    mapped.sort_unstable();
    let mut runs: Vec<DataRun> = Vec::new();
    let mut vcn = 0;
    for (lcn, length) in mapped {
        let end = lcn + length;
        if let Some(last) = runs.last_mut().filter(|r| r.lcn.is_some() && lcn <= vcn) {
            last.length = end.max(vcn) - last.starting_vcn;
            vcn = end.max(vcn);
            continue;
        }
        if lcn > vcn {
            runs.push(DataRun {
                starting_vcn: vcn,
                length: lcn - vcn,
                lcn: None,
            });
        }
        runs.push(DataRun {
            starting_vcn: lcn,
            length,
            lcn: Some(lcn),
        });
        vcn = end;
    }
    if covered > vcn {
        runs.push(DataRun {
            starting_vcn: vcn,
            length: covered - vcn,
            lcn: None,
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr_io::{self, AttrType};
    use crate::bitmap::{is_allocated_io, locate_bitmap_io};
    use crate::data_runs::{decode_runs, DataRun};
    use crate::mft_io::read_mft_record_io;
    use crate::repair_plan::plan_retire_clusters_io;

    struct Dev(Vec<u8>);
    impl BlockIo for Dev {
        fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
            let off = offset as usize;
            buf.copy_from_slice(&self.0[off..off + buf.len()]);
            Ok(())
        }
        fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
            let off = offset as usize;
            self.0[off..off + buf.len()].copy_from_slice(buf);
            Ok(())
        }
        fn size(&self) -> u64 {
            self.0.len() as u64
        }
    }

    fn formatted() -> Dev {
        const SIZE: u64 = 64 * 1024 * 1024;
        let mut dev = Dev(vec![0u8; SIZE as usize]);
        crate::mkfs::format_filesystem(
            &mut dev as &mut dyn BlockIo,
            SIZE,
            4096,
            1024,
            Some("BADCLUS"),
            Some(0x1006),
        )
        .expect("format_filesystem");
        dev
    }

    /// `$BadClus:$Bad`'s runs as the volume holds them.
    fn bad_runs(dev: &mut Dev) -> Vec<DataRun> {
        let (_, record) = read_mft_record_io(dev, 8).expect("read $BadClus");
        let loc =
            attr_io::find_attribute(&record, AttrType::Data, Some("$Bad")).expect("$BadClus:$Bad");
        let attr = &record[loc.attr_offset..loc.attr_offset + loc.attr_length];
        let mapping = u16::from_le_bytes([attr[0x20], attr[0x21]]) as usize;
        decode_runs(&attr[mapping..]).expect("decode $Bad")
    }

    /// Plan retiring `clusters` on `dev`, check planning wrote nothing, and
    /// apply the plan to `dev`, returning the clusters retired.
    fn plan_and_apply(dev: &mut Dev, clusters: &[u64]) -> Vec<u64> {
        let before = dev.0.clone();
        let plan = plan_retire_clusters_io(dev, clusters).expect("plan");
        assert!(dev.0 == before, "planning wrote to the source");
        for write in plan.units.iter().flat_map(|unit| &unit.writes) {
            dev.write_all_at(write.offset, &write.after).unwrap();
        }
        plan.retired
    }

    /// Free clusters spread over the volume, sorted, none adjacent to
    /// another; the cluster between the first two is free too.
    fn scattered_free(dev: &mut Dev) -> Vec<u64> {
        let bm = locate_bitmap_io(dev).unwrap();
        let clusters = dev.size() / 4096;
        let mut free_from = |start: u64, run: u64| -> u64 {
            (start..clusters - 1 - run)
                .find(|&lcn| (lcn..lcn + run).all(|c| !is_allocated_io(dev, &bm, c).unwrap()))
                .expect("a free run")
        };
        let first = free_from(clusters / 3, 4);
        let second = free_from((clusters / 2).max(first + 5), 1);
        let third = free_from((clusters * 3 / 4).max(second + 2), 1);
        vec![first, first + 2, second, third]
    }

    #[test]
    fn free_clusters_are_mapped_at_their_own_lcn_and_marked_in_use() {
        let mut dev = formatted();
        let covered: u64 = bad_runs(&mut dev).iter().map(|r| r.length).sum();
        let picks = scattered_free(&mut dev);
        let mut asked = picks.clone();
        asked.reverse();
        asked.push(picks[0]);

        assert_eq!(plan_and_apply(&mut dev, &asked), picks);

        let runs = bad_runs(&mut dev);
        let mut vcn = 0;
        for run in &runs {
            assert_eq!(
                run.starting_vcn, vcn,
                "runs sorted and contiguous: {runs:?}"
            );
            vcn += run.length;
        }
        assert_eq!(vcn, covered, "$Bad still covers the volume");
        let mapped: Vec<u64> = runs
            .iter()
            .filter_map(|r| r.lcn.map(|lcn| (lcn, r)))
            .flat_map(|(lcn, r)| {
                assert_eq!(lcn, r.starting_vcn, "mapped at its own LCN: {r:?}");
                lcn..lcn + r.length
            })
            .collect();
        assert_eq!(mapped, picks);
        let bm = locate_bitmap_io(&mut dev).unwrap();
        for &lcn in &picks {
            assert!(
                is_allocated_io(&mut dev, &bm, lcn).unwrap(),
                "cluster {lcn} in use"
            );
        }
    }

    #[test]
    fn adjacent_retired_clusters_share_one_run() {
        let mut dev = formatted();
        let picks = scattered_free(&mut dev);
        plan_and_apply(&mut dev, &[picks[0]]);
        plan_and_apply(&mut dev, &[picks[0] + 1, picks[1]]);
        let mapped: Vec<(u64, u64)> = bad_runs(&mut dev)
            .iter()
            .filter_map(|r| r.lcn.map(|lcn| (lcn, r.length)))
            .collect();
        assert_eq!(mapped, vec![(picks[0], 3)]);
    }

    #[test]
    fn retiring_a_cluster_already_in_badclus_plans_nothing() {
        let mut dev = formatted();
        let picks = scattered_free(&mut dev);
        plan_and_apply(&mut dev, &picks);
        let plan = plan_retire_clusters_io(&mut dev, &picks).expect("plan");
        assert!(plan.units.is_empty(), "{:?}", plan.units.len());
        assert!(plan.retired.is_empty());
    }

    #[test]
    fn a_cluster_in_use_is_refused_and_nothing_is_planned() {
        let mut dev = formatted();
        let picks = scattered_free(&mut dev);
        let plan = plan_retire_clusters_io(&mut dev, &[picks[0], 0]).expect("plan");
        assert_eq!(
            plan.retired,
            vec![picks[0]],
            "only the free cluster retires"
        );
        assert_eq!(
            plan.in_use,
            vec![InUseCluster {
                lcn: 0,
                owner: Some(ClusterOwner {
                    record: 7,
                    path: "/$Boot".to_string(),
                }),
            }],
            "cluster 0 is left alone and named as $Boot's"
        );
        for write in plan.units.iter().flat_map(|unit| &unit.writes) {
            dev.write_all_at(write.offset, &write.after).unwrap();
        }
        assert!(
            bad_runs(&mut dev).iter().all(|r| r.lcn != Some(0)),
            "$Bad does not map cluster 0"
        );
    }

    #[test]
    fn a_cluster_of_a_files_data_is_not_retired_and_the_file_is_named() {
        let mut dev = formatted();
        crate::write::create_file_io(&mut dev, "/", "victim.bin").expect("create");
        crate::write::write_file_contents_io(&mut dev, "/victim.bin", &vec![0x5A; 64 * 1024])
            .expect("write");
        let record_number =
            crate::write::resolve_path_to_record_number_io(&mut dev, "/victim.bin").unwrap();
        let (_, record) = read_mft_record_io(&mut dev, record_number).unwrap();
        let loc = attr_io::find_attribute(&record, AttrType::Data, None).expect("$DATA");
        assert!(!loc.is_resident, "64 KiB of data is non-resident");
        let attr = &record[loc.attr_offset..loc.attr_offset + loc.attr_length];
        let mapping = u16::from_le_bytes([attr[0x20], attr[0x21]]) as usize;
        let run = decode_runs(&attr[mapping..]).unwrap()[0];
        let victim = run.lcn.expect("mapped data") + run.length / 2;
        let before = dev.0.clone();

        let plan = plan_retire_clusters_io(&mut dev, &[victim]).expect("plan");

        assert!(plan.units.is_empty(), "nothing written: {:?}", plan.units);
        assert!(plan.retired.is_empty());
        assert_eq!(
            plan.in_use,
            vec![InUseCluster {
                lcn: victim,
                owner: Some(ClusterOwner {
                    record: record_number,
                    path: "/victim.bin".to_string(),
                }),
            }]
        );
        assert!(dev.0 == before, "planning wrote to the source");
        assert!(bad_runs(&mut dev).iter().all(|r| r.lcn.is_none()));
        let bm = locate_bitmap_io(&mut dev).unwrap();
        assert!(is_allocated_io(&mut dev, &bm, victim).unwrap());
    }
}
