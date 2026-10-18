//! `check_io` reports an `$ATTRIBUTE_LIST` entry naming a record that is
//! not one of the file's extension records (#820).
//!
//! The checker read every in-use record's attributes and never the lists
//! that tie a file's records together, so a list naming a record that is
//! not in use, carries another sequence number, or belongs to another file
//! was reported clean. A reader following that list reads another file's
//! attributes, or none.
//!
//! Graded by Windows: `/many.bin` on `test-disks/ntfs-attrlist.img`, whose
//! list Windows wrote when its 96 streams outgrew the base record. Each
//! case breaks one fact of one extension record the list names, on a
//! disposable copy, and requires a `BadAttributeList` for `/many.bin`'s
//! base record; the untouched copy has none.

mod common;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::fsck::{self, CheckFinding};
use fs_ntfs::{mft_io, read, write};
use std::path::Path;

const FIXTURE: &str = "test-disks/ntfs-attrlist.img";
const SEQUENCE: usize = 0x10;
const FLAGS: usize = 0x16;
const BASE_REFERENCE: usize = 0x20;
const ROOT: u64 = 5;

/// One case: what it breaks, and the edit to the fixed-up extension record.
type Case = (&'static str, fn(&mut [u8]));

/// A disposable copy of the fixture, `/many.bin`'s base record, and one
/// extension record its list names.
fn copy_with_an_extension(tag: &str) -> (String, u64, u64) {
    assert!(
        Path::new(FIXTURE).exists(),
        "missing {FIXTURE}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let img = common::temp_image_path(format!("checklist_{tag}"));
    std::fs::copy(FIXTURE, &img).expect("copy the fixture");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let base = write::resolve_path_to_record_number_io(&mut io, "/many.bin").expect("/many.bin");
    let list = read::read_attribute_value(&mut io, base, AttrType::AttributeList, None)
        .expect("read the $ATTRIBUTE_LIST");
    let extension = read::parse_attribute_list(&list)
        .expect("parse the $ATTRIBUTE_LIST")
        .into_iter()
        .map(|e| e.record_number)
        .find(|&n| n != base)
        .expect("/many.bin has an extension record");
    (img, base, extension)
}

/// The records `check_io` reports a `BadAttributeList` for.
fn bad_lists(img: &str) -> Vec<u64> {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    fsck::check_io(&mut io)
        .expect("check_io")
        .findings
        .into_iter()
        .filter_map(|f| match f {
            CheckFinding::BadAttributeList { record, .. } => Some(record),
            _ => None,
        })
        .collect()
}

#[test]
fn the_list_windows_wrote_is_clean() {
    let (img, _, _) = copy_with_an_extension("clean");
    assert_eq!(bad_lists(&img), Vec::<u64>::new());
}

#[test]
fn an_entry_naming_a_record_that_is_not_the_files_extension_is_reported() {
    let cases: [Case; 3] = [
        ("an extension record whose base is another file", |r| {
            r[BASE_REFERENCE..BASE_REFERENCE + 8]
                .copy_from_slice(&(ROOT | (u64::from(ROOT as u16) << 48)).to_le_bytes());
        }),
        ("an extension record with another sequence number", |r| {
            let seq = u16::from_le_bytes([r[SEQUENCE], r[SEQUENCE + 1]]);
            r[SEQUENCE..SEQUENCE + 2].copy_from_slice(&seq.wrapping_add(1).to_le_bytes());
        }),
        ("an extension record whose header is not in use", |r| {
            let flags = u16::from_le_bytes([r[FLAGS], r[FLAGS + 1]]);
            r[FLAGS..FLAGS + 2].copy_from_slice(&(flags & !mft_io::MFT_FLAG_IN_USE).to_le_bytes());
        }),
    ];
    let mut missed = Vec::new();
    for (i, (what, edit)) in cases.iter().enumerate() {
        let (img, base, extension) = copy_with_an_extension(&format!("case{i}"));
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        mft_io::update_mft_record_io(&mut io, extension, |r| {
            edit(r);
            Ok(())
        })
        .expect("corrupt the extension record");
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
        drop(io);
        if !bad_lists(&img).contains(&base) {
            missed.push(*what);
        }
    }
    assert!(missed.is_empty(), "check_io reported clean: {missed:?}");
}
