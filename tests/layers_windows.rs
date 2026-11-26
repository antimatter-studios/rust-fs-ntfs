//! NTFS through rust-fs-core's three layers (#1385), graded by Windows.
//!
//! `test-disks/ntfs-salvage-streams.img` is a volume Windows formatted and
//! wrote (`test-disks/build-windows-salvage-stream-fixtures.ps1`): a
//! fragmented file whose run list continues in an extension record, LZNT1
//! files with and without an attribute list, named streams and hard links in
//! extension records, and 1200 small files. `ntfs-salvage-streams.tsv` is
//! what Windows read back: each stream's length and SHA-256.
//!
//! The volume is read through `fs_ntfs::layers::NtfsVolume` only: the core
//! conformance checks, every file through `FileAPI` against Windows'
//! SHA-256, and every file rebuilt from its salvage walk's plan against the
//! same hashes. A walk's plan is also serialized, read by position, refused
//! past the source and read over unreadable bytes (#1390), and the times it
//! carries are compared with the independent `ntfs` reader's. The image is read from its file a span at a time; damage is
//! laid over it in memory.

use fs_core::layers::conformance::{self, Expected, Limits};
use fs_core::layers::{
    Candidate, CandidateKind, CandidateState, Extent, FileAPI, HealthState, OpenVolume, Plan,
    PlanStep, PrimitiveAPI, Recognition, SalvageAdapter, SalvageError, StructureRole, VolumeAPI,
    WalkEnd,
};
use fs_core::{BlockRead, FileDevice};
use fs_ntfs::layers::{NtfsAdapter, NtfsCandidate, NtfsVolume};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

const IMAGE: &str = "test-disks/ntfs-salvage-streams.img";
const REPORT: &str = "test-disks/ntfs-salvage-streams.tsv";
const PROVIDER: &str = "test-disks/build-windows-salvage-stream-fixtures.ps1";

/// The Windows image, with byte ranges replaced in memory.
#[derive(Clone)]
struct Image {
    file: Arc<FileDevice>,
    patches: Vec<(u64, Vec<u8>)>,
}

impl Image {
    fn open() -> Self {
        for path in [IMAGE, REPORT] {
            assert!(
                Path::new(path).exists(),
                "missing {path}; run {PROVIDER} on Windows"
            );
        }
        Self {
            file: Arc::new(FileDevice::open(IMAGE).unwrap()),
            patches: Vec::new(),
        }
    }

    fn patched(&self, offset: u64, bytes: Vec<u8>) -> Self {
        let mut image = self.clone();
        image.patches.push((offset, bytes));
        image
    }
}

impl BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.file.read_at(offset, output)?;
        let end = offset + output.len() as u64;
        for (at, bytes) in &self.patches {
            let from = (*at).max(offset);
            let to = (at + bytes.len() as u64).min(end);
            if from < to {
                output[(from - offset) as usize..(to - offset) as usize]
                    .copy_from_slice(&bytes[(from - at) as usize..(to - at) as usize]);
            }
        }
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.file.size_bytes()
    }
}

/// Windows' unnamed streams: path -> (length, SHA-256).
fn windows_files() -> BTreeMap<String, (u64, String)> {
    std::fs::read_to_string(REPORT)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (fields[0] == "stream" && fields[3].is_empty()).then(|| {
                (
                    fields[1].to_owned(),
                    (fields[4].parse().unwrap(), fields[5].to_owned()),
                )
            })
        })
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn limits() -> Limits {
    Limits {
        max_nodes: 4096,
        max_walks: 4096,
        max_lookups: 256,
        ..Limits::default()
    }
}

#[test]
fn the_windows_volume_passes_the_core_conformance_checks() {
    let volume = NtfsVolume::open(Image::open()).unwrap();
    assert_eq!(volume.format(), "ntfs");
    assert_eq!(volume.health().unwrap().state, HealthState::Clean);
    let expected = [
        Expected::directory("/packed"),
        Expected::directory("/links"),
        Expected::directory("/filler"),
        Expected::absent("/no such file"),
    ];
    conformance::check_healthy(&volume, &expected, &limits()).assert_passed();
    conformance::check_files_against_candidates(&volume, &limits()).assert_passed();
}

#[test]
fn every_file_reads_through_the_file_layer_as_windows_read_it() {
    let volume = NtfsVolume::open(Image::open()).unwrap();
    let files = windows_files();
    assert!(files.len() >= 5);
    for (path, (length, digest)) in &files {
        let node = volume
            .lookup(path)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(volume.metadata(&node).unwrap().size, *length, "{path}");
        let mut bytes = Vec::new();
        volume.copy_to(&node, &mut bytes).unwrap();
        assert_eq!(&sha256(&bytes), digest, "{path} differs from Windows");
    }
}

