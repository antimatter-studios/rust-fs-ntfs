//! A directory whose `$INDEX_ROOT` lives in an extension record can be
//! listed and looked up in (#471).
//!
//! When a directory's attributes outgrow its base record -- a long name,
//! many hard links -- NTFS moves some of them into extension records and
//! puts an `$ATTRIBUTE_LIST` in the base record saying where they went.
//! The directory readers took `$INDEX_ROOT` from the base record only, so
//! such a directory answered "$INDEX_ROOT:$I30 not found" to a listing and
//! to every lookup inside it. A volume ntfs-3g populated has this shape.
//!
//! The fixture is assembled by hand, as `attribute_list_split.rs`'s is:
//! nothing in this crate writes an `$ATTRIBUTE_LIST`. A real directory's
//! `$INDEX_ROOT` is moved into a genuine extension record, and the base
//! record gets the list. The listing is graded against the `ntfs` crate, a
//! parser that is not this one.

mod common;

use fs_ntfs::attr_io::{self, attr_off, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::facade::Filesystem;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{mft_bitmap, mft_io, read, record_build};
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u32 = 4096;

// MFT record header fields this test writes by hand.
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

/// One `$ATTRIBUTE_LIST` entry, rounded up to 8 bytes.
fn attribute_list_entry(type_code: u32, name: &str, holder_ref: u64, attr_id: u16) -> Vec<u8> {
    let utf16: Vec<u16> = name.encode_utf16().collect();
    let len = record_build::align8(0x1A + utf16.len() * 2);
    let mut e = vec![0u8; len];
    e[0..4].copy_from_slice(&type_code.to_le_bytes());
    e[4..6].copy_from_slice(&(len as u16).to_le_bytes());
    e[6] = utf16.len() as u8;
    e[7] = 0x1A;
    e[16..24].copy_from_slice(&holder_ref.to_le_bytes());
    e[24..26].copy_from_slice(&attr_id.to_le_bytes());
    for (i, c) in utf16.iter().enumerate() {
        e[0x1A + i * 2..0x1A + i * 2 + 2].copy_from_slice(&c.to_le_bytes());
    }
    e
}

/// A resident attribute with an arbitrary type and value.
fn resident_attribute(type_code: u32, attr_id: u16, value: &[u8]) -> Vec<u8> {
    let header = 0x18;
    let len = record_build::align8(header + value.len());
    let mut a = vec![0u8; len];
    a[0..4].copy_from_slice(&type_code.to_le_bytes());
    a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    a[attr_off::ATTRIBUTE_ID..attr_off::ATTRIBUTE_ID + 2].copy_from_slice(&attr_id.to_le_bytes());
    a[attr_off::RESIDENT_VALUE_LENGTH..attr_off::RESIDENT_VALUE_LENGTH + 4]
        .copy_from_slice(&(value.len() as u32).to_le_bytes());
    a[attr_off::RESIDENT_VALUE_OFFSET..attr_off::RESIDENT_VALUE_OFFSET + 2]
        .copy_from_slice(&(header as u16).to_le_bytes());
    a[header..header + value.len()].copy_from_slice(value);
    a
}

/// A whole extension MFT record holding `attr` and nothing else.
fn extension_record(
    params: &mft_io::BootParams,
    record_number: u64,
    base_ref: u64,
    attr: &[u8],
    next_attr_id: u16,
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
    rec[REC_OFF_LINK_COUNT..REC_OFF_LINK_COUNT + 2].copy_from_slice(&0u16.to_le_bytes());
    let attrs_offset = record_build::align8(USA_OFFSET + 2 + sectors * 2);
    rec[REC_OFF_ATTRS_OFFSET..REC_OFF_ATTRS_OFFSET + 2]
        .copy_from_slice(&(attrs_offset as u16).to_le_bytes());
    // An extension record of a directory is not itself flagged a directory.
    rec[REC_OFF_FLAGS..REC_OFF_FLAGS + 2].copy_from_slice(&mft_io::MFT_FLAG_IN_USE.to_le_bytes());
    rec[REC_OFF_BYTES_ALLOCATED..REC_OFF_BYTES_ALLOCATED + 4]
        .copy_from_slice(&(size as u32).to_le_bytes());
    rec[REC_OFF_BASE_FILE_REF..REC_OFF_BASE_FILE_REF + 8].copy_from_slice(&base_ref.to_le_bytes());
    rec[REC_OFF_NEXT_ATTR_ID..REC_OFF_NEXT_ATTR_ID + 2]
        .copy_from_slice(&next_attr_id.to_le_bytes());
    rec[REC_OFF_MFT_REC_NUM..REC_OFF_MFT_REC_NUM + 4]
        .copy_from_slice(&(record_number as u32).to_le_bytes());
    rec[USA_OFFSET..USA_OFFSET + 2].copy_from_slice(&1u16.to_le_bytes());
    rec[attrs_offset..attrs_offset + attr.len()].copy_from_slice(attr);
    let marker = attrs_offset + attr.len();
    rec[marker..marker + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let bytes_used = record_build::align8(marker + 8);
    rec[REC_OFF_BYTES_USED..REC_OFF_BYTES_USED + 4]
        .copy_from_slice(&(bytes_used as u32).to_le_bytes());
    rec
}

/// Take the attribute at `offset` out of `record`.
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

fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("f{i:03}")).collect()
}

