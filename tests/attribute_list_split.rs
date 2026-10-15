//! A `$DATA` run list split across MFT records must not read short.
//!
//! When a file's attributes outgrow its base record, NTFS moves some of
//! them into extension records and puts an `$ATTRIBUTE_LIST` in the base
//! record saying where everything went. A heavily fragmented file gets
//! its `$DATA` mapping pairs split that way: the segment covering VCN 0
//! stays in the base record and the continuations go elsewhere.
//!
//! `locate_attribute` refused that shape rather than return a truncated
//! value, so a fragmented file Windows wrote could not be read or even
//! `stat`ed (#473). It now stitches the segments together in VCN order,
//! and still refuses segments that leave a gap. This builds the shape and
//! checks the read against the `ntfs` crate, a parser that is not this
//! one.
//!
//! The fixture is assembled by hand because nothing in this crate splits
//! one attribute across records -- it moves whole named streams to
//! extension records (#480) -- so no self-generated volume can reach the
//! path. A real 8 KiB file is split down the
//! middle: the base record keeps VCN 0 and a genuine extension record,
//! allocated in `$MFT:$Bitmap` and pointing back at the base through its
//! `base_file_record_reference`, holds VCN 1.

mod common;

use fs_ntfs::attr_io::{self, attr_off, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::data_runs::{self, DataRun};
use fs_ntfs::facade::Filesystem;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{attr_resize, mft_bitmap, mft_io, read, record_build};
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u32 = 4096;
/// Two clusters: one segment each side of the split.
const FILE_SIZE: usize = 8192;
const CONTENT: u8 = 0x5A;
/// The second cluster's byte, so a read that maps VCN 1 wrongly shows.
const TAIL: u8 = 0xA5;

/// The file's contents: the first cluster `CONTENT`, the second `TAIL`.
fn contents() -> Vec<u8> {
    let mut v = vec![CONTENT; FILE_SIZE / 2];
    v.extend(vec![TAIL; FILE_SIZE / 2]);
    v
}

// MFT record header fields this test writes by hand. Same layout the
// crate's own `record_build` uses; it keeps its copies private.
const REC_OFF_USA_OFFSET: usize = 0x04;
const REC_OFF_USA_COUNT: usize = 0x06;
const REC_OFF_SEQ: usize = 0x10;
const REC_OFF_LINK_COUNT: usize = 0x12;
const REC_OFF_ATTRS_OFFSET: usize = 0x14;
const REC_OFF_FLAGS: usize = 0x16;
const REC_OFF_BYTES_USED: usize = 0x18;
const REC_OFF_BYTES_ALLOCATED: usize = 0x1C;
const REC_OFF_BASE_FILE_REF: usize = 0x20;
const REC_OFF_NEXT_ATTR_ID: usize = 0x28;
const REC_OFF_MFT_REC_NUM: usize = 0x2C;
const USA_OFFSET: usize = 0x30;

/// One `$ATTRIBUTE_LIST` entry, unnamed: the 0x1A-byte fixed part
/// rounded up to the 8-byte alignment the entries are walked with.
const AL_ENTRY_LEN: usize = 0x20;

fn attribute_list_entry(
    type_code: u32,
    starting_vcn: u64,
    holder_ref: u64,
    attr_id: u16,
) -> Vec<u8> {
    let mut e = vec![0u8; AL_ENTRY_LEN];
    e[0..4].copy_from_slice(&type_code.to_le_bytes());
    e[4..6].copy_from_slice(&(AL_ENTRY_LEN as u16).to_le_bytes());
    e[6] = 0; // name_length
    e[7] = 0x1A; // name_offset (nothing there; the entry is unnamed)
    e[8..16].copy_from_slice(&starting_vcn.to_le_bytes());
    e[16..24].copy_from_slice(&holder_ref.to_le_bytes());
    e[24..26].copy_from_slice(&attr_id.to_le_bytes());
    e
}

/// Take the attribute at `offset` out of `record`, as the driver's own
/// unlink of an attribute does.
fn remove_attribute(record: &mut [u8], offset: usize, length: usize) {
    let used = u32::from_le_bytes(
        record[REC_OFF_BYTES_USED..REC_OFF_BYTES_USED + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    record.copy_within(offset + length..used, offset);
    record[used - length..used].fill(0);
    record[REC_OFF_BYTES_USED..REC_OFF_BYTES_USED + 4]
        .copy_from_slice(&((used - length) as u32).to_le_bytes());
}

/// A resident attribute blob with an arbitrary type code and value.
/// `record_build` has builders for the specific resident attributes the
/// driver writes; `$ATTRIBUTE_LIST` is not one of them, because the
/// driver never writes one.
fn resident_attribute(type_code: u32, attr_id: u16, value: &[u8]) -> Vec<u8> {
    const HEADER: usize = 0x18;
    let attr_length = record_build::align8(HEADER + value.len());
    let mut a = vec![0u8; attr_length];
    a[0..4].copy_from_slice(&type_code.to_le_bytes());
    a[4..8].copy_from_slice(&(attr_length as u32).to_le_bytes());
    a[8] = 0; // resident
    a[9] = 0; // unnamed
    a[10..12].copy_from_slice(&(HEADER as u16).to_le_bytes()); // name_offset
    a[12..14].copy_from_slice(&0u16.to_le_bytes()); // flags
    a[14..16].copy_from_slice(&attr_id.to_le_bytes());
    a[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes()); // value_length
    a[20..22].copy_from_slice(&(HEADER as u16).to_le_bytes()); // value_offset
    a[HEADER..HEADER + value.len()].copy_from_slice(value);
    a
}

/// A whole extension MFT record holding one non-resident `$DATA`
/// segment that starts at `first_vcn`.
fn extension_record(
    params: &mft_io::BootParams,
    record_number: u64,
    base_ref: u64,
    first_vcn: u64,
    lcn: u64,
    clusters: u64,
    lengths: u64,
) -> Vec<u8> {
    let size = params.file_record_size as usize;
    let sectors = size / params.bytes_per_sector as usize;
    let mut rec = vec![0u8; size];
    rec[0..4].copy_from_slice(b"FILE");
    rec[REC_OFF_USA_OFFSET..REC_OFF_USA_OFFSET + 2]
        .copy_from_slice(&(USA_OFFSET as u16).to_le_bytes());
    rec[REC_OFF_USA_COUNT..REC_OFF_USA_COUNT + 2]
        .copy_from_slice(&((sectors + 1) as u16).to_le_bytes());
    rec[REC_OFF_SEQ..REC_OFF_SEQ + 2].copy_from_slice(&1u16.to_le_bytes());
    // An extension record is not a file: it has no name and no links.
    rec[REC_OFF_LINK_COUNT..REC_OFF_LINK_COUNT + 2].copy_from_slice(&0u16.to_le_bytes());
    let attrs_offset = record_build::align8(USA_OFFSET + 2 + sectors * 2);
    rec[REC_OFF_ATTRS_OFFSET..REC_OFF_ATTRS_OFFSET + 2]
        .copy_from_slice(&(attrs_offset as u16).to_le_bytes());
    rec[REC_OFF_FLAGS..REC_OFF_FLAGS + 2].copy_from_slice(&mft_io::MFT_FLAG_IN_USE.to_le_bytes());
    rec[REC_OFF_BYTES_ALLOCATED..REC_OFF_BYTES_ALLOCATED + 4]
        .copy_from_slice(&(size as u32).to_le_bytes());
    rec[REC_OFF_BASE_FILE_REF..REC_OFF_BASE_FILE_REF + 8].copy_from_slice(&base_ref.to_le_bytes());
    rec[REC_OFF_NEXT_ATTR_ID..REC_OFF_NEXT_ATTR_ID + 2].copy_from_slice(&1u16.to_le_bytes());
    rec[REC_OFF_MFT_REC_NUM..REC_OFF_MFT_REC_NUM + 4]
        .copy_from_slice(&(record_number as u32).to_le_bytes());
    rec[USA_OFFSET..USA_OFFSET + 2].copy_from_slice(&1u16.to_le_bytes());

    // A segment's mapping pairs are relative to the segment: the LCN
    // delta chain restarts at zero, so encoding it as a VCN-0 run and
    // then setting first_vcn is what the on-disk form is.
    let pairs = data_runs::encode_runs(&[DataRun {
        starting_vcn: 0,
        length: clusters,
        lcn: Some(lcn),
    }])
    .expect("encode the extension segment's runs");
    // Only the VCN-0 segment carries the value's lengths; the
    // continuations leave them zero.
    let mut data = record_build::build_nonresident_data_attribute(
        0,
        lengths,
        lengths,
        lengths,
        (first_vcn + clusters - 1) as i64,
        &pairs,
    )
    .expect("build the extension segment's $DATA");
    data[attr_off::NONRES_FIRST_VCN..attr_off::NONRES_FIRST_VCN + 8]
        .copy_from_slice(&first_vcn.to_le_bytes());

    rec[attrs_offset..attrs_offset + data.len()].copy_from_slice(&data);
    let marker = attrs_offset + data.len();
    rec[marker..marker + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let bytes_used = record_build::align8(marker + 8);
    rec[REC_OFF_BYTES_USED..REC_OFF_BYTES_USED + 4]
        .copy_from_slice(&(bytes_used as u32).to_le_bytes());
    rec
}

/// An 8 KiB file whose `$DATA` really is split: VCN 0 in the base
/// record, VCN 1 in an extension record, and an `$ATTRIBUTE_LIST` in the
/// base record that says so. Returns the image path and the base record
/// number.
fn volume_with_a_split_file(tag: &str) -> (String, u64) {
    volume_with_a_segment_at(tag, 1, false)
}

/// The same file with both segments in extension records and none in the
/// base record, the way Windows lays a split attribute out.
fn volume_split_across_extensions(tag: &str) -> (String, u64) {
    volume_with_a_segment_at(tag, 1, true)
}

/// The split file, with the extension segment starting at
/// `ext_first_vcn` -- 1 is the file above, and 2 leaves VCN 1 described
/// nowhere -- and the VCN-0 segment in a second extension record when
/// `all_in_extensions`.
fn volume_with_a_segment_at(
    tag: &str,
    ext_first_vcn: u64,
    all_in_extensions: bool,
) -> (String, u64) {
    std::fs::create_dir_all("test-disks").expect("create test-disks dir");
    let dst = common::temp_image_path(format!("alsplit_{tag}"));
    let f = std::fs::File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, CLUSTER, Some("ALST"), Some(2))
        .expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);

    // A real file with real content, and a second file whose record we
    // borrow: creating it puts the record inside the MFT's allocated
    // extent, and unlinking it takes the name back out of the index.
    let fs = Filesystem::mount_rw(&dst).expect("mount_rw");
    fs.create_file("/", "split.bin").expect("create split.bin");
    fs.write_file_contents("/split.bin", &contents())
        .expect("write split.bin");
    fs.create_file("/", "donor.tmp").expect("create donor.tmp");
    let ext_rec = {
        let mut probe = PathIo::open_ro(Path::new(&dst)).expect("open_ro");
        read::resolve_path(&mut probe, "/donor.tmp").expect("resolve donor")
    };
    fs.create_file("/", "donor2.tmp")
        .expect("create donor2.tmp");
    let first_rec = {
        let mut probe = PathIo::open_ro(Path::new(&dst)).expect("open_ro");
        read::resolve_path(&mut probe, "/donor2.tmp").expect("resolve donor2")
    };
    fs.unlink("/donor.tmp").expect("unlink donor.tmp");
    fs.unlink("/donor2.tmp").expect("unlink donor2.tmp");
    drop(fs);

    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    let base_rec = read::resolve_path(&mut io, "/split.bin").expect("resolve split.bin");
    let (params, record) = mft_io::read_mft_record_io(&mut io, base_rec).expect("read base record");
    let base_seq = u16::from_le_bytes([record[REC_OFF_SEQ], record[REC_OFF_SEQ + 1]]);
    let base_ref = record_build::encode_file_reference(base_rec, base_seq);

    let loc = attr_io::find_attribute(&record, AttrType::Data, None).expect("$DATA");
    let mpo = loc.non_resident_mapping_pairs_offset.expect("mpo") as usize;
    let runs =
        data_runs::decode_runs(&record[loc.attr_offset + mpo..loc.attr_offset + loc.attr_length])
            .expect("decode base runs");
    assert_eq!(runs.len(), 1, "expected split.bin to be one extent");
    assert_eq!(runs[0].length, 2, "expected split.bin to own two clusters");
    let lcn = runs[0].lcn.expect("split.bin's run is allocated");
    let data_attr_id = u16::from_le_bytes([
        record[loc.attr_offset + attr_off::ATTRIBUTE_ID],
        record[loc.attr_offset + attr_off::ATTRIBUTE_ID + 1],
    ]);
    let si_id = attr_io::find_attribute(&record, AttrType::StandardInformation, None)
        .map(|l| {
            u16::from_le_bytes([
                record[l.attr_offset + attr_off::ATTRIBUTE_ID],
                record[l.attr_offset + attr_off::ATTRIBUTE_ID + 1],
            ])
        })
        .expect("$STANDARD_INFORMATION");
    let fn_id = attr_io::find_attribute(&record, AttrType::FileName, None)
        .map(|l| {
            u16::from_le_bytes([
                record[l.attr_offset + attr_off::ATTRIBUTE_ID],
                record[l.attr_offset + attr_off::ATTRIBUTE_ID + 1],
            ])
        })
        .expect("$FILE_NAME");

    // The extension record: VCN 1, the file's second cluster.
    let mut extensions = vec![(
        ext_rec,
        extension_record(&params, ext_rec, base_ref, ext_first_vcn, lcn + 1, 1, 0),
    )];
    // And for the Windows layout, VCN 0, the first cluster, with the
    // value's lengths.
    if all_in_extensions {
        extensions.push((
            first_rec,
            extension_record(&params, first_rec, base_ref, 0, lcn, 1, FILE_SIZE as u64),
        ));
    }
    let bm = mft_bitmap::locate_io(&mut io).expect("locate $MFT:$Bitmap");
    for (rec, ext) in &extensions {
        let mut on_disk = ext.clone();
        mft_io::apply_fixup_on_write(&mut on_disk, params.bytes_per_sector).expect("fixup");
        io.write_all_at(mft_io::mft_record_offset(&params, *rec), &on_disk)
            .expect("write extension record");
        mft_bitmap::allocate_io(&mut io, &bm, *rec).expect("allocate the extension record");
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    let ext_ref = record_build::encode_file_reference(ext_rec, 1);
    mft_io::update_mft_record_io(&mut io, base_rec, |record| {
        // Shrink the base segment to VCN 0 only, keeping the value's
        // declared lengths — the file is still 8192 bytes, it is just
        // described in two places now.
        let loc = attr_io::find_attribute(record, AttrType::Data, None).expect("$DATA");
        let mut value = Vec::new();
        value.extend_from_slice(&attribute_list_entry(0x10, 0, base_ref, si_id));
        value.extend_from_slice(&attribute_list_entry(0x30, 0, base_ref, fn_id));
        if all_in_extensions {
            // The base record keeps no segment at all.
            remove_attribute(record, loc.attr_offset, loc.attr_length);
            let first_ref = record_build::encode_file_reference(first_rec, 1);
            value.extend_from_slice(&attribute_list_entry(0x80, 0, first_ref, 0));
        } else {
            let pairs = data_runs::encode_runs(&[DataRun {
                starting_vcn: 0,
                length: 1,
                lcn: Some(lcn),
            }])?;
            let shrunk = record_build::build_nonresident_data_attribute(
                data_attr_id,
                FILE_SIZE as u64,
                FILE_SIZE as u64,
                FILE_SIZE as u64,
                0,
                &pairs,
            )?;
            attr_resize::replace_attribute(record, loc.attr_offset, &shrunk)?;
            value.extend_from_slice(&attribute_list_entry(0x80, 0, base_ref, data_attr_id));
        }
        value.extend_from_slice(&attribute_list_entry(0x80, ext_first_vcn, ext_ref, 0));
        let al = resident_attribute(AttrType::AttributeList as u32, 3, &value);
        attr_resize::insert_attribute_sorted(record, &al)
    })
    .expect("rewrite the base record");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    // The fixture has to be the thing it claims to be, or the test below
    // proves nothing: the extension record parses, and its $DATA really
    // does start at VCN 1.
    let (_, ext_read) = mft_io::read_mft_record_io(&mut io, ext_rec).expect("read ext record");
    let ext_data = attr_io::find_attribute(&ext_read, AttrType::Data, None)
        .expect("the extension record holds a $DATA");
    let first_vcn = u64::from_le_bytes(
        ext_read[ext_data.attr_offset + attr_off::NONRES_FIRST_VCN
            ..ext_data.attr_offset + attr_off::NONRES_FIRST_VCN + 8]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        first_vcn, ext_first_vcn,
        "the extension segment must start at VCN {ext_first_vcn}"
    );
    let (_, base_read) = mft_io::read_mft_record_io(&mut io, base_rec).expect("re-read base");
    let al_loc = attr_io::find_attribute(&base_read, AttrType::AttributeList, None)
        .expect("the base record holds an $ATTRIBUTE_LIST");
    let al_value = read::read_attribute_value(&mut io, base_rec, AttrType::AttributeList, None)
        .expect("read the $ATTRIBUTE_LIST value");
    assert!(al_loc.is_resident);
    let entries = read::parse_attribute_list(&al_value).expect("parse the list");
    let data_segments: Vec<_> = entries.iter().filter(|e| e.type_code == 0x80).collect();
    assert_eq!(
        data_segments.len(),
        2,
        "the list must name both $DATA segments, got {entries:?}"
    );
    drop(io);
    (dst, base_rec)
}

#[test]
fn a_split_run_list_is_read_whole() {
    let (img, base_rec) = volume_with_a_split_file("read");
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    let bytes = read::read_attribute_value(&mut io, base_rec, AttrType::Data, None)
        .expect("read a $DATA split across two records");
    assert!(
        bytes == contents(),
        "read {} bytes that are not the file's {FILE_SIZE}",
        bytes.len()
    );
}

#[test]
fn the_ranged_read_reaches_the_segment_in_the_extension_record() {
    let (img, base_rec) = volume_with_a_split_file("range");
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    let tail = read::read_attribute_range(&mut io, base_rec, AttrType::Data, None, 4096, 4096)
        .expect("read the window the extension record holds");
    assert_eq!(tail, vec![TAIL; 4096]);
    let across = read::read_attribute_range(&mut io, base_rec, AttrType::Data, None, 4000, 200)
        .expect("read a window across the split");
    assert_eq!(across, contents()[4000..4200].to_vec());
}

/// Graded by the `ntfs` crate: the whole file, read through the facade,
/// is what that parser reads from the same image. That parser follows a
/// split attribute only when every segment is in an extension record, as
/// Windows lays one out, so this is that layout.
#[test]
fn the_stitched_read_matches_the_ntfs_crate() {
    let (img, _) = volume_split_across_extensions("oracle");
    let (ntfs, mut reader) = common::open(&img);
    let theirs = common::read_file_all(&ntfs, &mut reader, "/split.bin");
    let fs = Filesystem::mount(&img).expect("mount");
    let mut ours = vec![0u8; FILE_SIZE + 1];
    let n = fs
        .read_file("/split.bin", 0, &mut ours)
        .expect("read /split.bin");
    ours.truncate(n);
    assert!(theirs == contents(), "the ntfs crate read something else");
    assert!(
        ours == theirs,
        "this crate read {n} bytes the ntfs crate does not"
    );
}

/// Segments that leave a VCN described nowhere are refused, not read
/// with a hole where the missing cluster was.
#[test]
fn segments_with_a_gap_between_them_are_refused() {
    let (img, base_rec) = volume_with_a_segment_at("gap", 2, false);
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    let got = read::read_attribute_value(&mut io, base_rec, AttrType::Data, None);
    assert!(
        got.as_ref().is_err_and(|e| e.contains("VCN")),
        "a $DATA with no segment for VCN 1 was read: {:?}",
        got.map(|b| b.len())
    );
}

#[test]
fn stat_still_reads_a_split_file_through_the_base_record() {
    // $STANDARD_INFORMATION and $FILE_NAME are listed too, and each has
    // exactly one segment. Consulting the list first must not turn an
    // ordinary attribute lookup on a file that HAS a list into a
    // failure.
    let (img, base_rec) = volume_with_a_split_file("stat");
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    let si = read::read_attribute_value(&mut io, base_rec, AttrType::StandardInformation, None)
        .expect("$STANDARD_INFORMATION is listed once and lives in the base record");
    assert!(
        si.len() >= 48,
        "a $STANDARD_INFORMATION value is at least 48 bytes, got {}",
        si.len()
    );
    let fname = read::read_attribute_value(&mut io, base_rec, AttrType::FileName, None)
        .expect("$FILE_NAME is listed once and lives in the base record");
    assert!(!fname.is_empty());
}