#[test]
fn every_file_rebuilt_from_its_salvage_walk_is_what_windows_read() {
    let volume = NtfsVolume::open(Image::open()).unwrap();
    let salvage = volume.salvage().unwrap();
    let candidates = salvage.all_candidates().unwrap();
    for (path, (length, digest)) in windows_files() {
        let record = volume.lookup(&path).unwrap().record;
        let candidate = candidates
            .iter()
            .find(|c| c.detail.record == record && c.state == CandidateState::Live)
            .unwrap_or_else(|| panic!("{path} has no live candidate"));
        assert_eq!(candidate.kind, CandidateKind::File);
        assert_eq!(candidate.size, Some(length), "{path}");
        let walk = salvage.walk_extents(candidate).unwrap();
        assert_eq!(walk.end, WalkEnd::Complete, "{path}");
        let plan = walk.plan();
        let mut bytes = Vec::new();
        let report = salvage.materialize(&plan, &mut bytes).unwrap();
        assert!(report.ranges.iter().all(|range| range.readable), "{path}");
        assert_eq!(bytes.len() as u64, length, "{path}");
        assert_eq!(
            sha256(&bytes),
            digest,
            "{path} rebuilt from its walk differs from Windows"
        );
    }
}

#[test]
fn a_volume_whose_boot_sector_is_gone_opens_for_salvage_from_the_backup() {
    let image = Image::open().patched(0, vec![0; 512]);
    assert_eq!(
        NtfsVolume::<Image>::probe(&image).unwrap().recognition,
        Recognition::Damaged
    );
    assert!(NtfsVolume::open(image.clone()).is_err());
    let volume = NtfsVolume::open_for_salvage(image).unwrap();
    assert!(volume.opened_from_backup());
    assert!(!volume.support().unwrap().files);
    assert_eq!(volume.health().unwrap().state, HealthState::Damaged);
    let structures = volume.structures().unwrap();
    assert!(structures
        .iter()
        .any(|s| s.role == StructureRole::RecordTable && s.name == "$MFT"));
    conformance::check_damaged(&volume, &limits()).assert_passed();
    assert!(volume.root().is_err(), "a salvage open offered a namespace");
}

#[test]
fn a_torn_record_is_a_failed_candidate_and_a_damage_finding() {
    let clean = NtfsVolume::open(Image::open()).unwrap();
    let candidates = clean.salvage().unwrap().all_candidates().unwrap();
    let target = candidates
        .iter()
        .find(|c| c.name.as_deref() == Some("small.txt"))
        .unwrap();
    let mut torn = vec![0; 2];
    clean
        .read_volume_bytes(target.location.offset + 510, &mut torn)
        .unwrap();
    let image = Image::open().patched(target.location.offset + 510, vec![!torn[0], !torn[1]]);
    let volume = NtfsVolume::open_for_salvage(image).unwrap();
    let salvage = volume.salvage().unwrap();
    let found = salvage.all_candidates().unwrap();
    let failed = found
        .iter()
        .find(|c| c.location == target.location)
        .expect("the torn record is still a candidate");
    assert_eq!(failed.integrity, fs_core::layers::Integrity::Failed);
    let mut damage = Vec::new();
    salvage.report_damage(&mut damage).unwrap();
    assert!(damage.iter().any(|d| d.location == target.location));
    conformance::check_damaged(&volume, &limits()).assert_passed();
}

/// The Windows image, refusing every read that touches `bad`.
struct Unreadable {
    image: Image,
    bad: Extent,
}

impl BlockRead for Unreadable {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        if offset < self.bad.offset + self.bad.length
            && self.bad.offset < offset + output.len() as u64
        {
            return Err(fs_core::Error::Io(std::io::Error::other("bad sector")));
        }
        self.image.read_at(offset, output)
    }
    fn size_bytes(&self) -> u64 {
        self.image.size_bytes()
    }
}

/// The live candidate of the file at `path`.
fn live_candidate(
    volume: &NtfsVolume<Image>,
    candidates: &[Candidate<NtfsCandidate>],
    path: &str,
) -> Candidate<NtfsCandidate> {
    let record = volume.lookup(path).unwrap().record;
    candidates
        .iter()
        .find(|c| c.detail.record == record && c.state == CandidateState::Live)
        .unwrap_or_else(|| panic!("{path} has no live candidate"))
        .clone()
}

