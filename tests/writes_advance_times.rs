//! Writes advance the times of what they change (#493).
//!
//! No write path touched `$STANDARD_INFORMATION`'s times: only the
//! explicit `set_times` family wrote them. A file this crate rewrote kept
//! its modified and change times, and so did a directory it created a file
//! in, so backup tools, `rsync`, `make`, sync clients and Explorer's "date
//! modified" all took them as unchanged. On Windows, writing data advances
//! a file's LastWriteTime and ChangeTime, and adding or removing a
//! directory entry advances the directory's.
//!
//! Each case first sets the times to a value far in the past, so the check
//! needs no sleep: the write must leave them later than that.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::read;
use fs_ntfs::write::{self, FileTimes};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

/// One case: what it does, and the write that does it.
type Case = (&'static str, fn(&mut PathIo));
/// 1601-01-01 plus one 100 ns tick: before anything `now` can be.
const LONG_AGO: u64 = 1;

fn fresh_volume(tag: &str) -> PathIo {
    let img = common::temp_image_path(format!("stamp_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    write::create_file_io(&mut io, "/d", "f").expect("create /d/f");
    write::mkdir_io(&mut io, "/", "e").expect("mkdir /e");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    io
}

fn age(io: &mut PathIo, path: &str) {
    write::set_times_io(
        io,
        path,
        FileTimes {
            modification: Some(LONG_AGO),
            mft_record_modification: Some(LONG_AGO),
            ..FileTimes::default()
        },
    )
    .unwrap_or_else(|e| panic!("set {path}'s times: {e}"));
}

/// (modified, change) of `path`.
fn times(io: &mut PathIo, path: &str) -> (u64, u64) {
    let rec = read::resolve_path(io, path).unwrap_or_else(|e| panic!("resolve {path}: {e}"));
    let st = read::read_stat(io, rec).unwrap_or_else(|e| panic!("stat {path}: {e}"));
    (st.modified_nt, st.mft_modified_nt)
}

fn advanced(io: &mut PathIo, path: &str, after: &str) {
    let (modified, changed) = times(io, path);
    assert!(
        modified > LONG_AGO && changed > LONG_AGO,
        "after {after}, {path}'s modified time is {modified} and its change time {changed}: \
         neither may still be {LONG_AGO}"
    );
}

/// The issue's sequence, and every other way of writing a file's data.
#[test]
fn writing_a_files_data_advances_its_times() {
    let mut io = fresh_volume("data");
    let cases: [Case; 6] = [
        ("a resident write", |io| {
            write::write_file_contents_io(io, "/d/f", b"small")
                .map(drop)
                .unwrap()
        }),
        ("a non-resident write", |io| {
            write::write_file_contents_io(io, "/d/f", &[1u8; 20000])
                .map(drop)
                .unwrap()
        }),
        ("write_at", |io| {
            write::write_at_io(io, "/d/f", 100, b"xyz")
                .map(drop)
                .unwrap()
        }),
        ("a grow", |io| {
            write::truncate_io(io, "/d/f", 50000).map(drop).unwrap()
        }),
        ("a shrink", |io| {
            write::truncate_io(io, "/d/f", 10).map(drop).unwrap()
        }),
        ("a named stream", |io| {
            write::write_named_stream_io(io, "/d/f", "s", b"stream").unwrap()
        }),
    ];
    for (what, op) in cases {
        age(&mut io, "/d/f");
        op(&mut io);
        advanced(&mut io, "/d/f", what);
    }
}

/// Adding, removing and renaming an entry advance the directory's times.
#[test]
fn changing_a_directorys_entries_advances_its_times() {
    let mut io = fresh_volume("entries");
    let cases: [Case; 6] = [
        ("a create", |io| {
            write::create_file_io(io, "/d", "g").map(drop).unwrap()
        }),
        ("a mkdir", |io| {
            write::mkdir_io(io, "/d", "sub").map(drop).unwrap()
        }),
        ("an unlink", |io| write::unlink_io(io, "/d/g").unwrap()),
        ("an rmdir", |io| write::rmdir_io(io, "/d/sub").unwrap()),
        ("a rename to another length", |io| {
            write::rename_io(io, "/d/f", "ff").unwrap()
        }),
        ("a same-length rename", |io| {
            write::rename_io(io, "/d/ff", "fg").unwrap()
        }),
    ];
    for (what, op) in cases {
        age(&mut io, "/d");
        op(&mut io);
        advanced(&mut io, "/d", what);
    }
}

/// A rename and a link change the file's record, not its data: its change
/// time advances and its modified time stays. A link advances the
/// directory it lands in.
#[test]
fn a_rename_or_link_advances_the_files_change_time_only() {
    let mut io = fresh_volume("record");
    age(&mut io, "/d/f");
    write::rename_io(&mut io, "/d/f", "renamed").expect("rename");
    let (modified, changed) = times(&mut io, "/d/renamed");
    assert_eq!(
        modified, LONG_AGO,
        "a rename changed the file's modified time"
    );
    assert!(changed > LONG_AGO, "a rename left the file's change time");

    age(&mut io, "/d/renamed");
    age(&mut io, "/e");
    write::link_io(&mut io, "/d/renamed", "/e", "link").expect("link");
    let (modified, changed) = times(&mut io, "/d/renamed");
    assert_eq!(
        modified, LONG_AGO,
        "a link changed the file's modified time"
    );
    assert!(changed > LONG_AGO, "a link left the file's change time");
    advanced(&mut io, "/e", "a link into it");
}
