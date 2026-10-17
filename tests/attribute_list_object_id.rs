//! An object id set on a file whose base record has no room for it, or on
//! a file that has an `$ATTRIBUTE_LIST`, goes through the list, and
//! removing it takes its entry out (#809, part of #665).
//!
//! `write_object_id_io` edited the base record alone, so a file with a
//! list refused the object id ("the record carries an $ATTRIBUTE_LIST"),
//! where Windows moves attributes to extension records and lists where
//! each went. Here the `ntfs` crate, a parser that is not this one, finds
//! the `$OBJECT_ID` through the list with the id written; `check_io`
//! reports the volume clean, the list is checked to name the attribute
//! where it is, and `$Extend\$ObjId:$O` to be keyed by the id while the
//! file has it and not after.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fsck, mft_io, read, view_index, write};
use ntfs::{NtfsAttributeType, NtfsReadSeek};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

/// A fresh volume with 1 KiB records holding `/f`, with eight 64-byte
/// named streams: more than the base record holds, so `/f` has an
/// `$ATTRIBUTE_LIST` and an extension record before any object id is set.
/// Returns the image and `/f`'s record.
fn volume_with_a_listed_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("aloi_{tag}"));
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

/// The value of the `$OBJECT_ID` the `ntfs` crate finds on `path`,
/// following the file's `$ATTRIBUTE_LIST`; `None` when it finds none.
fn ntfs_crate_object_id(img: &str, path: &str) -> Option<Vec<u8>> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, path);
    let mut attrs = file.attributes();
    while let Some(item) = attrs.next(&mut reader) {
        let item = item.expect("an attribute of the file");
        let attr = item.to_attribute().expect("to_attribute");
        if attr.ty().expect("attribute type") != NtfsAttributeType::ObjectId {
            continue;
        }
        let mut value = attr.value(&mut reader).expect("the $OBJECT_ID's value");
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = value
                .read(&mut reader, &mut chunk)
                .expect("read the $OBJECT_ID");
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        return Some(bytes);
    }
    None
}

/// The records the list of the file at `rec` puts `$OBJECT_ID` in, having
/// checked that each entry names an in-use record of this file holding an
/// attribute of that type and id.
fn listed_object_id_records(img: &str, rec: u64) -> Vec<u64> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let mut out = Vec::new();
    for e in read::parse_attribute_list(&list).expect("parse the list") {
        if e.type_code != AttrType::ObjectId as u32 {
            continue;
        }
        let n = e.record_number;
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a listed record");
        assert_ne!(
            mft_io::record_flags(&record) & mft_io::MFT_FLAG_IN_USE,
            0,
            "the list puts the $OBJECT_ID in record {n}, which is not in use"
        );
        assert!(
            attr_io::iter_attributes(&record)
                .any(|l| l.type_code == e.type_code && l.attribute_id == e.attribute_id),
            "the list puts the $OBJECT_ID id {} in record {n}, and it is not there",
            e.attribute_id
        );
        out.push(n);
    }
    out
}

/// The keys of `$Extend\$ObjId:$O`.
fn objid_keys(img: &str) -> Vec<Vec<u8>> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let objid = write::resolve_path_to_record_number_io(&mut io, view_index::OBJID_PATH)
        .expect("$Extend\\$ObjId");
    view_index::keys_io(&mut io, objid, view_index::OBJID_INDEX)
        .expect("the keys of $ObjId:$O")
        .into_iter()
        .map(|(k, _)| k)
        .collect()
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

/// A file that has a list takes an object id, has it replaced by an
/// extended one with its three birth ids, and loses it: the list names the
/// `$OBJECT_ID` while it is there and not after, `$ObjId:$O` follows the
/// id, and the volume stays clean throughout.
#[test]
fn an_object_id_on_a_file_with_a_list_is_set_and_removed_through_it() {
    let (img, rec) = volume_with_a_listed_file("set");
    let id = [0x11u8; 16];
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::write_object_id_io(&mut io, "/f", &id)
            .unwrap_or_else(|e| panic!("set an object id on /f: {e}"));
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert_eq!(
        listed_object_id_records(&img, rec).len(),
        1,
        "the list names one $OBJECT_ID"
    );
    assert_eq!(
        ntfs_crate_object_id(&img, "/f"),
        Some(id.to_vec()),
        "the ntfs crate"
    );
    assert!(
        objid_keys(&img).contains(&id.to_vec()),
        "$ObjId:$O has the id"
    );
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/f"), b"contents");
    clean(&img);

    let extended = [0x22u8; 16];
    let (volume, object, domain) = ([0x33u8; 16], [0x44u8; 16], [0x55u8; 16]);
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        write::write_object_id_extended_io(&mut io, "/f", &extended, &volume, &object, &domain)
            .unwrap_or_else(|e| panic!("set an extended object id on /f: {e}"));
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert_eq!(
        listed_object_id_records(&img, rec).len(),
        1,
        "the list names one $OBJECT_ID"
    );
    let want: Vec<u8> = [extended, volume, object, domain].concat();
    assert_eq!(
        ntfs_crate_object_id(&img, "/f"),
        Some(want),
        "the ntfs crate"
    );
    let keys = objid_keys(&img);
    assert!(
        keys.contains(&extended.to_vec()),
        "$ObjId:$O has the new id"
    );
    assert!(
        !keys.contains(&id.to_vec()),
        "$ObjId:$O still has the old id"
    );
    clean(&img);

    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        let removed = write::remove_object_id_io(&mut io, "/f")
            .unwrap_or_else(|e| panic!("remove the object id of /f: {e}"));
        assert!(removed, "remove_object_id_io reports an object id removed");
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
    }
    assert!(
        listed_object_id_records(&img, rec).is_empty(),
        "the list still names an $OBJECT_ID"
    );
    assert_eq!(ntfs_crate_object_id(&img, "/f"), None, "the ntfs crate");
    assert!(
        !objid_keys(&img).contains(&extended.to_vec()),
        "$ObjId:$O still has the id"
    );
    clean(&img);
}
