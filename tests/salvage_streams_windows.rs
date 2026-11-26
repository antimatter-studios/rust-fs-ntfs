//! Salvage stream reads that follow a record out of itself (#1379), graded
//! by Windows.
//!
//! `test-disks/ntfs-salvage-streams.img` is a volume Windows wrote:
//! named streams and hard-link names its `$ATTRIBUTE_LIST` keeps in
//! extension records, a run list that continues in an extension record at a
//! later VCN, and LZNT1-compressed files with and without a list.
//! `test-disks/ntfs-salvage-streams.tsv` is what Windows reports of them:
//! each stream's length and SHA-256 read back through Windows' own
//! filesystem, each hard-link name, and each file's extent count.
//! `test-disks/build-windows-salvage-stream-fixtures.ps1` builds both.
//!
//! Every read here goes through `NtfsPrimitiveAPI` on a `RawVolume`, the way a
//! salvage tool reads a volume whose namespace it cannot trust: from the
//! boot sector's `$MFT` location to a record by number, never by path.

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::primitives::{MftMap, NtfsPrimitiveAPI, ResolvedRecord, SalvageStream};
use fs_ntfs::volume::RawVolume;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const IMAGE: &str = "test-disks/ntfs-salvage-streams.img";
const REPORT: &str = "test-disks/ntfs-salvage-streams.tsv";
const PROVIDER: &str = "test-disks/build-windows-salvage-stream-fixtures.ps1";
/// What a salvage tool reads at a time.
const CHUNK: usize = 64 * 1024;

mod common;

use common::PatchedImage as Image;

/// A stream Windows reported: its name, length and SHA-256.
type WindowsStream = (String, u64, String);

struct Fixture {
    image: Image,
    cluster_size: u64,
    mft_offset: u64,
    record_size: usize,
    sector_size: u16,
    /// path -> (record, [(stream name, length, sha256)])
    streams: BTreeMap<String, (u64, Vec<WindowsStream>)>,
    links: BTreeMap<String, Vec<String>>,
    extents: BTreeMap<String, usize>,
}

fn fixture() -> Fixture {
    for path in [IMAGE, REPORT] {
        assert!(
            Path::new(path).exists(),
            "missing {path}; run {PROVIDER} on Windows"
        );
    }
    let image = Image::open(IMAGE).unwrap();
    let boot = fs_ntfs::mft_io::parse_boot_sector(&image.read(0, 512)).unwrap();
    let mut fixture = Fixture {
        cluster_size: boot.cluster_size,
        mft_offset: boot.mft_lcn * boot.cluster_size,
        record_size: boot.file_record_size as usize,
        sector_size: boot.bytes_per_sector,
        image,
        streams: BTreeMap::new(),
        links: BTreeMap::new(),
        extents: BTreeMap::new(),
    };
    for line in std::fs::read_to_string(REPORT).unwrap().lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let record: u64 = fields[2].parse().unwrap();
        match fields[0] {
            "stream" => fixture
                .streams
                .entry(fields[1].to_owned())
                .or_insert_with(|| (record, Vec::new()))
                .1
                .push((
                    fields[3].to_owned(),
                    fields[4].parse().unwrap(),
                    fields[5].to_owned(),
                )),
            "link" => fixture
                .links
                .entry(fields[1].to_owned())
                .or_default()
                .push(fields[3].to_owned()),
            "extents" => {
                fixture
                    .extents
                    .insert(fields[1].to_owned(), fields[3].parse().unwrap());
            }
            other => panic!("unknown report row {other}"),
        }
    }
    assert!(
        fixture.streams.len() >= 5,
        "{REPORT} names {} files; {PROVIDER} writes five",
        fixture.streams.len()
    );
    fixture
}

impl Fixture {
    fn volume(&self) -> RawVolume<Image> {
        self.volume_over(self.image.clone())
    }

    fn volume_over(&self, image: Image) -> RawVolume<Image> {
        RawVolume::open(image)
    }

