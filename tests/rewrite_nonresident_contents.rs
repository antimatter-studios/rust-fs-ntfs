//! A file's contents can be replaced again once they are non-resident
//! (#470).
//!
//! `write_file_contents` tried a resident write, and on `NeedsPromotion`
//! promoted the `$DATA` to non-resident. Once it was non-resident the
//! promotion refused with "$DATA is already non-resident", so the second
//! save of any file over roughly 700 bytes failed, through the Rust API,
//! the facade and the C ABI alike.

mod common;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{read, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn fresh_volume() -> PathIo {
    let img = common::temp_image_path("rewrite_nonresident");
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    io
}

/// #470's three calls, a grow and a write back down to nothing, each read
/// back whole.
#[test]
fn a_nonresident_file_is_rewritten_at_every_size() {
    let mut io = fresh_volume();
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    let contents: [Vec<u8>; 5] = [
        vec![1u8; 20000],
        vec![2u8; 20000],
        b"0123456789".to_vec(),
        (0..50000u32).map(|i| (i % 251) as u8).collect(),
        Vec::new(),
    ];
    for (i, data) in contents.iter().enumerate() {
        let n = write::write_file_contents_io(&mut io, "/f", data)
            .unwrap_or_else(|e| panic!("write {i} ({} bytes): {e}", data.len()));
        assert_eq!(n, data.len() as u64, "write {i} reported {n} bytes");
        let got = read::read_attribute_value(&mut io, rec, AttrType::Data, None)
            .unwrap_or_else(|e| panic!("read back write {i}: {e}"));
        assert!(
            got == *data,
            "write {i} ({} bytes) read back as {} bytes that differ",
            data.len(),
            got.len()
        );
    }
}
