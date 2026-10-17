//! A hard link whose `$FILE_NAME` the base record cannot hold is added
//! through the file's `$ATTRIBUTE_LIST`, which is made if the file has
//! none (#779, part of #662).
//!
//! This used to be refused: the new `$FILE_NAME` went into the base record
//! alone, so a full base record, or one carrying a list, ended the link.
//! Windows keeps `$FILE_NAME`s in extension records too
//! (`test-disks/ntfs-attrlist.img`), so a name the base record has no room
//! for goes to an extension record, with its entry in the list.
//!
//! Each test checks that the list names every attribute where it is, that
//! the `ntfs` crate -- a parser that is not this one -- reads every name,
//! through the list, and that `check_io` is clean.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fsck, mft_io, read, write};
use ntfs::structured_values::NtfsFileName;
use ntfs::NtfsAttributeType;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const RECORD_SIZE: u32 = 1024;
const CLUSTER: u32 = 4096;
/// More names than a 1 KiB base record holds beside `/f`'s other
/// attributes: each `$FILE_NAME` of an eight-character name takes 112
/// bytes.
const LINKS: usize = 14;

/// `/f`, with `streams` named streams of 120 bytes each.
fn volume_with_a_file(tag: &str, streams: usize) -> (String, u64) {
    let img = common::temp_image_path(format!("allink_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, RECORD_SIZE, None, Some(1))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    for i in 0..streams {
        write::write_named_stream_io(&mut io, "/f", &format!("s{i}"), &[b'a' + i as u8; 120])
            .unwrap_or_else(|e| panic!("write /f:s{i}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, rec)
}

fn has_list(img: &str, rec: u64) -> bool {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    read::read_attribute_value_if_present(&mut io, rec, AttrType::AttributeList, None)
        .expect("read /f's list")
        .is_some()
}

fn link_name(i: usize) -> String {
    format!("link{i:04}")
}

/// Link `/f` as `/link0000` .. up to `count`.
fn add_links(img: &str, count: usize) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    for i in 0..count {
        let name = link_name(i);
        write::link_io(&mut io, "/f", "/", &name).unwrap_or_else(|e| panic!("link /{name}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
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

/// How many `$FILE_NAME`s the list places outside the base record.
fn names_in_extensions(img: &str, rec: u64) -> usize {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    read::parse_attribute_list(&list)
        .expect("parse the list")
        .iter()
        .filter(|e| e.type_code == AttrType::FileName as u32 && e.record_number != rec)
        .count()
}

/// The names the `ntfs` crate finds in `/f`'s `$FILE_NAME` attributes,
/// read through its list, sorted.
fn ntfs_names(img: &str, rec: u64) -> Vec<String> {
    let (ntfs, mut reader) = common::open(img);
    let file = ntfs
        .file(&mut reader, rec)
        .expect("the ntfs crate opens /f");
    let mut attributes = file.attributes();
    let mut names = Vec::new();
    while let Some(item) = attributes.next(&mut reader) {
        let item = item.expect("the ntfs crate reads /f's attributes");
        let attr = item.to_attribute().expect("to_attribute");
        if attr.ty().expect("attribute type") == NtfsAttributeType::FileName {
            let name = attr
                .structured_value::<_, NtfsFileName>(&mut reader)
                .expect("the ntfs crate decodes a $FILE_NAME");
            names.push(name.name().to_string_lossy());
        }
    }
    names.sort();
    names
}

fn expected_names(count: usize) -> Vec<String> {
    let mut names: Vec<String> = (0..count).map(link_name).collect();
    names.push("f".to_string());
    names.sort();
    names
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

fn hard_link_count(img: &str, rec: u64) -> u16 {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f");
    u16::from_le_bytes([record[0x12], record[0x13]])
}

/// Every name, and the data, read back through each name by the `ntfs`
/// crate.
fn every_name_reads(img: &str, count: usize) {
    let (ntfs, mut reader) = common::open(img);
    for i in 0..count {
        let path = format!("/{}", link_name(i));
        assert_eq!(
            common::read_file_all(&ntfs, &mut reader, &path),
            b"contents",
            "{path} does not read /f's data"
        );
    }
}

/// A file whose streams already gave it a list takes its new names
/// through the list, into extension records once the base record is full.
#[test]
fn links_to_a_file_with_a_list_go_through_the_list() {
    let (img, rec) = volume_with_a_file("listed", 8);
    assert!(
        has_list(&img, rec),
        "/f's base record never filled, so it has no $ATTRIBUTE_LIST to test"
    );
    add_links(&img, LINKS);
    list_is_true(&img, rec);
    assert!(
        names_in_extensions(&img, rec) > 0,
        "every name fitted in the base record, so none went through the list"
    );
    assert_eq!(ntfs_names(&img, rec), expected_names(LINKS));
    assert_eq!(hard_link_count(&img, rec) as usize, LINKS + 1);
    every_name_reads(&img, LINKS);
    clean(&img);
}

/// A file with no list whose base record its names fill is given one, and
/// the names that do not fit go through it.
#[test]
fn links_that_fill_the_base_record_give_the_file_a_list() {
    let (img, rec) = volume_with_a_file("unlisted", 2);
    assert!(
        !has_list(&img, rec),
        "/f already has an $ATTRIBUTE_LIST, so its making is not tested"
    );
    add_links(&img, LINKS);
    assert!(
        has_list(&img, rec),
        "/f's names never filled its base record"
    );
    list_is_true(&img, rec);
    assert_eq!(ntfs_names(&img, rec), expected_names(LINKS));
    assert_eq!(hard_link_count(&img, rec) as usize, LINKS + 1);
    every_name_reads(&img, LINKS);
    clean(&img);
}
