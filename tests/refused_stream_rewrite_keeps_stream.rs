//! A named-stream rewrite that is refused leaves the stream as it was
//! (#469).
//!
//! When the new contents were too big to stay resident,
//! `write_named_stream_io` deleted the existing stream first and only then
//! tried to write the non-resident replacement. Whenever that second step
//! failed -- no room in the record for the new attribute header, no free
//! run of clusters -- the call returned `Err` with the stream already
//! gone: a refused write that deleted the data it was only meant to
//! replace.

mod common;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::bitmap;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{read, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;

/// A fresh volume with 1 KiB records and an empty `/f`.
fn volume_with_a_file(tag: &str) -> (PathIo, u64) {
    let dst = common::temp_image_path(format!("streamkeep_{tag}"));
    let f = File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (io, rec)
}

/// Mark every free cluster of the volume in use, so that nothing written
/// after it can go into clusters.
fn take_every_free_cluster(io: &mut PathIo) {
    let bm = bitmap::locate_bitmap_io(io).expect("locate $Bitmap");
    let mut want = bitmap::count_free_io(io, &bm).expect("count free clusters");
    while want > 0 {
        match bitmap::find_free_run_io(io, &bm, want, 0).expect("find a free run") {
            Some(lcn) => {
                bitmap::allocate_io(io, &bm, lcn, want).expect("take a free run");
                want = bitmap::count_free_io(io, &bm).expect("count free clusters");
            }
            None => want /= 2,
        }
    }
    assert_eq!(
        bitmap::count_free_io(io, &bm).expect("count free clusters"),
        0
    );
}

fn stream<T: BlockIo + ?Sized>(io: &mut T, rec: u64, name: &str) -> Option<Vec<u8>> {
    read::read_attribute_value_if_present(io, rec, AttrType::Data, Some(name))
        .expect("read the stream")
}

/// #469's path: the record has room for neither the resident nor the
/// non-resident `zz`, and the refused rewrite must leave the empty
/// resident `zz` in place.
///
/// #469's reproduction was a file whose `$EA` filled the record. A stream
/// its record cannot hold now moves to an extension record instead, a
/// file's (#480) and, as Windows does, a directory's (#814), so a full
/// record alone no longer refuses it. A stream goes through the list only
/// when it has a cluster to go to otherwise, so here the volume has no
/// free cluster: the record fills with small streams until the next is
/// refused, and the rewrite has room neither in the record nor in clusters.
#[test]
fn a_rewrite_refused_for_want_of_record_space_keeps_the_resident_stream() {
    let (mut io, rec) = volume_with_a_file("record");
    write::write_named_stream_io(&mut io, "/f", "zz", b"").expect("create zz, empty");
    take_every_free_cluster(&mut io);
    let filled = (0..64).any(|i| {
        write::write_named_stream_io(&mut io, "/f", &format!("f{i:02}"), &[7u8; 8]).is_err()
    });
    assert!(filled, "64 small streams fitted in a file's record");

    let got = write::write_named_stream_io(&mut io, "/f", "zz", &[5u8; 556]);
    assert!(
        got.is_err(),
        "the rewrite was expected to be refused for want of record space, and succeeded; \
         this test no longer reaches #469's path"
    );
    assert_eq!(
        stream(&mut io, rec, "zz"),
        Some(Vec::new()),
        "the refused rewrite ({got:?}) did not leave zz as it was"
    );
}

/// The same for a stream that is already non-resident, refused because
/// the volume has no run of clusters big enough: its contents and its
/// clusters are both left as they were.
#[test]
fn a_rewrite_refused_for_want_of_clusters_keeps_the_nonresident_stream() {
    let (mut io, rec) = volume_with_a_file("clusters");
    let old: Vec<u8> = (0..8192u32).map(|i| (i * 7) as u8).collect();
    write::write_named_stream_io(&mut io, "/f", "zz", &old).expect("write zz, non-resident");
    let bm = bitmap::locate_bitmap_io(&mut io).expect("locate $Bitmap");
    let free_before = bitmap::count_free_io(&mut io, &bm).expect("count free clusters");

    let got = write::write_named_stream_io(&mut io, "/f", "zz", &vec![1u8; 2 * VOL_SIZE as usize]);
    assert!(got.is_err(), "a rewrite bigger than the volume succeeded");
    assert_eq!(
        stream(&mut io, rec, "zz").as_deref(),
        Some(&old[..]),
        "the refused rewrite ({got:?}) did not leave zz's contents as they were"
    );
    assert_eq!(
        bitmap::count_free_io(&mut io, &bm).expect("count free clusters"),
        free_before,
        "the refused rewrite changed how many clusters are free"
    );
}

/// A non-resident rewrite that succeeds gives the old stream's clusters
/// back: rewriting at the same size leaves as many clusters free.
#[test]
fn a_nonresident_rewrite_frees_the_clusters_it_replaces() {
    let (mut io, rec) = volume_with_a_file("frees");
    write::write_named_stream_io(&mut io, "/f", "zz", &[3u8; 8192]).expect("write zz");
    let bm = bitmap::locate_bitmap_io(&mut io).expect("locate $Bitmap");
    let free_before = bitmap::count_free_io(&mut io, &bm).expect("count free clusters");

    write::write_named_stream_io(&mut io, "/f", "zz", &[4u8; 8192]).expect("rewrite zz");
    assert_eq!(stream(&mut io, rec, "zz"), Some(vec![4u8; 8192]));
    assert_eq!(
        bitmap::count_free_io(&mut io, &bm).expect("count free clusters"),
        free_before,
        "rewriting zz at the same size changed how many clusters are free"
    );
}
