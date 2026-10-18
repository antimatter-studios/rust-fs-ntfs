//! Rewriting a split named stream keeps its extents true (#805, part of
//! #664).
//!
//! A named stream whose run list did not fit one MFT record is split into
//! extents across extension records, each named in the `$ATTRIBUTE_LIST`
//! with its starting VCN (#799). #800 taught a rewrite, a write at an
//! offset and a truncate to move such extents for the unnamed `$DATA`
//! only; a rewrite of a split named stream was still refused, because the
//! list lookup it went through refuses any attribute split across
//! records. Named streams are written whole, so growing one, writing
//! inside it, rewriting it, cutting it short and extending it are all
//! rewrites. This splits a named stream, then does each of those, and
//! checks after each change that the list names every extent with its
//! starting VCN, that the extents cover the stream end to end, that the
//! `ntfs` crate -- a parser that is not this one -- reads the stream back,
//! that the file's other streams are untouched, and that `check_io` is
//! clean.
mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{bitmap, data_runs, fsck, mft_io, read, write};
use ntfs::NtfsReadSeek;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const RECORD_SIZE: u32 = 1024;
const CLUSTER: u32 = 4096;
/// One-cluster holes the free space is cut into.
const HOLES: usize = 420;
/// The stream's length in clusters: one run per hole, more runs than the
/// mapping pairs of one MFT record hold, fewer than there are holes.
const STREAM_CLUSTERS: usize = 400;

