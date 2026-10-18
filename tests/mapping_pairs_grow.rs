//! A non-resident `$DATA` whose mapping pairs outgrow its attribute grows the
//! attribute into its record (#1240).
//!
//! Two files are extended in turn, so neither gets contiguous clusters and each
//! write adds a run. `/f` also holds a named stream, recorded after its unnamed
//! `$DATA`, which has to move when the attribute grows. The `ntfs` crate, a
//! parser that is not this one, reads the contents back, and the volume checks
//! clean. The Windows matrix scenario `cli-fragmented-data-win-verify-chkdsk`
//! has `chkdsk` grade the same shape.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fsck, mft_io, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 64 * 1024 * 1024;
const RECORD_SIZE: u32 = 1024;
const CLUSTER: u32 = 4096;
const STREAM: &[u8] = b"held in a named stream after the unnamed data";

fn volume(tag: &str) -> String {
    let img = common::temp_image_path(format!("mapgrow_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, RECORD_SIZE, None, Some(1))
        .expect("format_filesystem");
    write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::create_file_io(&mut io, "/", "g").expect("create /g");
    write::write_file_contents_io(&mut io, "/f", &vec![b'f'; 8192]).expect("write /f");
    write::write_file_contents_io(&mut io, "/g", &vec![b'g'; 8192]).expect("write /g");
    write::write_named_stream_io(&mut io, "/f", "s", STREAM).expect("write /f:s");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    img
}

/// The bytes of `/f`'s unnamed `$DATA` attribute record.
fn data_attribute_length(img: &str, rec: u64) -> usize {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f");
    attr_io::find_attribute(&record, AttrType::Data, None)
        .expect("/f has an unnamed $DATA")
        .attr_length
}

#[test]
fn a_fragmented_file_grows_its_attribute_and_keeps_what_follows_it() {
    let img = volume("frag");
    let rec = write::resolve_path_to_record_number(Path::new(&img), "/f").expect("find /f");
    let before = data_attribute_length(&img, rec);
    let mut f = vec![b'f'; 8192];
    let mut g = vec![b'g'; 8192];
    for round in 0..40u8 {
        // Past each end, so each write allocates, in turn, so runs alternate.
        let chunk = [round; 4096];
        write::write_at(Path::new(&img), "/f", f.len() as u64, &chunk)
            .unwrap_or_else(|e| panic!("round {round}: extend /f: {e}"));
        f.extend_from_slice(&chunk);
        write::write_at(Path::new(&img), "/g", g.len() as u64, &chunk)
            .unwrap_or_else(|e| panic!("round {round}: extend /g: {e}"));
        g.extend_from_slice(&chunk);
    }
    assert!(
        data_attribute_length(&img, rec) > before,
        "the test proves nothing: /f's attribute never had to grow"
    );

    let (ntfs, mut reader) = common::open(&img);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/f"), f);
    assert_eq!(common::read_file_all(&ntfs, &mut reader, "/g"), g);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    assert_eq!(
        fs_ntfs::read::read_attribute_value(&mut io, rec, AttrType::Data, Some("s"))
            .expect("read /f:s"),
        STREAM
    );
    let report = fsck::check_io(&mut io).expect("check the volume");
    assert!(
        report.findings.is_empty(),
        "check_io finds the volume unclean: {:?}",
        report.findings
    );
    std::fs::remove_file(&img).ok();
}
