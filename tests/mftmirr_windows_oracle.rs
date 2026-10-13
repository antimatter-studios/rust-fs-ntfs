//! `$MFTMirr` on a 64 KiB-cluster volume, graded by Windows (#481).
//!
//! When a cluster holds more than the four records the mirror exists for,
//! two formatters disagree about how many it declares: this crate declares
//! four records (`mkfs::MFTMIRR_RECORDS`), and `mkntfs -c 65536` declares
//! the whole cluster. ntfs-3g prints a fixup warning for every zero record
//! it reads past the fourth on a volume of ours with that geometry. The
//! formatter is held to neither of them but to Windows: the fixture is an
//! empty volume Windows' own formatter made with 64 KiB clusters
//! (`test-disks/build-windows-native-read-fixtures.ps1`), and the volume
//! this crate formats with the same size, cluster and record size must
//! declare, and hold, what Windows' does.

mod common;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{mft_io, read};
use std::path::Path;

const WINDOWS_IMG: &str = "test-disks/ntfs-format-64k.img";
/// `$MFTMirr` is MFT record 1.
const MFTMIRR_RECORD: u64 = 1;
const CLUSTER: u32 = 65536;

/// What a volume's `$MFTMirr` is: its record size, the bytes its `$DATA`
/// declares, and how many records from its start carry a `FILE` header.
#[derive(Debug, PartialEq, Eq)]
struct Mirror {
    record_size: u64,
    declared_bytes: u64,
    written: u64,
}

fn mirror(img: &str) -> Mirror {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let (start, declared_bytes) =
        read::nonresident_contiguous_disk_range(&mut io, MFTMIRR_RECORD, AttrType::Data, None)
            .expect("$MFTMirr's $DATA is one extent");
    let (params, _) = mft_io::read_mft_record_io(&mut io, MFTMIRR_RECORD).expect("read record 1");
    let record_size = params.file_record_size;
    // Read the whole cluster, not only what is declared, so a mirror
    // that holds more than it declares is counted too.
    let span = declared_bytes.max(u64::from(CLUSTER));
    let mut bytes = vec![0u8; span as usize];
    io.read_exact_at(start, &mut bytes)
        .expect("read the mirror");
    let written = bytes
        .chunks_exact(record_size as usize)
        .take_while(|c| &c[0..4] == b"FILE")
        .count() as u64;
    Mirror {
        record_size,
        declared_bytes,
        written,
    }
}

#[test]
fn the_mirror_on_64k_clusters_is_what_windows_writes() {
    assert!(
        Path::new(WINDOWS_IMG).exists(),
        "missing {WINDOWS_IMG}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let windows = mirror(WINDOWS_IMG);
    let size = std::fs::metadata(WINDOWS_IMG).expect("stat").len();

    let ours = common::temp_image_path("mftmirr_windows_64k");
    let f = std::fs::File::create(&ours).expect("create image");
    f.set_len(size).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&ours)).expect("open_rw");
    format_filesystem(
        &mut io,
        size,
        CLUSTER,
        windows.record_size as u32,
        Some("C64K"),
        Some(6),
    )
    .expect("format at 64 KiB clusters");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);

    assert_eq!(
        mirror(&ours),
        windows,
        "$MFTMirr on a {size}-byte volume with 64 KiB clusters: ours (left) against Windows' \
         (right)"
    );
    let _ = std::fs::remove_file(&ours);
}
