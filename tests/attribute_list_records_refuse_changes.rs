//! A record that carries an `$ATTRIBUTE_LIST` gets no attribute added or
//! removed that the list would not follow (#468), and its named streams
//! are added and removed through the list (#480).
//!
//! The list names every attribute the file has and where it lives. The
//! write paths treated the base record as the whole file: a new stream
//! went into the base record with no list entry, so ntfs-3g could not find
//! it and the file was internally inconsistent, while `Ok` was returned.
//! Named streams are now written through the list, which gains or loses
//! each one's entry. Every other attribute added to or removed from such a
//! record -- an EA here -- is still refused with `Kind::Refused`, before
//! anything is written. Rewriting an attribute in place, which leaves the
//! list as true as it was, still works.

mod common;

use fs_ntfs::attr_io::{self, attr_off, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::error::{Error, Kind};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{mft_io, read, record_build, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn refused<V: std::fmt::Debug>(what: &str, got: Result<V, Error>) {
    match got {
        Err(e) if e.kind() == Kind::Refused => {}
        other => panic!("{what}: expected a refusal (EPERM), got {other:?}"),
    }
}

fn unchanged(img: &str, before: &[u8]) {
    let after = std::fs::read(img).expect("read image");
    let first = before.iter().zip(&after).position(|(a, b)| a != b);
    assert!(
        first.is_none(),
        "a refused write changed the image, first at byte {first:?}"
    );
}

/// A file whose base record has gained an `$ATTRIBUTE_LIST` naming each of
/// its attributes where they are: in the base record. The list is true, so
/// the file is sound, and any attribute added or removed would make it
/// false.
fn volume_with_a_listed_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("allist_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    write::write_named_stream_io(&mut io, "/f", "old", b"stream").expect("write /f:old");
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f");
    let seq = u16::from_le_bytes([record[0x10], record[0x11]]);
    let base_ref = record_build::encode_file_reference(rec, seq);
    let mut list = Vec::new();
    for loc in attr_io::iter_attributes(&record) {
        let name: Vec<u16> = attr_io::decode_attr_name(&record, &loc)
            .encode_utf16()
            .collect();
        let len = record_build::align8(0x1A + name.len() * 2);
        let mut e = vec![0u8; len];
        e[0..4].copy_from_slice(&loc.type_code.to_le_bytes());
        e[4..6].copy_from_slice(&(len as u16).to_le_bytes());
        e[6] = name.len() as u8;
        e[7] = 0x1A;
        e[16..24].copy_from_slice(&base_ref.to_le_bytes());
        e[24..26].copy_from_slice(&loc.attribute_id.to_le_bytes());
        for (i, c) in name.iter().enumerate() {
            e[0x1A + 2 * i..0x1A + 2 * i + 2].copy_from_slice(&c.to_le_bytes());
        }
        list.extend(e);
    }
    let header = 0x18;
    let len = record_build::align8(header + list.len());
    let mut al = vec![0u8; len];
    al[0..4].copy_from_slice(&(AttrType::AttributeList as u32).to_le_bytes());
    al[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    al[attr_off::ATTRIBUTE_ID..attr_off::ATTRIBUTE_ID + 2].copy_from_slice(&0x40u16.to_le_bytes());
    al[attr_off::RESIDENT_VALUE_LENGTH..attr_off::RESIDENT_VALUE_LENGTH + 4]
        .copy_from_slice(&(list.len() as u32).to_le_bytes());
    al[attr_off::RESIDENT_VALUE_OFFSET..attr_off::RESIDENT_VALUE_OFFSET + 2]
        .copy_from_slice(&(header as u16).to_le_bytes());
    al[header..header + list.len()].copy_from_slice(&list);
    mft_io::update_mft_record_io(&mut io, rec, |record| {
        fs_ntfs::attr_resize::insert_attribute_sorted(record, &al)
    })
    .expect("give /f an $ATTRIBUTE_LIST");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, rec)
}

/// An attribute the list does not follow -- an EA -- is refused, and the
/// image is byte-for-byte what it was.
#[test]
fn adding_an_attribute_the_list_does_not_follow_is_refused() {
    let (img, _) = volume_with_a_listed_file("refused");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let before = std::fs::read(&img).expect("read image");
    refused(
        "write an EA",
        write::write_ea_io(&mut io, "/f", b"USER.KEY", b"value", 0),
    );
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    unchanged(&img, &before);
}

/// The names of the named streams the list of the file at `rec` has
/// entries for, sorted.
fn listed_streams<T: BlockIo + ?Sized>(io: &mut T, rec: u64) -> Vec<String> {
    let list = read::read_attribute_value(io, rec, AttrType::AttributeList, None)
        .expect("read the $ATTRIBUTE_LIST");
    let mut names: Vec<String> = read::parse_attribute_list(&list)
        .expect("parse the $ATTRIBUTE_LIST")
        .into_iter()
        .filter(|e| e.type_code == AttrType::Data as u32)
        .filter_map(|e| e.name)
        .collect();
    names.sort();
    names
}

/// A stream added, resident or not, gains an entry, and one removed loses
/// it: the list goes on naming exactly the streams the file has (#480).
#[test]
fn adding_or_removing_a_stream_goes_through_the_list() {
    let (img, rec) = volume_with_a_listed_file("streams");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::write_named_stream_io(&mut io, "/f", "added", b"x").expect("write a new stream");
    let big = vec![1u8; 64 * 1024];
    write::write_named_stream_io(&mut io, "/f", "big", &big)
        .expect("write a new non-resident stream");
    write::delete_named_stream_io(&mut io, "/f", "old").expect("delete a stream");
    assert_eq!(listed_streams(&mut io, rec), ["added", "big"]);
    assert_eq!(
        read::read_attribute_value_if_present(&mut io, rec, AttrType::Data, Some("big"))
            .expect("read /f:big")
            .as_deref(),
        Some(&big[..])
    );
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

/// Rewriting an existing stream in place leaves the list true, and still
/// works; the ntfs crate, a parser that is not this one, reads it back.
#[test]
fn rewriting_an_attribute_in_place_still_works() {
    let (img, rec) = volume_with_a_listed_file("inplace");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::write_named_stream_io(&mut io, "/f", "old", b"STREAM").expect("rewrite /f:old");
    let got = read::read_attribute_value_if_present(&mut io, rec, AttrType::Data, Some("old"))
        .expect("read /f:old");
    assert_eq!(got.as_deref(), Some(&b"STREAM"[..]));
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/f"), b"contents");
}

/// Graded by Windows: `/many.bin` on `test-disks/ntfs-attrlist.img`, whose
/// list Windows wrote when its 96 streams outgrew the base record. A
/// stream added there and one deleted are entered in and taken out of
/// Windows' own list, and every other stream still reads back.
#[test]
fn a_file_windows_spread_over_extension_records_takes_a_new_stream() {
    const FIXTURE: &str = "test-disks/ntfs-attrlist.img";
    assert!(
        Path::new(FIXTURE).exists(),
        "missing {FIXTURE}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let img = common::temp_image_path("allist_windows");
    std::fs::copy(FIXTURE, &img).expect("copy the fixture");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let rec = write::resolve_path_to_record_number_io(&mut io, "/many.bin").expect("/many.bin");
    let before: Vec<Vec<u8>> = (0..95)
        .map(|i| {
            read::read_attribute_value(
                &mut io,
                rec,
                AttrType::Data,
                Some(&format!("stream-{i:03}")),
            )
            .expect("read a stream Windows wrote")
        })
        .collect();
    write::write_named_stream_io(&mut io, "/many.bin", "added", b"x").expect("write a new stream");
    write::delete_named_stream_io(&mut io, "/many.bin", "stream-095").expect("delete a stream");
    let mut want: Vec<String> = (0..95).map(|i| format!("stream-{i:03}")).collect();
    want.push("added".into());
    want.sort();
    assert_eq!(listed_streams(&mut io, rec), want);
    for (i, old) in before.iter().enumerate() {
        let name = format!("stream-{i:03}");
        assert_eq!(
            &read::read_attribute_value(&mut io, rec, AttrType::Data, Some(&name))
                .expect("read a stream Windows wrote"),
            old,
            "{name} changed"
        );
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}
