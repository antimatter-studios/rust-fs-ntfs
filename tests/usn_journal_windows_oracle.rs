//! A write leaves no change journal that does not record it (#479).
//!
//! A volume with an active change journal, `$Extend\$UsnJrnl`, promises
//! whoever reads it a record of every change after the last USN they saw.
//! This crate does not append to `$UsnJrnl:$J`, and its writes used to leave
//! the journal reporting the same id and next USN, so a reader carrying on
//! from its last USN missed them with no way to tell. Before its first write
//! it now sets `$VOLUME_INFORMATION`'s `0x0008` flag, which Windows answers
//! on its next mount by giving the journal a new id
//! (`test-disks/measure-usn-journal.ps1` measured it).
//!
//! Graded by Windows: `test-disks/ntfs-usnjrnl.img` is a volume whose journal
//! Windows created and wrote to, `ntfs-usnjrnl.query.txt` is what `fsutil usn
//! queryjournal` reported of it and `ntfs-usnjrnl.records.csv` every record
//! `fsutil usn readjournal` read back, all from
//! `test-disks/build-windows-native-read-fixtures.ps1`. The matrix scenario
//! `win-format-win-usn-journal-mac-write-win-usn-retired-chkdsk` hands a
//! volume written here back to Windows' `fsutil usn queryjournal` and chkdsk.

#![allow(unused_unsafe)]

mod common;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::facade::Filesystem;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fs_ntfs_create_file_h, fs_ntfs_last_error, fs_ntfs_mount, fs_ntfs_umount, read};
use std::ffi::{CStr, CString};
use std::path::Path;

const IMG: &str = "test-disks/ntfs-usnjrnl.img";
const QUERY: &str = "test-disks/ntfs-usnjrnl.query.txt";
const RECORDS: &str = "test-disks/ntfs-usnjrnl.records.csv";
const JOURNAL: &str = "/$Extend/$UsnJrnl";
/// `$VOLUME_INFORMATION`'s flag Windows answers by giving the journal a new id.
const MOUNTED_ON_NT4: u16 = 0x0008;
/// `$MFT` and `$MFTMirr` are MFT records 0 and 1; `$Volume` is record 3.
const MFT_RECORD: u64 = 0;
const MFTMIRR_RECORD: u64 = 1;
const VOLUME_RECORD: u64 = 3;

fn require_fixtures() {
    for path in [IMG, QUERY, RECORDS] {
        assert!(
            Path::new(path).exists(),
            "missing {path}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
        );
    }
}

/// The number `fsutil usn queryjournal` printed for `field`, as in
/// `Maximum Size     : 0x0000000000100000 (  1.0 MB)`.
fn fsutil_field(query: &str, field: &str) -> u64 {
    let line = query
        .lines()
        .find(|l| l.split(':').next().is_some_and(|k| k.trim() == field))
        .unwrap_or_else(|| panic!("{QUERY} has no {field:?}:\n{query}"));
    let value = line.split(':').nth(1).unwrap().split_whitespace().next();
    let hex = value.and_then(|v| v.strip_prefix("0x"));
    u64::from_str_radix(hex.unwrap_or_else(|| panic!("{QUERY}: {line:?}")), 16)
        .unwrap_or_else(|e| panic!("{QUERY}: {line:?}: {e}"))
}

fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn volume_flags(img: &str) -> u16 {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    read::read_volume_info(&mut io).expect("volume info").flags
}

/// `$Volume`'s record as `$MFT` holds it and as `$MFTMirr` holds it.
fn volume_record_and_mirror(img: &str) -> (Vec<u8>, Vec<u8>) {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let (params, _) =
        fs_ntfs::mft_io::read_mft_record_io(&mut io, VOLUME_RECORD).expect("read $Volume");
    let size = params.file_record_size;
    let mut copies = [MFT_RECORD, MFTMIRR_RECORD].map(|record| {
        let (start, _) =
            read::nonresident_contiguous_disk_range(&mut io, record, AttrType::Data, None)
                .expect("the first extent");
        let mut bytes = vec![0u8; size as usize];
        io.read_exact_at(start + VOLUME_RECORD * size, &mut bytes)
            .expect("read record 3");
        bytes
    });
    let mirror = std::mem::take(&mut copies[1]);
    (std::mem::take(&mut copies[0]), mirror)
}

