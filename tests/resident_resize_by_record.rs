//! `truncate_by_record_number` resizes a resident `$DATA` as the path-based
//! `truncate` does (#1398, STRUCT-07): by path and by record number a resize
//! leaves the same content, and every cluster a resize retires is freed.
//!
//! Content, sizes and the directory entry are read back with the upstream
//! `ntfs` parser.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{bitmap, mft_io, write};
use ntfs::indexes::NtfsFileNameIndex;
use std::path::Path;

const VOL_SIZE: u64 = 32 * 1024 * 1024;
const CLUSTER: u32 = 4096;
const NAME: &str = "small.bin";
const FILE: &str = "/small.bin";
/// Far more than a 4096-byte file record holds.
const PROMOTED: usize = 16 * CLUSTER as usize;
/// The attribute header's flags word, and its "compressed" value.
const ATTR_FLAGS: usize = 0x0C;
const COMPRESSED: u8 = 0x01;

fn original() -> Vec<u8> {
    (0..100u8).collect()
}

/// A fresh volume holding one file with a resident `$DATA` of
/// [`original`], and that file's record number.
fn volume(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("resident_resize_{tag}"));
    std::fs::File::create(&img)
        .expect("create")
        .set_len(VOL_SIZE)
        .expect("set_len");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(
        &mut io,
        VOL_SIZE,
        CLUSTER,
        CLUSTER,
        Some("RESIDENT"),
        Some(1398),
    )
    .expect("format");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    let path = Path::new(&img);
    let record = write::create_file(path, "/", NAME).expect("create");
    write::write_file_contents(path, FILE, &original()).expect("write");
    assert!(is_resident(&img, record), "the fixture starts resident");
    (img, record)
}

fn is_resident(img: &str, record: u64) -> bool {
    let (_, bytes) = mft_io::read_mft_record(Path::new(img), record).expect("record");
    attr_io::find_attribute(&bytes, AttrType::Data, None)
        .expect("unnamed $DATA")
        .is_resident
}

fn free_clusters(img: &str) -> u64 {
    let path = Path::new(img);
    let location = bitmap::locate_bitmap(path).expect("bitmap");
    bitmap::count_free(path, &location).expect("count")
}

fn contents(img: &str) -> Vec<u8> {
    let (ntfs, mut reader) = common::open(img);
    common::read_file_all(&ntfs, &mut reader, FILE)
}

/// The data size the root directory's entry for the file carries.
fn entry_size(img: &str) -> u64 {
    let (ntfs, mut reader) = common::open(img);
    let root = ntfs.root_directory(&mut reader).expect("root");
    let index = root.directory_index(&mut reader).expect("root index");
    let mut finder = index.finder();
    let entry = NtfsFileNameIndex::find(&mut finder, &ntfs, &mut reader, NAME)
        .expect("name present")
        .expect("entry");
    let key = entry.key().expect("key").expect("key reads");
    key.data_size()
}

/// [`original`] cut or zero-extended to `size`.
fn resized(size: usize) -> Vec<u8> {
    let mut value = original();
    value.resize(size, 0);
    value
}

fn truncate(img: &str, record: u64, size: usize) {
    let new = write::truncate_by_record_number(Path::new(img), record, size as u64)
        .unwrap_or_else(|e| panic!("truncate to {size}: {e}"));
    assert_eq!(new, size as u64);
}

/// Resize to `size` and check the file stays resident, holds exactly the
/// resized bytes, tells its directory entry, and moves no cluster.
fn resize_stays_resident(tag: &str, size: usize) {
    let (img, record) = volume(tag);
    let free = free_clusters(&img);
    truncate(&img, record, size);
    assert!(is_resident(&img, record));
    assert_eq!(contents(&img), resized(size));
    assert_eq!(entry_size(&img), size as u64);
    assert_eq!(free_clusters(&img), free);
}

#[test]
fn a_resident_value_is_cut_short_by_record_number() {
    resize_stays_resident("cut", 10);
}

#[test]
fn a_resident_value_is_extended_with_zeros_by_record_number() {
    resize_stays_resident("extend", 300);
}

#[test]
fn a_resident_value_is_emptied_by_record_number() {
    resize_stays_resident("empty", 0);
}

#[test]
fn a_resident_value_outgrowing_its_record_is_promoted_and_later_freed_whole() {
    let (img, record) = volume("promote");
    let free = free_clusters(&img);

    truncate(&img, record, PROMOTED);
    assert!(!is_resident(&img, record));
    assert_eq!(contents(&img), resized(PROMOTED));
    assert_eq!(entry_size(&img), PROMOTED as u64);
    assert_eq!(free_clusters(&img), free - 16);

    truncate(&img, record, 0);
    // A non-resident resize by record number leaves the directory entry to
    // its caller; see `refresh_directory_entry_sizes_io`.
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::refresh_directory_entry_sizes_io(&mut io, record).expect("refresh");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    assert_eq!(contents(&img), Vec::<u8>::new());
    assert_eq!(entry_size(&img), 0);
    assert_eq!(free_clusters(&img), free, "every retired cluster is freed");
}

#[test]
fn a_size_larger_than_the_volume_is_refused_with_nothing_written() {
    let (img, record) = volume("too_large");
    let before = common::snapshot_image(&img).expect("snapshot");
    let error = write::truncate_by_record_number(Path::new(&img), record, u64::MAX)
        .expect_err("no volume holds this");
    assert!(error.to_string().contains("no space"), "{error}");
    common::assert_image_unchanged(&img, &before, "a refused resize");
}

#[test]
fn a_transformed_resident_value_is_refused_with_nothing_written() {
    let (img, record) = volume("transformed");
    mft_io::update_mft_record(Path::new(&img), record, |bytes| {
        let data = attr_io::find_attribute(bytes, AttrType::Data, None).expect("unnamed $DATA");
        bytes[data.attr_offset + ATTR_FLAGS] |= COMPRESSED;
        Ok(())
    })
    .expect("flag the attribute");
    let before = common::snapshot_image(&img).expect("snapshot");
    let error = write::truncate_by_record_number(Path::new(&img), record, 10)
        .expect_err("compressed data is not resized");
    assert!(error.to_string().contains("compressed"), "{error}");
    common::assert_image_unchanged(&img, &before, "a refused resize");
}
