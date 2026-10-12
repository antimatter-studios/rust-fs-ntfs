//! A refusal names the operation that was refused (#501).
//!
//! `rmdir_io("/")` answered "cannot rename root", because every path-based
//! remove and rename shares one resolver and its message was written for
//! rename. `unlink_io` on a directory said "use rmdir (not implemented)",
//! sending the reader looking for a feature that exists. The errno comes
//! from the error's kind, which these keep; the message is what a user
//! or a host's log sees.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::error::{Error, Kind};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn fresh_volume() -> PathIo {
    let img = common::temp_image_path("refusal_names");
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    write::mkdir_io(&mut io, "/", "a").expect("mkdir /a");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    io
}

fn refusal<V: std::fmt::Debug>(what: &str, got: Result<V, Error>, kind: Kind) -> String {
    match got {
        Err(e) if e.kind() == kind => e.message().to_string(),
        other => panic!("{what}: expected a {kind:?} refusal, got {other:?}"),
    }
}

/// Each operation on the root says which operation it refused.
#[test]
fn each_refusal_of_the_root_names_its_operation() {
    let mut io = fresh_volume();
    for (op, got) in [
        ("rmdir", write::rmdir_io(&mut io, "/")),
        ("unlink", write::unlink_io(&mut io, "/")),
        ("remove", write::remove_io(&mut io, "/")),
        ("rename", write::rename_io(&mut io, "/", "x")),
    ] {
        let message = refusal(&format!("{op} /"), got, Kind::Refused);
        assert!(
            message.starts_with(&format!("{op}:")),
            "{op} / was refused with {message:?}, which does not name {op}"
        );
        if op != "rename" {
            assert!(
                !message.contains("rename"),
                "{op} / was refused with {message:?}, which names rename"
            );
        }
    }
}

/// Unlinking a directory points at rmdir, which exists.
#[test]
fn unlinking_a_directory_points_at_rmdir_without_calling_it_missing() {
    let mut io = fresh_volume();
    let message = refusal(
        "unlink /a",
        write::unlink_io(&mut io, "/a"),
        Kind::IsDirectory,
    );
    assert!(
        message.contains("rmdir"),
        "{message:?} does not point at rmdir"
    );
    assert!(
        !message.contains("not implemented"),
        "{message:?} calls rmdir not implemented"
    );
    write::rmdir_io(&mut io, "/a").expect("rmdir /a, which the message points at");
}
