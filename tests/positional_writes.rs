//! Positional writes start, grow and extend a file, as `pwrite` does
//! (#500).
//!
//! `write_at` -- and `fs_ntfs_write_file` over it -- took only a
//! non-resident `$DATA`, and only inside its current length. The first
//! write to every new file, every write to a small one and every append
//! were refused, and so were `truncate` and `grow` of a resident file. "Create
//! an empty file, then write it at increasing offsets" is how `cp`,
//! editors, archive extractors and every FUSE or FSKit write path work.
//!
//! Every file is read back through this crate and through the `ntfs`
//! crate, a parser that is not this one.

#![allow(unused_unsafe)]

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write;
use std::ffi::CString;
use std::fs::File;
use std::os::raw::c_void;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn fresh_volume(tag: &str) -> (String, PathIo) {
    let img = common::temp_image_path(format!("pwrite_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, io)
}

/// `path`'s contents, read by this crate and by the `ntfs` crate, which
/// must agree with each other and with `want`.
fn assert_contents(io: &mut PathIo, img: &str, path: &str, want: &[u8], after: &str) {
    <PathIo as BlockIo>::sync(io).expect("sync");
    let fs = fs_ntfs::facade::Filesystem::mount(img).expect("mount");
    let mut ours = vec![0u8; want.len() + 4096];
    let n = fs.read_file(path, 0, &mut ours).expect("read");
    ours.truncate(n);
    assert!(
        ours == want,
        "after {after}, {path} reads {} bytes that are not the {} expected",
        ours.len(),
        want.len()
    );
    let (ntfs, mut reader) = common::open(img);
    let theirs = common::read_file_all(&ntfs, &mut reader, path);
    assert!(
        theirs == want,
        "after {after}, the ntfs crate reads {path} differently"
    );
}

/// The issue's red test: an empty file written 1 MiB in 4 KiB chunks at
/// increasing offsets.
#[test]
fn a_new_file_is_written_in_chunks_at_increasing_offsets() {
    let (img, mut io) = fresh_volume("chunks");
    write::create_file_io(&mut io, "/", "new").expect("create /new");
    let mut want = Vec::new();
    for i in 0..256u32 {
        let chunk: Vec<u8> = (0..4096u32).map(|j| (i * 7 + j) as u8).collect();
        let n = write::write_at_io(&mut io, "/new", want.len() as u64, &chunk)
            .unwrap_or_else(|e| panic!("write chunk {i} at {}: {e}", want.len()));
        assert_eq!(n, 4096);
        want.extend(chunk);
    }
    assert_contents(&mut io, &img, "/new", &want, "256 chunked writes");
}

/// A small file overwritten in place, appended to, and written past its
/// end, which leaves zeros in the gap; then cut short and grown again.
#[test]
fn a_small_file_is_overwritten_appended_to_and_resized() {
    let (img, mut io) = fresh_volume("small");
    write::create_file_io(&mut io, "/", "s").expect("create /s");
    write::write_file_contents_io(&mut io, "/s", b"hello").expect("write /s");
    write::write_at_io(&mut io, "/s", 0, b"J").expect("overwrite");
    assert_contents(&mut io, &img, "/s", b"Jello", "an overwrite in place");
    write::write_at_io(&mut io, "/s", 5, b"!").expect("append");
    assert_contents(&mut io, &img, "/s", b"Jello!", "an append");
    write::write_at_io(&mut io, "/s", 10, b"x").expect("write past the end");
    assert_contents(
        &mut io,
        &img,
        "/s",
        b"Jello!\0\0\0\0x",
        "a write past the end",
    );
    write::truncate_io(&mut io, "/s", 3).expect("truncate to 3");
    assert_contents(&mut io, &img, "/s", b"Jel", "a truncate");
    write::grow_nonresident_io(&mut io, "/s", 6).expect("grow to 6");
    assert_contents(&mut io, &img, "/s", b"Jel\0\0\0", "a grow");
}

/// A non-resident file appended to past its end, and a resident one
/// outgrowing its record through a positional write.
#[test]
fn appends_extend_a_large_file_and_promote_a_small_one() {
    let (img, mut io) = fresh_volume("large");
    write::create_file_io(&mut io, "/", "big").expect("create /big");
    let mut want = vec![3u8; 8192];
    write::write_file_contents_io(&mut io, "/big", &want).expect("write /big");
    write::write_at_io(&mut io, "/big", 8192, b"tail").expect("append to /big");
    want.extend_from_slice(b"tail");
    assert_contents(&mut io, &img, "/big", &want, "an append to a large file");
    write::write_at_io(&mut io, "/big", 20000, b"far").expect("write far past the end");
    want.resize(20000, 0);
    want.extend_from_slice(b"far");
    assert_contents(&mut io, &img, "/big", &want, "a write far past the end");

    write::create_file_io(&mut io, "/", "grows").expect("create /grows");
    write::write_at_io(&mut io, "/grows", 0, b"start").expect("first write");
    write::write_at_io(&mut io, "/grows", 3000, &[9u8; 3000]).expect("outgrow the record");
    let mut want = b"start".to_vec();
    want.resize(3000, 0);
    want.extend(vec![9u8; 3000]);
    assert_contents(
        &mut io,
        &img,
        "/grows",
        &want,
        "a write that outgrows the record",
    );
}

/// Through the C ABI's positional write, the first write to a new file.
#[test]
fn the_c_abi_positional_write_starts_a_new_file() {
    let (img, mut io) = fresh_volume("capi");
    write::create_file_io(&mut io, "/", "c").expect("create /c");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    let img_c = CString::new(img.as_str()).unwrap();
    let path_c = CString::new("/c").unwrap();
    let data = vec![0x5Au8; 4096];
    let n = unsafe {
        fs_ntfs::fs_ntfs_write_file(
            img_c.as_ptr(),
            path_c.as_ptr(),
            0,
            data.as_ptr() as *const c_void,
            data.len() as u64,
        )
    };
    assert_eq!(n, 4096, "fs_ntfs_write_file returned {n}");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    assert_contents(&mut io, &img, "/c", &data, "fs_ntfs_write_file");
}
