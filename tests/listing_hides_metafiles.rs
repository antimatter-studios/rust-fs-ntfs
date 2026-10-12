//! A host's directory listing leaves the NTFS metafiles out (#499).
//!
//! Records 0 to 15 (`$MFT`, `$LogFile`, `$Volume`, `$Bitmap`, `$UpCase`,
//! `$Extend`, ...) are named in the root's index. Windows does not
//! enumerate them, ntfs-3g hides them unless mounted with
//! `show_sys_files`, and this crate's own `ls` leaves them out unless
//! `--all` asks. `facade::Filesystem::read_dir` and the C ABI's
//! `fs_ntfs_dir_open` listed them, so a host mounting through either showed
//! them to its user, and a select-all in the root reached every one.

#![allow(unused_unsafe)]

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::facade::Filesystem;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{
    fs_ntfs_dir_close, fs_ntfs_dir_next, fs_ntfs_dir_open, fs_ntfs_mount, fs_ntfs_umount,
};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const METAFILES: [&str; 11] = [
    "$MFT", "$MFTMirr", "$LogFile", "$Volume", "$AttrDef", "$Bitmap", "$Boot", "$BadClus",
    "$Secure", "$UpCase", "$Extend",
];

/// The C ABI's dirent, read through its C layout.
#[repr(C)]
struct DirentMirror {
    file_record_number: u64,
    file_type: u8,
    name_len: u16,
    name: [u8; 256],
}

fn volume_with_a_file() -> String {
    let img = common::temp_image_path("hide_metafiles");
    let f = std::fs::File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    fs_ntfs::write::create_file_io(&mut io, "/", "user.txt").expect("create /user.txt");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    img
}

fn check(listing: &[String], how: &str) {
    for name in [".", "..", "user.txt"] {
        assert!(
            listing.iter().any(|n| n == name),
            "{how} lists no {name}: {listing:?}"
        );
    }
    let shown: Vec<&&str> = METAFILES
        .iter()
        .filter(|m| listing.iter().any(|n| n == **m))
        .collect();
    assert!(
        shown.is_empty(),
        "{how} lists the metafiles {shown:?}: {listing:?}"
    );
}

#[test]
fn the_facade_listing_leaves_the_metafiles_out() {
    let img = volume_with_a_file();
    let fs = Filesystem::mount(&img).expect("mount");
    let names: Vec<String> = fs
        .read_dir("/")
        .expect("read_dir /")
        .into_iter()
        .map(|e| e.name)
        .collect();
    check(&names, "Filesystem::read_dir(\"/\")");
}

#[test]
fn the_c_abi_listing_leaves_the_metafiles_out() {
    let img = volume_with_a_file();
    let img_c = CString::new(img.as_str()).unwrap();
    let fs = unsafe { fs_ntfs_mount(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount {img}");
    let root = CString::new("/").unwrap();
    let it = unsafe { fs_ntfs_dir_open(fs, root.as_ptr() as *const c_char) };
    assert!(!it.is_null(), "dir_open /");
    let mut names = Vec::new();
    loop {
        let de = unsafe { fs_ntfs_dir_next(it) };
        if de.is_null() {
            break;
        }
        let mirror = unsafe { &*(de as *const DirentMirror) };
        let name = unsafe { CStr::from_ptr(mirror.name.as_ptr() as *const c_char) };
        names.push(name.to_string_lossy().into_owned());
    }
    unsafe { fs_ntfs_dir_close(it) };
    unsafe { fs_ntfs_umount(fs) };
    check(&names, "fs_ntfs_dir_open(\"/\")");
}

/// The metafiles are still there to be named: hiding them from a listing
/// does not take them out of the volume.
#[test]
fn a_metafile_is_still_found_by_name() {
    let img = volume_with_a_file();
    let fs = Filesystem::mount(&img).expect("mount");
    fs.stat("/$MFT").expect("stat /$MFT");
    let extend = fs.read_dir("/$Extend").expect("read_dir /$Extend");
    assert!(extend.iter().any(|e| e.name == "."), "/$Extend still lists");
}
