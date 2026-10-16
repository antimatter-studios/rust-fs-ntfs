//! Bounded candidate scan over the unused tail of one `$I30` index node.
//!
//! The namespace walk in `index_io` stays authoritative: this module never
//! feeds it and never changes what a directory listing returns. It looks only
//! at the bytes between a node's checked used length and its allocated length,
//! on 8-byte boundaries, for a complete index entry whose key is a complete
//! `$FILE_NAME`. What it reports is unverified slack metadata: it is not proof
//! a file was deleted and it is not recoverable data.
//!
//! The caller hands over a region that is already validated and fixed up
//! (update sequence array applied). Nothing here infers block geometry.
//! It stays crate-internal until a caller exists that validates that geometry
//! end to end.
#![cfg_attr(not(test), allow(dead_code))]

use crate::error::Error;

const IE_FILE_REFERENCE: usize = 0x00;
const IE_LENGTH: usize = 0x08;
const IE_KEY_LENGTH: usize = 0x0A;
const IE_FLAGS: usize = 0x0C;
const IE_KEY_START: usize = 0x10;
const IE_KNOWN_FLAGS: u16 = 0x0003;
const FN_NAME_LENGTH_OFFSET: usize = 0x40;
const FN_NAMESPACE_OFFSET: usize = 0x41;
const FN_NAME_OFFSET: usize = 0x42;
const FN_MAX_NAMESPACE: u8 = 3;
const ENTRY_ALIGNMENT: usize = 8;
const REFERENCE_RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// A file or parent reference read from slack. Absent when the bytes are zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SlackReference {
    pub record_number: u64,
    pub sequence: u16,
}

/// One complete entry-shaped structure found in a node's unused tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlackCandidate {
    /// Offset of the entry from the start of the region passed in.
    pub region_offset: usize,
    pub entry_length: usize,
    pub file_reference: Option<SlackReference>,
    pub parent_reference: Option<SlackReference>,
    pub namespace: u8,
    /// The raw UTF-16 name, undecoded.
    pub name: Vec<u16>,
}

/// Limits that end a scan deterministically.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SlackBudget {
    pub max_offsets: usize,
    pub max_candidates: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SlackScan {
    pub candidates: Vec<SlackCandidate>,
    /// True when a budget ended the scan before the allocated end.
    pub budget_exhausted: bool,
}

fn reference(raw: u64) -> Option<SlackReference> {
    (raw != 0).then_some(SlackReference {
        record_number: raw & REFERENCE_RECORD_MASK,
        sequence: (raw >> 48) as u16,
    })
}

