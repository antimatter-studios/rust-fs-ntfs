//! Renaming a name of a file with an `$ATTRIBUTE_LIST` rewrites its
//! `$FILE_NAME` wherever it lives, and keeps the list entry true (#780,
//! part of #662).
//!
//! Rename found the `$FILE_NAME` by reading the base record alone. Windows
//! keeps names in extension records too (`test-disks/ntfs-attrlist.img`),
//! so a name held there was renamed in the directory and left as it was in
//! the file: the two disagreed, and the `ntfs` crate read the old name.
//!
//! Each test checks that the list names every attribute where it is, that
//! the `ntfs` crate -- a parser that is not this one -- reads every new
//! name through the list and none of the old, and that `check_io` is clean.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fsck, idx_block, index_io, mft_io, read, write};
use ntfs::indexes::NtfsFileNameIndex;
use ntfs::structured_values::{NtfsFileName, NtfsFileNamespace};
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
    let img = common::temp_image_path(format!("alrename_{tag}"));
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

fn clean(img: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let report = fsck::check_io(&mut io).expect("check the volume");
    assert!(
        report.findings.is_empty(),
        "check_io finds the volume unclean: {:?}",
        report.findings
    );
}

fn renamed(i: usize, long: bool) -> String {
    if long {
        format!("a-longer-name-{i:04}")
    } else {
        format!("name{i:04}")
    }
}

