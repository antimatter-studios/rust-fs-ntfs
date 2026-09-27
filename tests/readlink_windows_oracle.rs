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

#![allow(unused_unsafe)]

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
    for (name, want) in &rows {
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
