//! The errno is decided where the error is raised, not read back out of the
//! message (#382).
//!
//! Many messages quote a name the caller chose -- `parent '{path}' is not a
//! directory`, `resolve_path: '{component}' not found` -- so when the errno
//! was recovered by searching the message for keywords, the caller's name
//! could decide it. Each test here names its files after those keywords and
//! checks the errno is the one the failure calls for, whatever the name.
//!
//! The volumes are formatted in-process, so no fixture is needed.

#![allow(unused_unsafe)]

mod common;

use std::ffi::{c_int, CStr, CString};
use std::path::Path;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{
    fs_ntfs_create_file, fs_ntfs_last_errno, fs_ntfs_last_error, fs_ntfs_mkdir, fs_ntfs_mount,
    fs_ntfs_rmdir, fs_ntfs_stat, fs_ntfs_umount, FsNtfsAttr,
};

const VOL_SIZE: u64 = 16 * 1024 * 1024;

/// Names that each contain a phrase the old keyword search mapped to an
/// errno of its own, plus one that contains none.
const NAMES: &[&str] = &[
    "plain",
    "not found",
    "already exists",
    "full",
    "no room",
    "invalid",
    "is a directory",
    "not empty",
    "refused",
    "permission",
];

fn fresh_volume(tag: &str) -> String {
    let dst = common::temp_image_path(format!("errno_as_data_{tag}"));
    let f = std::fs::File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 4096, Some("ERRNO"), Some(0x382)).expect("mkfs");
    io.sync().expect("sync");
    dst
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn last_error() -> String {
    unsafe { CStr::from_ptr(fs_ntfs_last_error()) }
        .to_string_lossy()
        .into_owned()
}

fn create_file(img: &str, parent: &str, name: &str) -> i64 {
    unsafe { fs_ntfs_create_file(c(img).as_ptr(), c(parent).as_ptr(), c(name).as_ptr()) }
}

/// Check every `(what, errno)` against `want`, and report every mismatch at
/// once rather than the first.
fn assert_all(want: c_int, want_name: &str, got: Vec<(String, c_int, String)>) {
    let wrong: Vec<String> = got
        .iter()
        .filter(|(_, errno, _)| *errno != want)
        .map(|(what, errno, msg)| format!("{what}: errno {errno}, message {msg:?}"))
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} should be {want_name} ({want}):\n{}",
        wrong.len(),
        got.len(),
        wrong.join("\n")
    );
}

/// Creating a file under a regular file is ENOTDIR. The message quotes the
/// parent's path, and a parent named `already exists` used to make it
/// EEXIST, one named `full` ENOSPC, one named `invalid` EINVAL.
#[test]
fn creating_under_a_regular_file_is_enotdir_whatever_the_file_is_called() {
    let img = fresh_volume("create_under_file");
    let mut got = Vec::new();
    for name in NAMES {
        assert!(
            create_file(&img, "/", name) >= 0,
            "setup: creating /{name} failed: {}",
            last_error()
        );
        let rc = create_file(&img, &format!("/{name}"), "x");
        assert_eq!(
            rc, -1,
            "creating a file under the regular file /{name} succeeded"
        );
        got.push((
            format!("/{name}/x"),
            unsafe { fs_ntfs_last_errno() },
            last_error(),
        ));
    }
    assert_all(libc::ENOTDIR, "ENOTDIR", got);
}

/// A path that runs through a regular file is ENOTDIR at lookup too. The
/// message quotes the component after the file, which used to decide it.
#[test]
fn a_path_through_a_regular_file_is_enotdir_whatever_comes_next() {
    let img = fresh_volume("stat_through_file");
    assert!(
        create_file(&img, "/", "file") >= 0,
        "setup: {}",
        last_error()
    );
    let fs = unsafe { fs_ntfs_mount(c(&img).as_ptr()) };
    assert!(!fs.is_null(), "mount failed: {}", last_error());

    let mut got = Vec::new();
    for name in NAMES {
        let path = format!("/file/{name}");
        let mut attr: FsNtfsAttr = unsafe { std::mem::zeroed() };
        let rc = unsafe { fs_ntfs_stat(fs, c(&path).as_ptr(), &mut attr) };
        assert_eq!(rc, -1, "stat of {path} succeeded");
        got.push((path, unsafe { fs_ntfs_last_errno() }, last_error()));
    }
    unsafe { fs_ntfs_umount(fs) };
    assert_all(libc::ENOTDIR, "ENOTDIR", got);
}

/// A name that is not there is ENOENT, whatever it is called.
#[test]
fn a_missing_name_is_enoent_whatever_it_is_called() {
    let img = fresh_volume("missing");
    let fs = unsafe { fs_ntfs_mount(c(&img).as_ptr()) };
    assert!(!fs.is_null(), "mount failed: {}", last_error());

    let mut got = Vec::new();
    for name in NAMES {
        let path = format!("/{name}");
        let mut attr: FsNtfsAttr = unsafe { std::mem::zeroed() };
        let rc = unsafe { fs_ntfs_stat(fs, c(&path).as_ptr(), &mut attr) };
        assert_eq!(rc, -1, "stat of {path} succeeded on an empty volume");
        got.push((path, unsafe { fs_ntfs_last_errno() }, last_error()));
    }
    unsafe { fs_ntfs_umount(fs) };
    assert_all(libc::ENOENT, "ENOENT", got);
}

/// A name that is already there is EEXIST, whatever it is called.
#[test]
fn an_existing_name_is_eexist_whatever_it_is_called() {
    let img = fresh_volume("exists");
    let mut got = Vec::new();
    for name in NAMES {
        assert!(create_file(&img, "/", name) >= 0, "setup: {}", last_error());
        assert_eq!(
            create_file(&img, "/", name),
            -1,
            "creating /{name} twice succeeded"
        );
        got.push((
            format!("/{name}"),
            unsafe { fs_ntfs_last_errno() },
            last_error(),
        ));
    }
    assert_all(libc::EEXIST, "EEXIST", got);
}

/// ENOTEMPTY is the value of the platform the crate was built for. It was
/// hard-coded to 66, which is macOS's: Linux's is 39, so a Linux caller
/// comparing against its own `<errno.h>` never saw ENOTEMPTY.
#[test]
fn removing_a_directory_that_is_not_empty_is_the_platforms_enotempty() {
    let img = fresh_volume("notempty");
    let dir = unsafe { fs_ntfs_mkdir(c(&img).as_ptr(), c("/").as_ptr(), c("d").as_ptr()) };
    assert!(dir >= 0, "setup: mkdir /d failed: {}", last_error());
    assert!(create_file(&img, "/d", "f") >= 0, "setup: {}", last_error());

    let rc = unsafe { fs_ntfs_rmdir(c(&img).as_ptr(), c("/d").as_ptr()) };
    assert_eq!(rc, -1, "rmdir of a directory holding a file succeeded");
    assert_eq!(
        unsafe { fs_ntfs_last_errno() },
        libc::ENOTEMPTY,
        "message {:?}",
        last_error()
    );
}