/// `/f`, with an EA and enough named streams that its base record filled
/// and it gained an `$ATTRIBUTE_LIST`.
fn volume_with_a_listed_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("alsplit_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, RECORD_SIZE, None, Some(1))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    write::write_ea_io(&mut io, "/f", b"USER.TAG", &[b'e'; 64], 0).expect("write /f's EA");
    for i in 0..8 {
        write::write_named_stream_io(&mut io, "/f", &format!("s{i}"), &[b'a' + i as u8; 120])
            .unwrap_or_else(|e| panic!("write /f:s{i}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    assert!(
        read::read_attribute_value_if_present(&mut io, rec, AttrType::AttributeList, None)
            .expect("read /f's list")
            .is_some(),
        "/f's base record never filled, so it has no $ATTRIBUTE_LIST to test"
    );
    (img, rec)
}

/// The longest run of free clusters on the volume.
fn longest_free_run(io: &mut PathIo) -> u64 {
    let bm = bitmap::locate_bitmap_io(io).expect("locate $Bitmap");
    let (mut lo, mut hi) = (0u64, bm.total_bits);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let found = bitmap::find_free_run_io(io, &bm, mid, 0).expect("scan $Bitmap");
        if found.is_some() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Cut the volume's free space into one-cluster holes: `2 * HOLES`
/// one-cluster files, every free run longer than one cluster filled by a
/// file of its own, then every other one-cluster file unlinked.
fn fragment_free_space(img: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let one = vec![0xC5u8; CLUSTER as usize];
    for i in 0..2 * HOLES {
        let name = format!("c{i}");
        write::create_file_io(&mut io, "/", &name).expect("create a one-cluster file");
        write::write_file_contents_io(&mut io, &format!("/{name}"), &one)
            .expect("write a one-cluster file");
    }
    let mut fill = 0;
    loop {
        let longest = longest_free_run(&mut io);
        if longest < 2 {
            break;
        }
        let name = format!("fill{fill}");
        write::create_file_io(&mut io, "/", &name).expect("create a filler");
        write::write_file_contents_io(
            &mut io,
            &format!("/{name}"),
            &vec![0x3Cu8; (longest * CLUSTER as u64) as usize],
        )
        .unwrap_or_else(|e| panic!("fill a free run of {longest} clusters: {e}"));
        fill += 1;
    }
    for i in (1..2 * HOLES).step_by(2) {
        write::unlink_io(&mut io, &format!("/c{i}")).expect("unlink a one-cluster file");
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let longest = longest_free_run(&mut io);
    assert!(
        longest < STREAM_CLUSTERS as u64,
        "the free space was not fragmented: a run of {longest} clusters holds the stream"
    );
}

/// Every attribute of the file at `rec` is named by a list entry where it
/// is, and every entry names an attribute that is there.
fn list_is_true(img: &str, rec: u64) {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let entries = read::parse_attribute_list(&list).expect("parse the list");
    let mut numbers: Vec<u64> = entries.iter().map(|e| e.record_number).collect();
    numbers.sort_unstable();
    numbers.dedup();
    let mut held = 0usize;
    for &n in &numbers {
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a listed record");
        assert_ne!(
            mft_io::record_flags(&record) & mft_io::MFT_FLAG_IN_USE,
            0,
            "the list names record {n}, which is not in use"
        );
        for loc in attr_io::iter_attributes(&record) {
            if loc.type_code == AttrType::AttributeList as u32 {
                continue;
            }
            held += 1;
            let named = entries.iter().any(|e| {
                e.record_number == n
                    && e.type_code == loc.type_code
                    && e.attribute_id == loc.attribute_id
            });
            assert!(
                named,
                "record {n} holds attribute {:#x} id {}, and the list does not name it",
                loc.type_code, loc.attribute_id
            );
        }
    }
    assert_eq!(
        held,
        entries.len(),
        "the list has entries for attributes that are not where it says"
    );
}

fn clean(img: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let report = fsck::check_io(&mut io).expect("check the volume");
    assert!(
        report.findings.is_empty(),
        "check_io finds the volume unclean: {:?}",
        report.findings
    );
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// The named stream split into extents.
const STREAM: &str = "big";

/// One extent of `/f:big`: the starting VCN its list entry names, the
/// record it is in, its own lowest and highest VCN, and its runs.
struct Extent {
    listed_vcn: u64,
    record: u64,
    lowest_vcn: u64,
    highest_vcn: u64,
    runs: Vec<data_runs::DataRun>,
}

/// Every extent of `/f:big`, in list order, read from the record its list
/// entry names.
fn named_stream_extents(img: &str, rec: u64) -> Vec<Extent> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let entries: Vec<_> = read::parse_attribute_list(&list)
        .expect("parse the list")
        .into_iter()
        .filter(|e| e.type_code == AttrType::Data as u32 && e.name.as_deref() == Some(STREAM))
        .collect();
    let mut extents = Vec::new();
    for entry in entries {
        let (_, record) =
            mft_io::read_mft_record_io(&mut io, entry.record_number).expect("read its record");
        let loc = attr_io::iter_attributes(&record)
            .find(|l| l.type_code == AttrType::Data as u32 && l.attribute_id == entry.attribute_id)
            .expect("the record holds the extent its entry names");
        assert!(!loc.is_resident, "an extent of the stream is resident");
        let at = loc.attr_offset;
        let lowest_vcn = u64::from_le_bytes(record[at + 16..at + 24].try_into().unwrap());
        let highest_vcn = u64::from_le_bytes(record[at + 24..at + 32].try_into().unwrap());
        let mpo = loc
            .non_resident_mapping_pairs_offset
            .expect("a mapping-pairs offset") as usize;
        let runs = data_runs::decode_runs(&record[at + mpo..at + loc.attr_length])
            .expect("decode the mapping pairs");
        extents.push(Extent {
            listed_vcn: entry.starting_vcn,
            record: entry.record_number,
            lowest_vcn,
            highest_vcn,
            runs,
        });
    }
    extents
}

/// The extents of `/f:big` are true for a stream of `clusters` clusters:
/// each starts where its list entry says and where the one before it
/// ended, maps exactly the VCNs it spans, and none but the first is in the
/// base record. Returns how many there are.
fn extents_are_true(img: &str, rec: u64, clusters: u64) -> usize {
    let extents = named_stream_extents(img, rec);
    let mut next_vcn = 0u64;
    for (i, extent) in extents.iter().enumerate() {
        assert_eq!(
            extent.listed_vcn, extent.lowest_vcn,
            "extent {i}'s list entry names VCN {}, and the extent starts at {}",
            extent.listed_vcn, extent.lowest_vcn
        );
        assert_eq!(
            extent.lowest_vcn, next_vcn,
            "extent {i} starts at VCN {}, not where the one before it ended",
            extent.lowest_vcn
        );
        if i > 0 {
            assert_ne!(extent.record, rec, "extent {i} is in the base record");
        }
        let mapped: u64 = extent.runs.iter().map(|r| r.length).sum();
        assert_eq!(
            extent.highest_vcn + 1 - extent.lowest_vcn,
            mapped,
            "extent {i} spans VCNs {}..={} and maps {mapped} clusters",
            extent.lowest_vcn,
            extent.highest_vcn
        );
        next_vcn = extent.highest_vcn + 1;
    }
    assert_eq!(
        next_vcn, clusters,
        "the extents map {next_vcn} clusters, not the stream's {clusters}"
    );
    extents.len()
}

/// `/f:big` as the `ntfs` crate reads it.
fn ntfs_crate_reads_stream(img: &str) -> Vec<u8> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, "/f");
    let item = file
        .data(&mut reader, STREAM)
        .expect("the ntfs crate finds /f:big")
        .expect("read /f:big's attribute");
    let attr = item.to_attribute().expect("to_attribute");
    let mut value = attr.value(&mut reader).expect("attribute value");
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = value.read(&mut reader, &mut chunk).expect("read");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    buf
}

/// After a change, `/f:big` holds `expected` by the list, the `ntfs`
/// crate and this driver alike, the file's other streams are untouched,
/// and the volume is clean. Returns how many extents the stream is in.
fn stream_is_true(img: &str, rec: u64, expected: &[u8], what: &str) -> usize {
    let clusters = (expected.len() as u64).div_ceil(CLUSTER as u64);
    let extents = extents_are_true(img, rec, clusters);
    list_is_true(img, rec);
    let read_back = ntfs_crate_reads_stream(img);
    assert!(
        read_back == expected,
        "after {what}, the ntfs crate reads {} bytes of /f:big, not the {} expected",
        read_back.len(),
        expected.len()
    );
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    assert_eq!(
        read::read_attribute_value(&mut io, rec, AttrType::Data, Some("s3")).expect("read /f:s3"),
        vec![b'd'; 120],
        "{what} changed another named stream"
    );
    assert_eq!(
        read::read_attribute_value(&mut io, rec, AttrType::Data, None).expect("read /f"),
        b"contents",
        "{what} changed the unnamed stream"
    );
    assert!(
        read::read_attribute_value(&mut io, rec, AttrType::Data, Some(STREAM))
            .expect("read /f:big")
            == expected,
        "after {what}, this driver reads /f:big back differently"
    );
    drop(io);
    clean(img);
    extents
}

fn rewrite(img: &str, data: &[u8], what: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    write::write_named_stream_io(&mut io, "/f", STREAM, data)
        .unwrap_or_else(|e| panic!("{what}: write {} bytes to /f:big: {e}", data.len()));
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

/// A split named stream grows, takes a change inside it, is rewritten
/// whole, is cut short and is extended again, each by a rewrite, and its
/// extents stay true each time: those no longer needed go with their
/// entries and records.
#[test]
fn a_split_named_stream_keeps_its_extents_true_through_rewrites() {
    let (img, rec) = volume_with_a_listed_file("named");
    fragment_free_space(&img);
    let cluster = CLUSTER as usize;
    let mut expected = pattern(STREAM_CLUSTERS * cluster - 100);
    rewrite(&img, &expected, "the first write");
    let extents = stream_is_true(&img, rec, &expected, "the first write");
    assert!(extents > 1, "the stream is in {extents} extent, not split");

    // The one-cluster files left between the holes go too, so a stream as
    // long again finds room, still one cluster at a time.
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    for i in (0..2 * HOLES).step_by(2) {
        write::unlink_io(&mut io, &format!("/c{i}")).expect("unlink a one-cluster file");
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);

    // Grown past its end.
    expected.resize(expected.len() + cluster, 0);
    expected.extend_from_slice(&vec![0x7Eu8; 9 * cluster]);
    rewrite(&img, &expected, "growing it");
    let extents = stream_is_true(&img, rec, &expected, "growing it");
    assert!(
        extents > 1,
        "the grown stream is in {extents} extent, not split"
    );

    // Changed inside.
    let at = 150 * cluster + 7;
    expected[at..at + 3 * cluster].fill(0xA5);
    rewrite(&img, &expected, "a change inside it");
    stream_is_true(&img, rec, &expected, "a change inside it");

    // Rewritten whole at the same length.
    expected = expected.iter().map(|b| b ^ 0x5A).collect();
    rewrite(&img, &expected, "a rewrite");
    stream_is_true(&img, rec, &expected, "a rewrite");

    // Cut short to two clusters: one extent, the rest gone with their
    // entries.
    expected.truncate(2 * cluster - 9);
    rewrite(&img, &expected, "cutting it short");
    let extents = stream_is_true(&img, rec, &expected, "cutting it short");
    assert_eq!(extents, 1, "a two-cluster stream is in {extents} extents");

    // Extended again with zeros.
    expected.resize(300 * cluster, 0);
    rewrite(&img, &expected, "extending it");
    stream_is_true(&img, rec, &expected, "extending it");
}
