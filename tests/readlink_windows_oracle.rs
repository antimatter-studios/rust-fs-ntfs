//! `fs_ntfs_readlink` graded by Windows.
//!
//! `test-disks/build-windows-native-read-fixtures.ps1` has Windows create
//! symlinks on a volume Windows formatted, then records the target Windows
//! itself reports for each one (.NET `LinkTarget`, the reparse point's
//! PrintName) in `test-disks/ntfs-symlink.targets.tsv`. Neither the reparse
//! bytes nor the expected answer come from this crate, so a misreading of the
//! symlink reparse buffer cannot be baked into both sides.
//!
//! CI builds the fixture in its Windows job and hands it to the Linux
//! integration job. Without it these tests fail, naming the builder.
//!
//! A relative link's target is a path the host walks, so readlink gives
//! it with `/` where Windows reports `\` (#505). The builder names its
//! relative links `rel-*`; `abs-dir` names a drive and keeps Windows' form.

#![allow(unused_unsafe)]

mod common;

use fs_ntfs::{fs_ntfs_last_error, fs_ntfs_mount, fs_ntfs_readlink, fs_ntfs_umount};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;

const IMG: &str = "test-disks/ntfs-symlink.img";
const REPORT: &str = "test-disks/ntfs-symlink.targets.tsv";

fn windows_report() -> Vec<(String, String)> {
    for p in [IMG, REPORT] {
        assert!(
            Path::new(p).exists(),
            "missing {p}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
        );
    }
    let text = std::fs::read_to_string(REPORT).expect("read targets report");
    let rows: Vec<(String, String)> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (name, target) = l.split_once('\t').expect("name<TAB>target");
            (name.to_string(), target.to_string())
        })
        .collect();
    // Three links are authored; an empty or short report is a broken
    // builder, not a pass.
    assert_eq!(rows.len(), 3, "Windows report rows: {rows:?}");
    rows
}