#[test]
fn a_plan_carries_its_name_and_the_times_the_independent_reader_reads() {
    let image = Image::open();
    let volume = NtfsVolume::open(image.clone()).unwrap();
    let adapter = NtfsAdapter::from_source(&image);
    let candidates = volume.salvage().unwrap().all_candidates().unwrap();
    let mut cursor = std::io::BufReader::new(std::fs::File::open(IMAGE).unwrap());
    let independent = ntfs::Ntfs::new(&mut cursor).unwrap();
    for path in windows_files().keys() {
        let candidate = live_candidate(&volume, &candidates, path);
        let metadata = adapter.plan_metadata(&image, &candidate).unwrap();
        assert!(metadata.name.is_some(), "{path}");
        assert_eq!(metadata.name, candidate.name, "{path}");
        if path == "/packed/small.txt" {
            assert_eq!(metadata.name.as_deref(), Some("small.txt"));
        }
        let info = independent
            .file(&mut cursor, candidate.detail.record)
            .unwrap()
            .info()
            .unwrap();
        for (ours, theirs) in [
            (&metadata.created, info.creation_time()),
            (&metadata.modified, info.modification_time()),
            (&metadata.accessed, info.access_time()),
        ] {
            assert_eq!(ours, &Some(theirs.nt_timestamp().to_string()), "{path}");
        }

        // A candidate of another use of the record is given no times.
        let mut stale = candidate.clone();
        stale.detail.sequence = stale.detail.sequence.wrapping_add(1);
        let metadata = adapter.plan_metadata(&image, &stale).unwrap();
        assert_eq!(metadata.name, candidate.name, "{path}");
        assert_eq!(
            (metadata.created, metadata.modified, metadata.accessed),
            (None, None, None),
            "{path}"
        );
    }
}

#[test]
fn a_walk_plan_survives_serialization_and_reads_by_position() {
    let image = Image::open();
    let volume = NtfsVolume::open(image.clone()).unwrap();
    let adapter = NtfsAdapter::from_source(&image);
    let salvage = volume.salvage().unwrap();
    let candidates = salvage.all_candidates().unwrap();
    for (path, (length, digest)) in windows_files() {
        let candidate = live_candidate(&volume, &candidates, &path);
        let mut plan = salvage.walk_extents(&candidate).unwrap().plan();
        plan.metadata = adapter.plan_metadata(&image, &candidate).unwrap();

        let text = serde_json::to_string(&plan).unwrap();
        let restored: Plan = serde_json::from_str(&text).unwrap();
        assert_eq!(restored, plan, "{path}");
        assert_eq!(restored.digest(), plan.digest(), "{path}");

        // Checked from the source's size alone.
        let check = adapter.check_plan(&restored, image.size_bytes()).unwrap();
        assert_eq!(check.length, length, "{path}");
        assert!(check.overlaps.is_empty(), "{path}: {:?}", check.overlaps);

        let reader = salvage.plan_reader(&restored).unwrap();
        let mut whole = vec![0; length as usize];
        // Back to front, so no read depends on the one before it.
        for (index, chunk) in whole.chunks_mut(4093).enumerate().rev() {
            let read = reader.read_at(index as u64 * 4093, chunk).unwrap();
            assert_eq!(read.length, chunk.len() as u64, "{path}");
            assert_eq!(read.unreadable().count(), 0, "{path}");
        }
        assert_eq!(sha256(&whole), digest, "{path} differs from Windows");
    }
}

#[test]
fn a_plan_past_the_source_is_refused_before_anything_is_written() {
    let image = Image::open();
    let volume = NtfsVolume::open(image.clone()).unwrap();
    let salvage = volume.salvage().unwrap();
    let plan = Plan::new(vec![
        PlanStep::Bytes(b"literal".to_vec()),
        PlanStep::Extent(Extent::new(image.size_bytes() - 512, 1024)),
    ]);
    assert!(matches!(
        salvage.check_plan(&plan),
        Err(SalvageError::Bounds)
    ));
    let mut written = Vec::new();
    assert!(matches!(
        salvage.materialize(&plan, &mut written),
        Err(SalvageError::Bounds)
    ));
    assert!(written.is_empty());
}

#[test]
fn an_unreadable_source_range_stays_unreadable_in_a_positioned_read() {
    let image = Image::open();
    let volume = NtfsVolume::open(image.clone()).unwrap();
    let salvage = volume.salvage().unwrap();
    let candidates = salvage.all_candidates().unwrap();
    let candidate = live_candidate(&volume, &candidates, "/fragmented.bin");
    let plan = salvage.walk_extents(&candidate).unwrap().plan();
    let Some(PlanStep::Extent(first)) = plan.steps.first() else {
        panic!("/fragmented.bin starts with no source extent: {plan:?}");
    };
    assert!(first.length >= 1536, "{first:?}");
    let mut clean = vec![0; 1536];
    salvage
        .plan_reader(&plan)
        .unwrap()
        .read_at(0, &mut clean)
        .unwrap();

    let damaged = Unreadable {
        image: image.clone(),
        bad: Extent::new(first.offset + 512, 512),
    };
    let adapter = NtfsAdapter::from_source(&image);
    let mut output = vec![0xa5; 1536];
    let read = adapter
        .plan_reader(&damaged, &plan)
        .unwrap()
        .read_at(0, &mut output)
        .unwrap();
    assert_eq!(
        read.unreadable().collect::<Vec<_>>(),
        vec![Extent::new(512, 512)]
    );
    assert_eq!(output[..512], clean[..512]);
    assert_eq!(output[512..1024], [0; 512]);
    assert_eq!(output[1024..], clean[1024..]);
}
