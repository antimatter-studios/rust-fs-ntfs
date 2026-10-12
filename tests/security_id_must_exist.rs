//! A security id is set only when `$Secure:$SII` has a descriptor under
//! it (#503).
//!
//! The security id is how NTFS finds a file's ACL: Windows looks it up in
//! `$Secure:$SII`. `set_security_id` wrote any 32-bit value, and a file
//! whose id has no `$SII` entry has no descriptor Windows can apply --
//! access checks behave as though it were missing, and chkdsk replaces the
//! id. Each such id is refused with `Kind::Invalid` before anything is
//! written.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::error::Kind;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{read, write};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn fresh_volume(tag: &str) -> (String, PathIo) {
    let img = common::temp_image_path(format!("secid_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    write::create_file_io(&mut io, "/", "f").expect("create /f");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, io)
}

fn refused(id: u32, got: Result<(), fs_ntfs::error::Error>) {
    match got {
        Err(e) if e.kind() == Kind::Invalid => {}
        other => panic!("security id {id:#x}: expected a refusal (Invalid), got {other:?}"),
    }
}

/// The issue's four ids, and one past the volume's only descriptor: each
/// refused, with the image byte-for-byte as it was.
#[test]
fn an_id_with_no_descriptor_is_refused_and_nothing_is_written() {
    let (img, mut io) = fresh_volume("refused");
    let before = std::fs::read(&img).expect("read image");
    for id in [0, 1, 9999, 0x200, 0xffff_ffff] {
        refused(id, write::set_security_id_io(&mut io, "/f", id));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let after = std::fs::read(&img).expect("read image");
    let first_change = before.iter().zip(&after).position(|(a, b)| a != b);
    assert!(
        first_change.is_none(),
        "a refused set_security_id changed the image, first at byte {first_change:?}"
    );
}

/// The id `format_filesystem` gives its one descriptor is set.
#[test]
fn an_id_with_a_descriptor_is_set() {
    let (_img, mut io) = fresh_volume("set");
    write::set_security_id_io(&mut io, "/f", 0x100).expect("set 0x100");
    assert_eq!(
        write::read_security_id_io(&mut io, "/f").expect("read the id"),
        Some(0x100)
    );
}

/// On a volume Windows wrote, the id one of Windows' own files carries is
/// one Windows' `$SII` holds, so it is set; an id far past any Windows
/// handed out is refused.
#[test]
fn an_id_from_windows_own_files_is_set_on_a_volume_windows_wrote() {
    let img = common::temp_image_path("secid_windows");
    let out = File::create(&img).expect("create image copy");
    let status = Command::new("gzip")
        .args(["-dc", "test-disks/windows-interrupted-1.recovered.img.gz"])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress the Windows fixture");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");

    let mut windows_id = None;
    for dir in read::read_dir_entries(&mut io, 5).expect("list the root") {
        let workload = dir.name.len() == 4
            && dir.name.starts_with('d')
            && dir.name[1..].bytes().all(|b| b.is_ascii_digit());
        if !workload {
            continue;
        }
        for e in read::read_dir_entries(&mut io, dir.record_number).expect("list a directory") {
            if e.name == "." {
                continue;
            }
            let path = format!("/{}/{}", dir.name, e.name);
            if let Ok(Some(id)) = write::read_security_id_io(&mut io, &path) {
                windows_id = Some(id);
                break;
            }
        }
        if windows_id.is_some() {
            break;
        }
    }
    let windows_id = windows_id.expect("a file Windows wrote, carrying a security id");

    write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::set_security_id_io(&mut io, "/f", windows_id)
        .unwrap_or_else(|e| panic!("set /f to Windows' id {windows_id:#x}: {e}"));
    assert_eq!(
        write::read_security_id_io(&mut io, "/f").expect("read the id"),
        Some(windows_id)
    );
    refused(
        0xffff_fff0,
        write::set_security_id_io(&mut io, "/f", 0xffff_fff0),
    );
}