#[test]
fn readlink_matches_the_target_windows_reports() {
    let rows = windows_report();
    let img = CString::new(IMG).unwrap();
    let fs = unsafe { fs_ntfs_mount(img.as_ptr()) };
    assert!(!fs.is_null(), "mount {IMG}");
    let mut mismatches = Vec::new();
    for (name, windows) in &rows {
        let want = &if name.starts_with("rel-") {
            windows.replace('\\', "/")
        } else {
            windows.clone()
        };
        let path = CString::new(format!("/{name}")).unwrap();
        // Exactly large enough: the contract's tightest successful fit.
        let mut buf = vec![0xA5u8; want.len() + 1];
        let rc = unsafe {
            fs_ntfs_readlink(
                fs,
                path.as_ptr(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
            )
        };
        if rc < 0 {
            let err = unsafe { CStr::from_ptr(fs_ntfs_last_error()) }.to_string_lossy();
            mismatches.push(format!("{name}: readlink failed: {err}"));
            continue;
        }
        let got = String::from_utf8_lossy(&buf[..rc as usize]).into_owned();
        if rc as usize != want.len() || got != *want || buf[rc as usize] != 0 {
            mismatches.push(format!(
                "{name}: driver {got:?} (rc {rc}), Windows {want:?}"
            ));
        }
    }
    unsafe { fs_ntfs_umount(fs) };
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

/// The reparse bit of `$FILE_NAME.file_attributes`, and the word after it,
/// which holds the packed EA size, or the reparse tag when that bit is set.
const FN_FILE_ATTRIBUTES: usize = 0x38;
const FN_EA_OR_REPARSE: usize = 0x3C;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// What the root's `$I30` entry for `name`, and the link's own record's
/// `$FILE_NAME`, carry in their duplicated reparse fields:
/// `(entry reparse bit, entry tag word, record reparse bit, record tag word)`.
fn link_directory_fields(io: &mut fs_ntfs::block_io::PathIo, name: &str) -> (bool, u32, bool, u32) {
    use fs_ntfs::{attr_io, idx_block, index_io, mft_io, upcase, write};
    const ROOT: u64 = 5;
    const IE_KEY_START: usize = 0x10;
    let word = |buf: &[u8], at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
    let upcase = upcase::UpcaseTable::load_io(io).expect("$UpCase");
    let (_, root) = mft_io::read_mft_record_io(io, ROOT).expect("root record");
    let (node, entry) = match index_io::find_index_entry(&root, name, Some(&upcase)).unwrap() {
        Some(entry) => (root, entry),
        None => {
            let ia = idx_block::load_for_directory_io(io, ROOT).expect("root $I30 blocks");
            ia.allocated_block_vcns()
                .into_iter()
                .find_map(|vcn| {
                    let block = idx_block::read_indx_block_io(io, &ia, vcn).unwrap();
                    index_io::find_entry_in_indx_block(&block, name, Some(&upcase))
                        .unwrap()
                        .map(|entry| (block, entry))
                })
                .unwrap_or_else(|| panic!("the root has no entry for {name}"))
        }
    };
    let key = entry.record_offset + IE_KEY_START;
    let entry_attributes = word(&node, key + FN_FILE_ATTRIBUTES);
    let entry_tag = word(&node, key + FN_EA_OR_REPARSE);

    let rec = write::resolve_path_to_record_number_io(io, &format!("/{name}")).expect("resolve");
    let (_, record) = mft_io::read_mft_record_io(io, rec).expect("link record");
    let names: Vec<_> = attr_io::iter_attributes(&record)
        .filter(|a| a.type_code == attr_io::AttrType::FileName as u32)
        .collect();
    assert_eq!(names.len(), 1, "{name}: one $FILE_NAME expected");
    let value = attr_io::resident_value_start(&names[0]).expect("resident $FILE_NAME");
    let record_attributes = word(&record, value + FN_FILE_ATTRIBUTES);
    let record_tag = word(&record, value + FN_EA_OR_REPARSE);
    (
        entry_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        entry_tag,
        record_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        record_tag,
    )
}

/// Windows lists a directory from the copy of each `$FILE_NAME` in its
/// `$I30` entries, so a symlink whose entry lacks the reparse bit and tag
/// is enumerated as a plain file, and opening it follows the link (#1357).
/// A symlink this crate writes carries in its directory entry, and in its
/// record's `$FILE_NAME`, exactly what Windows' own symlinks carry.
#[test]
fn a_written_symlink_carries_the_directory_fields_windows_writes() {
    use fs_ntfs::block_io::{BlockIo, PathIo};
    let rows = windows_report();
    let mut windows_io = PathIo::open_ro(Path::new(IMG)).expect("open the Windows image");

    let ours = common::temp_image_path("symlink_directory_fields");
    const VOL_SIZE: u64 = 16 * 1024 * 1024;
    std::fs::File::create(&ours)
        .and_then(|f| f.set_len(VOL_SIZE))
        .expect("create image");
    let mut io = PathIo::open_rw(Path::new(&ours)).expect("open_rw");
    fs_ntfs::mkfs::format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1))
        .expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    let mut mismatches = Vec::new();
    // The file links; `abs-dir` names a directory, which this crate's
    // symlink writer refuses.
    for (name, target) in rows.iter().filter(|(name, _)| name.starts_with("rel-")) {
        let windows = link_directory_fields(&mut windows_io, name);
        assert!(
            windows.0,
            "{name}: Windows' own entry has no reparse bit: {windows:?}"
        );
        fs_ntfs::write::create_symlink_io(&mut io, "/", name, &target.replace('\\', "/"), true)
            .expect("create_symlink_io");
        let written = link_directory_fields(&mut io, name);
        if written != windows {
            mismatches.push(format!(
                "{name}: written {written:x?}, Windows {windows:x?} \
                 (entry bit, entry tag, record bit, record tag)"
            ));
        }
    }
    drop(io);
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}
