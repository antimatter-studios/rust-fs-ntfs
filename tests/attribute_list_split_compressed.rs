//! A compressed stream whose run list is split into extents named in the
//! `$ATTRIBUTE_LIST` is read through every extent (#1073).
//!
//! Compressed reads go one compression unit at a time over the attribute's
//! run list. A large or fragmented compressed file's run list does not fit
//! one MFT record, and Windows splits it into extents, each in a record of
//! its own. This writes a stream with a run per one-cluster hole -- more
//! runs than one record holds -- then rewrites every extent the way
//! Windows lays out a compressed one: flagged compressed with sixteen-
//! cluster units, one unit LZNT1-compressed whose hole crosses from one
//! extent into the next, one LZNT1-compressed and one deallocated unit
//! wholly in the last extent, and the rest stored raw. The expected bytes
//! are built from the format by hand, not by this crate's codec, and
//! whole and ranged reads must return every one of them.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{bitmap, data_runs, mft_io, read, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const RECORD_SIZE: u32 = 1024;
const CLUSTER: u32 = 4096;
/// One-cluster holes the free space is cut into.
const HOLES: usize = 420;
/// The stream's length in clusters: one run per hole, more runs than the
/// mapping pairs of one MFT record hold, fewer than there are holes.
const STREAM_CLUSTERS: usize = 400;

