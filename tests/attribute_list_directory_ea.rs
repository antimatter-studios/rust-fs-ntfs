//! An EA a directory's base record cannot hold goes through an
//! `$ATTRIBUTE_LIST`, as a regular file's does (#814, part of #666).
//!
//! `ListedFile::open` refused every directory ("whose attributes this
//! driver does not move between records"), and `write_ea_io` kept a
//! directory on the base-record path, so an EA the record had no room for
//! was refused with "exceeds record capacity". The list path moves only
//! named streams and EAs, so a directory's `$INDEX_ROOT` and `$FILE_NAME`
//! stay in the base record, where the index edits find them; adding and
//! removing names afterwards resizes `$INDEX_ROOT` in place and leaves the
//! list true.
//!
//! The `ntfs` crate, a parser that is not this one, follows the list to
//! the `$EA` and lists the directory's entries; `check_io` reports the
//! volume clean; and every list entry is checked to name an attribute
//! that is where the entry says.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::ea_io::{self, Ea};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fsck, mft_io, read, write};
use ntfs::{NtfsAttributeType, NtfsReadSeek};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

/// A fresh volume with 1 KiB records holding the directory `/d` and the
/// files `/d/a` and `/d/b`. Returns the image and `/d`'s record.
fn volume_with_a_directory(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("aldirea_{tag}"));
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

fn write_ea(img: &str, path: &str, name: &[u8], value: &[u8]) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    write::write_ea_io(&mut io, path, name, value, 0)
        .unwrap_or_else(|e| panic!("write EA {} on {path}: {e}", String::from_utf8_lossy(name)));
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

fn remove_ea(img: &str, path: &str, name: &[u8]) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    write::remove_ea_io(&mut io, path, name).unwrap_or_else(|e| {
        panic!(
            "remove EA {} from {path}: {e}",
            String::from_utf8_lossy(name)
        )
    });
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

/// `(name, value)` of each EA `list_eas_io` reports on `path`.
fn our_eas(img: &str, path: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    write::list_eas_io(&mut io, path)
        .unwrap_or_else(|e| panic!("list the EAs of {path}: {e}"))
        .into_iter()
        .map(|ea| (ea.name, ea.value))
        .collect()
}

/// `(name, value)` of each EA in the `$EA` the `ntfs` crate finds on
/// `path`, following its `$ATTRIBUTE_LIST`; empty when it finds no `$EA`.
fn ntfs_crate_eas(img: &str, path: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, path);
    let mut attrs = file.attributes();
    while let Some(item) = attrs.next(&mut reader) {
        let item = item.expect("an attribute of the directory");
        let attr = item.to_attribute().expect("to_attribute");
        if attr.ty().expect("attribute type") != NtfsAttributeType::EA {
            continue;
        }
        let mut value = attr.value(&mut reader).expect("the $EA's value");
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = value.read(&mut reader, &mut chunk).expect("read the $EA");
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        return ea_io::decode(&bytes)
            .expect("decode the $EA the ntfs crate read")
            .into_iter()
            .map(|ea: Ea| (ea.name, ea.value))
            .collect();
    }
    Vec::new()
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

