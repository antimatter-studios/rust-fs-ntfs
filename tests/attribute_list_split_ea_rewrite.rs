//! Rewriting a split `$EA` keeps its extents true (#1058, part of #805).
//!
//! A non-resident `$EA` whose run list did not fit one MFT record is split
//! into extents across extension records, each named in the
//! `$ATTRIBUTE_LIST` with its starting VCN (#803). The `$EA` write path
//! still found the attribute through the list lookup that refuses any
//! attribute split across records, so changing the EAs of such a file was
//! refused. A file's EAs are capped at 64 KiB, so with the formatter's
//! 1024-byte records an `$EA`'s run list always fits one record and is
//! never split by allocation; this splits it by hand into the shape #803
//! gives a run list too long for one record -- the first runs where they
//! are, the rest in another extension record with a list entry naming its
//! starting VCN -- checks that shape is true, then adds, changes and
//! removes EAs so the `$EA` grows and shrinks, splitting it again before
//! each change. It checks after each step that the list names every
//! extent with its starting VCN,
//! that the extents cover the `$EA` end to end, that the `ntfs` crate -- a
//! parser that is not this one -- reads the EAs back, that the file's
//! streams are untouched, and that `check_io` is clean.
mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::ea_io::{self, Ea};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{bitmap, data_runs, fsck, mft_io, read, write};
use ntfs::{NtfsAttributeType, NtfsReadSeek};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const RECORD_SIZE: u32 = 1024;
const CLUSTER: u32 = 4096;
/// One-cluster holes the free space is cut into: room for the largest
/// `$EA` twice over, as a rewrite allocates the new clusters before it
/// frees the old.
const HOLES: usize = 64;
/// `$EA`'s attribute type code.
const EA_TYPE: u32 = AttrType::ExtendedAttribute as u32;

