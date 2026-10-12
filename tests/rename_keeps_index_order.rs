//! A rename leaves the directory index in collation order (#466).
//!
//! A same-length rename rewrote the name inside the existing `$I30` entry
//! and left the entry where it was. The index is ordered by
//! `COLLATION_FILE_NAME`, so whenever the new name sorted into a different
//! slot, every B-tree lookup -- this crate's, ntfs-3g's and the Linux
//! kernel's -- missed it: `rename_io` returned `Ok` and the file could no
//! longer be found by name.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{index_io, mft_io, read, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const ROOT: u64 = 5;

fn fresh_volume(tag: &str) -> PathIo {
    let dst = common::temp_image_path(format!("renameorder_{tag}"));
    let f = File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    io
}

/// The directory's names in index order, without `.`.
fn listing<T: BlockIo + ?Sized>(io: &mut T, dir: u64) -> Vec<String> {
    read::read_dir_entries(io, dir)
        .expect("list the directory")
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n != ".")
        .collect()
}

/// Every name the directory lists is found by a lookup, which descends
/// the B-tree by comparing names -- so a misplaced entry is one it
/// misses -- and finds the record the listing names.
///
/// The listing itself is not in key order once the directory has INDX
/// blocks: it gives `$INDEX_ROOT`'s entries first and then each block in
/// VCN order.
fn assert_every_name_is_found<T: BlockIo + ?Sized>(io: &mut T, dir_path: &str, dir: u64) {
    for e in read::read_dir_entries(io, dir).expect("list the directory") {
        if e.name == "." {
            continue;
        }
        let path = format!("{}/{}", dir_path.trim_end_matches('/'), e.name);
        let found = read::resolve_path(io, &path)
            .unwrap_or_else(|err| panic!("{path} is listed but a lookup misses it: {err}"));
        assert_eq!(
            found, e.record_number,
            "{path} is listed as record {} but a lookup finds {found}",
            e.record_number
        );
    }
}

/// Sorted as `COLLATION_FILE_NAME` sorts these ASCII names: case-blind.
/// Only for a directory whose index is all in `$INDEX_ROOT`, where the
/// listing is the index in order.
fn assert_collated(names: &[String]) {
    let keys: Vec<String> = names.iter().map(|n| n.to_ascii_uppercase()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys, sorted,
        "the index is out of collation order: {names:?}"
    );
}

/// #466's reproduction: in the root, `d` renamed to `a` must sort first
/// and still be found.
#[test]
fn a_rename_that_sorts_earlier_is_found_and_listed_in_order() {
    let mut io = fresh_volume("root");
    for n in ["b", "c", "d"] {
        write::create_file_io(&mut io, "/", n).expect("create");
    }
    write::rename_io(&mut io, "/d", "a").expect("rename /d to a");
    let rec = read::resolve_path(&mut io, "/a").expect("/a is found after the rename");
    assert!(
        read::resolve_path(&mut io, "/d").is_err(),
        "/d is still found"
    );
    assert_every_name_is_found(&mut io, "/", ROOT);
    let names = listing(&mut io, ROOT);
    assert_collated(&names);
    assert!(
        names.contains(&"a".to_string()),
        "a is not listed: {names:?}"
    );
    // And it is the same file.
    assert_eq!(read::resolve_path(&mut io, "/a").unwrap(), rec);
}

/// The same in a directory big enough to have INDX blocks, in both
/// directions, for renames like the issue's fuzzer found (`n90 -> n11`).
#[test]
fn renames_across_a_directory_with_index_blocks_keep_it_in_order() {
    let mut io = fresh_volume("indx");
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    for i in 0..120 {
        write::create_file_io(&mut io, "/d", &format!("n{i:03}")).expect("create in /d");
    }
    let d = read::resolve_path(&mut io, "/d").expect("/d");
    for (from, to) in [
        ("n090", "m090"),
        ("n005", "z005"),
        ("n060", "a060"),
        // Between n009 and n010: X sorts after the digits.
        ("n119", "n00x"),
    ] {
        write::rename_io(&mut io, &format!("/d/{from}"), to)
            .unwrap_or_else(|e| panic!("rename {from} to {to}: {e}"));
        read::resolve_path(&mut io, &format!("/d/{to}"))
            .unwrap_or_else(|e| panic!("/d/{to} is not found after renaming {from}: {e}"));
        assert!(
            read::resolve_path(&mut io, &format!("/d/{from}")).is_err(),
            "/d/{from} is still found after renaming it to {to}"
        );
        assert_every_name_is_found(&mut io, "/d", d);
    }
    assert_eq!(
        listing(&mut io, d).len(),
        120,
        "a rename lost or added a name"
    );
}

/// An entry in an interior node of the index moves too: it is taken out
/// as a B-tree takes one out, its place filled by its in-order neighbour,
/// and inserted again under the new name (#495). This was refused, since
/// shifting it away would have orphaned the subtree it points to.
#[test]
fn a_rename_that_moves_an_interior_entry_works() {
    let mut io = fresh_volume("interior");
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    for i in 0..120 {
        write::create_file_io(&mut io, "/d", &format!("n{i:03}")).expect("create in /d");
    }
    let d = read::resolve_path(&mut io, "/d").expect("/d");
    // With INDX blocks below it, every entry in $INDEX_ROOT is interior.
    let (_, record) = mft_io::read_mft_record_io(&mut io, d).expect("read /d");
    let mut root = Vec::new();
    index_io::collect_index_root_entries(&record, &mut root).expect("/d's $INDEX_ROOT");
    let interior = root
        .first()
        .expect("/d's $INDEX_ROOT has an entry")
        .name
        .clone();
    let moved = format!("z{}", &interior[1..]);

    write::rename_io(&mut io, &format!("/d/{interior}"), &moved)
        .unwrap_or_else(|e| panic!("rename interior {interior} to {moved}: {e}"));
    read::resolve_path(&mut io, &format!("/d/{moved}")).expect("the new name is found");
    assert!(
        read::resolve_path(&mut io, &format!("/d/{interior}")).is_err(),
        "the old name is still found"
    );
    assert_every_name_is_found(&mut io, "/d", d);
    assert_eq!(
        listing(&mut io, d).len(),
        120,
        "a rename lost or added a name"
    );
}

/// A rename that keeps its slot still works, and in place.
#[test]
fn a_rename_that_keeps_its_slot_still_works() {
    let mut io = fresh_volume("inplace");
    for n in ["b", "d", "f"] {
        write::create_file_io(&mut io, "/", n).expect("create");
    }
    write::rename_io(&mut io, "/d", "e").expect("rename /d to e");
    read::resolve_path(&mut io, "/e").expect("/e is found");
    assert_every_name_is_found(&mut io, "/", ROOT);
    assert_collated(&listing(&mut io, ROOT));
}
