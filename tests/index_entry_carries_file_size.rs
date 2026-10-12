//! A directory entry carries the size of the file it names (#506).
//!
//! Every `$I30` entry holds a copy of the file's `$FILE_NAME`, whose
//! "duplicated information" includes the data size and the allocated
//! size. Windows serves directory enumeration from that copy -- `dir`,
//! Explorer, `FindFirstFile` -- and refreshes it when the last handle to
//! the file closes. This crate filled it in at create time and never
//! again, so every file it wrote listed on Windows as 0 bytes until
//! something on Windows opened and closed it.
//!
//! The entries are decoded here with the `ntfs` crate, a parser that is
//! not this one.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mft_io;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{read, write};
use ntfs::indexes::NtfsFileNameIndex;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn fresh_volume(tag: &str) -> (String, PathIo) {
    let img = common::temp_image_path(format!("dupsize_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, io)
}

/// The (data size, allocated size) the entry for `name` in `dir` carries,
/// as the `ntfs` crate decodes it.
fn entry_sizes(img: &str, dir: &str, name: &str) -> (u64, u64) {
    let (ntfs, mut reader) = common::open(img);
    let d = common::navigate(&ntfs, &mut reader, dir);
    let index = d.directory_index(&mut reader).expect("directory index");
    let mut finder = index.finder();
    let entry = NtfsFileNameIndex::find(&mut finder, &ntfs, &mut reader, name)
        .unwrap_or_else(|| panic!("{dir}/{name} has no index entry"))
        .expect("read the index entry");
    let key = entry
        .key()
        .expect("the entry has a key")
        .expect("decode the key");
    (key.data_size(), key.allocated_size())
}

/// The (data size, allocated size) the file's own unnamed `$DATA` says:
/// a resident value's length, rounded up to 8 for its allocation, or a
/// non-resident attribute's header fields.
fn record_sizes<T: BlockIo + ?Sized>(io: &mut T, path: &str) -> (u64, u64) {
    let rec = read::resolve_path(io, path).expect("resolve");
    let (_, record) = mft_io::read_mft_record_io(io, rec).expect("read the record");
    let loc = attr_io::find_attribute(&record, AttrType::Data, None).expect("$DATA");
    if loc.is_resident {
        let len = loc.resident_value_length.expect("value length") as u64;
        return (len, len.div_ceil(8) * 8);
    }
    let field = |off: usize| {
        u64::from_le_bytes(
            record[loc.attr_offset + off..loc.attr_offset + off + 8]
                .try_into()
                .unwrap(),
        )
    };
    (field(0x30), field(0x28))
}

fn assert_entry_matches<T: BlockIo + ?Sized>(
    io: &mut T,
    img: &str,
    dir: &str,
    name: &str,
    want_size: u64,
    after: &str,
) {
    <T as BlockIo>::sync(io).expect("sync");
    let path = format!("{}/{name}", dir.trim_end_matches('/'));
    let (size, allocated) = record_sizes(io, &path);
    assert_eq!(size, want_size, "{path}'s own $DATA after {after}");
    assert_eq!(
        entry_sizes(img, dir, name),
        (size, allocated),
        "{path}'s directory entry after {after} does not carry the file's (size, allocated)"
    );
}

/// #506's measurements: a resident write, a non-resident write, a grow,
/// a write inside the file and a shrink, each reflected in the root's entry.
/// (`write_at` does not extend a file, #500.)
#[test]
fn every_size_change_reaches_the_directory_entry() {
    let (img, mut io) = fresh_volume("root");
    write::create_file_io(&mut io, "/", "f").expect("create /f");

    write::write_file_contents_io(&mut io, "/f", b"0123456789").expect("write 10 bytes");
    assert_entry_matches(&mut io, &img, "/", "f", 10, "a resident write");

    write::write_file_contents_io(&mut io, "/f", &[1u8; 20000]).expect("write 20000 bytes");
    assert_entry_matches(&mut io, &img, "/", "f", 20000, "a non-resident write");

    write::truncate_io(&mut io, "/f", 50000).expect("grow to 50000");
    assert_entry_matches(&mut io, &img, "/", "f", 50000, "a grow");

    write::write_at_io(&mut io, "/f", 46000, &[2u8; 4000]).expect("write inside the file");
    assert_entry_matches(&mut io, &img, "/", "f", 50000, "a write inside the file");

    write::truncate_io(&mut io, "/f", 100).expect("shrink to 100");
    assert_entry_matches(&mut io, &img, "/", "f", 100, "a shrink");
}

/// The same for an entry in an INDX block, and for every name a file
/// has: both hard links carry the size.
#[test]
fn entries_in_index_blocks_and_every_link_carry_the_size() {
    let (img, mut io) = fresh_volume("indx");
    write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    for i in 0..120 {
        write::create_file_io(&mut io, "/d", &format!("n{i:03}")).expect("create in /d");
    }
    write::link_io(&mut io, "/d/n077", "/", "link").expect("link /d/n077 as /link");

    write::write_file_contents_io(&mut io, "/d/n077", &[3u8; 12345]).expect("write /d/n077");
    assert_entry_matches(&mut io, &img, "/d", "n077", 12345, "a write in /d");
    assert_entry_matches(
        &mut io,
        &img,
        "/",
        "link",
        12345,
        "a write through the other link",
    );

    write::truncate_io(&mut io, "/link", 5).expect("truncate /link");
    assert_entry_matches(&mut io, &img, "/", "link", 5, "a truncate through /link");
    assert_entry_matches(
        &mut io,
        &img,
        "/d",
        "n077",
        5,
        "a truncate through the other link",
    );
}
