//! A whole rewrite of the unnamed `$DATA` of a file that carries an
//! `$ATTRIBUTE_LIST` goes through the list (#788, part of #663).
//!
//! The unnamed stream used to be written in the base record alone. A file
//! Windows made with its unnamed `$DATA` in an extension record
//! (`test-disks/ntfs-attrlist.img` keeps every attribute but
//! `$STANDARD_INFORMATION` and the list out of the base record) was
//! refused with "unnamed $DATA attribute not found", because the base
//! record does not hold it; the list says where it is.
//!
//! Each test grows the stream while it stays resident, promotes it into
//! clusters, and shrinks it again, checking after every write that the
//! list names every attribute where it is, that the `ntfs` crate -- a
//! parser that is not this one -- reads the contents back through the
//! list, and that `check_io` is clean.

mod common;

use fs_ntfs::attr_io::{self, attr_off, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fsck, mft_io, read, record_build, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const RECORD_SIZE: u32 = 1024;
const CLUSTER: u32 = 4096;

/// `/f`, holding `contents`, with an EA and enough named streams that its
/// base record filled and it gained an `$ATTRIBUTE_LIST`.
fn volume_with_a_listed_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("aldata_{tag}"));
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

/// Move `/f`'s unnamed `$DATA` out of its base record into the extension
/// record that holds its streams, and point its list entry there: the
/// layout Windows writes.
fn move_unnamed_data_to_extension(img: &str, rec: u64) -> u64 {
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
    let loc = attr_io::find_attribute(&base, AttrType::Data, None)
        .expect("the base record holds the unnamed $DATA");
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
    .expect("put the unnamed $DATA in the extension record");
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
            let name_len = record[at + 6];
            let reference = u64::from_le_bytes(record[at + 16..at + 24].try_into().unwrap());
            let id = u16::from_le_bytes([record[at + 24], record[at + 25]]);
            if ty == AttrType::Data as u32 && name_len == 0 && reference == base_ref && id == old_id
            {
                record[at + 16..at + 24].copy_from_slice(&ext_ref.to_le_bytes());
                record[at + 24..at + 26].copy_from_slice(&new_id.to_le_bytes());
                return Ok(());
            }
            at += len;
        }
        Err("the list has no entry for the unnamed $DATA".into())
    })
    .expect("move the unnamed $DATA's list entry");
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
    let unnamed = entries
        .iter()
        .filter(|e| e.type_code == AttrType::Data as u32 && e.name.is_none())
        .count();
    assert_eq!(
        unnamed, 1,
        "the list names {unnamed} unnamed $DATA attributes"
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

/// Write `data` as the whole of `/f`, then check the list, the contents
/// the `ntfs` crate reads, the streams beside it and the volume.
fn rewrite_and_check(img: &str, rec: u64, data: &[u8]) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let written = write::write_file_contents_io(&mut io, "/f", data)
        .unwrap_or_else(|e| panic!("write {} bytes to /f: {e}", data.len()));
    assert_eq!(written, data.len() as u64);
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    list_is_true(img, rec);
    let (ntfs, mut reader) = common::open(img);
    let read_back = common::read_file_all(&ntfs, &mut reader, "/f");
    assert!(
        read_back == data,
        "the ntfs crate reads {} bytes of /f, not the {} written",
        read_back.len(),
        data.len()
    );
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    assert_eq!(
        read::read_attribute_value(&mut io, rec, AttrType::Data, Some("s3")).expect("read /f:s3"),
        vec![b'd'; 120],
        "rewriting the unnamed stream changed a named one"
    );
    drop(io);
    clean(img);
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// The unnamed `$DATA` of a listed file grows resident, goes into
/// clusters, and shrinks, wherever the list keeps it.
#[test]
fn rewriting_the_unnamed_stream_of_a_listed_file_keeps_the_list_true() {
    let (img, rec) = volume_with_a_listed_file("base");
    list_is_true(&img, rec);
    rewrite_and_check(&img, rec, &pattern(600));
    rewrite_and_check(&img, rec, &pattern(9000));
    rewrite_and_check(&img, rec, &pattern(20_000));
    rewrite_and_check(&img, rec, &pattern(20));
}

/// A Windows-made file whose unnamed `$DATA` lives in an extension record
/// is rewritten through the list, not refused for want of one in the base
/// record.
#[test]
fn the_unnamed_stream_in_an_extension_record_is_rewritten_there() {
    let (img, rec) = volume_with_a_listed_file("ext");
    let ext = move_unnamed_data_to_extension(&img, rec);
    list_is_true(&img, rec);
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/f"), b"contents");
    drop(reader);
    rewrite_and_check(&img, rec, &pattern(300));
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    let (_, base) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f");
    assert!(
        attr_io::find_attribute(&base, AttrType::Data, None).is_none(),
        "the rewrite put a second unnamed $DATA in the base record"
    );
    let (_, ext_record) = mft_io::read_mft_record_io(&mut io, ext).expect("read the extension");
    assert!(
        attr_io::find_attribute(&ext_record, AttrType::Data, None).is_some(),
        "the rewrite moved the unnamed $DATA out of its extension record, which had room"
    );
    drop(io);
    rewrite_and_check(&img, rec, &pattern(9000));
    rewrite_and_check(&img, rec, &pattern(5));
}

/// A base record with no list, too full of named streams to hold even the
/// unnamed stream's non-resident form, gains a list when the stream grows,
/// rather than refusing the write.
#[test]
fn a_base_record_too_full_to_promote_the_unnamed_stream_gains_a_list() {
    let img = common::temp_image_path("aldata_full");
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, RECORD_SIZE, None, Some(1))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    let free = |io: &mut PathIo| {
        let (_, record) = mft_io::read_mft_record_io(io, rec).expect("read /f");
        let used = u32::from_le_bytes(record[0x18..0x1C].try_into().unwrap()) as usize;
        let allocated = u32::from_le_bytes(record[0x1C..0x20].try_into().unwrap()) as usize;
        allocated - used
    };
    // A one-letter name's value starts at 0x20, past the header and the
    // name; the stream that fills the record leaves 8 bytes free.
    let fill = free(&mut io) - 8 - 0x20;
    write::write_named_stream_io(&mut io, "/f", "z", &vec![b'z'; fill]).expect("write /f:z");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    assert_eq!(free(&mut io), 8, "the stream did not fill /f's base record");
    assert!(
        read::read_attribute_value_if_present(&mut io, rec, AttrType::AttributeList, None)
            .expect("read /f's list")
            .is_none(),
        "/f gained a list before the test grew its unnamed stream"
    );
    drop(io);
    let data = pattern(600);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::write_file_contents_io(&mut io, "/f", &data).expect("grow /f to 600 bytes");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    assert_eq!(
        read::read_attribute_value(&mut io, rec, AttrType::Data, Some("z")).expect("read /f:z"),
        vec![b'z'; fill],
        "growing the unnamed stream changed a named one"
    );
    drop(io);
    list_is_true(&img, rec);
    let (ntfs, mut reader) = common::open(&img);
    assert!(common::read_file_all(&ntfs, &mut reader, "/f") == data);
    drop(reader);
    clean(&img);
}