/// `/f`, with an EA and enough named streams that its base record filled
/// and it gained an `$ATTRIBUTE_LIST` and extension records.
fn volume_with_a_listed_file(tag: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("alsplitea_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, RECORD_SIZE, None, Some(1))
        .expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", b"contents").expect("write /f");
    for i in 0..16 {
        write::write_named_stream_io(&mut io, "/f", &format!("s{i}"), &[b'a' + i as u8; 120])
            .unwrap_or_else(|e| panic!("write /f:s{i}: {e}"));
    }
    // Written once the base record is full, the EA goes to an extension
    // record, where every extent of a split `$EA` is.
    write::write_ea_io(&mut io, "/f", b"USER.TAG", &[b'e'; 64], 0).expect("write /f's EA");
    assert!(
        read::read_attribute_value_if_present(&mut io, rec, AttrType::AttributeList, None)
            .expect("read /f's list")
            .is_some(),
        "/f's base record never filled, so it has no $ATTRIBUTE_LIST to test"
    );
    // Of the named streams but `s3`, those in the base record go, and all
    // but one in each extension record, so the list and the records have
    // room for the extents and entries the splits add.
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("read /f's list");
    let mut records: Vec<u64> = read::parse_attribute_list(&list)
        .expect("parse the list")
        .iter()
        .map(|e| e.record_number)
        .collect();
    records.sort_unstable();
    records.dedup();
    let mut doomed = Vec::new();
    for n in records {
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a record");
        let names: Vec<String> = attr_io::iter_attributes(&record)
            .filter(|l| l.type_code == AttrType::Data as u32 && l.name_length > 0)
            .map(|l| attr_io::decode_attr_name(&record, &l))
            .filter(|name| name != "s3")
            .collect();
        let keep = usize::from(n != rec);
        doomed.extend(names.into_iter().skip(keep));
    }
    for name in doomed {
        write::delete_named_stream_io(&mut io, "/f", &name)
            .unwrap_or_else(|e| panic!("delete /f:{name}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
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
        longest < 2,
        "the free space was not fragmented: a run of {longest} clusters is free"
    );
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// `record` with the `old_len` bytes at `at` replaced by `new`, the
/// attributes after them moved and the used size set; panics when the
/// record has no room.
fn splice(record: &mut Vec<u8>, at: usize, old_len: usize, new: &[u8], what: &str) {
    let used = u32_at(record, 0x18) as usize;
    let allocated = u32_at(record, 0x1C) as usize;
    let mut out = record[..at].to_vec();
    out.extend_from_slice(new);
    out.extend_from_slice(&record[at + old_len..used]);
    assert!(
        out.len() <= allocated,
        "{what}: the record has no room for {} more bytes",
        out.len() - used
    );
    let new_used = out.len() as u32;
    out.resize(record.len(), 0);
    out[0x18..0x1C].copy_from_slice(&new_used.to_le_bytes());
    *record = out;
}

fn write_record(io: &mut PathIo, n: u64, mut record: Vec<u8>) {
    let params = mft_io::read_boot_params_io(io).expect("boot params");
    mft_io::apply_fixup_on_write(&mut record, params.bytes_per_sector).expect("fixups");
    let at = mft_io::mft_record_offset_io(io, &params, n).expect("record offset");
    io.write_all_at(at, &record).expect("write a record");
}

/// Split `/f`'s non-resident `$EA` by hand into two extents, the shape
/// #803 gives a run list too long for one record: its first half of runs
/// stays where it is with the attribute's sizes, the rest go to another of
/// the file's extension records as an extent of their own, with a list
/// entry naming its starting VCN after the first's.
fn split_ea_by_hand(img: &str, rec: u64) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let mut list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let entries = read::parse_attribute_list(&list).expect("parse the list");
    let ea: Vec<_> = entries.iter().filter(|e| e.type_code == EA_TYPE).collect();
    assert_eq!(ea.len(), 1, "the $EA to split is in {} extents", ea.len());
    let first = ea[0].record_number;
    assert_ne!(first, rec, "the $EA to split is in the base record");

    let (_, mut record) =
        mft_io::read_mft_record_io(&mut io, first).expect("read the $EA's record");
    let loc = attr_io::iter_attributes(&record)
        .find(|l| l.type_code == EA_TYPE && l.attribute_id == ea[0].attribute_id)
        .expect("the record holds the $EA its entry names");
    assert!(!loc.is_resident, "the $EA to split is resident");
    let at = loc.attr_offset;
    let mpo = loc
        .non_resident_mapping_pairs_offset
        .expect("mapping pairs") as usize;
    let runs = data_runs::decode_runs(&record[at + mpo..at + loc.attr_length]).expect("runs");
    assert!(runs.len() > 1, "the $EA to split has {} run", runs.len());
    let half = runs.len() / 2;
    let split_vcn: u64 = runs[..half].iter().map(|r| r.length).sum();
    let highest = u64_at(&record, at + 0x18);
    // Each extent's mapping pairs count VCNs from its own start.
    let tail_runs: Vec<data_runs::DataRun> = runs[half..]
        .iter()
        .map(|r| data_runs::DataRun {
            starting_vcn: r.starting_vcn - split_vcn,
            ..*r
        })
        .collect();
    let head = fs_ntfs::record_build::build_nonresident_attribute(
        EA_TYPE,
        None,
        loc.attribute_id,
        u64_at(&record, at + 0x30),
        u64_at(&record, at + 0x28),
        u64_at(&record, at + 0x38),
        split_vcn as i64 - 1,
        &data_runs::encode_runs(&runs[..half]).expect("encode the head's runs"),
    )
    .expect("build the head extent");
    let mut tail = fs_ntfs::record_build::build_nonresident_attribute(
        EA_TYPE,
        None,
        0,
        0,
        0,
        0,
        highest as i64,
        &data_runs::encode_runs(&tail_runs).expect("encode the tail's runs"),
    )
    .expect("build the tail extent");
    tail[0x10..0x18].copy_from_slice(&split_vcn.to_le_bytes());
    splice(&mut record, at, loc.attr_length, &head, "the head extent");
    let mut head_record = record;

    // The tail goes to another extension record of the file with room.
    let mut others: Vec<u64> = entries
        .iter()
        .map(|e| e.record_number)
        .filter(|&n| n != rec && n != first)
        .collect();
    others.sort_unstable();
    others.dedup();
    let (second, mut record) = others
        .into_iter()
        .map(|n| {
            (
                n,
                mft_io::read_mft_record_io(&mut io, n)
                    .expect("read a record")
                    .1,
            )
        })
        .find(|(_, r)| (u32_at(r, 0x1C) - u32_at(r, 0x18)) as usize >= tail.len())
        .expect("no other extension record of /f has room for the tail extent");
    // Every extent of an attribute carries the same instance, as the
    // driver's own splits do and the `ntfs` crate follows them by: one
    // neither record has given out yet.
    let next_of = |r: &[u8]| u16::from_le_bytes(r[0x28..0x2A].try_into().unwrap());
    let id = next_of(&head_record).max(next_of(&record));
    for r in [&mut head_record, &mut record] {
        r[0x28..0x2A].copy_from_slice(&(id + 1).to_le_bytes());
    }
    tail[0x0E..0x10].copy_from_slice(&id.to_le_bytes());
    let head_at = attr_io::iter_attributes(&head_record)
        .find(|l| l.type_code == EA_TYPE)
        .expect("the head extent")
        .attr_offset;
    head_record[head_at + 0x0E..head_at + 0x10].copy_from_slice(&id.to_le_bytes());
    write_record(&mut io, first, head_record);
    let attrs: Vec<_> = attr_io::iter_attributes(&record).collect();
    let insert_at = attrs
        .iter()
        .find(|l| l.type_code > EA_TYPE)
        .map(|l| l.attr_offset)
        .unwrap_or_else(|| attrs.last().map_or(0x38, |l| l.attr_offset + l.attr_length));
    let sequence = mft_io::record_sequence(&record);
    splice(&mut record, insert_at, 0, &tail, "the tail extent");
    write_record(&mut io, second, record);

    // Its list entry goes after the head's.
    let mut entry = vec![0u8; 0x20];
    entry[0..4].copy_from_slice(&EA_TYPE.to_le_bytes());
    entry[4..6].copy_from_slice(&0x20u16.to_le_bytes());
    entry[7] = 0x1A;
    entry[8..16].copy_from_slice(&split_vcn.to_le_bytes());
    entry[16..24].copy_from_slice(
        &fs_ntfs::record_build::encode_file_reference(second, sequence).to_le_bytes(),
    );
    entry[24..26].copy_from_slice(&id.to_le_bytes());
    let mut off = 0;
    while off < list.len() {
        let len = u16::from_le_bytes(list[off + 4..off + 6].try_into().unwrap()) as usize;
        off += len;
        if u32_at(&list, off - len) == EA_TYPE {
            break;
        }
    }
    list[off - 0x20 + 24..off - 0x20 + 26].copy_from_slice(&id.to_le_bytes());
    list.splice(off..off, entry);

    let (_, mut base) = mft_io::read_mft_record_io(&mut io, rec).expect("read the base record");
    let loc = attr_io::iter_attributes(&base)
        .find(|l| l.type_code == AttrType::AttributeList as u32)
        .expect("the base record holds the list");
    assert!(loc.is_resident, "/f's list is not resident");
    let value_at = loc.resident_value_offset.expect("value offset") as usize;
    let mut attr = base[loc.attr_offset..loc.attr_offset + value_at].to_vec();
    attr.extend_from_slice(&list);
    attr.resize(attr.len().div_ceil(8) * 8, 0);
    let attr_len = attr.len() as u32;
    attr[4..8].copy_from_slice(&attr_len.to_le_bytes());
    attr[0x10..0x14].copy_from_slice(&(list.len() as u32).to_le_bytes());
    splice(
        &mut base,
        loc.attr_offset,
        loc.attr_length,
        &attr,
        "the longer list",
    );
    write_record(&mut io, rec, base);
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

/// Every attribute of the file at `rec` is named by a list entry where it
/// is, and every entry names an attribute that is there.
fn list_is_true(img: &str, rec: u64) {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let entries = read::parse_attribute_list(&list).expect("parse the list");
    let mut numbers: Vec<u64> = entries.iter().map(|e| e.record_number).collect();
    numbers.sort_unstable();
    numbers.dedup();
    let mut held = 0usize;
    for &n in &numbers {
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a listed record");
        assert_ne!(
            mft_io::record_flags(&record) & mft_io::MFT_FLAG_IN_USE,
            0,
            "the list names record {n}, which is not in use"
        );
        for loc in attr_io::iter_attributes(&record) {
            if loc.type_code == AttrType::AttributeList as u32 {
                continue;
            }
            held += 1;
            let named = entries.iter().any(|e| {
                e.record_number == n
                    && e.type_code == loc.type_code
                    && e.attribute_id == loc.attribute_id
            });
            assert!(
                named,
                "record {n} holds attribute {:#x} id {}, and the list does not name it",
                loc.type_code, loc.attribute_id
            );
        }
    }
    assert_eq!(
        held,
        entries.len(),
        "the list has entries for attributes that are not where it says"
    );
}

fn clean(img: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let report = fsck::check_io(&mut io).expect("check the volume");
    assert!(
        report.findings.is_empty(),
        "check_io finds the volume unclean: {:?}",
        report.findings
    );
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| ((i % 251) as u8) ^ seed).collect()
}

/// One extent of `/f`'s `$EA`: the starting VCN its list entry names, the
/// record it is in, its own lowest and highest VCN, and its runs.
struct Extent {
    listed_vcn: u64,
    record: u64,
    lowest_vcn: u64,
    highest_vcn: u64,
    runs: Vec<data_runs::DataRun>,
}

/// Every extent of `/f`'s `$EA`, in list order, read from the record its
/// list entry names.
fn ea_extents(img: &str, rec: u64) -> Vec<Extent> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .expect("/f has an $ATTRIBUTE_LIST");
    let entries: Vec<_> = read::parse_attribute_list(&list)
        .expect("parse the list")
        .into_iter()
        .filter(|e| e.type_code == AttrType::ExtendedAttribute as u32)
        .collect();
    let mut extents = Vec::new();
    for entry in entries {
        let (_, record) =
            mft_io::read_mft_record_io(&mut io, entry.record_number).expect("read its record");
        let loc = attr_io::iter_attributes(&record)
            .find(|l| {
                l.type_code == AttrType::ExtendedAttribute as u32
                    && l.attribute_id == entry.attribute_id
            })
            .expect("the record holds the extent its entry names");
        assert!(!loc.is_resident, "an extent of the $EA is resident");
        let at = loc.attr_offset;
        let lowest_vcn = u64::from_le_bytes(record[at + 16..at + 24].try_into().unwrap());
        let highest_vcn = u64::from_le_bytes(record[at + 24..at + 32].try_into().unwrap());
        let mpo = loc
            .non_resident_mapping_pairs_offset
            .expect("a mapping-pairs offset") as usize;
        let runs = data_runs::decode_runs(&record[at + mpo..at + loc.attr_length])
            .expect("decode the mapping pairs");
        extents.push(Extent {
            listed_vcn: entry.starting_vcn,
            record: entry.record_number,
            lowest_vcn,
            highest_vcn,
            runs,
        });
    }
    extents
}

/// The extents of `/f`'s `$EA` are true for a value of `clusters`
/// clusters: each starts where its list entry says and where the one
/// before it ended, maps exactly the VCNs it spans, and none but the first
/// is in the base record. Returns how many there are.
fn extents_are_true(img: &str, rec: u64, clusters: u64) -> usize {
    let extents = ea_extents(img, rec);
    let mut next_vcn = 0u64;
    for (i, extent) in extents.iter().enumerate() {
        assert_eq!(
            extent.listed_vcn, extent.lowest_vcn,
            "extent {i}'s list entry names VCN {}, and the extent starts at {}",
            extent.listed_vcn, extent.lowest_vcn
        );
        assert_eq!(
            extent.lowest_vcn, next_vcn,
            "extent {i} starts at VCN {}, not where the one before it ended",
            extent.lowest_vcn
        );
        if i > 0 {
            assert_ne!(extent.record, rec, "extent {i} is in the base record");
        }
        let mapped: u64 = extent.runs.iter().map(|r| r.length).sum();
        assert_eq!(
            extent.highest_vcn + 1 - extent.lowest_vcn,
            mapped,
            "extent {i} spans VCNs {}..={} and maps {mapped} clusters",
            extent.lowest_vcn,
            extent.highest_vcn
        );
        next_vcn = extent.highest_vcn + 1;
    }
    assert_eq!(
        next_vcn, clusters,
        "the extents map {next_vcn} clusters, not the $EA's {clusters}"
    );
    extents.len()
}

/// `(name, value)` of each EA in the `$EA` the `ntfs` crate finds on `/f`,
/// following the file's `$ATTRIBUTE_LIST`; empty when it finds no `$EA`.
fn ntfs_crate_eas(img: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, "/f");
    let mut attrs = file.attributes();
    while let Some(item) = attrs.next(&mut reader) {
        let item = item.expect("an attribute of the file");
        let attr = item.to_attribute().expect("to_attribute");
        if attr.ty().expect("attribute type") != NtfsAttributeType::EA {
            continue;
        }
        let mut value = attr.value(&mut reader).expect("the $EA's value");
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = value.read(&mut reader, &mut chunk).expect("read the $EA");
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        return ea_io::decode(&bytes)
            .expect("decode the $EA the ntfs crate read")
            .into_iter()
            .map(|ea: Ea| (ea.name, ea.value))
            .collect();
    }
    Vec::new()
}

/// After a change, `/f`'s EAs are `expected` by the list, the `ntfs` crate
/// and this driver alike, its streams are untouched, and the volume is
/// clean. Returns how many extents the `$EA` is in.
fn eas_are_true(img: &str, rec: u64, expected: &[(&[u8], Vec<u8>)], what: &str) -> usize {
    let wanted: Vec<(Vec<u8>, Vec<u8>)> = expected
        .iter()
        .map(|(name, value)| (name.to_vec(), value.clone()))
        .collect();
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let packed =
        read::read_attribute_value_if_present(&mut io, rec, AttrType::ExtendedAttribute, None)
            .unwrap_or_else(|e| panic!("after {what}, read /f's $EA: {e}"))
            .unwrap_or_default();
    let ours: Vec<(Vec<u8>, Vec<u8>)> = write::list_eas_io(&mut io, "/f")
        .unwrap_or_else(|e| panic!("after {what}, list the EAs of /f: {e}"))
        .into_iter()
        .map(|ea| (ea.name, ea.value))
        .collect();
    assert_eq!(ours, wanted, "after {what}, this driver lists other EAs");
    assert_eq!(
        read::read_attribute_value(&mut io, rec, AttrType::Data, Some("s3")).expect("read /f:s3"),
        vec![b'd'; 120],
        "{what} changed the named stream"
    );
    assert_eq!(
        read::read_attribute_value(&mut io, rec, AttrType::Data, None).expect("read /f"),
        b"contents",
        "{what} changed the unnamed stream"
    );
    drop(io);
    let extents = if packed.is_empty() {
        assert!(
            ea_extents(img, rec).is_empty(),
            "after {what}, an $EA is left"
        );
        0
    } else {
        extents_are_true(img, rec, (packed.len() as u64).div_ceil(CLUSTER as u64))
    };
    list_is_true(img, rec);
    assert_eq!(
        ntfs_crate_eas(img),
        wanted,
        "after {what}, the ntfs crate reads other EAs"
    );
    clean(img);
    extents
}

fn write_ea(img: &str, name: &[u8], value: &[u8], what: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    write::write_ea_io(&mut io, "/f", name, value, 0).unwrap_or_else(|e| {
        panic!(
            "{what}: write EA {} of {} bytes on /f: {e}",
            String::from_utf8_lossy(name),
            value.len()
        )
    });
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

fn remove_ea(img: &str, name: &[u8], what: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    write::remove_ea_io(&mut io, "/f", name).unwrap_or_else(|e| {
        panic!(
            "{what}: remove EA {} from /f: {e}",
            String::from_utf8_lossy(name)
        )
    });
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

/// Split `/f`'s `$EA` by hand and check the split is true before the
/// change that follows it.
fn split(img: &str, rec: u64, expected: &[(&[u8], Vec<u8>)], what: &str) {
    split_ea_by_hand(img, rec);
    let extents = eas_are_true(img, rec, expected, &format!("splitting it before {what}"));
    assert_eq!(
        extents, 2,
        "split by hand before {what}, the $EA is in {extents} extents"
    );
}

/// A split `$EA` takes a new EA, a changed one at the same length, one
/// that shrinks it, one that grows it, and removals down to no EAs at
/// all, and its extents stay true each time: those no longer needed go
/// with their entries and records.
#[test]
fn a_split_ea_keeps_its_extents_true_through_rewrites() {
    let (img, rec) = volume_with_a_listed_file("ea");
    fragment_free_space(&img);
    let tag: &[u8] = b"USER.TAG";
    let big: &[u8] = b"USER.BIG";
    let two: &[u8] = b"USER.TWO";

    let mut expected = vec![(tag, vec![b'e'; 64]), (big, pattern(40_000, 0))];
    write_ea(&img, big, &expected[1].1, "the first write");
    eas_are_true(&img, rec, &expected, "the first write");

    // Another EA grows it.
    split(&img, rec, &expected, "adding an EA");
    expected.push((two, pattern(3_000, 0x11)));
    write_ea(&img, two, &expected[2].1, "adding an EA");
    eas_are_true(&img, rec, &expected, "adding an EA");

    // A changed EA at the same length.
    split(&img, rec, &expected, "changing an EA");
    expected[1].1 = pattern(40_000, 0x5A);
    write_ea(&img, big, &expected[1].1, "changing an EA");
    eas_are_true(&img, rec, &expected, "changing an EA");

    // A changed EA shrinks it.
    split(&img, rec, &expected, "shrinking an EA");
    expected[1].1 = pattern(9_000, 0x33);
    write_ea(&img, big, &expected[1].1, "shrinking an EA");
    eas_are_true(&img, rec, &expected, "shrinking an EA");

    // A changed EA grows it near the 64 KiB cap.
    split(&img, rec, &expected, "growing an EA");
    expected[1].1 = pattern(60_000, 0x77);
    write_ea(&img, big, &expected[1].1, "growing an EA");
    eas_are_true(&img, rec, &expected, "growing an EA");

    // Removing the large EA shrinks it to one cluster.
    split(&img, rec, &expected, "removing an EA");
    expected.remove(1);
    remove_ea(&img, big, "removing an EA");
    eas_are_true(&img, rec, &expected, "removing an EA");

    // Grown again, split, then every EA removed takes the $EA out.
    expected.push((big, pattern(50_000, 0x99)));
    write_ea(&img, big, &expected[2].1, "growing it again");
    eas_are_true(&img, rec, &expected, "growing it again");
    split(&img, rec, &expected, "removing every EA");
    for name in [tag, two, big] {
        expected.retain(|(n, _)| *n != name);
        remove_ea(&img, name, "removing every EA");
    }
    eas_are_true(&img, rec, &expected, "removing every EA");
}
