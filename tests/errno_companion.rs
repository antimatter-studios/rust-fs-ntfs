//! Tests for fs_ntfs_last_errno + fs_ntfs_clear_last_error (§4.1).

#![allow(unused_unsafe)]

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{
    fs_ntfs_clear_dirty, fs_ntfs_clear_last_error, fs_ntfs_dir_close, fs_ntfs_dir_next,
    fs_ntfs_dir_open, fs_ntfs_dir_skipped, fs_ntfs_get_volume_info, fs_ntfs_get_volume_info_v2,
    fs_ntfs_last_errno, fs_ntfs_last_error, fs_ntfs_mount, fs_ntfs_read_file, fs_ntfs_stat,
    fs_ntfs_umount, fs_ntfs_unlink, fs_ntfs_write_resident_contents, FsNtfsAttr, FsNtfsHandle,
    FsNtfsVolumeInfo, FsNtfsVolumeInfoV2,
};
use std::ffi::{c_char, CStr, CString};
use std::path::Path;

const BASIC_IMG: &str = "test-disks/ntfs-basic.img";
const VOL_SIZE: u64 = 16 * 1024 * 1024;
const ENOENT: i32 = 2;
const EINVAL: i32 = 22;
const EIO: i32 = 5;

fn working_copy(tag: &str) -> String {
    let dst = common::temp_image_path(format!("errno_{tag}"));
    std::fs::copy(BASIC_IMG, &dst).expect("copy");
    dst
}

#[test]
fn clean_state_is_zero() {
    unsafe { fs_ntfs_clear_last_error() };
    assert_eq!(unsafe { fs_ntfs_last_errno() }, 0);
}

#[test]
fn null_input_sets_einval() {
    unsafe { fs_ntfs_clear_last_error() };
    let rc = unsafe { fs_ntfs_clear_dirty(std::ptr::null()) };
    assert_eq!(rc, -1);
    assert_eq!(unsafe { fs_ntfs_last_errno() }, EINVAL);
}

#[test]
fn missing_path_sets_enoent() {
    unsafe { fs_ntfs_clear_last_error() };
    let img = working_copy("missing");
    let img_c = CString::new(img.as_str()).unwrap();
    let path_c = CString::new("/nonexistent.xx").unwrap();
    let rc = unsafe { fs_ntfs_unlink(img_c.as_ptr(), path_c.as_ptr()) };
    assert_eq!(rc, -1);
    assert_eq!(unsafe { fs_ntfs_last_errno() }, ENOENT);
}

#[test]
fn clear_resets_errno() {
    unsafe { fs_ntfs_clear_last_error() };
    // Force an error.
    let rc = unsafe { fs_ntfs_clear_dirty(std::ptr::null()) };
    assert_eq!(rc, -1);
    assert_ne!(unsafe { fs_ntfs_last_errno() }, 0);
    unsafe { fs_ntfs_clear_last_error() };
    assert_eq!(unsafe { fs_ntfs_last_errno() }, 0);
}

#[test]
fn fallback_eio_for_generic_errors() {
    // Use fs_ntfs_write_resident_contents with non-zero len but
    // null buf — that's an explicit null-buf-with-non-zero-len
    // rejection, which the impl classifies as EINVAL.
    unsafe { fs_ntfs_clear_last_error() };
    let img = working_copy("fallback");
    let img_c = CString::new(img.as_str()).unwrap();
    let p_c = CString::new("/hello.txt").unwrap();
    let rc = unsafe {
        fs_ntfs_write_resident_contents(img_c.as_ptr(), p_c.as_ptr(), std::ptr::null(), 10)
    };
    assert_eq!(rc, -1);
    let errno = unsafe { fs_ntfs_last_errno() };
    assert_eq!(errno, EINVAL);
    // silence unused-const
    let _ = EIO;
}

// --- the errno describes the most recent call (#381) ----------------------
//
// These format their own volume, so they need no fixture.

fn fresh_volume(tag: &str) -> String {
    let dst = common::temp_image_path(format!("errno_fresh_{tag}"));
    let f = std::fs::File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 4096, Some("ERRNO"), Some(0x5EED)).expect("mkfs");
    io.sync().expect("sync");
    dst
}

fn mount(img: &str) -> *mut FsNtfsHandle {
    let c = CString::new(img).unwrap();
    let fs = unsafe { fs_ntfs_mount(c.as_ptr()) };
    assert!(!fs.is_null(), "mount failed: {}", last_error());
    fs
}

fn last_error() -> String {
    unsafe { CStr::from_ptr(fs_ntfs_last_error()) }
        .to_string_lossy()
        .into_owned()
}

fn zeroed_attr() -> FsNtfsAttr {
    // SAFETY: a repr(C) struct of integers, for which all-zero is valid.
    unsafe { std::mem::zeroed() }
}

/// Leave ENOENT behind on this thread, the way a routine lookup of a name
/// that is not there does, so a later call that fails to record its own
/// errno is caught reporting this one.
fn plant_enoent(fs: *mut FsNtfsHandle) {
    let missing = CString::new("/no-such-file").unwrap();
    let mut attr = zeroed_attr();
    assert_eq!(unsafe { fs_ntfs_stat(fs, missing.as_ptr(), &mut attr) }, -1);
    assert_eq!(
        unsafe { fs_ntfs_last_errno() },
        ENOENT,
        "the planted failure must be ENOENT"
    );
}