/// The record the list puts the attribute of type `ty` in.
fn listed_record_of(records: &[(u32, u64)], ty: AttrType) -> u64 {
    records
        .iter()
        .find(|(t, _)| *t == ty as u32)
        .map(|&(_, n)| n)
        .unwrap_or_else(|| panic!("the list names no {ty:?}: {records:?}"))
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

fn value(len: usize) -> Vec<u8> {
    b"ea value ".iter().copied().cycle().take(len).collect()
}

/// An 800-byte EA does not fit in `/d`'s base record beside its index:
/// the directory is given a list, `$EA` goes to an extension record, and
/// `$INDEX_ROOT` and `$FILE_NAME` stay in the base record. Names added
/// and removed afterwards keep the list true, and the `ntfs` crate lists
/// every one.
#[test]
fn an_ea_a_directory_record_cannot_hold_gets_a_list_and_index_edits_keep_it() {
    let (img, rec) = volume_with_a_directory("first");
    write_ea(&img, "/d", b"USER.BIG", &value(800));

    let records = listed_records(&img, rec);
    assert_ne!(
        listed_record_of(&records, AttrType::ExtendedAttribute),
        rec,
        "the $EA fitted in the base record; the test no longer reaches the list path"
    );
    assert_eq!(
        listed_record_of(&records, AttrType::IndexRoot),
        rec,
        "$INDEX_ROOT left the base record"
    );
    assert_eq!(
        listed_record_of(&records, AttrType::FileName),
        rec,
        "$FILE_NAME left the base record"
    );
    let want = vec![(b"USER.BIG".to_vec(), value(800))];
    assert_eq!(our_eas(&img, "/d"), want, "list_eas_io");
    assert_eq!(ntfs_crate_eas(&img, "/d"), want, "the ntfs crate");
    assert_eq!(ntfs_crate_names(&img, "/d"), ["a", "b"]);
    clean(&img);

    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::create_file_io(&mut io, "/d", "c").expect("create /d/c");
        write::mkdir_io(&mut io, "/d", "e").expect("mkdir /d/e");
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert_eq!(
        listed_records(&img, rec),
        records,
        "adding names moved an attribute"
    );
    assert_eq!(ntfs_crate_names(&img, "/d"), ["a", "b", "c", "e"]);
    assert_eq!(
        ntfs_crate_eas(&img, "/d"),
        want,
        "the ntfs crate after adding names"
    );
    clean(&img);

    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::unlink_io(&mut io, "/d/a").expect("unlink /d/a");
        write::rmdir_io(&mut io, "/d/e").expect("rmdir /d/e");
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert_eq!(
        listed_records(&img, rec),
        records,
        "removing names moved an attribute"
    );
    assert_eq!(ntfs_crate_names(&img, "/d"), ["b", "c"]);
    assert_eq!(
        ntfs_crate_eas(&img, "/d"),
        want,
        "the ntfs crate after removing names"
    );
    clean(&img);
}

/// Removing a directory that has a list frees its extension records with
/// its base record: none is left in use, owned by a record that is not.
#[test]
fn rmdir_of_a_listed_directory_frees_its_extension_records() {
    let (img, rec) = volume_with_a_directory("rmdir");
    write_ea(&img, "/d", b"USER.BIG", &value(800));
    let extensions: Vec<u64> = listed_records(&img, rec)
        .into_iter()
        .map(|(_, n)| n)
        .filter(|&n| n != rec)
        .collect();
    assert!(
        !extensions.is_empty(),
        "the $EA fitted in the base record; the test no longer reaches the list path"
    );
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::unlink_io(&mut io, "/d/a").expect("unlink /d/a");
        write::unlink_io(&mut io, "/d/b").expect("unlink /d/b");
        write::rmdir_io(&mut io, "/d").expect("rmdir /d");
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    for n in extensions {
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a freed record");
        assert_eq!(
            mft_io::record_flags(&record) & mft_io::MFT_FLAG_IN_USE,
            0,
            "extension record {n} of the removed directory is still in use"
        );
    }
    drop(io);
    assert!(
        !ntfs_crate_names(&img, "/").contains(&"d".to_string()),
        "the ntfs crate still lists /d"
    );
    clean(&img);
}

/// A directory that has a list takes a second EA, a rewrite of the first,
/// and the removal of both: the list loses the `$EA` and
/// `$EA_INFORMATION` entries, and the directory's index is untouched.
#[test]
fn eas_of_a_listed_directory_are_rewritten_and_removed_through_the_list() {
    let (img, rec) = volume_with_a_directory("listed");
    write_ea(&img, "/d", b"USER.BIG", &value(800));
    write_ea(&img, "/d", b"USER.SMALL", b"two");
    write_ea(&img, "/d", b"USER.BIG", &value(600));
    let want = vec![
        (b"USER.BIG".to_vec(), value(600)),
        (b"USER.SMALL".to_vec(), b"two".to_vec()),
    ];
    assert_eq!(our_eas(&img, "/d"), want, "list_eas_io after the rewrite");
    assert_eq!(
        ntfs_crate_eas(&img, "/d"),
        want,
        "the ntfs crate after the rewrite"
    );
    listed_records(&img, rec);
    clean(&img);

    remove_ea(&img, "/d", b"USER.BIG");
    remove_ea(&img, "/d", b"USER.SMALL");
    assert!(
        our_eas(&img, "/d").is_empty(),
        "EAs left after removing both"
    );
    assert!(
        ntfs_crate_eas(&img, "/d").is_empty(),
        "the ntfs crate finds an $EA"
    );
    let records = listed_records(&img, rec);
    assert!(
        !records
            .iter()
            .any(|(ty, _)| *ty == AttrType::ExtendedAttribute as u32
                || *ty == AttrType::ExtendedAttributeInformation as u32),
        "the list still names an $EA or $EA_INFORMATION: {records:?}"
    );
    assert_eq!(ntfs_crate_names(&img, "/d"), ["a", "b"]);
    clean(&img);
}
