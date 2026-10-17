//! Unlinking one of several names of a file that carries an
//! `$ATTRIBUTE_LIST` takes the name's `$FILE_NAME` out of whichever record
//! holds it, and its entry out of the list (#778, part of #662).
//!
//! This used to be refused ("unlink a hard link: the record carries an
//! $ATTRIBUTE_LIST"), because removing the attribute from the base record
//! alone would leave the list naming it. Windows keeps `$FILE_NAME`s in
//! extension records too (`test-disks/ntfs-attrlist.img`), so a name is
//! found through the list, not by reading the base record.
//!
//! Each test checks that the list names every attribute where it is, that
//! the `ntfs` crate -- a parser that is not this one -- reads exactly the
//! names left, through the list, and that `check_io` is clean.

mod common;

use fs_ntfs::attr_io::{self, attr_off, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fsck, mft_io, read, record_build, write};
use ntfs::structured_values::NtfsFileName;
use ntfs::NtfsAttributeType;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const RECORD_SIZE: u32 = 1024;
const CLUSTER: u32 = 4096;
const FILE_NAME_OFFSET: usize = 0x42;

/// `/f` with the names `f`, `g` and `h`, given enough named streams that
/// its base record filled and it gained an `$ATTRIBUTE_LIST`.
fn volume_with_a_listed_linked_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("alunlink_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, RECORD_SIZE, None, Some(1))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    write::link_io(&mut io, "/f", "/", "g").expect("link /g");
    write::link_io(&mut io, "/f", "/", "h").expect("link /h");
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

fn file_name_of(record: &[u8], loc: &attr_io::AttrLocation) -> String {
    let value = loc.attr_offset
        + u16::from_le_bytes(
            record[loc.attr_offset + attr_off::RESIDENT_VALUE_OFFSET
                ..loc.attr_offset + attr_off::RESIDENT_VALUE_OFFSET + 2]
                .try_into()
                .unwrap(),
        ) as usize;
    let len = record[value + 0x40] as usize;
    let units: Vec<u16> = (0..len)
        .map(|i| {
            let at = value + FILE_NAME_OFFSET + 2 * i;
            u16::from_le_bytes([record[at], record[at + 1]])
        })
        .collect();
    String::from_utf16(&units).expect("a UTF-16 name")
}

/// Move the `$FILE_NAME` called `name` out of `/f`'s base record into the
/// extension record that holds its streams, and point its list entry
/// there: the layout Windows writes.
fn move_name_to_extension(img: &str, rec: u64, name: &str) -> u64 {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("read the list");
    let ext = read::parse_attribute_list(&list)
        .expect("parse the list")
        .into_iter()
        .map(|e| e.record_number)
        .find(|&n| n != rec)
        .expect("the list names an extension record");
    let (_, base) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f");
    let loc = attr_io::iter_attributes(&base)
        .find(|l| l.type_code == AttrType::FileName as u32 && file_name_of(&base, l) == name)
        .unwrap_or_else(|| panic!("no $FILE_NAME {name:?} in the base record"));
    let mut attr = base[loc.attr_offset..loc.attr_offset + loc.attr_length].to_vec();
    let old_id = loc.attribute_id;
    let (_, ext_record) = mft_io::read_mft_record_io(&mut io, ext).expect("read the extension");
    let new_id = u16::from_le_bytes([ext_record[0x28], ext_record[0x29]]);
    let ext_seq = mft_io::record_sequence(&ext_record);
    attr[attr_off::ATTRIBUTE_ID..attr_off::ATTRIBUTE_ID + 2].copy_from_slice(&new_id.to_le_bytes());
    mft_io::update_mft_record_io(&mut io, ext, |record| {
        fs_ntfs::attr_resize::insert_attribute_sorted(record, &attr)?;
        record[0x28..0x2A].copy_from_slice(&(new_id + 1).to_le_bytes());
        Ok(())
    })
    .expect("put the name in the extension record");
    let base_seq = mft_io::record_sequence(&base);
    let base_ref = record_build::encode_file_reference(rec, base_seq);
    let ext_ref = record_build::encode_file_reference(ext, ext_seq);
    mft_io::update_mft_record_io(&mut io, rec, |record| {
        let used = u32::from_le_bytes(record[0x18..0x1C].try_into().unwrap()) as usize;
        record.copy_within(loc.attr_offset + loc.attr_length..used, loc.attr_offset);
        for b in &mut record[used - loc.attr_length..used] {
            *b = 0;
        }
        record[0x18..0x1C].copy_from_slice(&((used - loc.attr_length) as u32).to_le_bytes());
        let al = attr_io::find_attribute(record, AttrType::AttributeList, None)
            .ok_or("the list is not resident in the base record")?;
        let value = al.attr_offset
            + u16::from_le_bytes(
                record[al.attr_offset + attr_off::RESIDENT_VALUE_OFFSET
                    ..al.attr_offset + attr_off::RESIDENT_VALUE_OFFSET + 2]
                    .try_into()
                    .unwrap(),
            ) as usize;
        let value_len = u32::from_le_bytes(
            record[al.attr_offset + attr_off::RESIDENT_VALUE_LENGTH
                ..al.attr_offset + attr_off::RESIDENT_VALUE_LENGTH + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let mut at = value;
        while at < value + value_len {
            let ty = u32::from_le_bytes(record[at..at + 4].try_into().unwrap());
            let len = u16::from_le_bytes([record[at + 4], record[at + 5]]) as usize;
            let reference = u64::from_le_bytes(record[at + 16..at + 24].try_into().unwrap());
            let id = u16::from_le_bytes([record[at + 24], record[at + 25]]);
            if ty == AttrType::FileName as u32 && reference == base_ref && id == old_id {
                record[at + 16..at + 24].copy_from_slice(&ext_ref.to_le_bytes());
                record[at + 24..at + 26].copy_from_slice(&new_id.to_le_bytes());
                return Ok(());
            }
            at += len;
        }
        Err("the list has no entry for the name".into())
    })
    .expect("move the name's list entry");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    ext
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

/// A name in the base record goes, with its list entry; the others stay.
#[test]
fn unlinking_a_name_in_the_base_record_takes_its_entry_out() {
    let (img, rec) = volume_with_a_listed_linked_file("base");
    list_is_true(&img, rec);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::unlink_io(&mut io, "/g").expect("unlink /g");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    list_is_true(&img, rec);
    assert_eq!(ntfs_names(&img, rec), ["f", "h"]);
    assert_eq!(hard_link_count(&img, rec), 2);
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/h"), b"contents");
    clean(&img);
}

/// A name Windows-style in an extension record goes from there, with its
/// list entry, and then the base record's names go down to the last.
#[test]
fn unlinking_a_name_in_an_extension_record_takes_it_from_there() {
    let (img, rec) = volume_with_a_listed_linked_file("ext");
    let ext = move_name_to_extension(&img, rec, "h");
    list_is_true(&img, rec);
    assert_eq!(ntfs_names(&img, rec), ["f", "g", "h"]);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::unlink_io(&mut io, "/h").expect("unlink /h");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let (_, ext_record) = mft_io::read_mft_record_io(&mut io, ext).expect("read the extension");
    assert!(
        !attr_io::iter_attributes(&ext_record).any(|l| l.type_code == AttrType::FileName as u32),
        "the extension record still holds a $FILE_NAME"
    );
    drop(io);
    list_is_true(&img, rec);
    assert_eq!(ntfs_names(&img, rec), ["f", "g"]);
    assert_eq!(hard_link_count(&img, rec), 2);
    clean(&img);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::unlink_io(&mut io, "/g").expect("unlink /g");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    list_is_true(&img, rec);
    assert_eq!(ntfs_names(&img, rec), ["f"]);
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/f"), b"contents");
    clean(&img);
}
