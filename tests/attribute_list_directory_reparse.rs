//! A reparse point set on a directory whose base record has no room for
//! it, or on a directory that has an `$ATTRIBUTE_LIST`, goes through the
//! list, and removing it takes its entry out, as a regular file's does
//! (#808), now that a directory can be given a list (#814).
//!
//! `write_reparse_point_io` and `remove_reparse_point_io` kept every
//! directory on the base-record path, so a directory symlink on a
//! directory with a list was refused ("the record carries an
//! $ATTRIBUTE_LIST"), and one its base record had no room for was refused
//! for want of room, where Windows moves attributes to extension records
//! and lists where each went.
//!
//! The `ntfs` crate, a parser that is not this one, finds the
//! `$REPARSE_POINT` through the list, and its symlink target is decoded
//! from the `REPARSE_DATA_BUFFER` layout Windows documents; it still lists
//! the directory's entries; `check_io` reports the volume clean; and every
//! list entry is checked to name an attribute that is where the entry
//! says.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::record_build::{build_symlink_reparse_data, reparse_tag};
use fs_ntfs::{fsck, mft_io, read, write};
use ntfs::{NtfsAttributeType, NtfsReadSeek};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

/// `FILE_ATTRIBUTE_REPARSE_POINT` in `$STANDARD_INFORMATION`.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// A fresh volume with 1 KiB records holding the directory `/d` and the
/// files `/d/a` and `/d/b`. Returns the image and `/d`'s record.
fn volume_with_a_directory(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("aldirrp_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let rec = write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    for name in ["a", "b"] {
        write::create_file_io(&mut io, "/d", name)
            .unwrap_or_else(|e| panic!("create /d/{name}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, rec)
}

/// [`volume_with_a_directory`], with an 800-byte EA on `/d`: more than its
/// base record holds beside its index, so `/d` has an `$ATTRIBUTE_LIST`
/// and an extension record before any reparse point is set.
fn volume_with_a_listed_directory(tag: &str) -> (String, u64) {
    let (img, rec) = volume_with_a_directory(tag);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let value: Vec<u8> = b"ea value ".iter().copied().cycle().take(800).collect();
    write::write_ea_io(&mut io, "/d", b"USER.BIG", &value, 0).expect("write an EA on /d");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).expect("read /d");
    assert!(
        attr_io::find_attribute(&record, AttrType::AttributeList, None).is_some(),
        "the EA left /d without a list; the test no longer starts from one"
    );
    (img, rec)
}

fn has_list(img: &str, rec: u64) -> bool {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).expect("read /d");
    attr_io::find_attribute(&record, AttrType::AttributeList, None).is_some()
}

/// The UTF-16 string at `offset`, `len` bytes long, into `buf`.
fn utf16_at(buf: &[u8], offset: usize, len: usize) -> String {
    let units: Vec<u16> = buf[offset..offset + len]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).expect("a UTF-16 name")
}

/// The tag and substitute name of the `$REPARSE_POINT` the `ntfs` crate
/// finds on `path`, following its `$ATTRIBUTE_LIST`, decoded from the
/// `REPARSE_DATA_BUFFER` of a symbolic link as Windows documents it;
/// `None` when it finds no `$REPARSE_POINT`.
fn ntfs_crate_symlink(img: &str, path: &str) -> Option<(u32, String)> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, path);
    let mut attrs = file.attributes();
    while let Some(item) = attrs.next(&mut reader) {
        let item = item.expect("an attribute of the directory");
        let attr = item.to_attribute().expect("to_attribute");
        if attr.ty().expect("attribute type") != NtfsAttributeType::ReparsePoint {
            continue;
        }
        let mut value = attr.value(&mut reader).expect("the $REPARSE_POINT's value");
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = value
                .read(&mut reader, &mut chunk)
                .expect("read the $REPARSE_POINT");
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        let tag = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let data_len = u16::from_le_bytes(bytes[4..6].try_into().unwrap()) as usize;
        assert_eq!(bytes.len(), 8 + data_len, "ReparseDataLength");
        // SymbolicLinkReparseBuffer: four u16 name offsets and lengths and
        // a u32 of flags, then the path buffer.
        let data = &bytes[8..];
        let sub_off = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
        let sub_len = u16::from_le_bytes(data[2..4].try_into().unwrap()) as usize;
        return Some((tag, utf16_at(&data[12..], sub_off, sub_len)));
    }
    None
}

