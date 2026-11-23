//! Cluster allocation rebuilt from the file records (#1388), graded by the
//! `$Bitmap` Windows wrote.
//!
//! `test-disks/ntfs-salvage-streams.img` is a volume Windows formatted and
//! wrote (`test-disks/build-windows-salvage-stream-fixtures.ps1`): a
//! fragmented file, compressed files with sparse units, named streams and
//! hard links in extension records. Its `$Bitmap` is Windows' own account of
//! which clusters are in use. The allocation `NtfsPrimitiveAPI` rebuilds from the
//! records has to equal it; then `$Bitmap` and some `$MFT` records are
//! destroyed on copies, and the rebuild has to go on and name what it lost.

use fs_ntfs::mft_io::{parse_boot_sector, BootParams};
use fs_ntfs::primitives::{MftMap, NtfsPrimitiveAPI};
use fs_ntfs::volume::RawVolume;
use std::path::Path;

const IMAGE: &str = "test-disks/ntfs-salvage-streams.img";
const PROVIDER: &str = "test-disks/build-windows-salvage-stream-fixtures.ps1";
const BITMAP_RECORD: u64 = 6;
const BADCLUS_RECORD: u64 = 8;

mod common;

/// The image file read a span at a time, damage laid over it in memory.
type Image = common::PatchedImage;

struct Fixture {
    bytes: Image,
    boot: BootParams,
    clusters: u64,
}

fn fixture() -> Fixture {
    assert!(
        Path::new(IMAGE).exists(),
        "missing {IMAGE}; run {PROVIDER} on Windows"
    );
    let bytes = Image::open(IMAGE).unwrap();
    let boot = parse_boot_sector(&bytes.read(0, 512)).unwrap();
    let clusters = boot.total_sectors * u64::from(boot.bytes_per_sector) / boot.cluster_size;
    Fixture {
        bytes,
        boot,
        clusters,
    }
}

impl Fixture {
    fn volume(&self, bytes: Image) -> RawVolume<Image> {
        RawVolume::open(bytes)
    }

    fn map(&self, volume: &RawVolume<Image>) -> MftMap {
        volume
            .read_mft_map(
                self.boot.mft_lcn * self.boot.cluster_size,
                self.boot.file_record_size as usize,
                self.boot.bytes_per_sector,
                self.boot.cluster_size,
            )
            .unwrap()
    }

    fn record_offset(&self, map: &MftMap, number: u64) -> usize {
        map.record_offset(number).unwrap() as usize
    }

    /// Windows' `$Bitmap`, one bool per cluster of the volume.
    fn windows_bitmap(&self) -> Vec<bool> {
        let volume = self.volume(self.bytes.clone());
        let map = self.map(&volume);
        let record = volume.read_mapped_record(&map, BITMAP_RECORD).unwrap();
        let stream = volume
            .resolve_salvage_record(&record, &map)
            .unwrap()
            .stream("")
            .unwrap();
        let mut bits = vec![0; stream.len() as usize];
        volume
            .read_resolved_stream_at(&stream, self.boot.cluster_size, 0, &mut bits)
            .unwrap();
        (0..self.clusters)
            .map(|c| bits[(c / 8) as usize] & (1 << (c % 8)) != 0)
            .collect()
    }

    /// The clusters `$Bitmap`'s own data occupies.
    fn bitmap_clusters(&self) -> Vec<(u64, u64)> {
        let volume = self.volume(self.bytes.clone());
        let map = self.map(&volume);
        let record = volume.read_mapped_record(&map, BITMAP_RECORD).unwrap();
        let stream = volume
            .resolve_salvage_record(&record, &map)
            .unwrap()
            .stream("")
            .unwrap();
        stream
            .runs
            .iter()
            .filter_map(|run| Some((run.lcn?, run.length)))
            .collect()
    }
}

/// Clusters where the two maps disagree: (cluster, Windows says in use).
fn disagreements(
    windows: &[bool],
    derived: &fs_ntfs::primitives::DerivedAllocation,
) -> Vec<(u64, bool)> {
    windows
        .iter()
        .enumerate()
        .filter(|(c, used)| derived.is_claimed(*c as u64) != **used)
        .map(|(c, used)| (c as u64, *used))
        .collect()
}