/// A volume with `/d` holding `files` files, whose `$INDEX_ROOT:$I30` has
/// been moved into an extension record. Returns the image path.
fn volume_with_index_root_in_extension(tag: &str, files: usize) -> String {
    let dst = common::temp_image_path(format!("irext_{tag}"));
    let f = std::fs::File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, CLUSTER, Some("IREXT"), Some(3))
        .expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);

    // The directory, its files, and a record borrowed from a donor file.
    let fs = Filesystem::mount_rw(&dst).expect("mount_rw");
    fs.mkdir("/", "d").expect("mkdir /d");
    for n in names(files) {
        fs.create_file("/d", &n).expect("create in /d");
    }
    fs.create_file("/", "donor.tmp").expect("create donor.tmp");
    let ext_rec = {
        let mut probe = PathIo::open_ro(Path::new(&dst)).expect("open_ro");
        read::resolve_path(&mut probe, "/donor.tmp").expect("resolve donor")
    };
    fs.unlink("/donor.tmp").expect("unlink donor.tmp");
    drop(fs);

    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    let dir = read::resolve_path(&mut io, "/d").expect("resolve /d");
    let (params, record) = mft_io::read_mft_record_io(&mut io, dir).expect("read /d");
    let seq = u16::from_le_bytes([record[REC_OFF_SEQ], record[REC_OFF_SEQ + 1]]);
    let base_ref = record_build::encode_file_reference(dir, seq);
    let id_of = |ty: AttrType, name: Option<&str>| {
        attr_io::find_attribute(&record, ty, name).map(|l| {
            u16::from_le_bytes([
                record[l.attr_offset + attr_off::ATTRIBUTE_ID],
                record[l.attr_offset + attr_off::ATTRIBUTE_ID + 1],
            ])
        })
    };
    let si_id = id_of(AttrType::StandardInformation, None).expect("$STANDARD_INFORMATION");
    let fn_id = id_of(AttrType::FileName, None).expect("$FILE_NAME");
    let ir_id = id_of(AttrType::IndexRoot, Some("$I30")).expect("$INDEX_ROOT:$I30");
    let ia_id = id_of(AttrType::IndexAllocation, Some("$I30"));
    let bm_id = id_of(AttrType::Bitmap, Some("$I30"));
    let ir = attr_io::find_attribute(&record, AttrType::IndexRoot, Some("$I30")).unwrap();
    let ir_bytes = record[ir.attr_offset..ir.attr_offset + ir.attr_length].to_vec();

    // The extension record takes the $INDEX_ROOT, attribute id and all.
    let ext = extension_record(&params, ext_rec, base_ref, &ir_bytes, ir_id + 1);
    let mut on_disk = ext.clone();
    mft_io::apply_fixup_on_write(&mut on_disk, params.bytes_per_sector).expect("fixup");
    // Through $MFT's run list: the creates above grew it, and it need not
    // be one extent any more.
    let at = mft_io::mft_record_offset_io(&mut io, &params, ext_rec).expect("place the record");
    io.write_all_at(at, &on_disk)
        .expect("write the extension record");
    let bm = mft_bitmap::locate_io(&mut io).expect("locate $MFT:$Bitmap");
    mft_bitmap::allocate_io(&mut io, &bm, ext_rec).expect("allocate the extension record");

    let ext_ref = record_build::encode_file_reference(ext_rec, 1);
    mft_io::update_mft_record_io(&mut io, dir, |record| {
        let ir = attr_io::find_attribute(record, AttrType::IndexRoot, Some("$I30"))
            .expect("$INDEX_ROOT");
        remove_attribute(record, ir.attr_offset, ir.attr_length);
        let mut list = Vec::new();
        list.extend(attribute_list_entry(0x10, "", base_ref, si_id));
        list.extend(attribute_list_entry(0x30, "", base_ref, fn_id));
        list.extend(attribute_list_entry(0x90, "$I30", ext_ref, ir_id));
        if let Some(id) = ia_id {
            list.extend(attribute_list_entry(0xA0, "$I30", base_ref, id));
        }
        if let Some(id) = bm_id {
            list.extend(attribute_list_entry(0xB0, "$I30", base_ref, id));
        }
        let al = resident_attribute(AttrType::AttributeList as u32, 0x30, &list);
        fs_ntfs::attr_resize::insert_attribute_sorted(record, &al)
    })
    .expect("rewrite /d's base record");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    // The fixture is what it claims: the base record has no $INDEX_ROOT.
    let (_, base) = mft_io::read_mft_record_io(&mut io, dir).expect("re-read /d");
    assert!(attr_io::find_attribute(&base, AttrType::IndexRoot, Some("$I30")).is_none());
    assert!(attr_io::find_attribute(&base, AttrType::AttributeList, None).is_some());
    dst
}

fn check(tag: &str, files: usize) {
    let img = volume_with_index_root_in_extension(tag, files);
    let (ntfs, mut reader) = common::open(&img);
    let mut theirs = common::list_names(&ntfs, &mut reader, "/d");
    theirs.sort();
    assert_eq!(theirs, names(files), "the ntfs crate's listing of /d");

    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    let dir = read::resolve_path(&mut io, "/d").expect("resolve /d");
    let mut ours: Vec<String> = read::read_dir_entries(&mut io, dir)
        .expect("list /d")
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n != ".")
        .collect();
    ours.sort();
    assert_eq!(ours, theirs, "this crate's listing of /d");
    for n in names(files) {
        read::resolve_path(&mut io, &format!("/d/{n}"))
            .unwrap_or_else(|e| panic!("/d/{n} is not found: {e}"));
    }
}

/// #471's shape with the whole index in `$INDEX_ROOT`.
#[test]
fn a_small_directory_with_its_index_root_in_an_extension_record_is_read() {
    check("small", 5);
}

/// The issue's layout: `$INDEX_ROOT` in an extension record, and
/// `$INDEX_ALLOCATION` and `$BITMAP` left in the base record.
#[test]
fn a_directory_with_index_blocks_and_its_root_in_an_extension_record_is_read() {
    check("indx", 120);
}
