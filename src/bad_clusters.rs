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

use crate::attr_io::{self, AttrType};
use crate::attr_resize::{refuse_if_attribute_list, replace_attribute};
use crate::bitmap::{allocate_io, is_allocated_io, locate_bitmap_io};
use crate::block_io::BlockIo;
use crate::data_runs::{decode_runs, encode_runs, vcn_to_lcn, DataRun};
use crate::error::Error;
use crate::mft_io::{read_mft_record_io, update_mft_record_io};

const BADCLUS_RECORD: u64 = 8;
const BAD_STREAM: &str = "$Bad";

/// Non-resident attribute header fields.
const ATTR_LENGTH: usize = 0x04;
const ATTR_FLAGS: usize = 0x0C;
const MAPPING_PAIRS_OFFSET: usize = 0x20;
/// Present only in the extended header a sparse or compressed attribute
/// carries: the bytes of the clusters actually mapped.
const TOTAL_ALLOCATED: usize = 0x40;
const FLAGS_SPARSE_OR_COMPRESSED: u16 = 0x8000 | 0x00FF;

/// Retire each free cluster in `clusters` into `$BadClus:$Bad` and mark it
/// in use in `$Bitmap`, returning the clusters retired, sorted. A cluster
/// `$Bad` already maps is left as it is and not returned.
///
/// A cluster `$Bitmap` marks in use is refused before anything is written:
/// another attribute owns it, and mapping it in `$Bad` too would
/// cross-link the two. So is a cluster past the span `$Bad` covers.
///
/// `$Bitmap` is written before `$BadClus`: a stop between the two leaves a
/// cluster in use that nothing maps, which `chkdsk` reclaims, never one
/// `$Bad` maps that an allocation could still hand out.
pub fn retire_clusters_io<T: BlockIo + ?Sized>(
    io: &mut T,
    clusters: &[u64],
) -> Result<Vec<u64>, Error> {
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
            return Err(Error::refused(format!(
                "cluster {lcn} is in use; retiring it would cross-link the attribute that owns it"
            )));
        }
        retired.push(lcn);
    }
    if retired.is_empty() {
        return Ok(retired);
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
    Ok(retired)
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
        let err = plan_retire_clusters_io(&mut dev, &[picks[0], 0]).unwrap_err();
        assert!(err.to_string().contains("cluster 0 is in use"), "{err}");
    }
}
