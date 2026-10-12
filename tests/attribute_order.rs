//! A record's attributes are kept in NTFS order: by type, and within a
//! type by name, case-insensitively under `$UpCase` and then
//! case-sensitively (#467).
//!
//! Readers rely on the order. ntfs-3g and Windows stop looking for an
//! attribute once they pass the slot it would sort into, so a named stream
//! written after one that sorts later is one they cannot find: with `zz`
//! written before `aa`, `ntfscat -n aa` says "Cannot find attribute". This
//! crate's own reader scans the whole record, which is why nothing here
//! noticed.

mod common;

use fs_ntfs::attr_io;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mft_io;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u32 = 4096;

fn volume_with_a_file(tag: &str) -> (String, PathIo, u64) {
    let dst = common::temp_image_path(format!("attrorder_{tag}"));
    let f = File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, CLUSTER, Some("ORDER"), Some(7))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (dst, io, rec)
}

/// Every attribute of the record, as (type, name), in on-disk order.
fn attributes<T: BlockIo + ?Sized>(io: &mut T, rec: u64) -> Vec<(u32, String)> {
    let (_, record) = mft_io::read_mft_record_io(io, rec).expect("read the record");
    attr_io::iter_attributes(&record)
        .map(|loc| (loc.type_code, attr_io::decode_attr_name(&record, &loc)))
        .collect()
}

/// NTFS order: type, then name case-insensitively, then case-sensitively.
/// The names here are ASCII, where `$UpCase` folds exactly as
/// `to_ascii_uppercase` does.
fn assert_ntfs_order(attrs: &[(u32, String)]) {
    for pair in attrs.windows(2) {
        let ((ta, na), (tb, nb)) = (&pair[0], &pair[1]);
        let key = |t: &u32, n: &String| (*t, n.to_ascii_uppercase(), n.clone());
        assert!(
            key(ta, na) <= key(tb, nb),
            "attribute ({ta:#x}, {na:?}) comes before ({tb:#x}, {nb:?}); the record is \
             out of NTFS order: {attrs:?}"
        );
    }
}

/// #467's reproduction: named streams written in an order that does not
/// collate, resident and non-resident, land in collated order.
#[test]
fn named_streams_are_inserted_in_collation_order() {
    let (_img, mut io, rec) = volume_with_a_file("streams");
    for (name, len) in [("zz", 8), ("aa", 8), ("B", 8), ("a", 8), ("m", 64 * 1024)] {
        write::write_named_stream_io(&mut io, "/f", name, &vec![b'x'; len])
            .unwrap_or_else(|e| panic!("write stream {name}: {e}"));
    }
    let names = write::list_named_streams_io(&mut io, "/f").expect("list streams");
    assert_eq!(
        names,
        ["a", "aa", "B", "m", "zz"],
        "named streams are not in collation order on disk"
    );
    assert_ntfs_order(&attributes(&mut io, rec));
}

/// A stream added after an `$EA` goes before it, by type.
#[test]
fn a_stream_added_after_an_ea_lands_before_it() {
    let (_img, mut io, rec) = volume_with_a_file("after_ea");
    write::write_ea_io(&mut io, "/f", b"USER.KEY", b"value", 0).expect("write an EA");
    write::write_named_stream_io(&mut io, "/f", "s", b"stream").expect("write a stream");
    assert_ntfs_order(&attributes(&mut io, rec));
}