/// Write `data` at `offset` of `/f` with `write_at`, apply the same write
/// to `model`, then check the list, the contents the `ntfs` crate reads,
/// the streams beside it and the volume (#789).
fn write_at_and_check(img: &str, rec: u64, model: &mut Vec<u8>, offset: usize, data: &[u8]) {
    let end = offset + data.len();
    if model.len() < end {
        model.resize(end, 0);
    }
    model[offset..end].copy_from_slice(data);
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let written = write::write_at_io(&mut io, "/f", offset as u64, data)
        .unwrap_or_else(|e| panic!("write {} bytes at {offset} of /f: {e}", data.len()));
    assert_eq!(written, data.len() as u64);
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    list_is_true(img, rec);
    let (ntfs, mut reader) = common::open(img);
    let read_back = common::read_file_all(&ntfs, &mut reader, "/f");
    assert!(
        read_back == *model,
        "after a write of {} bytes at {offset}, the ntfs crate reads {} bytes of /f \
         that differ from the {} expected",
        data.len(),
        read_back.len(),
        model.len()
    );
    drop(reader);
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    assert_eq!(
        read::read_attribute_value(&mut io, rec, AttrType::Data, Some("s3")).expect("read /f:s3"),
        vec![b'd'; 120],
        "writing into the unnamed stream changed a named one"
    );
    drop(io);
    clean(img);
}

/// Writes into the middle of and past the end of the unnamed stream of a
/// listed file, resident, then promoted by an extending write, then
/// non-resident.
fn partial_writes(img: &str, rec: u64) {
    let mut model = b"contents".to_vec();
    write_at_and_check(img, rec, &mut model, 2, b"NT");
    write_at_and_check(img, rec, &mut model, 8, &pattern(300));
    write_at_and_check(img, rec, &mut model, 400, b"gap");
    write_at_and_check(img, rec, &mut model, 100, &[b'm'; 50]);
    write_at_and_check(img, rec, &mut model, 600, &pattern(8400));
    write_at_and_check(img, rec, &mut model, 4090, &[b'x'; 20]);
    write_at_and_check(img, rec, &mut model, 8990, &pattern(11_010));
    write_at_and_check(img, rec, &mut model, 30_000, b"tail");
}

/// `write_at` on the unnamed `$DATA` of a listed file whose stream is in
/// its base record goes through the list as it grows (#789).
#[test]
fn write_at_into_the_unnamed_stream_of_a_listed_file_keeps_the_list_true() {
    let (img, rec) = volume_with_a_listed_file("at_base");
    partial_writes(&img, rec);
}

/// `write_at` on a Windows-made file whose unnamed `$DATA` lives in an
/// extension record writes it there, not refused for want of one in the
/// base record (#789).
#[test]
fn write_at_into_the_unnamed_stream_in_an_extension_record_writes_it_there() {
    let (img, rec) = volume_with_a_listed_file("at_ext");
    move_unnamed_data_to_extension(&img, rec);
    list_is_true(&img, rec);
    partial_writes(&img, rec);
}
