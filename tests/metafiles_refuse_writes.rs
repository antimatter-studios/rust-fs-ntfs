//! The volume's own metafiles are not files to write (#499).
//!
//! Records 0 to 15 are the metafiles (`$MFT`, `$LogFile`, `$Volume`,
//! `$Bitmap`, `$UpCase`, ...), 16 to 23 are reserved, and `$Extend`'s
//! children (`$Quota`, `$ObjId`, `$Reparse`, ...) are metafiles under
//! ordinary record numbers. No write path refused any of them: `$Volume`
//! and `$BadClus` could be unlinked, `$UpCase` truncated, every metafile
//! renamed, and `unlink` of `$MFT` was refused only after it had changed
//! the record and the index. A host that lists the root through this crate
//! shows them, so an `rm -rf` on the mount reaches every one.
//!
//! Each write is refused with `Kind::Refused` (`EPERM`) before anything is
//! touched, so the image is byte-for-byte what it was. The root directory
//! is record 5 and stays writable.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::error::Kind;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::read;
use fs_ntfs::write::{self, file_attr, FileAttributesChange, FileTimes};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u32 = 4096;
const ROOT: u64 = 5;
const EXTEND: u64 = 11;
const FIRST_USER_RECORD: u64 = 24;

/// Every path a write could name a metafile by: the root's entries below
/// the first user record, other than the root itself, and `$Extend`'s
/// children.
fn metafile_paths<T: BlockIo + ?Sized>(io: &mut T) -> Vec<String> {
    let mut paths: Vec<String> = read::read_dir_entries(io, ROOT)
        .expect("list the root")
        .into_iter()
        .filter(|e| e.record_number < FIRST_USER_RECORD && e.record_number != ROOT)
        .map(|e| format!("/{}", e.name))
        .collect();
    paths.extend(
        read::read_dir_entries(io, EXTEND)
            .expect("list $Extend")
            .into_iter()
            .map(|e| format!("/$Extend/{}", e.name)),
    );
    paths
}

fn refused<V: std::fmt::Debug>(what: &str, got: Result<V, fs_ntfs::error::Error>) {
    match got {
        Err(e) if e.kind() == Kind::Refused => {}
        other => panic!("{what}: expected a refusal (EPERM), got {other:?}"),
    }
}

#[test]
fn every_write_to_a_metafile_is_refused_and_the_volume_is_unchanged() {
    let img = common::temp_image_path("metafiles_refuse");
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, CLUSTER, Some("META"), Some(7))
        .expect("format_filesystem");
    write::create_file_io(&mut io, "/", "user.txt").expect("create /user.txt");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    let paths = metafile_paths(&mut io);
    for must in [
        "/$MFT",
        "/$LogFile",
        "/$Volume",
        "/$Bitmap",
        "/$UpCase",
        "/$Extend",
    ] {
        assert!(
            paths.iter().any(|p| p == must),
            "{must} is not among the metafiles found: {paths:?}"
        );
    }
    let before = std::fs::read(&img).expect("read image");

    for p in &paths {
        refused(&format!("unlink {p}"), write::unlink_io(&mut io, p));
        refused(&format!("remove {p}"), write::remove_io(&mut io, p));
        refused(
            &format!("rename {p}"),
            write::rename_io(&mut io, p, "renamed"),
        );
        refused(&format!("truncate {p}"), write::truncate_io(&mut io, p, 0));
        refused(
            &format!("write_file_contents {p}"),
            write::write_file_contents_io(&mut io, p, b"x"),
        );
        refused(
            &format!("write_at {p}"),
            write::write_at_io(&mut io, p, 0, b"x"),
        );
        refused(
            &format!("write_named_stream {p}"),
            write::write_named_stream_io(&mut io, p, "s", b"x"),
        );
        refused(
            &format!("write_ea {p}"),
            write::write_ea_io(&mut io, p, b"EA", b"v", 0),
        );
        refused(&format!("link {p}"), write::link_io(&mut io, p, "/", "lnk"));
        refused(
            &format!("set_times {p}"),
            write::set_times_io(
                &mut io,
                p,
                FileTimes {
                    modification: Some(1),
                    ..FileTimes::default()
                },
            ),
        );
        refused(
            &format!("set_file_attributes {p}"),
            write::set_file_attributes_io(
                &mut io,
                p,
                FileAttributesChange {
                    add: file_attr::HIDDEN,
                    remove: 0,
                },
            ),
        );
    }
    refused(
        "create in $Extend",
        write::create_file_io(&mut io, "/$Extend", "x"),
    );
    refused(
        "mkdir in $Extend",
        write::mkdir_io(&mut io, "/$Extend", "d"),
    );
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    let after = std::fs::read(&img).expect("read image");
    let first_change = before.iter().zip(&after).position(|(a, b)| a != b);
    assert!(
        first_change.is_none(),
        "a refused write changed the image, first at byte {first_change:?}"
    );

    // The root is record 5 and is a directory like any other.
    write::create_file_io(&mut io, "/", "after.txt").expect("create in the root");
    write::set_times_io(
        &mut io,
        "/",
        FileTimes {
            modification: Some(1),
            ..FileTimes::default()
        },
    )
    .expect("set the root's times");
}
