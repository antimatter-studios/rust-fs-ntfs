//! The C-ABI contract of `fs_ntfs_readlink`, shared by every driver in the
//! family:
//!
//! * success returns the target's length in bytes (excluding the NUL), as
//!   Linux `readlink(2)` does, and writes the target plus a NUL into `buf`;
//! * `bufsize < length + 1` returns -1 with errno `ERANGE`, names the size
//!   needed in the message, and writes nothing into `buf` — never a silent
//!   truncation;
//! * NULL `fs`/`path`/`buf` returns -1 with `EINVAL`;
//! * every other failure returns -1 with errno set.
//!
//! Fixture-free: each test formats its own volume and writes the symlink
//! through the crate, so the suite runs on any host. The target the driver
//! reads is checked against one Windows authored in
//! `tests/readlink_windows_oracle.rs`.

#![allow(unused_unsafe)]

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::facade::Filesystem;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{
    fs_ntfs_clear_last_error, fs_ntfs_last_errno, fs_ntfs_last_error, fs_ntfs_mount,
    fs_ntfs_readlink, fs_ntfs_umount, FsNtfsHandle,
};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;

const VOL_SIZE: u64 = 32 * 1024 * 1024;
const ENOENT: i32 = 2;
const EINVAL: i32 = 22;
const ERANGE: i32 = 34;

/// The byte every untouched position of a caller's buffer keeps.
const POISON: u8 = 0xA5;

/// The target as the driver reports it: the `\??\` NT prefix stripped.
const TARGET: &str = r"C:\Windows\System32";

fn volume_with_link(tag: &str) -> String {
    let img = common::temp_image_path(format!("capi_readlink_{tag}"));
    let f = std::fs::File::create(&img).expect("create");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 4096, Some("RL"), Some(0x5EAD_1111))
        .expect("format");
    io.sync().expect("sync");
    drop(io);

    let fs = Filesystem::mount_rw(&img).expect("mount_rw");
    fs.create_symlink("/", "link", &format!(r"\??\{TARGET}"), false)
        .expect("create_symlink");
    fs.create_file("/", "plain.txt").expect("create_file");
    drop(fs);
    img
}

struct Mounted(*mut FsNtfsHandle);

impl Mounted {
    fn new(img: &str) -> Self {
        let c = CString::new(img).unwrap();
        let fs = unsafe { fs_ntfs_mount(c.as_ptr()) };
        assert!(!fs.is_null(), "mount {img}");
        Mounted(fs)
    }

    /// Call readlink into a poisoned buffer of `bufsize` bytes, after
    /// clearing errno so a stale value cannot satisfy an assertion.
    fn readlink(&self, path: &str, bufsize: usize) -> (i32, Vec<u8>) {
        let p = CString::new(path).unwrap();
        // One guard byte past `bufsize` catches a write beyond the buffer.
        let mut buf = vec![POISON; bufsize + 1];
        unsafe { fs_ntfs_clear_last_error() };
        let rc = unsafe {
            fs_ntfs_readlink(self.0, p.as_ptr(), buf.as_mut_ptr() as *mut c_char, bufsize)
        };
        assert_eq!(buf[bufsize], POISON, "readlink wrote past bufsize");
        buf.truncate(bufsize);
        (rc, buf)
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        unsafe { fs_ntfs_umount(self.0) };
    }
}

fn last_error() -> String {
    unsafe { CStr::from_ptr(fs_ntfs_last_error()) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn success_returns_length_and_writes_target_plus_nul() {
    let img = volume_with_link("success");
    let m = Mounted::new(&img);
    let (rc, buf) = m.readlink("/link", 256);
    assert_eq!(rc, TARGET.len() as i32, "returns the length, not 0");
    assert_eq!(&buf[..TARGET.len()], TARGET.as_bytes());
    assert_eq!(buf[TARGET.len()], 0, "NUL terminator after the target");
}

#[test]
fn exact_fit_succeeds() {
    let img = volume_with_link("exact_fit");
    let m = Mounted::new(&img);
    let (rc, buf) = m.readlink("/link", TARGET.len() + 1);
    assert_eq!(rc, TARGET.len() as i32);
    assert_eq!(&buf[..TARGET.len()], TARGET.as_bytes());
    assert_eq!(buf[TARGET.len()], 0);
}

#[test]
fn one_byte_short_is_erange_and_leaves_buffer_untouched() {
    let img = volume_with_link("one_short");
    let m = Mounted::new(&img);
    let (rc, buf) = m.readlink("/link", TARGET.len());
    assert_eq!(rc, -1);
    assert_eq!(unsafe { fs_ntfs_last_errno() }, ERANGE, "{}", last_error());
    assert!(
        last_error().contains(&(TARGET.len() + 1).to_string()),
        "message names the size needed: {}",
        last_error()
    );
    assert!(
        buf.iter().all(|&b| b == POISON),
        "no bytes written: {buf:?}"
    );
}

#[test]
fn zero_bufsize_is_erange() {
    let img = volume_with_link("zero_buf");
    let m = Mounted::new(&img);
    let (rc, _) = m.readlink("/link", 0);
    assert_eq!(rc, -1);
    assert_eq!(unsafe { fs_ntfs_last_errno() }, ERANGE, "{}", last_error());
}

#[test]
fn non_symlink_is_einval_like_readlink_2() {
    let img = volume_with_link("non_symlink");
    let m = Mounted::new(&img);
    let (rc, buf) = m.readlink("/plain.txt", 256);
    assert_eq!(rc, -1);
    assert_eq!(unsafe { fs_ntfs_last_errno() }, EINVAL, "{}", last_error());
    assert!(buf.iter().all(|&b| b == POISON));
}

#[test]
fn missing_path_is_enoent() {
    let img = volume_with_link("missing");
    let m = Mounted::new(&img);
    let (rc, _) = m.readlink("/no-such-link", 256);
    assert_eq!(rc, -1);
    assert_eq!(unsafe { fs_ntfs_last_errno() }, ENOENT, "{}", last_error());
}

#[test]
fn null_arguments_are_einval() {
    let img = volume_with_link("nulls");
    let m = Mounted::new(&img);
    let p = CString::new("/link").unwrap();
    let mut buf = [0u8; 64];
    let bp = buf.as_mut_ptr() as *mut c_char;
    for (fs, path, b) in [
        (std::ptr::null_mut(), p.as_ptr(), bp),
        (m.0, std::ptr::null(), bp),
        (m.0, p.as_ptr(), std::ptr::null_mut()),
    ] {
        unsafe { fs_ntfs_clear_last_error() };
        let rc = unsafe { fs_ntfs_readlink(fs, path, b, buf.len()) };
        assert_eq!(rc, -1);
        assert_eq!(unsafe { fs_ntfs_last_errno() }, EINVAL, "{}", last_error());
    }
}
