//! An EA its file's base record cannot hold, or one written to a file that
//! has an `$ATTRIBUTE_LIST`, goes through the list (#660, part of #480).
//!
//! `write_ea_io` edited the base record alone. A full base record refused
//! the EA ("exceeds record capacity"), and a file with a list refused it
//! outright ("the record carries an $ATTRIBUTE_LIST"), where Windows moves
//! attributes to extension records and lists where each went. Windows
//! grades the result in the matrix (`mac-format-mac-write-eas-win-chkdsk`
//! and `win-format-win-write-streams-mac-write-eas-win-chkdsk`, which read
//! every EA back through `NtQueryEaFile`); here the `ntfs` crate, a parser
//! that is not this one, finds the `$EA` through the list, `check_io`
//! reports the volume clean, and the list is checked to name `$EA` and
//! `$EA_INFORMATION` where they are.

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

/// A fresh volume with 1 KiB records holding `/f`, whose unnamed stream
/// is `contents`. Returns the image and `/f`'s record.
fn volume_with_a_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("alea_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, rec)
}

/// `volume_with_a_file`, with eight 64-byte named streams: more than the
/// base record holds, so `/f` has an `$ATTRIBUTE_LIST` and an extension
/// record before any EA is written.
fn volume_with_a_listed_file(tag: &str) -> (String, u64) {
    let (img, rec) = volume_with_a_file(tag);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
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
/// `path`, following the file's `$ATTRIBUTE_LIST`; empty when it finds no
/// `$EA`.
fn ntfs_crate_eas(img: &str, path: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, path);
    let mut attrs = file.attributes();
    while let Some(item) = attrs.next(&mut reader) {
        let item = item.expect("an attribute of the file");
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

/// The records the list of the file at `rec` puts `$EA_INFORMATION` and
/// `$EA` in, as `(type, record)`, having checked that each entry names an
/// in-use record of this file holding an attribute of that type and id.
fn listed_ea_records(img: &str, rec: u64) -> Vec<(u32, u64)> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let mut out = Vec::new();
    for e in read::parse_attribute_list(&list).expect("parse the list") {
        if e.type_code != AttrType::ExtendedAttribute as u32
            && e.type_code != AttrType::ExtendedAttributeInformation as u32
        {
            continue;
        }
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
    out.sort_unstable();
    out
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

/// An 800-byte EA does not fit in the base record beside `/f`'s other
/// attributes, but does in an extension record: the file is given a list,
/// and `$EA` goes where it fits.
#[test]
fn an_ea_the_record_cannot_hold_gets_a_list_and_an_extension_record() {
    let (img, rec) = volume_with_a_file("first");
    write_ea(&img, "/f", b"USER.BIG", &value(800));
    let records = listed_ea_records(&img, rec);
    let ea_record = records
        .iter()
        .find(|(ty, _)| *ty == AttrType::ExtendedAttribute as u32)
        .map(|&(_, n)| n)
        .expect("the list names an $EA");
    assert_ne!(
        ea_record, rec,
        "the $EA fitted in the base record; the test no longer reaches #660's path"
    );
    assert!(
        records
            .iter()
            .any(|(ty, _)| *ty == AttrType::ExtendedAttributeInformation as u32),
        "the list names no $EA_INFORMATION: {records:?}"
    );
    let want = vec![(b"USER.BIG".to_vec(), value(800))];
    assert_eq!(our_eas(&img, "/f"), want, "list_eas_io");
    assert_eq!(ntfs_crate_eas(&img, "/f"), want, "the ntfs crate");
    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/f"), b"contents");
    clean(&img);
}

/// A file that already has a list takes an EA, a second, a rewrite of the
/// first that outgrows where it was, and the removal of both: the list
/// gains and loses the entries, and nothing of `$EA` or
/// `$EA_INFORMATION` is left behind.
#[test]
fn eas_of_a_file_with_a_list_are_written_rewritten_and_removed_through_it() {
    let (img, rec) = volume_with_a_listed_file("listed");
    write_ea(&img, "/f", b"USER.A", b"one");
    write_ea(&img, "/f", b"USER.B", b"two");
    assert_eq!(
        our_eas(&img, "/f"),
        vec![
            (b"USER.A".to_vec(), b"one".to_vec()),
            (b"USER.B".to_vec(), b"two".to_vec())
        ]
    );
    assert_eq!(
        listed_ea_records(&img, rec).len(),
        2,
        "one $EA, one $EA_INFORMATION"
    );
    clean(&img);

    write_ea(&img, "/f", b"USER.A", &value(700));
    let want = vec![
        (b"USER.A".to_vec(), value(700)),
        (b"USER.B".to_vec(), b"two".to_vec()),
    ];
    assert_eq!(our_eas(&img, "/f"), want, "list_eas_io after the rewrite");
    assert_eq!(
        ntfs_crate_eas(&img, "/f"),
        want,
        "the ntfs crate after the rewrite"
    );
    assert_eq!(
        listed_ea_records(&img, rec).len(),
        2,
        "one $EA, one $EA_INFORMATION"
    );
    clean(&img);

    remove_ea(&img, "/f", b"USER.A");
    remove_ea(&img, "/f", b"USER.B");
    assert!(
        our_eas(&img, "/f").is_empty(),
        "EAs left after removing both"
    );
    assert!(
        ntfs_crate_eas(&img, "/f").is_empty(),
        "the ntfs crate finds an $EA"
    );
    assert!(
        listed_ea_records(&img, rec).is_empty(),
        "the list still names an $EA or $EA_INFORMATION"
    );
    for i in 1..=8u8 {
        let got = {
            let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
            read::read_attribute_value(&mut io, rec, AttrType::Data, Some(&format!("s{i}")))
                .expect("read a stream")
        };
        assert_eq!(got, vec![b'0' + i; 64], "s{i} changed");
    }
    clean(&img);
}

/// An EA no record can hold is refused, and the image is byte-for-byte
/// what it was: nothing allocated for the attempt is left behind.
#[test]
fn an_ea_no_record_can_hold_is_refused_and_changes_nothing() {
    let (img, _) = volume_with_a_listed_file("refused");
    let before = std::fs::read(&img).expect("read image");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let got = write::write_ea_io(&mut io, "/f", b"USER.HUGE", &value(2000), 0);
    assert!(got.is_err(), "a 2000-byte EA on 1 KiB records was accepted");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    let after = std::fs::read(&img).expect("read image");
    let first = before.iter().zip(&after).position(|(a, b)| a != b);
    assert!(
        first.is_none(),
        "a refused write changed the image, first at byte {first:?}"
    );
}

/// Graded by Windows: `/many.bin` on `test-disks/ntfs-attrlist.img`, whose
/// list Windows wrote when its 96 streams outgrew the base record. An EA
/// written there is entered in Windows' own list, and every stream still
/// reads back.
#[test]
fn a_file_windows_spread_over_extension_records_takes_an_ea() {
    const FIXTURE: &str = "test-disks/ntfs-attrlist.img";
    assert!(
        Path::new(FIXTURE).exists(),
        "missing {FIXTURE}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let img = common::temp_image_path("alea_windows");
    std::fs::copy(FIXTURE, &img).expect("copy the fixture");
    let rec = {
        let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
        write::resolve_path_to_record_number_io(&mut io, "/many.bin").expect("/many.bin")
    };
    let streams = |img: &str| -> Vec<Vec<u8>> {
        let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
        (0..96)
            .map(|i| {
                read::read_attribute_value(
                    &mut io,
                    rec,
                    AttrType::Data,
                    Some(&format!("stream-{i:03}")),
                )
                .expect("read a stream Windows wrote")
            })
            .collect()
    };
    let before = streams(&img);
    write_ea(&img, "/many.bin", b"USER.ADDED", &value(300));
    let want = vec![(b"USER.ADDED".to_vec(), value(300))];
    assert_eq!(our_eas(&img, "/many.bin"), want, "list_eas_io");
    assert_eq!(ntfs_crate_eas(&img, "/many.bin"), want, "the ntfs crate");
    assert_eq!(
        listed_ea_records(&img, rec).len(),
        2,
        "one $EA, one $EA_INFORMATION in Windows' list"
    );
    assert!(streams(&img) == before, "a stream Windows wrote changed");
}