    fn map(&self, volume: &RawVolume<Image>) -> MftMap {
        let map = volume
            .read_mft_map(
                self.mft_offset,
                self.record_size,
                self.sector_size,
                self.cluster_size,
            )
            .unwrap();
        assert!(!map.is_hypothesis());
        assert!(map.limitations.is_empty(), "{:?}", map.limitations);
        map
    }

    fn record(&self, path: &str) -> u64 {
        self.streams[path].0
    }

    fn resolve(&self, volume: &RawVolume<Image>, map: &MftMap, path: &str) -> ResolvedRecord {
        let base = volume.read_mapped_record(map, self.record(path)).unwrap();
        assert!(base.issues.is_empty(), "{path}: {:?}", base.issues);
        volume.resolve_salvage_record(&base, map).unwrap()
    }
}

fn read_all(volume: &RawVolume<Image>, stream: &SalvageStream, cluster_size: u64) -> Vec<u8> {
    let mut whole = Vec::new();
    let mut chunk = vec![0; CHUNK];
    loop {
        let count = volume
            .read_resolved_stream_at(stream, cluster_size, whole.len() as u64, &mut chunk)
            .unwrap_or_else(|error| panic!("stream {:?}: {error}", stream.name));
        if count == 0 {
            break;
        }
        whole.extend_from_slice(&chunk[..count]);
    }
    whole
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn every_stream_windows_reports_reads_back_through_the_primitive_api() {
    let fixture = fixture();
    let volume = fixture.volume();
    let map = fixture.map(&volume);
    let mut checked = 0;
    for (path, (_, expected)) in &fixture.streams {
        let resolved = fixture.resolve(&volume, &map, path);
        assert!(
            resolved.limitations.is_empty() && resolved.missing.is_empty(),
            "{path}: {:?} {:?}",
            resolved.limitations,
            resolved.missing
        );
        let names: BTreeSet<String> = resolved.streams().into_iter().map(|s| s.name).collect();
        let windows: BTreeSet<String> = expected.iter().map(|(name, ..)| name.clone()).collect();
        assert_eq!(
            names, windows,
            "{path}: the streams are not the ones Windows lists"
        );
        for (name, length, digest) in expected {
            let stream = resolved.stream(name).unwrap();
            assert!(
                stream.is_complete(),
                "{path}:{name}: {:?}",
                stream.limitations
            );
            assert_eq!(stream.len(), *length, "{path}:{name}");
            let bytes = read_all(&volume, &stream, fixture.cluster_size);
            assert_eq!(bytes.len() as u64, *length, "{path}:{name}");
            assert_eq!(
                &sha256(&bytes),
                digest,
                "{path}:{name} differs from Windows"
            );
            checked += 1;
        }
    }
    assert!(checked >= 89, "only {checked} streams were graded");
}

#[test]
fn the_fixture_has_the_shapes_windows_was_asked_for() {
    let fixture = fixture();
    let volume = fixture.volume();
    let map = fixture.map(&volume);

    let ads = fixture.resolve(&volume, &map, "/ads.bin");
    assert!(ads.base.has_attribute_list());
    let moved = ads
        .attributes
        .iter()
        .filter(|entry| entry.attribute.type_code == AttrType::Data as u32 && !entry.in_base)
        .count();
    assert!(
        moved > 0,
        "Windows kept every named stream in the base record"
    );

    let fragmented = fixture.resolve(&volume, &map, "/fragmented.bin");
    let data = fragmented.stream("").unwrap();
    assert!(fixture.extents["/fragmented.bin"] >= 600);
    assert_eq!(data.runs.len(), fixture.extents["/fragmented.bin"]);
    assert!(
        data.extents
            .iter()
            .any(|extent| extent.first_vcn > 0 && extent.record != fragmented.record_number),
        "no extent of the run list continues in an extension record: {:?}",
        data.extents
    );

    let list = fixture.resolve(&volume, &map, "/packed/list.bin");
    let data = list.stream("").unwrap();
    assert!(data.is_compressed());
    assert!(data.extents.len() > 1, "{:?}", data.extents);

    let small = fixture.resolve(&volume, &map, "/packed/small.txt");
    assert!(!small.base.has_attribute_list());
    assert!(small.stream("").unwrap().is_compressed());

    let links = fixture.resolve(&volume, &map, "/links.bin");
    assert!(links
        .attributes
        .iter()
        .any(|entry| { entry.attribute.type_code == AttrType::FileName as u32 && !entry.in_base }));
}

#[test]
fn hard_link_names_in_extension_records_match_windows() {
    let fixture = fixture();
    let volume = fixture.volume();
    let map = fixture.map(&volume);
    let resolved = fixture.resolve(&volume, &map, "/links.bin");
    let ours: BTreeSet<String> = resolved
        .file_names()
        .into_iter()
        .filter(|name| !name.is_dos_only())
        .map(|name| name.name)
        .collect();
    let windows: BTreeSet<String> = fixture.links["/links.bin"]
        .iter()
        .map(|path| path.rsplit('\\').next().unwrap().to_owned())
        .collect();
    assert_eq!(windows.len(), 25);
    assert_eq!(ours, windows);
}

#[test]
fn a_fragmented_run_list_matches_the_independent_reader() {
    use ntfs::NtfsReadSeek;
    let fixture = fixture();
    let volume = fixture.volume();
    let map = fixture.map(&volume);
    let number = fixture.record("/fragmented.bin");
    let stream = fixture
        .resolve(&volume, &map, "/fragmented.bin")
        .stream("")
        .unwrap();
    let ours = read_all(&volume, &stream, fixture.cluster_size);

    let mut cursor = std::io::BufReader::new(std::fs::File::open(IMAGE).unwrap());
    let independent = ntfs::Ntfs::new(&mut cursor).unwrap();
    let file = independent.file(&mut cursor, number).unwrap();
    let item = file.data(&mut cursor, "").unwrap().unwrap();
    let attribute = item.to_attribute().unwrap();
    let mut value = attribute.value(&mut cursor).unwrap();
    let mut theirs = vec![0; ours.len()];
    let mut filled = 0;
    while filled < theirs.len() {
        let count = value.read(&mut cursor, &mut theirs[filled..]).unwrap();
        assert!(count > 0);
        filled += count;
    }
    assert_eq!(ours, theirs);
}

#[test]
fn a_single_record_compressed_stream_reads_through_the_record_api() {
    let fixture = fixture();
    let volume = fixture.volume();
    let map = fixture.map(&volume);
    let record = volume
        .read_mapped_record(&map, fixture.record("/packed/small.txt"))
        .unwrap();
    let index = record
        .attributes
        .iter()
        .position(|a| a.type_code == AttrType::Data as u32 && a.name.is_empty())
        .unwrap();
    let (_, length, digest) = &fixture.streams["/packed/small.txt"].1[0];
    let mut output = vec![0; *length as usize + 10];
    let count = volume
        .read_salvage_stream_at(&record, index, fixture.cluster_size, 0, &mut output)
        .unwrap();
    assert_eq!(count as u64, *length);
    assert_eq!(&sha256(&output[..count]), digest);

    let listed = volume
        .read_mapped_record(&map, fixture.record("/ads.bin"))
        .unwrap();
    let index = listed
        .attributes
        .iter()
        .position(|a| a.type_code == AttrType::Data as u32)
        .unwrap();
    let error = volume
        .read_salvage_stream_at(&listed, index, fixture.cluster_size, 0, &mut output)
        .unwrap_err()
        .to_string();
    assert!(error.contains("resolve_salvage_record"), "{error}");
}

/// The offset of the extension record holding the extent of `path`'s
/// unnamed stream that starts after VCN 0.
fn later_extent_record(fixture: &Fixture, path: &str) -> (u64, u64) {
    let volume = fixture.volume();
    let map = fixture.map(&volume);
    let stream = fixture.resolve(&volume, &map, path).stream("").unwrap();
    let number = stream
        .extents
        .iter()
        .find(|extent| extent.first_vcn > 0)
        .and_then(|extent| extent.record)
        .unwrap();
    (number, map.record_offset(number).unwrap())
}

fn assert_refused(fixture: &Fixture, image: Image, path: &str, why: &str) {
    let volume = fixture.volume_over(image);
    let map = fixture.map(&volume);
    let resolved = fixture.resolve(&volume, &map, path);
    assert!(
        resolved
            .missing
            .iter()
            .any(|gone| gone.reason.contains(why)),
        "{path}: {:?}",
        resolved.missing
    );
    let stream = resolved.stream("").unwrap();
    assert!(!stream.is_complete());
    assert!(stream.limitations.iter().any(|l| l.contains("missing")));
    let error = volume
        .read_resolved_stream_at(&stream, fixture.cluster_size, 0, &mut [0; 4096])
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing"), "{error}");
}

#[test]
fn a_missing_or_foreign_extension_record_is_reported_and_never_read_short() {
    let fixture = fixture();
    for path in ["/fragmented.bin", "/packed/list.bin"] {
        let (_, offset) = later_extent_record(&fixture, path);
        let at = offset;

        let mut zeroed = fixture.image.clone();
        zeroed.fill(at, fixture.record_size, 0);
        assert_refused(&fixture, zeroed, path, "does not validate");

        // A torn sector: the update sequence number no longer matches.
        let mut torn = fixture.image.clone();
        torn.update(at + 510, |byte| byte ^ 0xff);
        assert_refused(&fixture, torn, path, "does not validate");

        let mut free = fixture.image.clone();
        free.update(at + 22, |byte| byte & !1);
        assert_refused(&fixture, free, path, "not in use");

        // The extension names another base record (the field is outside the
        // update sequence's protected words, so the record still validates).
        let mut foreign = fixture.image.clone();
        foreign.update(at + 32, |byte| byte ^ 0x01);
        assert_refused(&fixture, foreign, path, "names base record");
    }
}

#[test]
fn a_damaged_compression_unit_is_an_error_not_zeros() {
    let fixture = fixture();
    let volume = fixture.volume();
    let map = fixture.map(&volume);
    let stream = fixture
        .resolve(&volume, &map, "/packed/list.bin")
        .stream("")
        .unwrap();
    // Unit 2 is compressed (units 0, 1 are zeros and stored): break the
    // signature of its first chunk header.
    let unit_vcn = 2 * (1u64 << stream.compression_unit);
    let run = stream
        .runs
        .iter()
        .find(|run| unit_vcn >= run.starting_vcn && unit_vcn - run.starting_vcn < run.length)
        .unwrap();
    let lcn = run.lcn.unwrap() + unit_vcn - run.starting_vcn;
    let at = lcn * fixture.cluster_size;
    let mut image = fixture.image.clone();
    image.update(at + 1, |byte| byte & 0x8f);
    let damaged = fixture.volume_over(image);
    let unit = (1usize << stream.compression_unit) * fixture.cluster_size as usize;
    let mut output = vec![0; unit];
    let error = damaged
        .read_resolved_stream_at(&stream, fixture.cluster_size, 2 * unit as u64, &mut output)
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not decode"), "{error}");
    // The units around it still read.
    assert!(damaged
        .read_resolved_stream_at(&stream, fixture.cluster_size, 3 * unit as u64, &mut output)
        .is_ok());
}

const ATTRIBUTE_LIST: u32 = 0x20;

/// Offset of the first attribute header of `kind` without a name in `record`,
/// walking the headers the way the record itself does.
fn find_unnamed_attribute(record: &[u8], kind: u32) -> Option<usize> {
    let mut offset = u16::from_le_bytes([record[0x14], record[0x15]]) as usize;
    loop {
        let found = u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
        if found == 0xffff_ffff {
            return None;
        }
        let length = u32::from_le_bytes(record[offset + 4..offset + 8].try_into().unwrap());
        assert!(length > 0, "attribute header of length 0 at {offset:#x}");
        if found == kind && record[offset + 9] == 0 {
            return Some(offset);
        }
        offset += length as usize;
    }
}

/// The extension record that holds the unnamed `$DATA` at VCN 0, as the
/// base record's `$ATTRIBUTE_LIST` names it.
fn first_extent_record_from_list(record: &[u8]) -> Option<u64> {
    let list = find_unnamed_attribute(record, ATTRIBUTE_LIST)?;
    assert_eq!(record[list + 8], 0, "$ATTRIBUTE_LIST must be resident");
    let value_length = u32::from_le_bytes(record[list + 0x10..list + 0x14].try_into().unwrap());
    let value = u16::from_le_bytes([record[list + 0x14], record[list + 0x15]]) as usize + list;
    let end = value + value_length as usize;
    let mut entry = value;
    while entry + 0x18 <= end {
        let kind = u32::from_le_bytes(record[entry..entry + 4].try_into().unwrap());
        let length = u16::from_le_bytes([record[entry + 4], record[entry + 5]]) as usize;
        assert!(length > 0, "attribute list entry of length 0 at {entry:#x}");
        let lowest_vcn = u64::from_le_bytes(record[entry + 8..entry + 16].try_into().unwrap());
        let reference = u64::from_le_bytes(record[entry + 0x10..entry + 0x18].try_into().unwrap());
        if kind == AttrType::Data as u32 && record[entry + 6] == 0 && lowest_vcn == 0 {
            return Some(reference & 0xffff_ffff_ffff);
        }
        entry += length;
    }
    None
}

/// Patches the flags of the unnamed `$DATA` attribute holding VCN 0 of
/// `path`. The base record holds it when it can; otherwise the attribute list
/// names the extension record that does. Salvage takes a stream's flags from
/// that first extent.
fn set_first_extent_data_flag(fixture: &mut Fixture, path: &str, flag: u16) {
    let number = fixture.record(path);
    let volume = fixture.volume();
    let map = fixture.map(&volume);
    let at = map.record_offset(number).unwrap() as usize;
    let record = fixture.image.read(at as u64, fixture.record_size);
    let data = AttrType::Data as u32;
    let (record_at, attribute) = match find_unnamed_attribute(&record, data) {
        Some(offset) => (at, offset),
        None => {
            let extension = first_extent_record_from_list(&record).expect("no VCN 0 list entry");
            let extension_at = map.record_offset(extension).unwrap() as usize;
            let extension_record = fixture.image.read(extension_at as u64, fixture.record_size);
            let offset = find_unnamed_attribute(&extension_record, data).expect("no unnamed $DATA");
            (extension_at, offset)
        }
    };
    let flags = record_at + attribute + 0x0c;
    let current = u16::from_le_bytes(fixture.image.read(flags as u64, 2).try_into().unwrap());
    fixture
        .image
        .write(flags as u64, &(current | flag).to_le_bytes());
}

#[test]
fn a_stale_sequence_extension_record_is_reported_and_never_read_short() {
    let fixture = fixture();
    for path in ["/fragmented.bin", "/packed/list.bin"] {
        let (_, offset) = later_extent_record(&fixture, path);

        // The record's sequence number sits outside the update sequence's
        // protected words, so the record still validates and only the list
        // entry's reference disagrees with it.
        let mut stale = fixture.image.clone();
        stale.update(offset + 0x10, |byte| byte ^ 0x01);
        assert_refused(&fixture, stale, path, "has sequence");
    }
}

#[test]
fn an_encrypted_stream_is_refused_by_name_through_salvage() {
    let mut fixture = fixture();
    const ATTR_FLAG_ENCRYPTED: u16 = 0x4000;
    set_first_extent_data_flag(&mut fixture, "/fragmented.bin", ATTR_FLAG_ENCRYPTED);
    let volume = fixture.volume();
    let map = fixture.map(&volume);
    let resolved = fixture.resolve(&volume, &map, "/fragmented.bin");
    let stream = resolved.stream("").unwrap();
    assert!(stream.is_encrypted());
    let error = volume
        .read_resolved_stream_at(&stream, fixture.cluster_size, 0, &mut [0; 4096])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("encrypted salvage streams are refused"),
        "{error}"
    );
}
