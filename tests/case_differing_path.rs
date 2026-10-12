//! A path that differs in case from the stored name reaches the stored
//! name (#491).
//!
//! Path resolution matches each component case-insensitively, as the
//! `$I30` index collates with `$UpCase`, but rename, unlink and rmdir then
//! looked the record's `$FILE_NAME` up by the caller's spelling,
//! case-sensitively. Renaming `FILE.TXT` through `/d/file.txt` renamed the
//! index entry and left the record's `$FILE_NAME` saying `FILE.TXT` -- a
//! file the Linux kernel's ntfs3 cannot open -- and unlinking `KEEP.TXT`
//! through `/d/keep.txt` was refused.
//!
//! The directory's names are read from its index with the `ntfs` crate, a
//! parser that is not this one, and compared with the records' own.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{read, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn fresh_volume(tag: &str) -> (String, PathIo) {
    let img = common::temp_image_path(format!("casepath_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, io)
}

/// The record's `$FILE_NAME`s, by name.
fn record_names<T: BlockIo + ?Sized>(io: &mut T, path: &str) -> Vec<String> {
    write::read_file_names_io(io, path)
        .unwrap_or_else(|e| panic!("read the $FILE_NAMEs of {path}: {e}"))
        .into_iter()
        .map(|n| n.name)
        .collect()
}

/// `/d`'s names as its index holds them, decoded by the `ntfs` crate.
fn index_names(io: &mut PathIo, img: &str) -> Vec<String> {
    <PathIo as BlockIo>::sync(io).expect("sync");
    let (ntfs, mut reader) = common::open(img);
    let mut names = common::list_names(&ntfs, &mut reader, "/d");
    names.retain(|n| n != ".");
    names
}

/// #491's rename: the record's `$FILE_NAME` takes the new name, the
/// same as the index entry, for both rename paths.
#[test]
fn a_rename_through_a_differently_cased_path_renames_the_record_too() {
    let (img, mut io) = fresh_volume("rename");
    for n in ["FILE.TXT", "ABCD.TXT"] {
        write::create_file_io(&mut io, "/d", n).expect("create");
    }
    write::rename_io(&mut io, "/d/file.txt", "other_name").expect("rename /d/file.txt");
    assert_eq!(record_names(&mut io, "/d/other_name"), ["other_name"]);
    write::rename_io(&mut io, "/d/abcd.txt", "WXYZ.TXT").expect("rename /d/abcd.txt");
    assert_eq!(record_names(&mut io, "/d/WXYZ.TXT"), ["WXYZ.TXT"]);

    let mut listed = index_names(&mut io, &img);
    listed.sort();
    assert_eq!(listed, ["WXYZ.TXT", "other_name"]);
}

/// A case-only rename through a path spelled like the new name changes
/// the stored case, where it was taken for a rename to the same name.
#[test]
fn a_case_only_rename_through_the_new_spelling_changes_the_case() {
    let (img, mut io) = fresh_volume("caseonly");
    write::create_file_io(&mut io, "/d", "MIXED.TXT").expect("create");
    write::rename_io(&mut io, "/d/mixed.txt", "mixed.txt").expect("rename to mixed.txt");
    assert_eq!(record_names(&mut io, "/d/mixed.txt"), ["mixed.txt"]);
    assert_eq!(index_names(&mut io, &img), ["mixed.txt"]);
}

/// #491's unlink, and rmdir the same way: both remove what the path
/// resolved to.
#[test]
fn unlink_and_rmdir_through_a_differently_cased_path_remove_it() {
    let (img, mut io) = fresh_volume("remove");
    write::create_file_io(&mut io, "/d", "KEEP.TXT").expect("create");
    write::mkdir_io(&mut io, "/d", "SUB").expect("mkdir");
    write::unlink_io(&mut io, "/d/keep.txt").expect("unlink /d/keep.txt");
    write::rmdir_io(&mut io, "/d/sub").expect("rmdir /d/sub");
    assert!(
        read::resolve_path(&mut io, "/d/KEEP.TXT").is_err(),
        "KEEP.TXT is still found"
    );
    assert!(
        read::resolve_path(&mut io, "/d/SUB").is_err(),
        "SUB is still found"
    );
    assert_eq!(index_names(&mut io, &img), Vec::<String>::new());
}
