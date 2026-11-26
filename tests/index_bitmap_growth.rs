//! One directory grows past the 64 index blocks an eight-byte resident
//! `$Bitmap:$I30` describes (#1398, source #1346).

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{mft_io, write};
use ntfs::indexes::NtfsFileNameIndex;
use ntfs::{Ntfs, NtfsReadSeek};
use std::io::BufReader;
use std::path::Path;

const VOL_SIZE: u64 = 64 * 1024 * 1024;
const CLUSTER: u32 = 4096;
/// Sorted names leave leaves about half full, so this many need well over
/// 64 blocks of 4 KiB.
const NAMES: usize = 3000;
/// Every this-many-th name holds content, written before the directory grows
/// further.
const CONTENT_EVERY: usize = 250;

fn name(i: usize) -> String {
    format!("f_{i:04}.txt")
}

fn content(i: usize) -> Vec<u8> {
    format!("content of file {i}").into_bytes()
}

/// `/d` as the upstream `ntfs` parser reads it: every name in index order,
/// and the bytes of each file `with_content` selects.
fn read_directory(img: &str, with_content: &[usize]) -> (Vec<String>, Vec<Vec<u8>>) {
    let file = std::fs::File::open(img).expect("open");
    let mut reader = BufReader::new(file);
    let mut ntfs = Ntfs::new(&mut reader).expect("ntfs");
    ntfs.read_upcase_table(&mut reader).expect("upcase");
    let root = ntfs.root_directory(&mut reader).expect("root");
    let root_index = root.directory_index(&mut reader).expect("root index");
    let mut finder = root_index.finder();
    let directory = NtfsFileNameIndex::find(&mut finder, &ntfs, &mut reader, "d")
        .expect("d present")
        .expect("d found")
        .to_file(&ntfs, &mut reader)
        .expect("d record");
    let index = directory.directory_index(&mut reader).expect("d index");

    let mut names = Vec::new();
    let mut entries = index.entries();
    while let Some(entry) = entries.next(&mut reader) {
        let entry = entry.expect("entry");
        let key = entry.key().expect("key").expect("key reads");
        names.push(key.name().to_string_lossy());
    }

    let mut contents = Vec::new();
    for &i in with_content {
        let mut finder = index.finder();
        let file = NtfsFileNameIndex::find(&mut finder, &ntfs, &mut reader, &name(i))
            .unwrap_or_else(|| panic!("{} absent", name(i)))
            .expect("found")
            .to_file(&ntfs, &mut reader)
            .expect("file record");
        let data = file
            .data(&mut reader, "")
            .expect("unnamed data")
            .expect("data item");
        let attribute = data.to_attribute().expect("data attribute");
        let mut value = attribute.value(&mut reader).expect("data value");
        let mut bytes = vec![0u8; value.len() as usize];
        value.read_exact(&mut reader, &mut bytes).expect("read");
        contents.push(bytes);
    }
    (names, contents)
}

#[test]
fn one_directory_grows_past_sixty_four_index_blocks() {
    let img = common::temp_image_path("index_bitmap_growth".to_string());
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
        Some("BITMAP"),
        Some(1398),
    )
    .expect("format");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    let path = Path::new(&img);
    let directory = write::mkdir(path, "/", "d").expect("mkdir");

    let with_content: Vec<usize> = (0..NAMES).step_by(CONTENT_EVERY).collect();
    for i in 0..NAMES {
        write::create_file(path, "/d", &name(i))
            .unwrap_or_else(|e| panic!("create {} after {i} names: {e}", name(i)));
        if i % CONTENT_EVERY == 0 {
            write::write_file_contents(path, &format!("/d/{}", name(i)), &content(i))
                .expect("write content");
        }
    }

    let (_, record) = mft_io::read_mft_record(path, directory).expect("directory record");
    let bitmap = attr_io::find_attribute(&record, AttrType::Bitmap, Some("$I30"))
        .expect("directory index bitmap");
    let length = bitmap.resident_value_length.expect("resident bitmap");
    assert!(
        length > 8 && length % 8 == 0,
        "bitmap must grow in eight-byte steps past 64 blocks, is {length} bytes"
    );
    let allocation = attr_io::find_attribute(&record, AttrType::IndexAllocation, Some("$I30"))
        .expect("directory index allocation");
    let blocks = allocation.non_resident_value_length.expect("length") / u64::from(CLUSTER);
    assert!(
        blocks > 64 && blocks <= u64::from(length) * 8,
        "{blocks} index blocks against a {length}-byte bitmap"
    );

    let expected: Vec<String> = (0..NAMES).map(name).collect();
    let (names, contents) = read_directory(&img, &with_content);
    assert_eq!(
        names, expected,
        "independent parser lost or reordered names"
    );
    for (&i, bytes) in with_content.iter().zip(&contents) {
        assert_eq!(bytes, &content(i), "content of {}", name(i));
    }

    // Removing names from blocks past the first eight bytes frees them.
    for i in (NAMES - 200)..NAMES {
        write::unlink(path, &format!("/d/{}", name(i))).expect("unlink");
    }
    let (names, _) = read_directory(&img, &[]);
    assert_eq!(names, expected[..NAMES - 200], "names after removal");
}
