//! A directory whose index has grown interior nodes can be emptied and
//! removed (#495).
//!
//! An entry of an interior node points down to the subtree of every name
//! below it, so taking it out by shifting the node's entries orphans that
//! subtree. Unlink refused it, and so did rmdir any directory with
//! `$INDEX_ALLOCATION`: a directory past one index level could never be
//! emptied, nor removed. The issue measured 27 of 600 unlinks refused.
//!
//! Here 600 files with names of 5 to 41 characters are created in `/d`
//! and unlinked in shuffled order. Every unlink must succeed, every name
//! still there must be found -- by this crate's descending lookup and in
//! the `ntfs` crate's listing -- and every name gone must not be, until
//! `/d` is empty and `rmdir` takes it.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{read, write};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 64 * 1024 * 1024;
const FILES: usize = 600;

/// A small deterministic generator, so a failure names the same sequence
/// every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Distinct names of 5 to 41 characters.
fn names(rng: &mut Lcg) -> Vec<String> {
    let mut out = BTreeSet::new();
    while out.len() < FILES {
        let len = 5 + (rng.next() % 37) as usize;
        let name: String = (0..len)
            .map(|_| (b'a' + (rng.next() % 26) as u8) as char)
            .collect();
        out.insert(name);
    }
    out.into_iter().collect()
}

fn check(img: &str, io: &mut PathIo, live: &BTreeSet<String>, gone: &[String], after: &str) {
    for name in live {
        read::resolve_path(io, &format!("/d/{name}"))
            .unwrap_or_else(|e| panic!("after {after}, /d/{name} is not found: {e}"));
    }
    for name in gone {
        assert!(
            read::resolve_path(io, &format!("/d/{name}")).is_err(),
            "after {after}, the unlinked /d/{name} is still found"
        );
    }
    <PathIo as BlockIo>::sync(io).expect("sync");
    let (ntfs, mut reader) = common::open(img);
    let listed: BTreeSet<String> = common::list_names(&ntfs, &mut reader, "/d")
        .into_iter()
        .filter(|n| n != ".")
        .collect();
    assert!(
        listed == *live,
        "after {after}, the ntfs crate lists {} names in /d where {} remain",
        listed.len(),
        live.len()
    );
}

#[test]
fn a_directory_past_one_index_level_is_emptied_and_removed() {
    let img = common::temp_image_path("interior_delete");
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 4096, None, Some(1)).expect("format_filesystem");
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");

    let mut rng = Lcg(0x5EED_0495);
    let mut order = names(&mut rng);
    for name in &order {
        write::create_file_io(&mut io, "/d", name).unwrap_or_else(|e| panic!("create {name}: {e}"));
    }
    // Fisher-Yates, so interior and leaf entries go in no particular order.
    for i in (1..order.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }

    let mut live: BTreeSet<String> = order.iter().cloned().collect();
    let mut gone = Vec::new();
    for (i, name) in order.iter().enumerate() {
        write::unlink_io(&mut io, &format!("/d/{name}"))
            .unwrap_or_else(|e| panic!("unlink {i} of {FILES}, /d/{name}: {e}"));
        live.remove(name);
        gone.push(name.clone());
        if (i + 1) % 100 == 0 {
            check(&img, &mut io, &live, &gone, &format!("{} unlinks", i + 1));
        }
    }
    let d = read::resolve_path(&mut io, "/d").expect("/d");
    let left: Vec<String> = read::read_dir_entries(&mut io, d)
        .expect("list /d")
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n != ".")
        .collect();
    assert!(left.is_empty(), "/d still lists {left:?}");

    write::rmdir_io(&mut io, "/d").expect("rmdir the emptied /d");
    assert!(
        read::resolve_path(&mut io, "/d").is_err(),
        "/d is still found"
    );
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let (ntfs, mut reader) = common::open(&img);
    assert!(
        !common::list_names(&ntfs, &mut reader, "/")
            .iter()
            .any(|n| n == "d"),
        "the ntfs crate still lists /d"
    );
}
