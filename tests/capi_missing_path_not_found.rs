//! A missing path reaches C callers as ENOENT from every named-stream,
//! extended-attribute and security-descriptor entry point, and the refused
//! call leaves the volume byte for byte as it was (#1402, PLATFORM-03).
//!
//! The control is a path that runs through a file: the same entry points
//! answer ENOTDIR, so ENOENT is not a blanket errno for a failed lookup.

mod common;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{
    fs_ntfs_clear_last_error, fs_ntfs_delete_named_stream, fs_ntfs_last_errno,
    fs_ntfs_list_ea_keys, fs_ntfs_list_named_streams, fs_ntfs_read_security_descriptor,
    fs_ntfs_read_security_id, fs_ntfs_remove_ea, fs_ntfs_write_ea, fs_ntfs_write_named_stream,
};
use std::ffi::{c_char, c_int, CString};
use std::path::Path;
use std::ptr::{null, null_mut};

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const ENOENT: i32 = 2;
const ENOTDIR: i32 = 20;

/// A fresh volume holding one file, `/plain`.
fn volume(tag: &str) -> String {
    let image = common::temp_image_path(format!("capi_not_found_{tag}"));
    let file = std::fs::File::create(&image).expect("create image");
    file.set_len(VOL_SIZE).expect("size image");
    drop(file);
    let mut io = PathIo::open_rw(Path::new(&image)).expect("open image");
    format_filesystem(&mut io, VOL_SIZE, 4096, 4096, Some("NOTFOUND"), Some(1402))
        .expect("format image");
    drop(io);
    fs_ntfs::write::create_file(Path::new(&image), "/", "plain").expect("create /plain");
    image
}

/// Every stream, EA and security entry point, called on `path`.
fn calls(image: *const c_char, path: *const c_char) -> Vec<(&'static str, c_int, i32)> {
    let name = c"user.note".as_ptr();
    let mut total = 0usize;
    let mut id = 0u32;
    let mut out = Vec::new();
    macro_rules! call {
        ($function:ident($($argument:expr),*)) => {{
            fs_ntfs_clear_last_error();
            let status = $function($($argument),*);
            out.push((stringify!($function), status, fs_ntfs_last_errno()));
        }};
    }
    call!(fs_ntfs_list_named_streams(
        image,
        path,
        null_mut(),
        0,
        &mut total
    ));
    call!(fs_ntfs_write_named_stream(image, path, name, null(), 0));
    call!(fs_ntfs_delete_named_stream(image, path, name));
    call!(fs_ntfs_list_ea_keys(image, path, null_mut(), 0, &mut total));
    call!(fs_ntfs_write_ea(image, path, name, null(), 0, 0));
    call!(fs_ntfs_remove_ea(image, path, name));
    call!(fs_ntfs_read_security_descriptor(
        image,
        path,
        null_mut(),
        0,
        &mut total
    ));
    call!(fs_ntfs_read_security_id(image, path, &mut id));
    out
}

fn assert_refused(tag: &str, paths: &[&str], errno: i32) {
    let image = volume(tag);
    let before = common::temp_image_path(format!("capi_not_found_{tag}_before"));
    std::fs::copy(&image, &before).expect("keep the volume as it was");
    let image_c = CString::new(image.as_str()).unwrap();
    for path in paths {
        let path_c = CString::new(*path).unwrap();
        let results = calls(image_c.as_ptr(), path_c.as_ptr());
        assert_eq!(results.len(), 8, "every entry point was called");
        for (function, status, got) in results {
            assert_eq!(status, -1, "{function}({path}) status");
            assert_eq!(got, errno, "{function}({path}) errno");
        }
    }
    let difference = common::first_image_difference(&before, &image).expect("compare images");
    assert!(
        difference.is_none(),
        "a refused call changed the volume: {difference:?}"
    );
}

#[test]
fn a_missing_leaf_or_parent_is_enoent_and_writes_nothing() {
    assert_refused("missing", &["/missing", "/missing/f"], ENOENT);
}

#[test]
fn a_path_through_a_file_is_enotdir_not_enoent() {
    assert_refused("through_file", &["/plain/f"], ENOTDIR);
}
