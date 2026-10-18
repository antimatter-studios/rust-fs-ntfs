//! A reparse point set on a file whose base record has no room for it, or
//! on a file that has an `$ATTRIBUTE_LIST`, goes through the list, and
//! removing it takes its entry out (#808, part of #665 and #480).
//!
//! `write_reparse_point_io` edited the base record alone, so a file with a
//! list refused the reparse point ("the record carries an
//! $ATTRIBUTE_LIST"), where Windows moves attributes to extension records
//! and lists where each went. Here the `ntfs` crate, a parser that is not
//! this one, finds the `$REPARSE_POINT` through the list and its symlink
//! target is decoded from the `REPARSE_DATA_BUFFER` layout Windows
//! documents; `check_io` reports the volume clean, and the list is checked
//! to name the attribute where it is, and no longer once it is removed.

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

/// A fresh volume with 1 KiB records holding `/f`, with eight 64-byte
/// named streams: more than the base record holds, so `/f` has an
/// `$ATTRIBUTE_LIST` and an extension record before any reparse point is
/// set. Returns the image and `/f`'s record.
fn volume_with_a_listed_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("alrp_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    for i in 1..=8 {
        write::write_named_stream_io(&mut io, "/f", &format!("s{i}"), &[b'0' + i; 64])
            .unwrap_or_else(|e| panic!("write /f:s{i}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f");
    assert!(
        attr_io::find_attribute(&record, AttrType::AttributeList, None).is_some(),
        "eight streams left /f without a list; the test no longer starts from one"
    );
    (img, rec)
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
/// finds on `path`, following the file's `$ATTRIBUTE_LIST`, decoded from
/// the `REPARSE_DATA_BUFFER` of a symbolic link as Windows documents it;
/// `None` when it finds no `$REPARSE_POINT`.
fn ntfs_crate_symlink(img: &str, path: &str) -> Option<(u32, String)> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, path);
    let mut attrs = file.attributes();
    while let Some(item) = attrs.next(&mut reader) {
        let item = item.expect("an attribute of the file");
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

/// The records the list of the file at `rec` puts `$REPARSE_POINT` in,
/// having checked that each entry names an in-use record of this file
/// holding an attribute of that type and id.
fn listed_reparse_records(img: &str, rec: u64) -> Vec<u64> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let mut out = Vec::new();
    for e in read::parse_attribute_list(&list).expect("parse the list") {
        if e.type_code != AttrType::ReparsePoint as u32 {
            continue;
        }
        let n = e.record_number;
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a listed record");
        assert_ne!(
            mft_io::record_flags(&record) & mft_io::MFT_FLAG_IN_USE,
            0,
            "the list puts the $REPARSE_POINT in record {n}, which is not in use"
        );
        assert!(
            attr_io::iter_attributes(&record)
                .any(|l| l.type_code == e.type_code && l.attribute_id == e.attribute_id),
            "the list puts the $REPARSE_POINT id {} in record {n}, and it is not there",
            e.attribute_id
        );
        out.push(n);
    }
    out
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

/// A file that has a list takes a symlink reparse point, and loses it: the
/// list gains and loses its entry, the reparse bit follows, and the volume
/// stays clean throughout.
#[test]
fn a_reparse_point_on_a_file_with_a_list_is_set_and_removed_through_it() {
    let (img, rec) = volume_with_a_listed_file("set");
    let target = "\\??\\C:\\target\\of\\the\\link";
    let data = build_symlink_reparse_data(target, Some("C:\\target\\of\\the\\link"), false);
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::write_reparse_point_io(&mut io, "/f", reparse_tag::SYMLINK, &data)
            .unwrap_or_else(|e| panic!("set a reparse point on /f: {e}"));
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert_eq!(
        listed_reparse_records(&img, rec).len(),
        1,
        "the list names one $REPARSE_POINT"
    );
    assert_eq!(
        ntfs_crate_symlink(&img, "/f"),
        Some((reparse_tag::SYMLINK, target.to_string())),
        "the ntfs crate"
    );
    assert_ne!(
        si_file_attributes(&img, rec) & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "the reparse bit is set"
    );
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/f"), b"contents");
    clean(&img);

    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::remove_reparse_point_io(&mut io, "/f")
            .unwrap_or_else(|e| panic!("remove the reparse point of /f: {e}"));
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert!(
        listed_reparse_records(&img, rec).is_empty(),
        "the list still names a $REPARSE_POINT"
    );
    assert_eq!(ntfs_crate_symlink(&img, "/f"), None, "the ntfs crate");
    assert_eq!(
        si_file_attributes(&img, rec) & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "the reparse bit is cleared"
    );
    clean(&img);
}

/// A reparse point too large for the room left in the base record goes to
/// an extension record, and the driver's own reader finds it there through
/// the list, with the tag and bytes written, as the `ntfs` crate does
/// (#812). `read_reparse_point_io` read the base record alone and reported
/// no reparse point at all.
#[test]
fn a_reparse_point_in_an_extension_record_is_read_back_through_the_list() {
    let (img, rec) = volume_with_a_listed_file("ext");
    let path = format!("C:\\{}", "long-directory-name\\".repeat(8));
    let target = format!("\\??\\{path}");
    let data = build_symlink_reparse_data(&target, Some(&path), false);
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::write_reparse_point_io(&mut io, "/f", reparse_tag::SYMLINK, &data)
            .unwrap_or_else(|e| panic!("set a reparse point on /f: {e}"));
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    let records = listed_reparse_records(&img, rec);
    assert_eq!(records.len(), 1, "the list names one $REPARSE_POINT");
    assert_ne!(
        records[0], rec,
        "the $REPARSE_POINT fit in the base record; the test no longer reads one from an extension"
    );
    assert_eq!(
        ntfs_crate_symlink(&img, "/f"),
        Some((reparse_tag::SYMLINK, target.clone())),
        "the ntfs crate"
    );
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    assert_eq!(
        write::read_reparse_point_io(&mut io, "/f").expect("read the reparse point of /f"),
        Some(write::ReparsePoint {
            reparse_tag: reparse_tag::SYMLINK,
            data,
        }),
        "the driver's own reader"
    );
    drop(io);
    clean(&img);
}