/// The names the `ntfs` crate lists in the directory `path`, sorted.
fn ntfs_crate_names(img: &str, path: &str) -> Vec<String> {
    let (ntfs, mut reader) = common::open(img);
    let mut names = common::list_names(&ntfs, &mut reader, path);
    names.sort();
    names
}

/// Every entry of the list of the directory at `rec`, as
/// `(type, record)`, having checked that each names an in-use record of
/// this directory holding an attribute of that type and id.
fn listed_records(img: &str, rec: u64) -> Vec<(u32, u64)> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/d has an $ATTRIBUTE_LIST");
    let mut out = Vec::new();
    for e in read::parse_attribute_list(&list).expect("parse the list") {
        let n = e.record_number;
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a listed record");
        assert_ne!(
            mft_io::record_flags(&record) & mft_io::MFT_FLAG_IN_USE,
            0,
            "the list puts attribute {:#x} in record {n}, which is not in use",
            e.type_code
        );
        let owner = u64::from_le_bytes(record[0x20..0x28].try_into().unwrap()) & 0xFFFF_FFFF_FFFF;
        assert_eq!(
            owner,
            if n == rec { 0 } else { rec },
            "record {n}'s base record"
        );
        assert!(
            attr_io::iter_attributes(&record)
                .any(|l| l.type_code == e.type_code && l.attribute_id == e.attribute_id),
            "the list puts attribute {:#x} id {} in record {n}, and it is not there",
            e.type_code,
            e.attribute_id
        );
        out.push((e.type_code, n));
    }
    out
}

/// The records the list of the directory at `rec` puts `ty` in.
fn listed_records_of(img: &str, rec: u64, ty: AttrType) -> Vec<u64> {
    listed_records(img, rec)
        .into_iter()
        .filter(|(t, _)| *t == ty as u32)
        .map(|(_, n)| n)
        .collect()
}

