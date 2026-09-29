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
