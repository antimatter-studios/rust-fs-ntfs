//! A stream its file's base record cannot hold goes to an extension record,
//! named in an `$ATTRIBUTE_LIST` this crate creates and keeps true (#480).
//!
//! The writer refused instead: "no room for new attribute" for a third or
//! fourth stream on a 1 KiB record, where Windows moves attributes out to
//! extension records and lists where each went. Windows grades the result
//! in the matrix (`mac-format-mac-write-streams-win-chkdsk` and its two
//! siblings); here the `ntfs` crate, a parser that is not this one, reads
//! every stream back, `check_io` reports the volume clean, and the list is
//! checked to name every attribute where it is.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::bitmap;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::error::{Error, Kind};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{data_runs, fsck, mft_bitmap, mft_io, read, write};
use ntfs::NtfsReadSeek;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const RECORD_SIZE: u32 = 1024;
const CLUSTER: u32 = 4096;

/// Stream `i`'s contents: "stream <i> " repeated to `len` bytes, so a
/// stream read back from the wrong place shows it.
fn contents(i: usize, len: usize) -> Vec<u8> {
    format!("stream {i} ").bytes().cycle().take(len).collect()
}

/// A fresh volume with 1 KiB records holding `/f`, whose unnamed stream
/// is `contents`. Returns the image and `/f`'s record.
fn volume_with_a_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("alcreate_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, RECORD_SIZE, None, Some(1))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, rec)
}

fn write_streams(img: &str, path: &str, range: std::ops::RangeInclusive<usize>, len: usize) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    for i in range {
        write::write_named_stream_io(&mut io, path, &format!("s{i}"), &contents(i, len))
            .unwrap_or_else(|e| panic!("write {path}:s{i}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

/// The `ntfs` crate's reading of the stream `name` of `path`.
fn ntfs_stream(img: &str, path: &str, name: &str) -> Vec<u8> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, path);
    let item = file
        .data(&mut reader, name)
        .unwrap_or_else(|| panic!("the ntfs crate finds no stream {path}:{name}"))
        .unwrap_or_else(|e| panic!("the ntfs crate cannot read {path}:{name}: {e}"));
    let attr = item.to_attribute().expect("to_attribute");
    let mut value = attr.value(&mut reader).expect("attribute value");
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = value.read(&mut reader, &mut chunk).expect("read");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
    }
    out
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

/// The record numbers of `rec`'s extension records, having checked that
/// its list names every attribute of the file where it is: each entry's
/// record in use, owned by `rec`, holding an attribute of that type, id
/// and name; and every attribute in those records but the list itself
/// named by an entry.
fn list_is_true(img: &str, rec: u64) -> Vec<u64> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let entries = read::parse_attribute_list(&list).expect("parse the list");
    let mut numbers: Vec<u64> = entries.iter().map(|e| e.record_number).collect();
    numbers.sort_unstable();
    numbers.dedup();
    assert!(
        numbers.contains(&rec),
        "the list names nothing in the base record"
    );
    let mut held = 0usize;
    for &n in &numbers {
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a listed record");
        assert_ne!(
            mft_io::record_flags(&record) & mft_io::MFT_FLAG_IN_USE,
            0,
            "the list names record {n}, which is not in use"
        );
        let owner = u64::from_le_bytes(record[0x20..0x28].try_into().unwrap()) & 0xFFFF_FFFF_FFFF;
        assert_eq!(
            owner,
            if n == rec { 0 } else { rec },
            "record {n}'s base record"
        );
        for loc in attr_io::iter_attributes(&record) {
            if loc.type_code == AttrType::AttributeList as u32 {
                continue;
            }
            held += 1;
            let name = attr_io::decode_attr_name(&record, &loc);
            let named = entries.iter().any(|e| {
                e.record_number == n
                    && e.type_code == loc.type_code
                    && e.attribute_id == loc.attribute_id
                    && e.name.clone().unwrap_or_default() == name
            });
            assert!(
                named,
                "record {n} holds attribute {:#x} {name:?} id {}, and the list does not name it",
                loc.type_code, loc.attribute_id
            );
        }
    }
    assert_eq!(
        held,
        entries.len(),
        "the list has entries for attributes that are not where it says"
    );
    numbers.into_iter().filter(|&n| n != rec).collect()
}