/// The header says 0 means no error: after a successful call the errno is
/// 0, however an earlier call on the thread ended. Before, the errno of
/// the thread's last failure stayed until the next one.
#[test]
fn a_successful_call_resets_the_errno_to_zero() {
    let img = fresh_volume("success");
    let fs = mount(&img);
    plant_enoent(fs);
    let message = last_error();

    let root = CString::new("/").unwrap();
    let mut attr = zeroed_attr();
    assert_eq!(
        unsafe { fs_ntfs_stat(fs, root.as_ptr(), &mut attr) },
        0,
        "stat of / failed: {}",
        last_error()
    );
    assert_eq!(
        unsafe { fs_ntfs_last_errno() },
        0,
        "a successful stat must reset the errno"
    );
    assert_eq!(
        last_error(),
        message,
        "the message describes the most recent failure and is kept until the next one"
    );
    unsafe { fs_ntfs_umount(fs) };
}

/// A NULL from `fs_ntfs_dir_next` is either the end of the listing or a
/// failure, and the errno is what tells them apart: 0 at a clean end, even
/// on a thread where an earlier lookup failed.
#[test]
fn a_clean_end_of_directory_is_errno_zero_after_an_earlier_failure() {
    let img = fresh_volume("dirend");
    let fs = mount(&img);
    plant_enoent(fs);

    let root = CString::new("/").unwrap();
    let iter = unsafe { fs_ntfs_dir_open(fs, root.as_ptr()) };
    assert!(!iter.is_null(), "opening / failed: {}", last_error());
    let mut entries = 0;
    while !unsafe { fs_ntfs_dir_next(iter) }.is_null() {
        entries += 1;
    }
    assert!(entries >= 2, "the root listed fewer than . and ..");
    assert_eq!(
        unsafe { fs_ntfs_last_errno() },
        0,
        "a clean end of directory must read as errno 0, not the earlier failure's"
    );
    unsafe { fs_ntfs_dir_close(iter) };
    unsafe { fs_ntfs_umount(fs) };
}

/// A call to make, named for the failure report; true when it failed.
type Case<'a> = (&'static str, Box<dyn Fn() -> bool + 'a>);

/// Every failure records an errno of its own. Each of these used to return
/// its sentinel without recording anything, so the caller read the ENOENT
/// an unrelated earlier call left behind.
#[test]
fn every_rejected_argument_records_einval() {
    let img = fresh_volume("einval");
    let fs = mount(&img);
    let root = CString::new("/").unwrap();
    // 0xFF never appears in UTF-8.
    let not_utf8: &[u8] = b"/\xff\xfe\0";
    let not_utf8 = not_utf8.as_ptr() as *const c_char;
    let null_fs: *mut FsNtfsHandle = std::ptr::null_mut();
    let mut buf = [0u8; 16];
    let buf_ptr = buf.as_mut_ptr() as *mut std::ffi::c_void;

    let cases: Vec<Case> = vec![
        (
            "get_volume_info(NULL fs)",
            Box::new(|| {
                let mut info: FsNtfsVolumeInfo = unsafe { std::mem::zeroed() };
                unsafe { fs_ntfs_get_volume_info(null_fs, &mut info) == -1 }
            }),
        ),
        (
            "get_volume_info(NULL info)",
            Box::new(|| unsafe { fs_ntfs_get_volume_info(fs, std::ptr::null_mut()) == -1 }),
        ),
        (
            "get_volume_info_v2(NULL info)",
            Box::new(|| unsafe {
                fs_ntfs_get_volume_info_v2(fs, std::ptr::null_mut::<FsNtfsVolumeInfoV2>()) == -1
            }),
        ),
        (
            "stat(NULL fs)",
            Box::new(|| unsafe {
                let mut a = zeroed_attr();
                fs_ntfs_stat(null_fs, root.as_ptr(), &mut a) == -1
            }),
        ),
        (
            "stat(NULL attr)",
            Box::new(|| unsafe { fs_ntfs_stat(fs, root.as_ptr(), std::ptr::null_mut()) == -1 }),
        ),
        (
            "stat(non-UTF-8 path)",
            Box::new(|| unsafe {
                let mut a = zeroed_attr();
                fs_ntfs_stat(fs, not_utf8, &mut a) == -1
            }),
        ),
        (
            "dir_open(NULL path)",
            Box::new(|| unsafe { fs_ntfs_dir_open(fs, std::ptr::null()).is_null() }),
        ),
        (
            "dir_open(non-UTF-8 path)",
            Box::new(|| unsafe { fs_ntfs_dir_open(fs, not_utf8).is_null() }),
        ),
        (
            "dir_next(NULL iter)",
            Box::new(|| unsafe { fs_ntfs_dir_next(std::ptr::null_mut()).is_null() }),
        ),
        (
            "dir_skipped(NULL iter)",
            Box::new(|| unsafe { fs_ntfs_dir_skipped(std::ptr::null()) == -1 }),
        ),
        (
            "read_file(NULL buf)",
            Box::new(|| unsafe {
                fs_ntfs_read_file(fs, root.as_ptr(), std::ptr::null_mut(), 0, 16) == -1
            }),
        ),
        (
            "read_file(non-UTF-8 path)",
            Box::new(|| unsafe { fs_ntfs_read_file(fs, not_utf8, buf_ptr, 0, 16) == -1 }),
        ),
        (
            "read_file(length over isize::MAX)",
            Box::new(|| unsafe {
                fs_ntfs_read_file(fs, root.as_ptr(), buf_ptr, 0, u64::MAX) == -1
            }),
        ),
    ];

    let mut failures = Vec::new();
    for (what, call) in &cases {
        plant_enoent(fs);
        if !call() {
            failures.push(format!("{what}: did not fail"));
            continue;
        }
        let errno = unsafe { fs_ntfs_last_errno() };
        if errno != EINVAL {
            failures.push(format!("{what}: errno {errno}, message {:?}", last_error()));
        }
    }
    unsafe { fs_ntfs_umount(fs) };
    assert!(
        failures.is_empty(),
        "{} of {} rejected arguments did not record EINVAL:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
