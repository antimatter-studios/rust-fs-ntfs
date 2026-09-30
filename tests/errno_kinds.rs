//! Raise sites whose errno was the one their wording happened to earn under
//! the old keyword search, and is now the one the failure calls for (#394).
//!
//! The volumes are formatted in-process, so no fixture is needed.

#![allow(unused_unsafe)]

mod common;

use std::ffi::{c_void, CStr, CString};
use std::path::Path;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{
    fs_ntfs_create_file, fs_ntfs_last_errno, fs_ntfs_last_error, fs_ntfs_link, fs_ntfs_mkdir,
    fs_ntfs_rename, fs_ntfs_write_file_contents,
};

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn fresh_volume(tag: &str) -> String {
    let dst = common::temp_image_path(format!("errno_kinds_{tag}"));
    let f = std::fs::File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 4096, Some("KINDS"), Some(0x394)).expect("mkfs");
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

/// A file bigger than the free space is ENOSPC. The allocator's refusal,
/// "no contiguous free run of N clusters", matched none of the old
/// keywords and reached the caller as EIO.
#[test]
fn writing_more_than_the_volume_holds_is_enospc() {
    let img = fresh_volume("enospc");
    let created =
        unsafe { fs_ntfs_create_file(c(&img).as_ptr(), c("/").as_ptr(), c("big").as_ptr()) };
    assert!(created >= 0, "setup: {}", last_error());

    let data = vec![0x5Au8; 2 * VOL_SIZE as usize];
    let rc = unsafe {
        fs_ntfs_write_file_contents(
            c(&img).as_ptr(),
            c("/big").as_ptr(),
            data.as_ptr() as *const c_void,
            data.len() as u64,
        )
    };
    assert_eq!(rc, -1, "writing twice the volume's size succeeded");
    assert_eq!(
        unsafe { fs_ntfs_last_errno() },
        libc::ENOSPC,
        "message {:?}",
        last_error()
    );
}

/// Hard-linking a directory is refused, as link(2) refuses it: EPERM. The
/// message says "refusing", which the old search for "refuse" missed.
#[test]
fn hard_linking_a_directory_is_eperm() {
    let img = fresh_volume("linkdir");
    let dir = unsafe { fs_ntfs_mkdir(c(&img).as_ptr(), c("/").as_ptr(), c("d").as_ptr()) };
    assert!(dir >= 0, "setup: {}", last_error());

    let rc = unsafe {
        fs_ntfs_link(
            c(&img).as_ptr(),
            c("/d").as_ptr(),
            c("/").as_ptr(),
            c("e").as_ptr(),
        )
    };
    assert_eq!(rc, -1, "hard-linking a directory succeeded");
    assert_eq!(
        unsafe { fs_ntfs_last_errno() },
        libc::EPERM,
        "message {:?}",
        last_error()
    );
}

/// The root cannot be renamed: EPERM, not EIO.
#[test]
fn renaming_the_root_is_eperm() {
    let img = fresh_volume("renameroot");
    let rc = unsafe { fs_ntfs_rename(c(&img).as_ptr(), c("/").as_ptr(), c("x").as_ptr()) };
    assert_eq!(rc, -1, "renaming the root succeeded");
    assert_eq!(
        unsafe { fs_ntfs_last_errno() },
        libc::EPERM,
        "message {:?}",
        last_error()
    );
}