fn list_location(img: &str, rec: u64) -> attr_io::AttrLocation {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f");
    attr_io::find_attribute(&record, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST")
}

fn every_stream_reads_back(img: &str, range: std::ops::RangeInclusive<usize>, len: usize) {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let mut listed = write::list_named_streams_io(&mut io, "/f").expect("list /f's streams");
    listed.sort();
    let mut want: Vec<String> = range.clone().map(|i| format!("s{i}")).collect();
    want.sort();
    assert_eq!(listed, want, "the streams listed");
    for i in range {
        assert_eq!(
            ntfs_stream(img, "/f", &format!("s{i}")),
            contents(i, len),
            "the ntfs crate's reading of s{i}"
        );
    }
    let (ntfs, mut reader) = common::open(img);
    assert_eq!(
        common::read_file_all(&ntfs, &mut reader, "/f"),
        b"contents",
        "the unnamed stream"
    );
}

/// Eight 64-byte streams: the eighth does not fit, so the file gets a
/// list, its streams move to an extension record, and the list stays in
/// the base record.
#[test]
fn a_stream_the_record_cannot_hold_gets_a_list_and_an_extension_record() {
    let (img, rec) = volume_with_a_file("first");
    write_streams(&img, "/f", 1..=8, 64);
    let extensions = list_is_true(&img, rec);
    assert!(
        !extensions.is_empty(),
        "eight streams fitted in the base record; the test no longer reaches #480's path"
    );
    assert!(
        list_location(&img, rec).is_resident,
        "a list of eleven entries left the base record"
    );
    every_stream_reads_back(&img, 1..=8, 64);
    clean(&img);
}

/// 150 streams: the list outgrows the base record and goes to clusters,
/// and outgrows its first cluster and moves to a run of two.
#[test]
fn a_list_too_big_for_the_record_goes_to_clusters() {
    let (img, rec) = volume_with_a_file("clusters");
    write_streams(&img, "/f", 1..=150, 16);
    list_is_true(&img, rec);
    let loc = list_location(&img, rec);
    assert!(!loc.is_resident, "a list of 153 entries stayed resident");
    every_stream_reads_back(&img, 1..=150, 16);
    clean(&img);
}

/// The clusters the named non-resident streams in `records` map.
fn stream_clusters(img: &str, records: &[u64]) -> Vec<u64> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let mut lcns = Vec::new();
    for &n in records {
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a record");
        for loc in attr_io::iter_attributes(&record) {
            if loc.type_code != AttrType::Data as u32 || loc.name_length == 0 || loc.is_resident {
                continue;
            }
            let at = loc.attr_offset + loc.non_resident_mapping_pairs_offset.unwrap() as usize;
            let runs = data_runs::decode_runs(&record[at..loc.attr_offset + loc.attr_length])
                .expect("decode a stream's runs");
            for run in runs {
                if let Some(lcn) = run.lcn {
                    lcns.extend(lcn..lcn + run.length);
                }
            }
        }
    }
    lcns
}

