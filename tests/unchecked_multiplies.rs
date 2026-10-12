//! A field read off the disk is multiplied only by checked arithmetic.
//!
//! Each case below multiplied an on-disk value raw. In a debug build that
//! is `attempt to multiply with overflow`, a panic; in release, where this
//! crate ships with overflow checks off, the product wraps and the wrong
//! number goes on to size a volume, count records or name a record a
//! replay writes. Each was found by structure-aware fuzzing, and each test
//! here is the issue's own reproduction, asserting a refusal.
//!
//! - #475: `read_volume_info` multiplied `total_sectors` by the sector
//!   size.
//! - #476: `$MFT:$Bitmap`'s value length was multiplied by eight, from
//!   `fsck::check_io`, `write::create_file_io` and `write::unlink_io`.
//! - #507: `$LogFile` replay multiplied a log record's `target_vcn` by
//!   the cluster size to name a `FILE` block's record.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fsck, mft_io, read, write};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const CLUSTER: u32 = 4096;

/// A freshly formatted 16 MiB volume with 4 KiB clusters, holding one
/// file, `/victim.txt`, for the unlink.
fn fresh_volume(tag: &str) -> String {
    let dst = common::temp_image_path(format!("unchecked_mul_{tag}"));
    let f = File::create(&dst).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&dst)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, CLUSTER, CLUSTER, Some("UMUL"), Some(7))
        .expect("format_filesystem");
    write::create_file_io(&mut io, "/", "victim.txt").expect("create /victim.txt");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    dst
}

/// #475: a boot sector whose `total_sectors` times the sector size
/// overflows is refused, not multiplied.
#[test]
fn a_total_sectors_whose_volume_size_overflows_is_refused() {
    let img = fresh_volume("total_sectors");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    // The issue's value: 0x70f9cd968c587e0e sectors.
    io.write_all_at(0x28, &[0x0e, 0x7e, 0x58, 0x8c, 0x96, 0xcd, 0xf9, 0x70])
        .expect("write total_sectors");
    let got = read::read_volume_info(&mut io);
    assert!(
        got.is_err(),
        "read_volume_info accepted a total_sectors whose byte count overflows: {:?}",
        got.map(|v| v.total_size)
    );
}

/// #476: a `$MFT:$Bitmap` whose value length times eight overflows is
/// refused by every entry point that reads it, not multiplied.
#[test]
fn an_mft_bitmap_whose_bit_count_overflows_is_refused() {
    let img = fresh_volume("mft_bitmap");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");

    let (params, record) = mft_io::read_mft_record_io(&mut io, 0).expect("read $MFT record 0");
    let bm = attr_io::find_attribute(&record, AttrType::Bitmap, None)
        .expect("$MFT has an unnamed $Bitmap");
    assert!(
        !bm.is_resident,
        "this layout's $MFT:$Bitmap is non-resident, which is the case under test"
    );
    // A non-resident attribute's data size is at +0x30. The update-sequence
    // words are the last two bytes of each sector; the field must not
    // cover one, or the record would fail its fixup check rather than
    // reach the multiply.
    let field = (bm.attr_offset + 0x30) as u64;
    let sector = u64::from(params.bytes_per_sector);
    assert!(
        field % sector + 8 <= sector - 2,
        "the data-size field at {field:#x} overlaps an update-sequence word"
    );
    let record_at = mft_io::mft_record_offset_io(&mut io, &params, 0).expect("record 0 offset");
    io.write_all_at(record_at + field, &0xc6f8_e7e1_e2d2_6763u64.to_le_bytes())
        .expect("write $MFT:$Bitmap data size");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    let checked = fsck::check_io(&mut io);
    assert!(
        checked.is_err(),
        "check_io accepted an $MFT:$Bitmap whose bit count overflows"
    );
    let created = write::create_file_io(&mut io, "/", "new.txt");
    assert!(
        created.is_err(),
        "create_file_io allocated a record from an $MFT:$Bitmap whose bit count overflows: {created:?}"
    );
    let unlinked = write::unlink_io(&mut io, "/victim.txt");
    assert!(
        unlinked.is_err(),
        "unlink_io freed a record in an $MFT:$Bitmap whose bit count overflows"
    );
}

/// The committed Windows capture of an interrupted transaction, copied
/// out of its archive.
fn unpack(name: &str) -> String {
    let path = common::temp_image_path(format!("unchecked_mul_{}", name.replace('.', "_")));
    let out = File::create(&path).expect("create image copy");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    path
}

/// #507: a log record whose `target_vcn` times the cluster size
/// overflows is refused by replay, not multiplied into a record number.
#[test]
fn a_log_record_whose_target_vcn_overflows_is_refused_by_replay() {
    let img = unpack("windows-interrupted-undo");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    // The issue's edits, all inside `$LogFile` record pages, none on an
    // update-sequence word.
    let edits: [(u64, &[u8]); 4] = [
        (0x286_ec7c, &[0x8c, 0x5d, 0x58, 0xa0]),
        (0x286_eccc, &[0xdd]),
        (0x28b_b917, &[0x10]),
        (
            0x28b_bb48,
            &[0x44, 0x27, 0xf1, 0x47, 0x6e, 0x54, 0x80, 0x02],
        ),
    ];
    for (at, bytes) in edits {
        io.write_all_at(at, bytes).expect("edit $LogFile");
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    // Refused, and refused for this: in release, where the multiply
    // wrapped instead of panicking, replay went on with the wrong record
    // number and failed later for some other reason, which a bare
    // `is_err` cannot tell from the refusal.
    let replayed = fsck::replay_logfile_io(&mut io).map(|_| ());
    let refused_for_the_vcn = replayed
        .as_ref()
        .err()
        .is_some_and(|e| e.to_string().contains("names VCN"));
    assert!(
        refused_for_the_vcn,
        "replay did not refuse a log record whose target_vcn overflows the cluster size: \
         {replayed:?}"
    );
}