fn last_error() -> String {
    let p = fs_ntfs_last_error();
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// A copy of the Windows fixture, to write to.
fn copy_of_fixture(stem: &str) -> String {
    require_fixtures();
    let copy = common::temp_image_path(stem);
    std::fs::copy(IMG, &copy).expect("copy the fixture");
    copy
}

#[test]
fn the_journal_reads_as_windows_reports_it() {
    require_fixtures();
    let query = std::fs::read_to_string(QUERY).expect("read the query");
    let mut io = PathIo::open_ro(Path::new(IMG)).expect("open the fixture");
    let journal = read::resolve_path(&mut io, JOURNAL).expect("the volume has a journal");

    // $Max: MaximumSize, AllocationDelta, UsnJournalID, LowestValidUsn.
    let max = read::read_attribute_value(&mut io, journal, AttrType::Data, Some("$Max"))
        .expect("read $UsnJrnl:$Max");
    assert_eq!(max.len(), 32, "$Max is {} bytes", max.len());
    assert_eq!(le_u64(&max, 0), fsutil_field(&query, "Maximum Size"));
    assert_eq!(le_u64(&max, 8), fsutil_field(&query, "Allocation Delta"));
    assert_eq!(le_u64(&max, 16), fsutil_field(&query, "Usn Journal ID"));
    assert_eq!(le_u64(&max, 24), fsutil_field(&query, "Lowest Valid Usn"));

    // $J holds every record up to the next USN fsutil reported.
    let j = read::read_attribute_value(&mut io, journal, AttrType::Data, Some("$J"))
        .expect("read $UsnJrnl:$J");
    let next_usn = fsutil_field(&query, "Next Usn");
    assert!(
        j.len() as u64 >= next_usn,
        "$J is {} bytes, short of the next USN {next_usn:#x}",
        j.len()
    );

    // Each record fsutil read back is where its USN says, as USN_RECORD_V2:
    // what Windows writes, whatever version fsutil converts it to.
    let csv = std::fs::read_to_string(RECORDS).expect("read the records");
    let mut lines = csv
        .lines()
        .skip_while(|l| !l.starts_with("Usn,"))
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            l.split(',')
                .map(|f| f.trim_matches('"'))
                .collect::<Vec<_>>()
        });
    let header = lines.next().expect("the records' header");
    let column = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .unwrap_or_else(|| panic!("{RECORDS} has no column {name:?}"))
    };
    let (c_usn, c_name, c_reason, c_attrs, c_id, c_parent) = (
        column("Usn"),
        column("File name"),
        column("Reason #"),
        column("File attributes #"),
        column("File ID"),
        column("Parent file ID"),
    );
    let hex = |s: &str| u128::from_str_radix(s.trim_start_matches("0x"), 16).expect(s);
    let mut checked = 0;
    for row in lines {
        // fsutil lists the V4 extent columns in the header but omits them
        // from V2/V3 rows. Validate every row, including those shorter rows.
        assert!(
            row.len() > column("Record length") && row.len() <= header.len(),
            "{RECORDS}: malformed record: {row:?}"
        );
        let usn: usize = row[c_usn].parse().expect("a USN");
        let rec = &j[usn..];
        let length = le_u32(rec, 0) as usize;
        let name_length = le_u16(rec, 0x38) as usize;
        let name_offset = le_u16(rec, 0x3A) as usize;
        let name: Vec<u16> = rec[name_offset..name_offset + name_length]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let what = format!("the record at USN {usn}");
        assert_eq!(le_u16(rec, 4), 2, "{what}: major version");
        assert_eq!(
            length,
            (0x3C + name_length).next_multiple_of(8),
            "{what}: length"
        );
        assert_eq!(le_u64(rec, 0x18), usn as u64, "{what}: its own USN");
        assert_eq!(
            u128::from(le_u64(rec, 0x08)),
            hex(row[c_id]),
            "{what}: file"
        );
        assert_eq!(
            u128::from(le_u64(rec, 0x10)),
            hex(row[c_parent]),
            "{what}: parent"
        );
        assert_eq!(
            u128::from(le_u32(rec, 0x28)),
            hex(row[c_reason]),
            "{what}: reason"
        );
        assert_eq!(
            u128::from(le_u32(rec, 0x34)),
            hex(row[c_attrs]),
            "{what}: attributes"
        );
        assert_eq!(String::from_utf16_lossy(&name), row[c_name], "{what}: name");
        checked += 1;
    }
    assert!(
        checked > 20,
        "only {checked} records were read back from {RECORDS}"
    );
}

#[test]
fn a_write_retires_the_journal_windows_keeps() {
    let copy = copy_of_fixture("usn_retired_facade");
    assert_eq!(
        volume_flags(&copy) & MOUNTED_ON_NT4,
        0,
        "the fixture arrives flagged"
    );

    let fs = Filesystem::mount_rw(&copy).expect("mount_rw");
    fs.create_file("/", "written-here.txt").expect("create");
    fs.write_file_contents("/written-here.txt", b"not in the journal")
        .expect("write");

    let flags = volume_flags(&copy);
    assert_ne!(
        flags & MOUNTED_ON_NT4,
        0,
        "flags {flags:#06x}: the journal was left as it was"
    );
    let (record, mirror) = volume_record_and_mirror(&copy);
    assert!(
        record == mirror,
        "$MFTMirr's copy of $Volume disagrees with $MFT's"
    );

    // The journal itself is Windows' to re-stamp; nothing here touches it.
    let mut ours = PathIo::open_ro(Path::new(&copy)).expect("open the copy");
    let mut theirs = PathIo::open_ro(Path::new(IMG)).expect("open the fixture");
    let journal = read::resolve_path(&mut ours, JOURNAL).expect("still a journal");
    assert_eq!(Some(journal), read::resolve_path(&mut theirs, JOURNAL).ok());
    for stream in ["$Max", "$J"] {
        let read = |io: &mut PathIo| {
            read::read_attribute_value(io, journal, AttrType::Data, Some(stream))
                .unwrap_or_else(|e| panic!("read $UsnJrnl:{stream}: {e}"))
        };
        assert!(
            read(&mut ours) == read(&mut theirs),
            "$UsnJrnl:{stream} changed"
        );
    }
    let _ = std::fs::remove_file(&copy);
}

