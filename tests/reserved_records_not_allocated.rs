//! Records 16 to 23 are reserved for the volume's own use and are never
//! handed out to a file (#499).
//!
//! Windows keeps them for `$MFT`'s own extension records and gives a new
//! file a record from 24 up, as ntfs-3g does. This crate's search for a
//! free record began at 24 and then wrapped round to 0, so once every
//! record from 24 up was in use, a volume whose `$MFT:$Bitmap` leaves 16 to
//! 23 clear -- as Windows' does -- had them handed to user files, and a
//! file in a reserved record is one the metafile guard then refuses to
//! write.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mft_bitmap::{self, MftBitmap};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u32 = 4096;
const FIRST_USER_RECORD: u64 = 24;

/// A fresh volume whose bitmap shows records 16 to 23 free and every
/// record from 24 up that `$MFT` holds in use.
fn only_reserved_records_free(tag: &str) -> (PathIo, MftBitmap) {
    let img = common::temp_image_path(format!("reserved_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, CLUSTER, Some("RSVD"), Some(7))
        .expect("format_filesystem");
    let bm = mft_bitmap::locate_io(&mut io).expect("locate $MFT:$Bitmap");
    for n in 16..FIRST_USER_RECORD {
        if mft_bitmap::is_allocated_io(&mut io, &bm, n).expect("read a bit") {
            mft_bitmap::free_io(&mut io, &bm, n).expect("clear a reserved record's bit");
        }
    }
    for n in FIRST_USER_RECORD..bm.record_limit() {
        if !mft_bitmap::is_allocated_io(&mut io, &bm, n).expect("read a bit") {
            mft_bitmap::allocate_io(&mut io, &bm, n).expect("mark a record in use");
        }
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (io, bm)
}

#[test]
fn a_record_for_a_file_is_never_a_reserved_one() {
    let (mut io, bm) = only_reserved_records_free("search");
    for hint in [0, 16, FIRST_USER_RECORD, bm.record_limit()] {
        let (_, got) = mft_bitmap::find_or_grow_free_record_io(&mut io, hint)
            .unwrap_or_else(|e| panic!("find a record from {hint}: {e}"));
        assert!(
            got >= bm.record_limit(),
            "with only records 16 to 23 free, a record found from {hint} is {got}, not one \
             $MFT grew to hold"
        );
    }
}

#[test]
fn a_new_file_gets_a_record_from_24_up_when_only_reserved_ones_are_free() {
    let (mut io, bm) = only_reserved_records_free("create");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    assert!(
        rec >= bm.record_limit(),
        "/f was given record {rec}; with every record from 24 to {} in use, $MFT grows",
        bm.record_limit()
    );
    write::write_file_contents_io(&mut io, "/f", b"data").expect("write /f");
}
