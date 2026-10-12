//! A rename gives the new name the `$FILE_NAME` namespace that create
//! would (#492).
//!
//! `create_file_io` writes `WIN32_AND_DOS` (3) only for a name that fits
//! DOS 8.3, and `POSIX` (0) otherwise, because chkdsk rejects namespace 3
//! on a name that does not fit ("An invalid filename X (N) was found in
//! directory M"). A rename kept the old name's namespace, so `file.txt`
//! renamed to `longer_name.txt` stayed in `WIN32_AND_DOS`. The
//! variable-length path was worse: its new index entry took the derived
//! namespace while the record's `$FILE_NAME` kept the old one.
//!
//! The record's `$FILE_NAME` is read with this crate; the index entry's
//! key with the `ntfs` crate, a parser that is not this one.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write;
use ntfs::indexes::NtfsFileNameIndex;
use ntfs::structured_values::NtfsFileNamespace;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const POSIX: u8 = 0;
const WIN32_AND_DOS: u8 = 3;

fn fresh_volume(tag: &str) -> (String, PathIo) {
    let img = common::temp_image_path(format!("renamens_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    // A directory whose index is all in $INDEX_ROOT, where a rename of
    // either length is supported (#474).
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, io)
}

/// The namespace of the index entry for `name` in `/d`, as the `ntfs`
/// crate decodes it.
fn index_namespace(img: &str, name: &str) -> u8 {
    let (ntfs, mut reader) = common::open(img);
    let d = common::navigate(&ntfs, &mut reader, "/d");
    let index = d.directory_index(&mut reader).expect("directory index");
    let mut finder = index.finder();
    let entry = NtfsFileNameIndex::find(&mut finder, &ntfs, &mut reader, name)
        .unwrap_or_else(|| panic!("/d/{name} has no index entry"))
        .expect("read the index entry");
    match entry
        .key()
        .expect("a key")
        .expect("decode the key")
        .namespace()
    {
        NtfsFileNamespace::Posix => 0,
        NtfsFileNamespace::Win32 => 1,
        NtfsFileNamespace::Dos => 2,
        NtfsFileNamespace::Win32AndDos => 3,
    }
}

/// The namespace of the record's `$FILE_NAME` called `name`.
fn record_namespace<T: BlockIo + ?Sized>(io: &mut T, name: &str) -> u8 {
    let names = write::read_file_names_io(io, &format!("/d/{name}")).expect("read $FILE_NAMEs");
    names
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("/d/{name} has no $FILE_NAME called {name}: {names:?}"))
        .namespace
}

fn rename_and_check(io: &mut PathIo, img: &str, from: &str, to: &str, want: u8) {
    write::rename_io(io, &format!("/d/{from}"), to)
        .unwrap_or_else(|e| panic!("rename {from} to {to}: {e}"));
    <PathIo as BlockIo>::sync(io).expect("sync");
    assert_eq!(
        record_namespace(io, to),
        want,
        "{from} renamed to {to}: the record's $FILE_NAME namespace"
    );
    assert_eq!(
        index_namespace(img, to),
        want,
        "{from} renamed to {to}: the index entry's namespace"
    );
}

/// #492's reproduction, and its reverse: the variable-length path.
#[test]
fn a_rename_to_another_length_rederives_the_namespace() {
    let (img, mut io) = fresh_volume("varlen");
    write::create_file_io(&mut io, "/d", "file.txt").expect("create /d/file.txt");
    assert_eq!(record_namespace(&mut io, "file.txt"), WIN32_AND_DOS);
    rename_and_check(&mut io, &img, "file.txt", "longer_name.txt", POSIX);
    rename_and_check(&mut io, &img, "longer_name.txt", "short.txt", WIN32_AND_DOS);
}

/// The same-length path, in place and moved to another index slot, in
/// both directions.
#[test]
fn a_rename_to_the_same_length_rederives_the_namespace() {
    let (img, mut io) = fresh_volume("samelen");
    for n in ["abcd.txt", "m.txt"] {
        write::create_file_io(&mut io, "/d", n).expect("create");
    }
    // Two dots: not 8.3. Sorts before m.txt, as abcd.txt does: in place.
    rename_and_check(&mut io, &img, "abcd.txt", "a.bc.txt", POSIX);
    rename_and_check(&mut io, &img, "a.bc.txt", "abcd.txt", WIN32_AND_DOS);
    // Sorts after m.txt: the entry moves.
    rename_and_check(&mut io, &img, "abcd.txt", "z.bc.txt", POSIX);
    rename_and_check(&mut io, &img, "z.bc.txt", "abcd.txt", WIN32_AND_DOS);
}
