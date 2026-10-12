//! A header field another field depends on is checked before it is
//! trusted.
//!
//! Each case below served or acted on a structure whose header was
//! inconsistent with itself, and each test is the issue's reproduction,
//! asserting a refusal:
//!
//! - #509: an MFT record whose `bytes_allocated` is not the record size
//!   was parsed and served, where ntfs-3g refuses it ("corrupt allocation
//!   size").
//! - #508: the boot sector's index block size was derived without the
//!   rule the reader enforces, so `mkdir` built directories with 69 KiB
//!   or 1 GiB index blocks that this crate's own reader refuses.
//! - #477: inserting into an INDX block walked its entries bounded by
//!   the block rather than by the header's `index_length`, and sliced
//!   past the end: a panic on any create in that directory.

mod common;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{idx_block, mft_io, read, write};
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u32 = 4096;
/// Where a FILE record keeps `bytes_allocated`.
const REC_BYTES_ALLOCATED: u64 = 0x1C;
/// Where the boot sector keeps `clusters_per_index_block`.
const BOOT_CLUSTERS_PER_INDEX_BLOCK: u64 = 0x44;
/// Where an INDX block keeps its index header's `index_length`.
const INDX_INDEX_LENGTH: usize = 0x1C;

fn fresh_volume(tag: &str) -> String {
    let dst = common::temp_image_path(format!("hdrval_{tag}"));
    let f = File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, CLUSTER, Some("HDRV"), Some(7))
        .expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    dst
}

/// #509: a record whose `bytes_allocated` is not the record size is
/// refused, not served. ntfs-3g: "Record N has corrupt allocation size".
#[test]
fn a_record_whose_allocation_size_is_not_the_record_size_is_refused() {
    let img = fresh_volume("bytes_allocated");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", &[0x5a; 20_000]).expect("write /f");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    let params = mft_io::read_boot_params_io(&mut io).expect("boot params");
    let at = mft_io::mft_record_offset_io(&mut io, &params, rec).expect("record offset")
        + REC_BYTES_ALLOCATED;
    let mut original = [0u8; 4];
    io.read_exact_at(at, &mut original)
        .expect("read bytes_allocated");
    assert_eq!(
        u32::from_le_bytes(original) as u64,
        params.file_record_size,
        "a record mkfs and the writer made allocates exactly the record size"
    );
    for corrupt in [56u32, 0] {
        io.write_all_at(at, &corrupt.to_le_bytes())
            .expect("write bytes_allocated");
        let stat = read::read_stat(&mut io, rec);
        assert!(
            stat.is_err(),
            "read_stat served a record that allocates {corrupt} of {} bytes",
            params.file_record_size
        );
        let data = read::read_attribute_value(&mut io, rec, AttrType::Data, None);
        assert!(
            data.is_err(),
            "the reader served {:?} bytes of $DATA from a record that allocates {corrupt} of {}",
            data.map(|d| d.len()),
            params.file_record_size
        );
        io.write_all_at(at, &original)
            .expect("restore bytes_allocated");
    }
}

/// #508: a boot sector whose index block size is not a power of two
/// between one sector and 64 KiB is refused, the rule the reader already
/// applies to every directory.
#[test]
fn a_boot_sector_whose_index_block_size_is_not_one_is_refused() {
    // 17 and 127 clusters, and 2^27 and 2^30 bytes.
    for value in [0x11u8, 0x7f, 0xe5, 0xe2] {
        let img = fresh_volume(&format!("cpib_{value:02x}"));
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
        io.write_all_at(BOOT_CLUSTERS_PER_INDEX_BLOCK, &[value])
            .expect("write clusters_per_index_block");
        let params = mft_io::read_boot_params_io(&mut io);
        assert!(
            params.is_err(),
            "boot byte 0x44 = {value:#04x} was accepted as an index block size of {:?}",
            params.map(|p| p.index_block_size)
        );
        let made = write::mkdir_io(&mut io, "/", "big");
        assert!(
            made.is_err(),
            "mkdir built a directory from boot byte 0x44 = {value:#04x}"
        );
    }
}

/// #477: an INDX block whose `index_length` is shorter than its entry
/// chain is refused by an insert, not walked past.
#[test]
fn an_indx_block_whose_index_length_ends_inside_its_entries_is_refused_by_an_insert() {
    let img = fresh_volume("indx_length");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let dir = write::mkdir_io(&mut io, "/", "d").expect("mkdir /d");
    // As the issue's volume did: a directory of about 120 names, whose
    // index lives in INDX blocks rather than in $INDEX_ROOT.
    const NAMES: usize = 120;
    for i in 0..NAMES {
        write::create_file_io(&mut io, "/d", &format!("f{i:03}")).expect("create in /d");
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    // The leaf an insert of a name sorting after all of them goes to is
    // the one holding the last name.
    // Each block's place is found through the run list: $INDEX_ALLOCATION
    // fragments whenever $MFT grows between two of the directory's blocks.
    let params = mft_io::read_boot_params_io(&mut io).expect("boot params");
    let ia = idx_block::load_for_directory_io(&mut io, dir).expect("/d's $INDEX_ALLOCATION:$I30");
    let device = <PathIo as BlockIo>::size(&io);
    let size = params.index_block_size as usize;
    let last: Vec<u8> = format!("f{:03}", NAMES - 1)
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut target = None;
    for vcn in ia.allocated_block_vcns() {
        let at = idx_block::vcn_to_disk_offset(&ia, vcn, device).expect("place an INDX block");
        let mut block = vec![0u8; size];
        io.read_exact_at(at, &mut block)
            .expect("read an INDX block");
        if mft_io::apply_fixup_on_read_magic(&mut block, params.bytes_per_sector, b"INDX").is_ok()
            && block.windows(last.len()).any(|w| w == last.as_slice())
        {
            target = Some((at, block));
            break;
        }
    }
    let (at, mut block) = target.expect("an INDX block holds the last name");
    let index_length = u32::from_le_bytes(
        block[INDX_INDEX_LENGTH..INDX_INDEX_LENGTH + 4]
            .try_into()
            .unwrap(),
    );
    assert!(
        index_length > 0xff * 2,
        "the leaf holding the last name fills more than {} bytes (index_length \
         {index_length:#x})",
        0xff * 2
    );
    // The issue's value: the header now ends the entries a few entries in.
    block[INDX_INDEX_LENGTH..INDX_INDEX_LENGTH + 4].copy_from_slice(&0xffu32.to_le_bytes());
    mft_io::apply_fixup_on_write_magic(&mut block, params.bytes_per_sector, b"INDX")
        .expect("re-stamp the INDX block");
    io.write_all_at(at, &block)
        .expect("write the INDX block back");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    // A name that sorts after every entry: the insertion point is the
    // end of the real chain, past the end the header now claims.
    let created = write::create_file_io(&mut io, "/d", "zzzz");
    assert!(
        created.is_err(),
        "create_file_io inserted into an INDX block whose index_length ends inside its \
         entries: {created:?}"
    );
}