/// Rename every link of `/f` from `from(i)` to `to(i)`.
fn rename_links(img: &str, from: impl Fn(usize) -> String, to: impl Fn(usize) -> String) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    for i in 0..LINKS {
        let (old, new) = (from(i), to(i));
        write::rename_io(&mut io, &format!("/{old}"), &new)
            .unwrap_or_else(|e| panic!("rename /{old} to {new}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

fn expected(name: impl Fn(usize) -> String) -> Vec<String> {
    let mut names: Vec<String> = (0..LINKS).map(name).collect();
    names.push("f".to_string());
    names.sort();
    names
}

/// Every name, and the data, read back through each name by the `ntfs`
/// crate.
fn every_name_reads(img: &str, name: impl Fn(usize) -> String) {
    let (ntfs, mut reader) = common::open(img);
    for i in 0..LINKS {
        let path = format!("/{}", name(i));
        assert_eq!(
            common::read_file_all(&ntfs, &mut reader, &path),
            b"contents",
            "{path} does not read /f's data"
        );
    }
}

fn listed_file_with_names_in_extensions(tag: &str) -> (String, u64) {
    let (img, rec) = volume_with_a_file(tag, 8);
    assert!(
        has_list(&img, rec),
        "/f's base record never filled, so it has no $ATTRIBUTE_LIST to test"
    );
    add_links(&img, LINKS);
    assert!(
        names_in_extensions(&img, rec) > 0,
        "every name fitted in the base record, so no extension record holds one to rename"
    );
    (img, rec)
}

/// A rename to a name of the same length rewrites each `$FILE_NAME` in the
/// record that holds it, base or extension.
#[test]
fn same_length_renames_follow_the_list() {
    let (img, rec) = listed_file_with_names_in_extensions("same");
    rename_links(&img, link_name, |i| renamed(i, false));
    list_is_true(&img, rec);
    assert_eq!(ntfs_names(&img, rec), expected(|i| renamed(i, false)));
    every_name_reads(&img, |i| renamed(i, false));
    clean(&img);
}

/// A rename to a longer name rewrites each `$FILE_NAME` where it is, or
/// moves it through the list when its record no longer has room for it.
#[test]
fn longer_renames_follow_the_list() {
    let (img, rec) = listed_file_with_names_in_extensions("longer");
    rename_links(&img, link_name, |i| renamed(i, true));
    list_is_true(&img, rec);
    assert_eq!(ntfs_names(&img, rec), expected(|i| renamed(i, true)));
    every_name_reads(&img, |i| renamed(i, true));
    clean(&img);
}

/// A rename back to a shorter name rewrites each `$FILE_NAME` where it is.
#[test]
fn shorter_renames_follow_the_list() {
    let (img, rec) = listed_file_with_names_in_extensions("shorter");
    rename_links(&img, link_name, |i| renamed(i, true));
    rename_links(&img, |i| renamed(i, true), |i| renamed(i, false));
    list_is_true(&img, rec);
    assert_eq!(ntfs_names(&img, rec), expected(|i| renamed(i, false)));
    every_name_reads(&img, |i| renamed(i, false));
    clean(&img);
}

const WIN32: u8 = 1;
const DOS: u8 = 2;
const WIN32_NAME: &str = "long-name-0000";
const DOS_NAME: &str = "LONGNA~1";

/// The `$FILE_NAME` called `name` in `record`: the offset of its value.
fn file_name_value(record: &[u8], name: &str) -> Option<usize> {
    let wanted: Vec<u16> = name.encode_utf16().collect();
    attr_io::iter_attributes(record)
        .filter(|loc| loc.type_code == AttrType::FileName as u32)
        .find_map(|loc| {
            let value = loc.attr_offset + loc.resident_value_offset? as usize;
            let length = record[value + 0x40] as usize;
            let units: Vec<u16> = record[value + 0x42..value + 0x42 + length * 2]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            (units == wanted).then_some(value)
        })
}

/// Put `/name`'s `$FILE_NAME`, in whichever record holds it, and its
/// entry in the root's index into `namespace`. Neither this crate's link
/// nor create makes a WIN32 + DOS pair, so the pair Windows makes is
/// written over the two names a link made.
fn set_namespace(img: &str, rec: u64, name: &str, namespace: u8) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let mut numbers: Vec<u64> = read::parse_attribute_list(&list)
        .expect("parse the list")
        .iter()
        .map(|e| e.record_number)
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    let mut held = false;
    for n in numbers {
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a listed record");
        if file_name_value(&record, name).is_none() {
            continue;
        }
        mft_io::update_mft_record_io(&mut io, n, |record| {
            let value = file_name_value(record, name).expect("the name is still there");
            record[value + 0x41] = namespace;
            Ok(())
        })
        .expect("set the $FILE_NAME's namespace");
        held = true;
    }
    assert!(held, "no record of /f holds a $FILE_NAME called {name}");
    // The key's namespace is the byte after its name length, 0x41 into
    // the $FILE_NAME that follows the 16-byte entry header.
    const KEY_NAMESPACE: usize = 0x10 + 0x41;
    let (_, root) = mft_io::read_mft_record_io(&mut io, 5).expect("read the root");
    if let Some(entry) = index_io::find_index_entry(&root, name, None).expect("search the root") {
        mft_io::update_mft_record_io(&mut io, 5, |record| {
            record[entry.record_offset + KEY_NAMESPACE] = namespace;
            Ok(())
        })
        .expect("set the root entry's namespace");
    } else {
        let ia = idx_block::load_for_directory_io(&mut io, 5).expect("the root's INDX blocks");
        let mut found = false;
        for vcn in ia.allocated_block_vcns() {
            let block = idx_block::read_indx_block_io(&mut io, &ia, vcn).expect("read INDX");
            let Some(entry) =
                index_io::find_entry_in_indx_block(&block, name, None).expect("search INDX")
            else {
                continue;
            };
            idx_block::update_indx_block_io(&mut io, &ia, vcn, |block| {
                block[entry.record_offset + KEY_NAMESPACE] = namespace;
                Ok(())
            })
            .expect("set the INDX entry's namespace");
            found = true;
            break;
        }
        assert!(found, "the root has no index entry for {name}");
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

/// The namespace of the root's index entry for `name`, as the `ntfs` crate
/// decodes it.
fn index_namespace(img: &str, name: &str) -> u8 {
    let (ntfs, mut reader) = common::open(img);
    let root = ntfs.root_directory(&mut reader).expect("the root");
    let index = root.directory_index(&mut reader).expect("the root's index");
    let mut finder = index.finder();
    let entry = NtfsFileNameIndex::find(&mut finder, &ntfs, &mut reader, name)
        .unwrap_or_else(|| panic!("/{name} has no index entry"))
        .expect("read the index entry");
    match entry
        .key()
        .expect("a key")
        .expect("decode the key")
        .namespace()
    {
        NtfsFileNamespace::Posix => 0,
        NtfsFileNamespace::Win32 => 1,
        NtfsFileNamespace::Dos => 2,
        NtfsFileNamespace::Win32AndDos => 3,
    }
}

/// A rename to another length rebuilds the parent's index entry, and the
/// entry keeps the namespace the old name had wherever its `$FILE_NAME`
/// lives (#785). The WIN32 half of a WIN32 + DOS pair stays WIN32: its DOS
/// partner still names the file. The namespace was read from the base
/// record alone, so a name held in an extension record took the one
/// derived from the new name, POSIX, and the index disagreed with the
/// file.
#[test]
fn a_renamed_win32_name_in_an_extension_keeps_its_namespace() {
    let (img, rec) = listed_file_with_names_in_extensions("win32");
    add_links_named(&img, &[WIN32_NAME, DOS_NAME]);
    {
        let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
        let (_, base) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f");
        assert!(
            file_name_value(&base, WIN32_NAME).is_none(),
            "the base record holds {WIN32_NAME}, so no extension record does"
        );
    }
    set_namespace(&img, rec, WIN32_NAME, WIN32);
    set_namespace(&img, rec, DOS_NAME, DOS);
    assert_eq!(index_namespace(&img, WIN32_NAME), WIN32);

    let new = "long-name-00000";
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::rename_io(&mut io, &format!("/{WIN32_NAME}"), new)
            .unwrap_or_else(|e| panic!("rename /{WIN32_NAME} to {new}: {e}"));
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert_eq!(
        index_namespace(&img, new),
        WIN32,
        "the rebuilt index entry for {new} does not keep {WIN32_NAME}'s WIN32 namespace"
    );
    list_is_true(&img, rec);
    assert!(ntfs_names(&img, rec).contains(&new.to_string()));
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(
        common::read_file_all(&ntfs, &mut reader, &format!("/{new}")),
        b"contents"
    );
    clean(&img);
}

/// Link `/f` under each of `names`.
fn add_links_named(img: &str, names: &[&str]) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    for name in names {
        write::link_io(&mut io, "/f", "/", name).unwrap_or_else(|e| panic!("link /{name}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}
