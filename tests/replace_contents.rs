//! `write::replace_file_contents_io`, which `fs.ntfs write` uses to replace
//! a file: every transition across the resident / non-resident line, in
//! both directions, and a grow that cannot append a run in place. Each
//! result is read back by the upstream `ntfs` crate, a reader that shares
//! none of this crate's code; Windows grades the same writes in the `cli`
//! matrix scenarios.

mod common;

use fs_ntfs::block_io::PathIo;
use std::path::Path;

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[test]
fn every_replacement_reads_back_as_the_new_content() {
    let img = common::temp_image_path("replace_contents");
    {
        let f = std::fs::File::create(&img).unwrap();
        f.set_len(64 << 20).unwrap();
    }
    {
        let mut dev = PathIo::open_rw(Path::new(&img)).unwrap();
        fs_ntfs::mkfs::format_filesystem(&mut dev, 64 << 20, 4096, 4096, None, Some(3)).unwrap();
    }
    // (first size, replacement size): shrinking and growing, resident to
    // non-resident and back, to and from empty, and a one-cluster file
    // growing to a MiB -- which cannot append its run in place.
    let cases = [
        (4097, 1),
        (1 << 20, 500),
        (1, 4097),
        (4096, 1 << 20),
        (500, 0),
        (0, 4096),
        (4097, 8192),
    ];
    for (i, (first, second)) in cases.iter().enumerate() {
        let name = format!("f{i}");
        let path = format!("/{name}");
        let mut dev = PathIo::open_rw(Path::new(&img)).unwrap();
        fs_ntfs::write::create_file_io(&mut dev, "/", &name).unwrap();
        fs_ntfs::write::replace_file_contents_io(&mut dev, &path, &pattern(*first, 1)).unwrap();
        let n = fs_ntfs::write::replace_file_contents_io(&mut dev, &path, &pattern(*second, 2))
            .unwrap_or_else(|e| panic!("replace {first} by {second} bytes: {e}"));
        assert_eq!(n, *second as u64);
        drop(dev);

        let (ntfs, mut reader) = common::open(&img);
        let got = common::read_file_all(&ntfs, &mut reader, &path);
        assert_eq!(got.len(), *second, "{first} -> {second}: length");
        assert!(got == pattern(*second, 2), "{first} -> {second}: content");
    }
}

/// A grow the volume has no room for fails, and the file keeps what it
/// held: running out of space is not a reason to lose the old content.
#[test]
fn a_replacement_the_volume_cannot_hold_keeps_the_old_content() {
    let img = common::temp_image_path("replace_contents_no_room");
    {
        let f = std::fs::File::create(&img).unwrap();
        f.set_len(64 << 20).unwrap();
    }
    {
        let mut dev = PathIo::open_rw(Path::new(&img)).unwrap();
        fs_ntfs::mkfs::format_filesystem(&mut dev, 64 << 20, 4096, 4096, None, Some(3)).unwrap();
    }
    let old = pattern(8192, 1);
    let mut dev = PathIo::open_rw(Path::new(&img)).unwrap();
    fs_ntfs::write::create_file_io(&mut dev, "/", "f").unwrap();
    fs_ntfs::write::replace_file_contents_io(&mut dev, "/f", &old).unwrap();
    let err = fs_ntfs::write::replace_file_contents_io(&mut dev, "/f", &pattern(96 << 20, 2))
        .expect_err("96 MiB cannot fit on a 64 MiB volume");
    assert!(!err.contains("now empty"), "the file was emptied: {err}");
    drop(dev);

    let (ntfs, mut reader) = common::open(&img);
    let got = common::read_file_all(&ntfs, &mut reader, "/f");
    assert!(
        got == old,
        "the old content did not survive ({} bytes back)",
        got.len()
    );
}
