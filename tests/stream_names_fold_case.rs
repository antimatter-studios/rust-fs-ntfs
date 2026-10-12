//! Named streams are matched by name under `$UpCase` (#494).
//!
//! Stream names were compared code unit for code unit, so writing `s1` to
//! a file that had `S1` made a second `$DATA` attribute rather than
//! rewriting the first. Windows matches stream names case-insensitively
//! for a case-insensitive open, the Win32 default (MS-FSA, "Open of an
//! Existing File"), so a file carrying both is one Windows never makes and
//! a Win32 caller can reach only one of. A stream keeps the spelling it
//! was created with, as on Windows.

mod common;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{read, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

fn volume_with_a_file(tag: &str) -> (PathIo, u64) {
    let img = common::temp_image_path(format!("streamcase_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (io, rec)
}

fn stream<T: BlockIo + ?Sized>(io: &mut T, rec: u64, name: &str) -> Option<Vec<u8>> {
    read::read_attribute_value_if_present(io, rec, AttrType::Data, Some(name))
        .unwrap_or_else(|e| panic!("read stream {name}: {e}"))
}

/// #494's reproduction, read back under both spellings.
#[test]
fn a_case_variant_rewrites_the_stream_it_names() {
    let (mut io, rec) = volume_with_a_file("ascii");
    write::write_named_stream_io(&mut io, "/f", "S1", b"upper").expect("write S1");
    write::write_named_stream_io(&mut io, "/f", "s1", b"lower").expect("write s1");
    assert_eq!(
        write::list_named_streams_io(&mut io, "/f").expect("list"),
        ["S1"],
        "a case variant made a second stream, or renamed the first"
    );
    assert_eq!(stream(&mut io, rec, "S1").as_deref(), Some(&b"lower"[..]));
    assert_eq!(stream(&mut io, rec, "s1").as_deref(), Some(&b"lower"[..]));
}

/// Folding is `$UpCase`'s, not ASCII's, and holds for a non-resident
/// stream too.
#[test]
fn a_case_variant_matches_under_the_upcase_table_and_when_non_resident() {
    let (mut io, rec) = volume_with_a_file("upcase");
    write::write_named_stream_io(&mut io, "/f", "Äpfel", b"one").expect("write Äpfel");
    write::write_named_stream_io(&mut io, "/f", "äPFEL", b"two").expect("write äPFEL");
    write::write_named_stream_io(&mut io, "/f", "Big", &[1u8; 64 * 1024]).expect("write Big");
    write::write_named_stream_io(&mut io, "/f", "BIG", &[2u8; 64 * 1024]).expect("write BIG");
    let mut listed = write::list_named_streams_io(&mut io, "/f").expect("list");
    listed.sort();
    assert_eq!(listed, ["Big", "Äpfel"]);
    assert_eq!(stream(&mut io, rec, "äpfel").as_deref(), Some(&b"two"[..]));
    assert_eq!(stream(&mut io, rec, "big"), Some(vec![2u8; 64 * 1024]));
}

/// Deleting through a case variant deletes the stream.
#[test]
fn a_case_variant_deletes_the_stream_it_names() {
    let (mut io, rec) = volume_with_a_file("delete");
    write::write_named_stream_io(&mut io, "/f", "Tags", b"t").expect("write Tags");
    write::delete_named_stream_io(&mut io, "/f", "TAGS").expect("delete TAGS");
    assert!(
        write::list_named_streams_io(&mut io, "/f")
            .expect("list")
            .is_empty(),
        "Tags is still listed"
    );
    assert_eq!(stream(&mut io, rec, "Tags"), None);
}