/// Thirty non-resident streams spread over extension records, then all
/// deleted: every extension record is freed, and so is every cluster the
/// streams held.
#[test]
fn non_resident_streams_in_extension_records_are_freed_with_them() {
    let (img, rec) = volume_with_a_file("nonres");
    write_streams(&img, "/f", 1..=30, 3000);
    let extensions = list_is_true(&img, rec);
    assert!(
        !extensions.is_empty(),
        "thirty streams fitted in the base record"
    );
    every_stream_reads_back(&img, 1..=30, 3000);
    clean(&img);
    let mut records = extensions.clone();
    records.push(rec);
    let held = stream_clusters(&img, &records);
    assert_eq!(
        held.len(),
        30,
        "thirty one-cluster streams map {} clusters",
        held.len()
    );

    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    for i in 1..=30 {
        write::delete_named_stream_io(&mut io, "/f", &format!("s{i}"))
            .unwrap_or_else(|e| panic!("delete /f:s{i}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let mbm = mft_bitmap::locate_io(&mut io).expect("locate $MFT:$Bitmap");
    for &n in &extensions {
        assert!(
            !mft_bitmap::is_allocated_io(&mut io, &mbm, n).expect("read $MFT:$Bitmap"),
            "extension record {n} is still allocated with every stream deleted"
        );
    }
    let bm = bitmap::locate_bitmap_io(&mut io).expect("locate $Bitmap");
    for lcn in held {
        assert!(
            !bitmap::is_allocated_io(&mut io, &bm, lcn).expect("read $Bitmap"),
            "cluster {lcn} a deleted stream held is still allocated"
        );
    }
    drop(io);
    list_is_true(&img, rec);
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    assert_eq!(
        write::list_named_streams_io(&mut io, "/f").expect("list /f's streams"),
        Vec::<String>::new(),
        "streams are listed after every one was deleted"
    );
    drop(io);
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/f"), b"contents");
    clean(&img);
}

/// Streams already in extension records are rewritten where they are, or
/// moved when they no longer fit, and the list follows them.
#[test]
fn a_stream_in_an_extension_record_is_rewritten_and_the_list_follows() {
    let (img, rec) = volume_with_a_file("rewrite");
    write_streams(&img, "/f", 1..=12, 64);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let big = contents(1, 10_000);
    write::write_named_stream_io(&mut io, "/f", "s1", &big).expect("rewrite s1, non-resident");
    write::write_named_stream_io(&mut io, "/f", "s2", b"s2").expect("rewrite s2, shorter");
    let wide = contents(3, 700);
    write::write_named_stream_io(&mut io, "/f", "s3", &wide).expect("rewrite s3, wider");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    list_is_true(&img, rec);
    assert_eq!(ntfs_stream(&img, "/f", "s1"), big);
    assert_eq!(ntfs_stream(&img, "/f", "s2"), b"s2");
    assert_eq!(ntfs_stream(&img, "/f", "s3"), wide);
    for i in 4..=12 {
        assert_eq!(ntfs_stream(&img, "/f", &format!("s{i}")), contents(i, 64));
    }
    clean(&img);
}

/// A write through the list that is refused -- here for want of
/// clusters -- leaves the image byte for byte as it was.
#[test]
fn a_write_through_the_list_that_is_refused_changes_nothing() {
    let (img, _) = volume_with_a_file("refused");
    write_streams(&img, "/f", 1..=8, 64);
    let before = std::fs::read(&img).expect("read image");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let got =
        write::write_named_stream_io(&mut io, "/f", "huge", &vec![1u8; 2 * VOL_SIZE as usize]);
    assert!(
        matches!(&got, Err(e) if e.kind() == Kind::NoSpace),
        "a stream bigger than the volume: {got:?}"
    );
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    let after = std::fs::read(&img).expect("read image");
    let first = before.iter().zip(&after).position(|(a, b)| a != b);
    assert!(
        first.is_none(),
        "a refused write changed the image, first at byte {first:?}"
    );
}

/// A directory's attributes are not moved between records: a stream its
/// record cannot hold is refused, with nothing written.
#[test]
fn a_directory_is_given_no_list() {
    let (img, _) = volume_with_a_file("dir");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    let mut refused: Option<Error> = None;
    for i in 1..=16 {
        if let Err(e) = write::write_named_stream_io(&mut io, "/d", &format!("s{i}"), &[7u8; 64]) {
            refused = Some(e);
            break;
        }
    }
    let refused = refused.expect("sixteen streams fitted in a directory's record");
    assert_eq!(refused.kind(), Kind::Refused, "{refused}");
}
