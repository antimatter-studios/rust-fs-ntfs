//! A symlink is written in Windows path syntax and read back in POSIX
//! syntax (#505).
//!
//! `create_symlink_io` stored its target verbatim, with `/` separators,
//! and a POSIX absolute target such as `/d/small` with the relative flag
//! clear: a path Windows cannot resolve, and ntfs-3g reports the link as
//! an unsupported reparse point. The reader returned a Windows link's
//! print name verbatim, so `mklink link ..\d\small` came back to a POSIX
//! host as one path component full of backslashes. Each side round-tripped
//! only with itself.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const RELATIVE: u32 = 1;

fn fresh_volume() -> PathIo {
    let img = common::temp_image_path("symlink_separators");
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    io
}

/// (flags, substitute name, print name) of a symlink reparse buffer.
fn decode(data: &[u8]) -> (u32, String, String) {
    let field = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]) as usize;
    let name = |off: usize, len: usize| {
        let bytes = &data[12 + off..12 + off + len];
        String::from_utf16(
            &bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )
        .expect("UTF-16 name")
    };
    let flags = u32::from_le_bytes(data[8..12].try_into().unwrap());
    (flags, name(field(0), field(2)), name(field(4), field(6)))
}

/// The link's reparse buffer as written, and the target the reader gives
/// a POSIX host for it.
fn written<T: BlockIo + ?Sized>(io: &mut T, path: &str) -> ((u32, String, String), String) {
    let rp = write::read_reparse_point_io(io, path)
        .expect("read the reparse point")
        .expect("a reparse point");
    let mut buf = rp.reparse_tag.to_le_bytes().to_vec();
    buf.extend_from_slice(&(rp.data.len() as u16).to_le_bytes());
    buf.extend_from_slice(&[0, 0]);
    buf.extend_from_slice(&rp.data);
    let target = fs_ntfs::reparse_link_target(&buf).expect("a link target");
    (decode(&rp.data), target)
}

#[test]
fn a_relative_target_is_written_with_backslashes_and_read_with_slashes() {
    let mut io = fresh_volume();
    write::create_symlink_io(&mut io, "/", "relup", "../d/small", true).expect("create relup");
    let (fields, target) = written(&mut io, "/relup");
    assert_eq!(
        fields,
        (
            RELATIVE,
            r"..\d\small".to_string(),
            r"..\d\small".to_string()
        )
    );
    assert_eq!(target, "../d/small");
}

/// A POSIX absolute target names no drive, so it is written as Windows
/// writes a rooted path with none: relative, resolved from the root of
/// the link's own volume.
#[test]
fn a_posix_absolute_target_is_written_rooted_on_the_links_volume() {
    let mut io = fresh_volume();
    write::create_symlink_io(&mut io, "/", "abs", "/d/small", false).expect("create abs");
    let (fields, target) = written(&mut io, "/abs");
    assert_eq!(
        fields,
        (RELATIVE, r"\d\small".to_string(), r"\d\small".to_string())
    );
    assert_eq!(target, "/d/small");
}

/// A target with a drive is an NT path in the substitute name, and a Win32
/// one in the print name, as Windows writes them.
#[test]
fn a_drive_target_is_written_as_an_nt_path() {
    let mut io = fresh_volume();
    write::create_symlink_io(&mut io, "/", "drive", "C:/dir/file", false).expect("create drive");
    let (fields, target) = written(&mut io, "/drive");
    assert_eq!(
        fields,
        (
            0,
            r"\??\C:\dir\file".to_string(),
            r"C:\dir\file".to_string()
        )
    );
    assert_eq!(target, r"C:\dir\file");
}

/// Graded by Windows: the link Windows made for `sub\file.txt` in
/// `test-disks/ntfs-symlink.img` (see `readlink_windows_oracle.rs`) carries
/// the same flags and names as the one written here for `sub/file.txt`.
#[test]
fn a_relative_link_matches_the_one_windows_wrote() {
    const IMG: &str = "test-disks/ntfs-symlink.img";
    assert!(
        Path::new(IMG).exists(),
        "missing {IMG}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let mut windows = PathIo::open_ro(Path::new(IMG)).expect("open the Windows fixture");
    let (theirs, _) = written(&mut windows, "/rel-file");

    let mut io = fresh_volume();
    write::create_symlink_io(&mut io, "/", "rel-file", "sub/file.txt", true).expect("create");
    let (ours, target) = written(&mut io, "/rel-file");
    assert_eq!(ours, theirs, "ours, then the link Windows wrote");
    assert_eq!(target, "sub/file.txt");
}