#[test]
fn the_allocation_rebuilt_from_the_records_is_the_bitmap_windows_wrote() {
    let fixture = fixture();
    let volume = fixture.volume(fixture.bytes.clone());
    let map = fixture.map(&volume);
    let derived = volume.derive_allocation(&map, fixture.clusters).unwrap();
    assert!(derived.complete, "{:?}", derived.limitations);
    assert_eq!(derived.unreadable_count, 0);
    assert!(derived.records_in_use > 1000, "{}", derived.records_in_use);
    let windows = fixture.windows_bitmap();
    let used = windows.iter().filter(|u| **u).count() as u64;
    assert_eq!(
        disagreements(&windows, &derived),
        Vec::new(),
        "the rebuilt allocation ({} clusters) differs from Windows' $Bitmap ({used} clusters)",
        derived.claimed_clusters()
    );
    assert_eq!(derived.claimed_clusters(), used);
}

#[test]
fn bad_clusters_are_claimed_through_badclus() {
    // $BadClus:$Bad is sparse over the whole volume, with a real run for
    // each bad cluster; the rebuild claims those like any run. Windows made
    // no bad cluster here, so the record is changed on a copy: its
    // $Bad stream given one real run over a free cluster.
    let fixture = fixture();
    let volume = fixture.volume(fixture.bytes.clone());
    let map = fixture.map(&volume);
    let record = volume.read_mapped_record(&map, BADCLUS_RECORD).unwrap();
    let bad = record
        .attributes
        .iter()
        .find(|a| a.name == "$Bad")
        .expect("$BadClus has a $Bad stream");
    let fs_ntfs::primitives::AttributeValue::NonResident { runs, .. } = &bad.value else {
        panic!("$Bad is resident");
    };
    assert!(
        runs.iter().all(|run| run.lcn.is_none()),
        "Windows recorded a bad cluster"
    );
    let before = volume.derive_allocation(&map, fixture.clusters).unwrap();
    let free = (1..fixture.clusters)
        .find(|c| !before.is_claimed(*c))
        .unwrap();
    // Rewrite the record's $Bad mapping pairs: a hole up to `free`, one
    // real cluster there, and a hole to the end.
    let mut fixed = record.fixed.clone().unwrap();
    let at = bad.offset;
    let pairs_at = at + usize::from(u16::from_le_bytes([fixed[at + 0x20], fixed[at + 0x21]]));
    let length = u32::from_le_bytes(fixed[at + 4..at + 8].try_into().unwrap()) as usize;
    use fs_ntfs::data_runs::{encode_runs, DataRun};
    let total: u64 = runs.iter().map(|run| run.length).sum();
    let pairs = encode_runs(&[
        DataRun {
            starting_vcn: 0,
            length: free,
            lcn: None,
        },
        DataRun {
            starting_vcn: free,
            length: 1,
            lcn: Some(free),
        },
        DataRun {
            starting_vcn: free + 1,
            length: total - free - 1,
            lcn: None,
        },
    ])
    .unwrap();
    // $Bad is the record's last attribute: it grows in place, and the end
    // marker and the record's used size move after it.
    assert_eq!(
        u32::from_le_bytes(fixed[at + length..at + length + 4].try_into().unwrap()),
        u32::MAX,
        "$Bad is not the last attribute"
    );
    let grown = (pairs_at - at + pairs.len()).div_ceil(8) * 8;
    let end = at + grown.max(length);
    assert!(
        end + 8 <= fixed.len() - 8,
        "the grown attribute fits the record"
    );
    fixed[pairs_at..end].fill(0);
    fixed[pairs_at..pairs_at + pairs.len()].copy_from_slice(&pairs);
    fixed[at + 4..at + 8].copy_from_slice(&((end - at) as u32).to_le_bytes());
    fixed[end..end + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    fixed[end + 4..end + 8].fill(0);
    fixed[0x18..0x1c].copy_from_slice(&((end + 8) as u32).to_le_bytes());
    fs_ntfs::mft_io::apply_fixup_on_write(&mut fixed, fixture.boot.bytes_per_sector).unwrap();
    let mut bytes = fixture.bytes.clone();
    let offset = fixture.record_offset(&map, BADCLUS_RECORD);
    bytes.write(offset as u64, &fixed);

    let damaged = fixture.volume(bytes);
    let after = damaged
        .derive_allocation(&fixture.map(&damaged), fixture.clusters)
        .unwrap();
    assert!(after.complete, "{:?}", after.limitations);
    assert!(
        after.is_claimed(free),
        "the bad cluster {free} is not claimed"
    );
    assert_eq!(after.claimed_clusters(), before.claimed_clusters() + 1);
}

#[test]
fn a_destroyed_bitmap_does_not_stop_the_rebuild() {
    let fixture = fixture();
    let before = {
        let volume = fixture.volume(fixture.bytes.clone());
        volume
            .derive_allocation(&fixture.map(&volume), fixture.clusters)
            .unwrap()
    };

    // $Bitmap's data overwritten: the records still say the same.
    let mut bytes = fixture.bytes.clone();
    let cluster = fixture.boot.cluster_size as usize;
    for (lcn, length) in fixture.bitmap_clusters() {
        bytes.fill(lcn * cluster as u64, length as usize * cluster, 0xa5);
    }
    let volume = fixture.volume(bytes.clone());
    let derived = volume
        .derive_allocation(&fixture.map(&volume), fixture.clusters)
        .unwrap();
    assert!(derived.complete, "{:?}", derived.limitations);
    assert_eq!(derived.ranges, before.ranges);

    // $Bitmap's record destroyed too: still produced, and it says which
    // record it lost, and that it is not complete.
    let map = fixture.map(&volume);
    let at = fixture.record_offset(&map, BITMAP_RECORD);
    bytes.fill(at as u64, fixture.boot.file_record_size as usize, 0);
    let volume = fixture.volume(bytes);
    let derived = volume
        .derive_allocation(&fixture.map(&volume), fixture.clusters)
        .unwrap();
    assert!(!derived.complete);
    assert_eq!(derived.unreadable, vec![(BITMAP_RECORD, 1)]);
    assert!(
        derived
            .limitations
            .iter()
            .any(|l| l.contains("1 records could not be read (6)")),
        "{:?}",
        derived.limitations
    );
    for (lcn, length) in fixture.bitmap_clusters() {
        assert!((lcn..lcn + length).all(|c| !derived.is_claimed(c)));
    }
    assert_eq!(
        derived.claimed_clusters(),
        before.claimed_clusters() - fixture.bitmap_clusters().iter().map(|r| r.1).sum::<u64>()
    );
}

#[test]
fn destroyed_records_are_named_and_record_zero_is_walked_through_the_mirror() {
    let fixture = fixture();
    let volume = fixture.volume(fixture.bytes.clone());
    let map = fixture.map(&volume);
    let before = volume.derive_allocation(&map, fixture.clusters).unwrap();

    // Records 0, 40 to 42 and 90: $MFT's own record, and three file records.
    let mut bytes = fixture.bytes.clone();
    let size = fixture.boot.file_record_size as usize;
    for number in [0u64, 40, 41, 42, 90] {
        let at = fixture.record_offset(&map, number);
        let pattern: Vec<u8> = (0..size).map(|i| (i * 13 + 1) as u8).collect();
        bytes.write(at as u64, &pattern);
    }
    let damaged = fixture.volume(bytes);
    assert!(damaged
        .read_mft_map(
            fixture.boot.mft_lcn * fixture.boot.cluster_size,
            size,
            fixture.boot.bytes_per_sector,
            fixture.boot.cluster_size
        )
        .is_err());
    let mirror = damaged.read_mft_map_from_mirror(&fixture.boot).unwrap();
    let derived = damaged
        .derive_allocation(&mirror, fixture.clusters)
        .unwrap();
    assert!(!derived.complete);
    assert_eq!(derived.unreadable, vec![(0, 1), (40, 3), (90, 1)]);
    assert_eq!(derived.unreadable_count, 5);
    assert!(
        derived.limitations[0].contains("$MFTMirr"),
        "{:?}",
        derived.limitations
    );
    assert!(
        derived
            .limitations
            .iter()
            .any(|l| l.contains("5 records could not be read (0, 40-42, 90)")),
        "{:?}",
        derived.limitations
    );
    // $MFT's clusters stay claimed from the map; nothing claimed is new.
    assert!(derived
        .ranges
        .iter()
        .all(|&(first, count)| { (first..first + count).all(|c| before.is_claimed(c)) }));
    for run in mirror.runs() {
        let lcn = run.lcn.unwrap();
        assert!((lcn..lcn + run.length).all(|c| derived.is_claimed(c)));
    }

    // A map that does not know how many records there are is refused.
    let guess = MftMap::contiguous(
        fixture.boot.mft_lcn * fixture.boot.cluster_size,
        size,
        fixture.boot.bytes_per_sector,
        fixture.boot.cluster_size,
    )
    .unwrap();
    assert!(damaged.derive_allocation(&guess, fixture.clusters).is_err());
}