/// The file attributes in the `$STANDARD_INFORMATION` of record `rec`.
fn si_file_attributes(img: &str, rec: u64) -> u32 {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let si = read::read_attribute_value(&mut io, rec, AttrType::StandardInformation, None)
        .expect("$STANDARD_INFORMATION");
    u32::from_le_bytes(si[0x20..0x24].try_into().unwrap())
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

fn set_reparse_point(img: &str, path: &str, data: &[u8]) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    write::write_reparse_point_io(&mut io, path, reparse_tag::SYMLINK, data)
        .unwrap_or_else(|e| panic!("set a reparse point on {path}: {e}"));
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

fn remove_reparse_point(img: &str, path: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    write::remove_reparse_point_io(&mut io, path)
        .unwrap_or_else(|e| panic!("remove the reparse point of {path}: {e}"));
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

/// The reparse point the driver's own reader finds on `path`.
fn our_reparse_point(img: &str, path: &str) -> Option<write::ReparsePoint> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    write::read_reparse_point_io(&mut io, path)
        .unwrap_or_else(|e| panic!("read the reparse point of {path}: {e}"))
}

/// A directory that has a list takes a directory symlink, and loses it:
/// the list gains and loses its entry, the reparse bit follows, the
/// directory's index is untouched, and the volume stays clean throughout.
#[test]
fn a_reparse_point_on_a_directory_with_a_list_is_set_and_removed_through_it() {
    let (img, rec) = volume_with_a_listed_directory("set");
    let target = "\\??\\C:\\target\\of\\the\\link";
    let data = build_symlink_reparse_data(target, Some("C:\\target\\of\\the\\link"), false);
    set_reparse_point(&img, "/d", &data);

    assert_eq!(
        listed_records_of(&img, rec, AttrType::ReparsePoint).len(),
        1,
        "the list names one $REPARSE_POINT"
    );
    assert_eq!(
        listed_records_of(&img, rec, AttrType::IndexRoot),
        [rec],
        "$INDEX_ROOT left the base record"
    );
    assert_eq!(
        ntfs_crate_symlink(&img, "/d"),
        Some((reparse_tag::SYMLINK, target.to_string())),
        "the ntfs crate"
    );
    assert_eq!(
        our_reparse_point(&img, "/d"),
        Some(write::ReparsePoint {
            reparse_tag: reparse_tag::SYMLINK,
            data: data.clone(),
        }),
        "the driver's own reader"
    );
    assert_ne!(
        si_file_attributes(&img, rec) & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "the reparse bit is set"
    );
    assert_eq!(ntfs_crate_names(&img, "/d"), ["a", "b"]);
    clean(&img);

    remove_reparse_point(&img, "/d");
    assert!(
        listed_records_of(&img, rec, AttrType::ReparsePoint).is_empty(),
        "the list still names a $REPARSE_POINT"
    );
    assert_eq!(ntfs_crate_symlink(&img, "/d"), None, "the ntfs crate");
    assert_eq!(
        our_reparse_point(&img, "/d"),
        None,
        "the driver's own reader"
    );
    assert_eq!(
        si_file_attributes(&img, rec) & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "the reparse bit is cleared"
    );
    assert_eq!(ntfs_crate_names(&img, "/d"), ["a", "b"]);
    clean(&img);
}

/// A reparse point too large for the room left in a directory's base
/// record, on a directory with no list, gives the directory a list and
/// goes to an extension record, while `$INDEX_ROOT` stays in the base
/// record; names added afterwards keep the list true.
#[test]
fn a_reparse_point_a_directory_record_cannot_hold_gets_a_list() {
    let (img, rec) = volume_with_a_directory("full");
    assert!(!has_list(&img, rec), "/d starts with a list");
    let path = format!("C:\\{}", "long-directory-name\\".repeat(10));
    let target = format!("\\??\\{path}");
    let data = build_symlink_reparse_data(&target, Some(&path), false);
    set_reparse_point(&img, "/d", &data);

    let records = listed_records_of(&img, rec, AttrType::ReparsePoint);
    assert_eq!(records.len(), 1, "the list names one $REPARSE_POINT");
    assert_ne!(
        records[0], rec,
        "the $REPARSE_POINT fit in the base record; the test no longer reaches the list path"
    );
    assert_eq!(
        listed_records_of(&img, rec, AttrType::IndexRoot),
        [rec],
        "$INDEX_ROOT left the base record"
    );
    assert_eq!(
        ntfs_crate_symlink(&img, "/d"),
        Some((reparse_tag::SYMLINK, target.clone())),
        "the ntfs crate"
    );
    assert_eq!(
        our_reparse_point(&img, "/d"),
        Some(write::ReparsePoint {
            reparse_tag: reparse_tag::SYMLINK,
            data,
        }),
        "the driver's own reader"
    );
    assert_eq!(ntfs_crate_names(&img, "/d"), ["a", "b"]);
    clean(&img);

    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::create_file_io(&mut io, "/d", "c").expect("create /d/c");
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert_eq!(ntfs_crate_names(&img, "/d"), ["a", "b", "c"]);
    assert_eq!(
        ntfs_crate_symlink(&img, "/d"),
        Some((reparse_tag::SYMLINK, target)),
        "the ntfs crate after adding a name"
    );
    listed_records(&img, rec);
    clean(&img);
}