fn read_u16(buf: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    buf.get(at..end).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn read_u64(buf: &[u8], at: usize) -> Option<u64> {
    let end = at.checked_add(8)?;
    buf.get(at..end)
        .map(|b| u64::from_le_bytes(b.try_into().expect("eight bytes")))
}

/// Parse one complete entry at `at` that ends at or before `limit`.
fn candidate_at(region: &[u8], at: usize, limit: usize) -> Option<SlackCandidate> {
    let tail = region.get(..limit)?;
    let length = usize::from(read_u16(tail, at.checked_add(IE_LENGTH)?)?);
    let key_length = usize::from(read_u16(tail, at.checked_add(IE_KEY_LENGTH)?)?);
    let flags = read_u16(tail, at.checked_add(IE_FLAGS)?)?;
    if flags & !IE_KNOWN_FLAGS != 0 || !length.is_multiple_of(ENTRY_ALIGNMENT) {
        return None;
    }
    let entry_end = at.checked_add(length)?;
    let key_start = at.checked_add(IE_KEY_START)?;
    let key_end = key_start.checked_add(key_length)?;
    if entry_end > limit || key_length < FN_NAME_OFFSET || key_end > entry_end {
        return None;
    }
    let name_length = usize::from(*tail.get(key_start + FN_NAME_LENGTH_OFFSET)?);
    let namespace = *tail.get(key_start + FN_NAMESPACE_OFFSET)?;
    let name_start = key_start + FN_NAME_OFFSET;
    let name_end = name_start.checked_add(name_length.checked_mul(2)?)?;
    if name_length == 0 || namespace > FN_MAX_NAMESPACE || name_end > key_end {
        return None;
    }
    let name = tail[name_start..name_end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(SlackCandidate {
        region_offset: at,
        entry_length: length,
        file_reference: reference(read_u64(tail, at + IE_FILE_REFERENCE)?),
        parent_reference: reference(read_u64(tail, key_start)?),
        namespace,
        name,
    })
}

/// Scan `region[used..allocated]` for complete entries on 8-byte boundaries.
///
/// `used` and `allocated` are the node's own checked lengths, measured from
/// the start of `region`. Impossible lengths are refused, not clamped.
pub(crate) fn scan_index_slack(
    region: &[u8],
    used: usize,
    allocated: usize,
    budget: SlackBudget,
) -> Result<SlackScan, Error> {
    if used > allocated {
        return Err(Error::io(format!(
            "index node uses {used} bytes of an allocation of {allocated}"
        )));
    }
    if allocated > region.len() {
        return Err(Error::io(format!(
            "index node allocation of {allocated} bytes exceeds its {}-byte region",
            region.len()
        )));
    }
    if !used.is_multiple_of(ENTRY_ALIGNMENT) {
        return Err(Error::io(format!(
            "index node used length {used} is not 8-byte aligned"
        )));
    }
    let mut scan = SlackScan::default();
    let mut at = used;
    let mut offsets = 0usize;
    while at < allocated {
        if offsets >= budget.max_offsets || scan.candidates.len() >= budget.max_candidates {
            scan.budget_exhausted = true;
            break;
        }
        offsets += 1;
        match candidate_at(region, at, allocated) {
            Some(candidate) => {
                at = at
                    .checked_add(candidate.entry_length)
                    .ok_or(Error::io("slack entry length overflows"))?;
                scan.candidates.push(candidate);
            }
            None => at += ENTRY_ALIGNMENT,
        }
    }
    Ok(scan)
}

/// Slack candidates from one allocated `$INDEX_ALLOCATION:$I30` block.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BlockSlack {
    pub vcn: u64,
    /// Offset of the scanned region (the block's index header) in the block.
    pub region_start: usize,
    pub scan: SlackScan,
}

/// Scans the unused tail of every allocated INDX block of a directory.
///
/// Blocks come through `idx_block::load_for_directory_io` and
/// `read_indx_block_io`, so the runlist, block size and update sequence array
/// are the reader's own checked values. Used and allocated lengths are read
/// from each block's index header and handed to [`scan_index_slack`]
/// unchanged, which refuses impossible ones rather than clamping. A directory
/// with no allocated block is an error, not an empty result: a caller that
/// claims allocation coverage must not pass on a resident-only index.
pub(crate) fn scan_directory_allocation_slack_io<T: crate::block_io::BlockIo + ?Sized>(
    io: &mut T,
    directory_record: u64,
    budget: SlackBudget,
) -> Result<Vec<BlockSlack>, Error> {
    let ia = crate::idx_block::load_for_directory_io(io, directory_record)?;
    let vcns = ia.allocated_block_vcns();
    if vcns.is_empty() {
        return Err(Error::io(format!(
            "directory record {directory_record} has no allocated INDX block"
        )));
    }
    let start = crate::idx_block::INDX_INDEX_HEADER_OFFSET;
    let mut out = Vec::with_capacity(vcns.len());
    for vcn in vcns {
        let block = crate::idx_block::read_indx_block_io(io, &ia, vcn)?;
        let region = block.get(start..).ok_or_else(|| {
            Error::io(format!(
                "INDX block at VCN {vcn} is shorter than its header"
            ))
        })?;
        let header_u32 = |at: usize| -> Result<usize, Error> {
            region
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
                .ok_or_else(|| Error::io(format!("INDX block at VCN {vcn} header truncated")))
        };
        let used = header_u32(0x04)?;
        let allocated = header_u32(0x08)?;
        let scan = scan_index_slack(region, used, allocated, budget)?;
        out.push(BlockSlack {
            vcn,
            region_start: start,
            scan,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ZeroDev(Vec<u8>);

    impl crate::block_io::BlockIo for ZeroDev {
        fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
            let start = offset as usize;
            let src = self
                .0
                .get(start..start + buf.len())
                .ok_or_else(|| "read past end".to_string())?;
            buf.copy_from_slice(src);
            Ok(())
        }
        fn write_all_at(&mut self, _offset: u64, _buf: &[u8]) -> Result<(), String> {
            Err("read-only".to_string())
        }
        fn size(&self) -> u64 {
            self.0.len() as u64
        }
    }

    #[test]
    fn bridge_refuses_a_volume_without_checked_geometry() {
        let mut dev = ZeroDev(vec![0u8; 1 << 20]);
        let budget = SlackBudget {
            max_offsets: 64,
            max_candidates: 4,
        };
        assert!(scan_directory_allocation_slack_io(&mut dev, 5, budget).is_err());
    }

    const WIDE: SlackBudget = SlackBudget {
        max_offsets: 4096,
        max_candidates: 64,
    };

    fn entry(file_ref: u64, parent_ref: u64, namespace: u8, name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let key_length = FN_NAME_OFFSET + units.len() * 2;
        let length = (IE_KEY_START + key_length).div_ceil(8) * 8;
        let mut e = vec![0u8; length];
        e[0..8].copy_from_slice(&file_ref.to_le_bytes());
        e[8..10].copy_from_slice(&(length as u16).to_le_bytes());
        e[10..12].copy_from_slice(&(key_length as u16).to_le_bytes());
        e[16..24].copy_from_slice(&parent_ref.to_le_bytes());
        e[16 + FN_NAME_LENGTH_OFFSET] = units.len() as u8;
        e[16 + FN_NAMESPACE_OFFSET] = namespace;
        for (i, u) in units.iter().enumerate() {
            let at = 16 + FN_NAME_OFFSET + i * 2;
            e[at..at + 2].copy_from_slice(&u.to_le_bytes());
        }
        e
    }

    fn region(live: &[u8], slack: &[u8], allocated: usize) -> Vec<u8> {
        let mut r = live.to_vec();
        r.extend_from_slice(slack);
        r.resize(allocated, 0);
        r
    }

    fn name(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn a_complete_slack_entry_is_reported_with_its_offset_and_references() {
        let live = entry((3 << 48) | 40, (5 << 48) | 5, 1, "live.txt");
        let gone = entry((7 << 48) | 41, (5 << 48) | 5, 1, "deleted.txt");
        let r = region(&live, &gone, 512);
        let scan = scan_index_slack(&r, live.len(), 512, WIDE).unwrap();
        assert!(!scan.budget_exhausted);
        assert_eq!(scan.candidates.len(), 1);
        let c = &scan.candidates[0];
        assert_eq!(c.region_offset, live.len());
        assert_eq!(c.name, name("deleted.txt"));
        assert_eq!(c.namespace, 1);
        assert_eq!(
            c.file_reference,
            Some(SlackReference {
                record_number: 41,
                sequence: 7
            })
        );
        assert_eq!(
            c.parent_reference,
            Some(SlackReference {
                record_number: 5,
                sequence: 5
            })
        );
    }

    #[test]
    fn live_entries_before_the_used_length_are_not_slack() {
        let live = entry(40, 5, 1, "live.txt");
        let r = region(&live, &[], 256);
        let scan = scan_index_slack(&r, live.len(), 256, WIDE).unwrap();
        assert!(scan.candidates.is_empty());
    }

    #[test]
    fn a_zero_file_reference_stays_unknown() {
        let gone = entry(0, 0, 2, "A");
        let r = region(&[], &gone, 128);
        let scan = scan_index_slack(&r, 0, 128, WIDE).unwrap();
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].file_reference, None);
        assert_eq!(scan.candidates[0].parent_reference, None);
    }

    #[test]
    fn impossible_lengths_are_refused_not_clamped() {
        let r = vec![0u8; 64];
        assert!(scan_index_slack(&r, 72, 64, WIDE).is_err());
        assert!(scan_index_slack(&r, 0, 72, WIDE).is_err());
        assert!(scan_index_slack(&r, 4, 64, WIDE).is_err());
    }

    #[test]
    fn an_entry_running_past_the_allocation_is_not_reported() {
        let gone = entry(41, 5, 1, "truncated-name.txt");
        let allocated = gone.len() - 8;
        let r = region(&[], &gone, gone.len());
        let scan = scan_index_slack(&r, 0, allocated, WIDE).unwrap();
        assert!(scan.candidates.is_empty());
        assert!(!scan.budget_exhausted);
    }

    #[test]
    fn header_like_bytes_with_impossible_fields_are_not_reported() {
        let mut bad_namespace = entry(41, 5, 1, "x.txt");
        bad_namespace[16 + FN_NAMESPACE_OFFSET] = 4;
        let mut bad_flags = entry(41, 5, 1, "x.txt");
        bad_flags[IE_FLAGS] = 0x80;
        let mut name_past_key = entry(41, 5, 1, "x.txt");
        name_past_key[16 + FN_NAME_LENGTH_OFFSET] = 200;
        let mut short_key = entry(41, 5, 1, "x.txt");
        short_key[10..12].copy_from_slice(&0x20u16.to_le_bytes());
        for bytes in [bad_namespace, bad_flags, name_past_key, short_key] {
            let r = region(&[], &bytes, 256);
            let scan = scan_index_slack(&r, 0, 256, WIDE).unwrap();
            assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
        }
        let mut indx_magic = vec![0u8; 256];
        indx_magic[0..4].copy_from_slice(b"INDX");
        assert!(scan_index_slack(&indx_magic, 0, 256, WIDE)
            .unwrap()
            .candidates
            .is_empty());
    }

    #[test]
    fn budgets_end_the_scan_deterministically() {
        let mut slack = Vec::new();
        for n in 0..4 {
            slack.extend(entry(41 + n, 5, 1, "same"));
        }
        let r = region(&[], &slack, 1024);
        let capped = SlackBudget {
            max_offsets: 4096,
            max_candidates: 2,
        };
        let scan = scan_index_slack(&r, 0, 1024, capped).unwrap();
        assert_eq!(scan.candidates.len(), 2);
        assert!(scan.budget_exhausted);
        let few = SlackBudget {
            max_offsets: 1,
            max_candidates: 64,
        };
        let scan = scan_index_slack(&vec![0u8; 1024], 0, 1024, few).unwrap();
        assert!(scan.candidates.is_empty());
        assert!(scan.budget_exhausted);
        let full = scan_index_slack(&r, 0, 1024, WIDE).unwrap();
        assert_eq!(full.candidates.len(), 4);
        assert!(!full.budget_exhausted);
    }
}

/// Compares bounded slack candidates with the Windows-authored capture made
/// by test-disks/capture-i30-slack.ps1. Names and file IDs come from Windows
/// (fsutil); nothing here is produced by this crate's writer.
#[cfg(all(test, feature = "windows-i30-oracle"))]
mod windows_capture {
    use super::*;

    fn env_path(var: &str) -> std::path::PathBuf {
        std::env::var_os(var)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| panic!("{var} is unset; run test-disks/capture-i30-slack.ps1 first"))
    }

    #[test]
    fn candidates_match_the_windows_manifest() {
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(env_path("NTFS_I30_SLACK_MANIFEST")).expect("manifest readable"),
        )
        .expect("manifest is JSON");
        let directory_id = manifest["directory_file_id"]
            .as_u64()
            .expect("directory_file_id");
        let directory_record = directory_id & 0xFFFF_FFFF_FFFF;
        let control = manifest["never_created_control_name"]
            .as_str()
            .expect("control name");
        let mut native: std::collections::HashMap<String, Vec<(u64, String)>> = Default::default();
        for op in manifest["operations"].as_array().expect("operations") {
            if op["operation"] == "create" {
                let id = op["file_id"].as_u64().expect("native file_id");
                native
                    .entry(op["name"].as_str().unwrap().to_string())
                    .or_default()
                    .push((id, op["role"].as_str().unwrap().to_string()));
            }
        }
        let mut io = crate::block_io::PathIo::open_ro(&env_path("NTFS_I30_SLACK_IMAGE"))
            .expect("image opens");
        let budget = SlackBudget {
            max_offsets: 1 << 16,
            max_candidates: 4096,
        };
        let blocks = scan_directory_allocation_slack_io(&mut io, directory_record, budget)
            .expect("Windows allocated INDX blocks for the capture directory");
        assert!(!blocks.is_empty(), "no allocated INDX block was scanned");
        // The positive control: entries the capture script observed in the
        // raw INDX bytes, bound to Windows' name and native ID. The script
        // refuses to produce a fixture without one, so none here is a fault.
        let attested: Vec<(String, u64, u64)> = manifest["raw_deleted_evidence"]
            .as_array()
            .expect("raw_deleted_evidence")
            .iter()
            .map(|e| {
                (
                    e["name"].as_str().expect("name").to_string(),
                    e["record_number"].as_u64().expect("record_number"),
                    e["vcn"].as_u64().expect("vcn"),
                )
            })
            .collect();
        assert!(
            !attested.is_empty(),
            "PRECONDITION i30-raw-deleted-candidate: the manifest attests no deleted entry"
        );
        for (name, record, _) in &attested {
            let ids = native
                .get(name)
                .expect("attested name was created by Windows");
            assert!(
                ids.iter()
                    .any(|(id, role)| role == "deleted" && id & 0xFFFF_FFFF_FFFF == *record),
                "attested {name:?} record {record} is not a deleted native ID"
            );
            // Wrong-ID control: a neighbouring record is not this name's.
            assert!(
                !ids.iter()
                    .any(|(id, _)| id & 0xFFFF_FFFF_FFFF == record ^ 1),
                "wrong-ID control {} is a native ID of {name:?}",
                record ^ 1
            );
        }
        let mut recovered = vec![false; attested.len()];
        let (mut matched, mut deleted_matched) = (0usize, 0usize);
        for block in &blocks {
            for c in &block.scan.candidates {
                let name = String::from_utf16(&c.name).expect("candidate name is UTF-16");
                assert_ne!(name, control, "a never-created name was reported");
                let ids = native.get(&name).unwrap_or_else(|| {
                    panic!(
                        "candidate {name:?} at VCN {} was never created by Windows",
                        block.vcn
                    )
                });
                if let Some(r) = c.file_reference {
                    let hit = ids
                        .iter()
                        .find(|(id, _)| id & 0xFFFF_FFFF_FFFF == r.record_number);
                    let (_, role) = hit.unwrap_or_else(|| {
                        panic!(
                            "candidate {name:?} names record {} that Windows never gave it",
                            r.record_number
                        )
                    });
                    matched += 1;
                    for (i, (n, rec, vcn)) in attested.iter().enumerate() {
                        if *n == name && *rec == r.record_number && *vcn == block.vcn {
                            recovered[i] = true;
                        }
                    }
                    if role == "deleted" {
                        deleted_matched += 1;
                    }
                }
            }
        }
        // Provenance and confidence: slack retention is Windows' choice, so
        // the counts are reported, not required; every reported candidate is
        // held to the names and IDs Windows recorded.
        eprintln!(
            "windows_capture: blocks={} matched={matched} deleted={deleted_matched}",
            blocks.len()
        );
        for ((name, record, vcn), hit) in attested.iter().zip(&recovered) {
            assert!(
                hit,
                "Windows left {name:?} (record {record}) in INDX VCN {vcn}, but the reader did not recover it"
            );
        }
    }
}
