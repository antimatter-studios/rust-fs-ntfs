//! `check_io` finds a record whose attributes are malformed (#515).
//!
//! `check_io` read each in-use record's header and nothing past it, so a
//! record whose attributes no reader could walk was reported clean: a
//! resident value running past its attribute, attributes out of type
//! order, an attribute id the header never handed out, a chain that does
//! not end where the header says. This crate's own reader then failed on
//! the file, and ntfs-3g refused it, while the check called the volume
//! clean.
//!
//! Each case corrupts one field of one attribute of `/f` on a volume
//! `check_io` first reports clean, and requires a `BadRecord` for `/f`'s
//! record. Volumes Windows wrote, and its chkdsk left, are checked too:
//! the rules must not reject what Windows itself writes.

mod common;

use fs_ntfs::attr_io::{self, attr_off, AttrLocation, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::fsck::{self, CheckFinding};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{mft_io, write};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u64 = 4096;
const BYTES_IN_USE: usize = 0x18;
const NEXT_ATTRIBUTE_ID: usize = 0x28;

/// One case: what it breaks, whether `/f`'s `$DATA` is non-resident, and
/// the edit, given the fixed-up record and the attribute to break.
type Case = (&'static str, bool, fn(&mut [u8], &[AttrLocation]));

fn u16_at(r: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([r[at], r[at + 1]])
}

fn put_u16(r: &mut [u8], at: usize, v: u16) {
    r[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn u32_at(r: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(r[at..at + 4].try_into().unwrap())
}

fn put_u32(r: &mut [u8], at: usize, v: u32) {
    r[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn u64_at(r: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(r[at..at + 8].try_into().unwrap())
}

fn put_u64(r: &mut [u8], at: usize, v: u64) {
    r[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn data(attrs: &[AttrLocation]) -> &AttrLocation {
    attrs
        .iter()
        .find(|a| a.type_code == AttrType::Data as u32 && a.name_length == 0)
        .expect("/f has an unnamed $DATA")
}

fn standard_information(attrs: &[AttrLocation]) -> &AttrLocation {
    attrs
        .iter()
        .find(|a| a.type_code == AttrType::StandardInformation as u32)
        .expect("/f has a $STANDARD_INFORMATION")
}

/// A fresh volume holding `/f`, resident or not, that `check_io` reports
/// clean. Returns the image and `/f`'s record.
fn volume_with_a_file(tag: &str, non_resident: bool) -> (String, u64) {
    let img = common::temp_image_path(format!("checkattr_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER as u32, 1024, None, Some(1))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    let contents = if non_resident {
        vec![7u8; 16 * CLUSTER as usize]
    } else {
        b"x".to_vec()
    };
    write::write_file_contents_io(&mut io, "/f", &contents).expect("write /f");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let report = fsck::check_io(&mut io).expect("check the untouched volume");
    assert!(
        report.findings.is_empty(),
        "the untouched volume is not clean: {:?}",
        report.findings
    );
    (img, rec)
}

const CASES: [Case; 13] = [
    (
        "a resident value running past its attribute",
        false,
        |r, a| {
            let d = data(a);
            put_u32(r, d.attr_offset + attr_off::RESIDENT_VALUE_LENGTH, 9000);
        },
    ),
    (
        "a resident value offset that is not 8-aligned",
        false,
        |r, a| {
            let d = data(a);
            let at = d.attr_offset + attr_off::RESIDENT_VALUE_OFFSET;
            let v = u16_at(r, at);
            put_u16(r, at, v + 1);
        },
    ),
    ("attributes out of type order", false, |r, a| {
        let si = standard_information(a);
        put_u32(
            r,
            si.attr_offset + attr_off::TYPE,
            AttrType::ObjectId as u32,
        );
    }),
    ("an attribute type no NTFS defines", false, |r, a| {
        let d = data(a);
        put_u32(r, d.attr_offset + attr_off::TYPE, 0xFF);
    }),
    (
        "an attribute id the header never handed out",
        false,
        |r, a| {
            let next = u16_at(r, NEXT_ATTRIBUTE_ID);
            let d = data(a);
            put_u16(r, d.attr_offset + attr_off::ATTRIBUTE_ID, next);
        },
    ),
    ("two attributes sharing an id", false, |r, a| {
        let si = standard_information(a).attribute_id;
        let d = data(a);
        put_u16(r, d.attr_offset + attr_off::ATTRIBUTE_ID, si);
    }),
    ("an attribute of length 0", false, |r, a| {
        let d = data(a);
        put_u32(r, d.attr_offset + attr_off::LENGTH, 0);
    }),
    (
        "an attribute running past the bytes in use",
        false,
        |r, a| {
            let d = data(a);
            let len = u32_at(r, d.attr_offset + attr_off::LENGTH);
            put_u32(r, d.attr_offset + attr_off::LENGTH, len + 0x100);
        },
    ),
    ("bytes in use not where the chain ends", false, |r, _| {
        let used = u32_at(r, BYTES_IN_USE);
        put_u32(r, BYTES_IN_USE, used + 8);
    }),
    ("a name running past its attribute", false, |r, a| {
        let d = data(a);
        r[d.attr_offset + attr_off::NAME_LENGTH] = 200;
    }),
    (
        "a non-resident flag that is neither 0 nor 1",
        false,
        |r, a| {
            let d = data(a);
            r[d.attr_offset + attr_off::NON_RESIDENT] = 2;
        },
    ),
    ("a highest VCN its run list does not reach", true, |r, a| {
        let d = data(a);
        let at = d.attr_offset + attr_off::NONRES_LAST_VCN;
        let v = u64_at(r, at);
        put_u64(r, at, v + 1);
    }),
    (
        "an allocated size its run list does not hold",
        true,
        |r, a| {
            let d = data(a);
            let at = d.attr_offset + attr_off::NONRES_ALLOCATED_LENGTH;
            let v = u64_at(r, at);
            put_u64(r, at, v + CLUSTER);
        },
    ),
];

/// #515's reproduction and every other corruption its differential found
/// `check_io` passing.
#[test]
fn each_malformed_attribute_is_a_bad_record() {
    let mut missed = Vec::new();
    for (i, (what, non_resident, edit)) in CASES.iter().enumerate() {
        let (img, rec) = volume_with_a_file(&format!("case{i}"), *non_resident);
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        mft_io::update_mft_record_io(&mut io, rec, |record| {
            let attrs: Vec<AttrLocation> = attr_io::iter_attributes(record).collect();
            edit(record, &attrs);
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{what}: corrupt /f: {e}"));
        <PathIo as BlockIo>::sync(&mut io).expect("sync");
        let report = fsck::check_io(&mut io).unwrap_or_else(|e| panic!("{what}: check: {e}"));
        let found = report
            .findings
            .iter()
            .any(|f| matches!(f, CheckFinding::BadRecord { record, .. } if *record == rec));
        if !found {
            missed.push(format!("{what}: {:?}", report.findings));
        }
        drop(io);
        std::fs::remove_file(&img).ok();
    }
    assert!(
        missed.is_empty(),
        "check_io reported no bad record for:\n{}",
        missed.join("\n")
    );
}

fn no_bad_record(img: &str, name: &str) {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let report = fsck::check_io(&mut io).unwrap_or_else(|e| panic!("{name}: check: {e}"));
    let bad: Vec<&CheckFinding> = report
        .findings
        .iter()
        .filter(|f| matches!(f, CheckFinding::BadRecord { .. }))
        .collect();
    assert!(
        bad.is_empty(),
        "{name}, which Windows wrote, has records check_io calls bad: {bad:?}"
    );
}

/// Graded by Windows: the volumes its chkdsk left after recovering an
/// interrupted log, and the ones the native-read fixtures build has
/// Windows write (an `$ATTRIBUTE_LIST`, compressed files, symlinks).
#[test]
fn volumes_windows_wrote_have_no_bad_record() {
    let mut recovered: Vec<String> = std::fs::read_dir("test-disks")
        .expect("list test-disks")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".recovered.img.gz"))
        .collect();
    recovered.sort();
    assert!(
        !recovered.is_empty(),
        "no test-disks/*.recovered.img.gz; they are committed with the repository"
    );
    for name in &recovered {
        let img = common::temp_image_path(format!("checkattr_{}", name.replace('.', "_")));
        let out = File::create(&img).expect("create image copy");
        let status = Command::new("gzip")
            .args(["-dc", &format!("test-disks/{name}")])
            .stdout(Stdio::from(out))
            .status()
            .expect("run gzip");
        assert!(status.success(), "decompress test-disks/{name}");
        no_bad_record(&img, name);
        std::fs::remove_file(&img).ok();
    }
    for fixture in [
        "ntfs-attrlist.img",
        "ntfs-compressed.img",
        "ntfs-symlink.img",
        "ntfs-cli-read.img",
    ] {
        let path = format!("test-disks/{fixture}");
        assert!(
            Path::new(&path).exists(),
            "missing {path}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
        );
        no_bad_record(&path, fixture);
    }
}