#[test]
fn a_write_through_a_mounted_handle_retires_the_journal() {
    let copy = copy_of_fixture("usn_retired_handle");
    let path = CString::new(copy.clone()).unwrap();
    let handle = unsafe { fs_ntfs_mount(path.as_ptr()) };
    assert!(!handle.is_null(), "fs_ntfs_mount");
    assert_eq!(
        volume_flags(&copy) & MOUNTED_ON_NT4,
        0,
        "a mount that has written nothing has nothing to retire the journal for"
    );

    let parent = CString::new("/").unwrap();
    for name in ["first.txt", "second.txt"] {
        let name = CString::new(name).unwrap();
        let record = unsafe { fs_ntfs_create_file_h(handle, parent.as_ptr(), name.as_ptr()) };
        assert!(record > 0, "fs_ntfs_create_file_h: {}", last_error());
    }
    unsafe { fs_ntfs_umount(handle) };

    let flags = volume_flags(&copy);
    assert_ne!(
        flags & MOUNTED_ON_NT4,
        0,
        "flags {flags:#06x}: the journal was left as it was"
    );
    let (record, mirror) = volume_record_and_mirror(&copy);
    assert!(
        record == mirror,
        "$MFTMirr's copy of $Volume disagrees with $MFT's"
    );
    let _ = std::fs::remove_file(&copy);
}

#[test]
fn retirement_retries_a_failed_mirror_write() {
    struct FailMirrorWrite {
        io: PathIo,
        mirror_at: u64,
        fail: bool,
    }
    impl BlockIo for FailMirrorWrite {
        fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
            self.io.read_exact_at(offset, buf)
        }
        fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
            if self.fail && offset == self.mirror_at {
                return Err("injected mirror write failure".into());
            }
            self.io.write_all_at(offset, buf)
        }
        fn size(&self) -> u64 {
            self.io.size()
        }
        fn sync(&mut self) -> Result<(), String> {
            self.io.sync()
        }
    }

    let copy = copy_of_fixture("usn_mirror_retry");
    let mut io = PathIo::open_rw(Path::new(&copy)).expect("open_rw");
    let params = fs_ntfs::mft_io::read_boot_params_io(&mut io).expect("boot parameters");
    let (mirror, _) =
        read::nonresident_contiguous_disk_range(&mut io, MFTMIRR_RECORD, AttrType::Data, None)
            .expect("mirror range");
    let mut io = FailMirrorWrite {
        io,
        mirror_at: mirror + VOLUME_RECORD * params.file_record_size,
        fail: true,
    };
    for _ in 0..2 {
        let error = fs_ntfs::fsck::retire_usn_journal_io(&mut io).expect_err("mirror must fail");
        assert!(error.to_string().contains("injected mirror write failure"));
    }
    io.fail = false;
    assert!(!fs_ntfs::fsck::retire_usn_journal_io(&mut io).expect("retry retirement"));
    drop(io);
    assert_ne!(volume_flags(&copy) & MOUNTED_ON_NT4, 0);
    let (record, mirror) = volume_record_and_mirror(&copy);
    assert_eq!(record, mirror, "retry must synchronize the mirror");
}

#[test]
fn a_volume_with_no_journal_is_written_as_before() {
    const SIZE: u64 = 32 * 1024 * 1024;
    let img = common::temp_image_path("usn_none");
    let f = std::fs::File::create(&img).expect("create image");
    f.set_len(SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, SIZE, 4096, 1024, Some("NOUSN"), None).expect("format");
    drop(io);
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    assert!(
        read::resolve_path(&mut io, JOURNAL).is_err(),
        "a fresh volume has a journal"
    );
    drop(io);

    let before = volume_flags(&img);
    let fs = Filesystem::mount_rw(&img).expect("mount_rw");
    let after_mount = volume_flags(&img);
    fs.create_file("/", "plain.txt").expect("create");
    assert_eq!(
        volume_flags(&img),
        after_mount,
        "a write to a volume with no journal changed its flags (were {before:#06x})"
    );
    assert_eq!(after_mount & MOUNTED_ON_NT4, 0);
    let _ = std::fs::remove_file(&img);
}
