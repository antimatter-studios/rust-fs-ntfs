//! What Windows refuses to name or set, this crate refuses to write.
//!
//! - #497: a named stream's name goes through the same reserved-character,
//!   control-character and trailing-space-or-period checks as a file name.
//!   A stream named `a:b` or `x:$DATA` can never be opened through any
//!   Windows API, since the colon is the stream delimiter, and a NUL cuts
//!   the name short differently for each reader.
//! - #502: `set_file_attributes` changes only the bits `SetFileAttributes`
//!   can. DIRECTORY, SPARSE_FILE, REPARSE_POINT, COMPRESSED, ENCRYPTED and
//!   the reserved bits each promise an on-disk structure the call does not
//!   create.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write::{self, FileAttributesChange};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u32 = 4096;

fn volume_with_a_file(tag: &str) -> (String, PathIo) {
    let dst = common::temp_image_path(format!("namesattrs_{tag}"));
    let f = File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, CLUSTER, Some("NAMES"), Some(7))
        .expect("format_filesystem");
    write::create_file_io(&mut io, "/", "f.txt").expect("create /f.txt");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (dst, io)
}

/// #497: the four writes the issue measured, each refused.
#[test]
fn a_stream_name_windows_cannot_address_is_refused() {
    let (_img, mut io) = volume_with_a_file("streams");
    for name in ["a:b", "x:$DATA", "a/b", "a\0b", "trailing.", "trailing "] {
        let small = write::write_named_stream_io(&mut io, "/f.txt", name, b"data");
        assert!(
            small.is_err(),
            "write_named_stream_io wrote a stream named {name:?}"
        );
        let large = write::write_named_stream_io(&mut io, "/f.txt", name, &[7u8; 64 * 1024]);
        assert!(
            large.is_err(),
            "write_named_stream_io wrote a non-resident stream named {name:?}"
        );
        let resident = write::write_named_stream_resident_io(&mut io, "/f.txt", name, b"data");
        assert!(
            resident.is_err(),
            "write_named_stream_resident_io wrote a stream named {name:?}"
        );
    }
    let streams = write::list_named_streams_io(&mut io, "/f.txt").expect("list streams");
    assert!(
        streams.is_empty(),
        "refused writes left streams behind: {streams:?}"
    );
    // A name Windows can address is still written.
    write::write_named_stream_io(&mut io, "/f.txt", "Zone.Identifier", b"[ZoneTransfer]")
        .expect("an ordinary stream name is written");
}

/// #502: each structural bit, and the undefined ones, refused in `add`
/// and in `remove`; the settable bits still change.
#[test]
fn a_structural_file_attribute_is_refused() {
    let (_img, mut io) = volume_with_a_file("attributes");
    // DIRECTORY, DEVICE, SPARSE_FILE, REPARSE_POINT, COMPRESSED,
    // ENCRYPTED, and every bit at once.
    for bits in [0x10u32, 0x40, 0x200, 0x400, 0x800, 0x4000, 0xffff_ffff] {
        let added = write::set_file_attributes_io(
            &mut io,
            "/f.txt",
            FileAttributesChange {
                add: bits,
                remove: 0,
            },
        );
        assert!(
            added.is_err(),
            "set_file_attributes added {bits:#x} to a plain file"
        );
        let removed = write::set_file_attributes_io(
            &mut io,
            "/f.txt",
            FileAttributesChange {
                add: 0,
                remove: bits,
            },
        );
        assert!(
            removed.is_err(),
            "set_file_attributes removed {bits:#x} from a plain file"
        );
    }
    // READONLY, HIDDEN, SYSTEM, ARCHIVE, TEMPORARY, OFFLINE and
    // NOT_CONTENT_INDEXED are what SetFileAttributes sets.
    write::set_file_attributes_io(
        &mut io,
        "/f.txt",
        FileAttributesChange {
            add: 0x1 | 0x2 | 0x4 | 0x20 | 0x100 | 0x1000 | 0x2000,
            remove: 0,
        },
    )
    .expect("the settable bits are set");
}
