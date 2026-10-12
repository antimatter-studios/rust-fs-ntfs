//! A rename to a name of another length works in a directory with INDX
//! blocks (#474).
//!
//! It was refused whenever the parent had `$INDEX_ALLOCATION`, and the
//! root of every freshly formatted volume does: no file in the root could
//! be renamed to a name of a different length, nor in any directory past
//! a few dozen entries.
//!
//! Each rename is checked through a lookup that descends the B-tree, and
//! the directory is read back with the `ntfs` crate, a parser that is not
//! this one.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{read, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn fresh_volume(tag: &str) -> (String, PathIo) {
    let img = common::temp_image_path(format!("varlenindx_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, io)
}

fn rename_and_check(io: &mut PathIo, img: &str, dir: &str, from: &str, to: &str) {
    let parent = dir.trim_end_matches('/');
    let rec = read::resolve_path(io, &format!("{parent}/{from}")).expect("resolve the source");
    write::rename_io(io, &format!("{parent}/{from}"), to)
        .unwrap_or_else(|e| panic!("rename {parent}/{from} to {to}: {e}"));
    assert_eq!(
        read::resolve_path(io, &format!("{parent}/{to}")).ok(),
        Some(rec),
        "{parent}/{to} does not find the renamed file"
    );
    assert!(
        read::resolve_path(io, &format!("{parent}/{from}")).is_err(),
        "{parent}/{from} is still found"
    );
    let names = write::read_file_names_io(io, &format!("{parent}/{to}")).expect("$FILE_NAMEs");
    assert_eq!(
        names.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
        [to],
        "the record's $FILE_NAME after renaming {from} to {to}"
    );
    <PathIo as BlockIo>::sync(io).expect("sync");
    let (ntfs, mut reader) = common::open(img);
    let listed = common::list_names(&ntfs, &mut reader, dir);
    assert!(
        listed.iter().any(|n| n == to) && !listed.iter().any(|n| n == from),
        "the ntfs crate lists {dir} as {listed:?} after renaming {from} to {to}"
    );
}

/// #474's reproduction: the root of a fresh volume has INDX blocks.
#[test]
fn a_root_file_is_renamed_to_a_longer_and_a_shorter_name() {
    let (img, mut io) = fresh_volume("root");
    write::create_file_io(&mut io, "/", "b").expect("create /b");
    rename_and_check(&mut io, &img, "/", "b", "zz");
    rename_and_check(&mut io, &img, "/", "zz", "a_much_longer_name.txt");
    rename_and_check(&mut io, &img, "/", "a_much_longer_name.txt", "c");
}

/// In a directory of 120 names, both ways, and onto a name that exists.
#[test]
fn renames_across_a_directory_with_index_blocks() {
    let (img, mut io) = fresh_volume("dir");
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    for i in 0..120 {
        write::create_file_io(&mut io, "/d", &format!("n{i:03}")).expect("create in /d");
    }
    rename_and_check(&mut io, &img, "/d", "n007", "renamed_much_longer");
    rename_and_check(&mut io, &img, "/d", "n100", "x");
    let clash = write::rename_io(&mut io, "/d/n050", "x");
    assert!(
        clash.is_err(),
        "renaming n050 onto the existing x was not refused"
    );
    read::resolve_path(&mut io, "/d/n050").expect("the refused rename's source is still found");
    let (ntfs, mut reader) = common::open(&img);
    let listed = common::list_names(&ntfs, &mut reader, "/d");
    assert_eq!(listed.len(), 120, "the ntfs crate lists /d as {listed:?}");
}