/// `/f`, with an EA and enough named streams that its base record filled
/// and it gained an `$ATTRIBUTE_LIST`.
fn volume_with_a_listed_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("alsplit_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, RECORD_SIZE, None, Some(1))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    write::write_ea_io(&mut io, "/f", b"USER.TAG", &[b'e'; 64], 0).expect("write /f's EA");
    for i in 0..8 {
        write::write_named_stream_io(&mut io, "/f", &format!("s{i}"), &[b'a' + i as u8; 120])
            .unwrap_or_else(|e| panic!("write /f:s{i}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    assert!(
        read::read_attribute_value_if_present(&mut io, rec, AttrType::AttributeList, None)
            .expect("read /f's list")
            .is_some(),
        "/f's base record never filled, so it has no $ATTRIBUTE_LIST to test"
    );
    (img, rec)
}

/// The longest run of free clusters on the volume.
fn longest_free_run(io: &mut PathIo) -> u64 {
    let bm = bitmap::locate_bitmap_io(io).expect("locate $Bitmap");
    let (mut lo, mut hi) = (0u64, bm.total_bits);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let found = bitmap::find_free_run_io(io, &bm, mid, 0).expect("scan $Bitmap");
        if found.is_some() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Cut the volume's free space into one-cluster holes: `2 * HOLES`
/// one-cluster files, every free run longer than one cluster filled by a
/// file of its own, then every other one-cluster file unlinked.
fn fragment_free_space(img: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let one = vec![0xC5u8; CLUSTER as usize];
    for i in 0..2 * HOLES {
        let name = format!("c{i}");
        write::create_file_io(&mut io, "/", &name).expect("create a one-cluster file");
        write::write_file_contents_io(&mut io, &format!("/{name}"), &one)
            .expect("write a one-cluster file");
    }
    let mut fill = 0;
    loop {
        let longest = longest_free_run(&mut io);
        if longest < 2 {
            break;
        }
        let name = format!("fill{fill}");
        write::create_file_io(&mut io, "/", &name).expect("create a filler");
        write::write_file_contents_io(
            &mut io,
            &format!("/{name}"),
            &vec![0x3Cu8; (longest * CLUSTER as u64) as usize],
        )
        .unwrap_or_else(|e| panic!("fill a free run of {longest} clusters: {e}"));
        fill += 1;
    }
    for i in (1..2 * HOLES).step_by(2) {
        write::unlink_io(&mut io, &format!("/c{i}")).expect("unlink a one-cluster file");
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let longest = longest_free_run(&mut io);
    assert!(
        longest < STREAM_CLUSTERS as u64,
        "the free space was not fragmented: a run of {longest} clusters holds the stream"
    );
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Non-resident attribute header: flags (u16 at +0x0C), compression unit
/// exponent (u16 at +0x22), and the flag that marks the value compressed.
const ATTR_FLAGS: usize = 0x0C;
const ATTR_COMPRESSION_UNIT: usize = 0x22;
const ATTR_FLAG_COMPRESSED: u16 = 0x0001;
/// Sixteen clusters, the only compression unit NTFS writes.
const UNIT_EXP: u16 = 4;
const UNIT_CLUSTERS: u64 = 1 << UNIT_EXP;
const UNIT_BYTES: usize = UNIT_CLUSTERS as usize * CLUSTER as usize;

/// One extent of `/f`'s unnamed `$DATA`: the record it is in, its
/// attribute id, its own lowest and highest VCN, and its runs (VCNs
/// relative to its lowest, as its mapping pairs encode them).
struct Extent {
    record: u64,
    attribute_id: u16,
    lowest_vcn: u64,
    highest_vcn: u64,
    runs: Vec<data_runs::DataRun>,
}

/// Every extent of `/f`'s unnamed `$DATA`, in VCN order.
fn unnamed_data_extents(img: &str, rec: u64) -> Vec<Extent> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let mut entries: Vec<_> = read::parse_attribute_list(&list)
        .expect("parse the list")
        .into_iter()
        .filter(|e| e.type_code == AttrType::Data as u32 && e.name.is_none())
        .collect();
    entries.sort_by_key(|e| e.starting_vcn);
    let mut extents = Vec::new();
    for entry in entries {
        let (_, record) =
            mft_io::read_mft_record_io(&mut io, entry.record_number).expect("read its record");
        let loc = attr_io::iter_attributes(&record)
            .find(|l| l.type_code == AttrType::Data as u32 && l.attribute_id == entry.attribute_id)
            .expect("the record holds the extent its entry names");
        assert!(!loc.is_resident, "an extent of the stream is resident");
        let at = loc.attr_offset;
        let mpo = loc
            .non_resident_mapping_pairs_offset
            .expect("a mapping-pairs offset") as usize;
        extents.push(Extent {
            record: entry.record_number,
            attribute_id: entry.attribute_id,
            lowest_vcn: u64::from_le_bytes(record[at + 16..at + 24].try_into().unwrap()),
            highest_vcn: u64::from_le_bytes(record[at + 24..at + 32].try_into().unwrap()),
            runs: data_runs::decode_runs(&record[at + mpo..at + loc.attr_length])
                .expect("decode the mapping pairs"),
        });
    }
    extents
}

/// Rewrite one extent in place: flag it compressed with sixteen-cluster
/// units, as Windows writes every extent of a compressed stream, and
/// make the absolute VCNs in `holes` sparse in its run list.
fn rewrite_extent(io: &mut PathIo, extent: &Extent, holes: &[std::ops::Range<u64>]) {
    let mut per_vcn: Vec<Option<u64>> = Vec::new();
    for run in &extent.runs {
        for k in 0..run.length {
            per_vcn.push(run.lcn.map(|l| l + k));
        }
    }
    for (i, lcn) in per_vcn.iter_mut().enumerate() {
        let vcn = extent.lowest_vcn + i as u64;
        if holes.iter().any(|h| h.contains(&vcn)) {
            *lcn = None;
        }
    }
    let mut runs: Vec<data_runs::DataRun> = Vec::new();
    for (i, lcn) in per_vcn.into_iter().enumerate() {
        if let Some(last) = runs.last_mut() {
            let follows = match (last.lcn, lcn) {
                (None, None) => true,
                (Some(a), Some(b)) => a + last.length == b,
                _ => false,
            };
            if follows {
                last.length += 1;
                continue;
            }
        }
        runs.push(data_runs::DataRun {
            starting_vcn: i as u64,
            length: 1,
            lcn,
        });
    }
    let pairs = data_runs::encode_runs(&runs).expect("encode the rewritten run list");
    let attribute_id = extent.attribute_id;
    mft_io::update_mft_record_io(io, extent.record, |record| {
        let loc = attr_io::iter_attributes(record)
            .find(|l| l.type_code == AttrType::Data as u32 && l.attribute_id == attribute_id)
            .expect("the extent is still in its record");
        let at = loc.attr_offset;
        let mpo = loc.non_resident_mapping_pairs_offset.unwrap() as usize;
        let room = loc.attr_length - mpo;
        assert!(
            pairs.len() < room,
            "the rewritten run list ({} bytes) does not fit the {room} the extent had",
            pairs.len()
        );
        record[at + mpo..at + loc.attr_length].fill(0);
        record[at + mpo..at + mpo + pairs.len()].copy_from_slice(&pairs);
        let flags = u16::from_le_bytes([record[at + ATTR_FLAGS], record[at + ATTR_FLAGS + 1]]);
        record[at + ATTR_FLAGS..at + ATTR_FLAGS + 2]
            .copy_from_slice(&(flags | ATTR_FLAG_COMPRESSED).to_le_bytes());
        record[at + ATTR_COMPRESSION_UNIT..at + ATTR_COMPRESSION_UNIT + 2]
            .copy_from_slice(&UNIT_EXP.to_le_bytes());
        Ok(())
    })
    .expect("rewrite the extent");
}

/// The LZNT1 form of a compression unit whose every byte is `fill`:
/// sixteen 4 KiB chunks, each one literal and one back-reference of
/// 4095 bytes at displacement 1, then the end-of-stream marker. Built by
/// hand from the format, not by this crate, so a reader that misreads
/// LZNT1 cannot agree with it.
fn lznt1_unit_of(fill: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..UNIT_BYTES / 4096 {
        out.extend_from_slice(&0xB003u16.to_le_bytes());
        out.extend_from_slice(&[0x02, fill, 0xFC, 0x0F]);
    }
    out.extend_from_slice(&[0, 0]);
    out.resize(CLUSTER as usize, 0);
    out
}

/// The LCN behind absolute `vcn`, from the extents as written.
fn lcn_of(extents: &[Extent], vcn: u64) -> u64 {
    let extent = extents
        .iter()
        .find(|e| (e.lowest_vcn..=e.highest_vcn).contains(&vcn))
        .expect("an extent maps the VCN");
    data_runs::vcn_to_lcn(&extent.runs, vcn - extent.lowest_vcn).expect("the VCN is allocated")
}

/// A compressed stream whose run list `$ATTRIBUTE_LIST` splits across
/// records reads back whole: units stored raw, LZNT1-compressed and
/// deallocated, in every extent and in a unit whose clusters span two
/// extents, by whole-value and ranged reads (#1073).
#[test]
fn a_compressed_stream_split_across_records_reads_every_extent() {
    let (img, rec) = volume_with_a_listed_file("compressed");
    fragment_free_space(&img);
    let len = STREAM_CLUSTERS * CLUSTER as usize - 100;
    let mut expected = pattern(len);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    write::write_file_contents_io(&mut io, "/f", &expected)
        .unwrap_or_else(|e| panic!("write {len} bytes to /f: {e}"));
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    let extents = unnamed_data_extents(&img, rec);
    assert!(
        extents.len() > 1,
        "the stream is in {} extent(s), and its run list did not fit one record",
        extents.len()
    );
    let last = extents.last().unwrap();
    let last_unit = (STREAM_CLUSTERS as u64 - 1) / UNIT_CLUSTERS;

    // A compressed unit whose hole crosses from the first extent into
    // the second (or, if they meet on a unit boundary, the unit the
    // second starts with).
    let boundary = extents[1].lowest_vcn;
    let spanning = boundary / UNIT_CLUSTERS;
    assert!(
        spanning < last_unit,
        "the first extent boundary (VCN {boundary}) falls in the stream's last, partial unit"
    );
    // A compressed unit and a deallocated one wholly in the last extent.
    let inside: Vec<u64> = (0..last_unit)
        .filter(|u| {
            u * UNIT_CLUSTERS >= last.lowest_vcn
                && (u + 1) * UNIT_CLUSTERS - 1 <= last.highest_vcn
                && *u != spanning
        })
        .collect();
    assert!(
        inside.len() >= 2,
        "the last extent (VCNs {}..={}) holds {} whole units, and the test needs two",
        last.lowest_vcn,
        last.highest_vcn,
        inside.len()
    );
    let (compressed, deallocated) = (inside[0], inside[1]);

    let mut holes = Vec::new();
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    for (unit, fill) in [(spanning, 0x5Au8), (compressed, 0xA5u8)] {
        let first = unit * UNIT_CLUSTERS;
        let at = lcn_of(&extents, first) * CLUSTER as u64;
        io.write_all_at(at, &lznt1_unit_of(fill))
            .expect("write the compressed unit");
        holes.push(first + 1..first + UNIT_CLUSTERS);
        expected[first as usize * CLUSTER as usize..][..UNIT_BYTES].fill(fill);
    }
    let first = deallocated * UNIT_CLUSTERS;
    holes.push(first..first + UNIT_CLUSTERS);
    expected[first as usize * CLUSTER as usize..][..UNIT_BYTES].fill(0);
    for extent in &extents {
        rewrite_extent(&mut io, extent, &holes);
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);

    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    let whole = read::read_attribute_value(&mut io, rec, AttrType::Data, None)
        .expect("read the split compressed /f");
    assert_eq!(whole.len(), len, "the whole read returns a short stream");
    if let Some(at) = whole.iter().zip(&expected).position(|(a, b)| a != b) {
        panic!(
            "the whole read differs at byte {at} (VCN {}): {:#04x}, not {:#04x}",
            at / CLUSTER as usize,
            whole[at],
            expected[at]
        );
    }

    // Ranged reads straddling every extent boundary and covering each
    // rewritten unit.
    let mut windows: Vec<(u64, usize)> = extents[1..]
        .iter()
        .map(|e| (e.lowest_vcn * CLUSTER as u64 - 3000, 9000))
        .collect();
    for unit in [spanning, compressed, deallocated] {
        windows.push((unit * UNIT_CLUSTERS * CLUSTER as u64 + 1, UNIT_BYTES));
    }
    windows.push((len as u64 - 5000, 10_000));
    for (offset, want) in windows {
        let got = read::read_attribute_range(&mut io, rec, AttrType::Data, None, offset, want)
            .unwrap_or_else(|e| panic!("ranged read of {want} bytes at {offset}: {e}"));
        let start = offset as usize;
        let end = (start + want).min(len);
        assert!(
            got == expected[start..end],
            "the ranged read of {want} bytes at {offset} returns {} bytes that differ from \
             the stream",
            got.len()
        );
    }
}
