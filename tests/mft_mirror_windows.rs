//! `$MFTMirr` as a salvage source when `$MFT`'s first records are damaged
//! (#1386).
//!
//! `test-disks/ntfs-salvage-streams.img` is a volume Windows formatted and
//! wrote (`test-disks/build-windows-salvage-stream-fixtures.ps1`); its
//! `$MFTMirr` holds Windows' own copy of `$MFT`'s first records. Each test
//! damages a copy of it in memory and reads through `PrimitiveAPI` on a
//! `RawVolume`, as a salvage tool does.

use fs_core::BlockRead;
use fs_ntfs::attr_io::AttrType;
use fs_ntfs::mft_io::{parse_boot_sector, BootParams};
use fs_ntfs::primitives::{AttributeValue, PrimitiveAPI};
use fs_ntfs::volume::RawVolume;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

const IMAGE: &str = "test-disks/ntfs-salvage-streams.img";
const REPORT: &str = "test-disks/ntfs-salvage-streams.tsv";
const PROVIDER: &str = "test-disks/build-windows-salvage-stream-fixtures.ps1";

#[derive(Clone)]
struct Image(Arc<Vec<u8>>);

impl BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let range = usize::try_from(offset)
            .ok()
            .and_then(|start| Some(start..start.checked_add(output.len())?))
            .and_then(|range| self.0.get(range));
        output.copy_from_slice(range.ok_or(fs_core::Error::OutOfBounds {
            offset,
            len: output.len() as u64,
            size: self.0.len() as u64,
        })?);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
}

fn windows_image() -> (Vec<u8>, BootParams) {
    for path in [IMAGE, REPORT] {
        assert!(
            Path::new(path).exists(),
            "missing {path}; run {PROVIDER} on Windows"
        );
    }
    let bytes = std::fs::read(IMAGE).unwrap();
    let boot = parse_boot_sector(&bytes[..512]).unwrap();
    (bytes, boot)
}

fn volume(bytes: Vec<u8>) -> RawVolume<Image> {
    RawVolume::open(Image(Arc::new(bytes)))
}

fn mft_offset(boot: &BootParams) -> usize {
    (boot.mft_lcn * boot.cluster_size) as usize
}

/// `$MFT`'s first four records overwritten.
fn with_first_records_destroyed(mut bytes: Vec<u8>, boot: &BootParams) -> Vec<u8> {
    let start = mft_offset(boot);
    let length = 4 * boot.file_record_size as usize;
    for (i, byte) in bytes[start..start + length].iter_mut().enumerate() {
        *byte = (i * 31 + 7) as u8;
    }
    bytes
}

#[test]
fn the_mirror_location_is_in_the_boot_parameters_and_is_where_mft_says() {
    let (bytes, boot) = windows_image();
    assert_ne!(boot.mft_mirror_lcn, 0);
    assert_ne!(boot.mft_mirror_lcn, boot.mft_lcn);
    assert_eq!(boot.mft_mirror_records(), 4);
    // $MFT's record 1 is $MFTMirr: its data run starts where the boot sector says.
    let raw = volume(bytes);
    let record = raw
        .read_file_record_at(
            mft_offset(&boot) as u64 + boot.file_record_size,
            boot.file_record_size as usize,
            boot.bytes_per_sector,
        )
        .unwrap();
    let data = record
        .attributes
        .iter()
        .find(|a| a.type_code == AttrType::Data as u32 && a.name.is_empty())
        .unwrap();
    let AttributeValue::NonResident { runs, .. } = &data.value else {
        panic!("$MFTMirr's data is resident");
    };
    assert_eq!(runs[0].lcn, Some(boot.mft_mirror_lcn));
    assert_eq!(
        boot.mft_mirror_offset(),
        Some(boot.mft_mirror_lcn * boot.cluster_size)
    );
}

#[test]
fn the_mirror_returns_the_records_windows_wrote_when_mft_has_lost_them() {
    let (bytes, boot) = windows_image();
    let original = volume(bytes.clone());
    let damaged = volume(with_first_records_destroyed(bytes, &boot));
    for number in 0..4u64 {
        let at = mft_offset(&boot) as u64 + number * boot.file_record_size;
        let size = boot.file_record_size as usize;
        let lost = damaged
            .read_file_record_at(at, size, boot.bytes_per_sector)
            .unwrap();
        assert!(
            lost.fixed.is_none(),
            "record {number} still reads from $MFT"
        );
        let windows = original
            .read_file_record_at(at, size, boot.bytes_per_sector)
            .unwrap();
        let mirrored = damaged.read_mirror_record(&boot, number).unwrap();
        assert!(
            mirrored.issues.is_empty(),
            "{number}: {:?}",
            mirrored.issues
        );
        assert_eq!(
            mirrored.fixed, windows.fixed,
            "record {number} from the mirror is not the one Windows wrote in $MFT"
        );
    }
    assert!(damaged.read_mirror_record(&boot, 4).is_err());
}

#[test]
fn files_are_found_through_a_map_built_from_the_mirror() {
    let (bytes, boot) = windows_image();
    let damaged = volume(with_first_records_destroyed(bytes, &boot));
    let mft = mft_offset(&boot) as u64;
    assert!(damaged
        .read_mft_map(
            mft,
            boot.file_record_size as usize,
            boot.bytes_per_sector,
            boot.cluster_size
        )
        .is_err());
    let map = damaged.read_mft_map_from_mirror(&boot).unwrap();
    assert!(
        map.limitations[0].contains("$MFTMirr"),
        "{:?}",
        map.limitations
    );
    assert_eq!(map.record_offset(0), Some(mft));

    let report = std::fs::read_to_string(REPORT).unwrap();
    let row: Vec<&str> = report
        .lines()
        .find(|line| line.starts_with("stream\t/fragmented.bin\t"))
        .unwrap()
        .split('\t')
        .collect();
    let base = damaged
        .read_mapped_record(&map, row[2].parse().unwrap())
        .unwrap();
    let stream = damaged
        .resolve_salvage_record(&base, &map)
        .unwrap()
        .stream("")
        .unwrap();
    let mut bytes = vec![0; row[4].parse().unwrap()];
    let count = damaged
        .read_resolved_stream_at(&stream, boot.cluster_size, 0, &mut bytes)
        .unwrap();
    assert_eq!(count, bytes.len());
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        digest, row[5],
        "/fragmented.bin differs from what Windows read"
    );
}

#[test]
fn an_unreadable_mirror_is_reported_as_damage() {
    let (bytes, boot) = windows_image();
    let mirror = (boot.mft_mirror_lcn * boot.cluster_size) as usize;

    // Torn: the records are there, and fail their fixups.
    let mut torn = bytes.clone();
    torn[mirror + 510] ^= 0xff;
    let raw = volume(torn);
    let record = raw.read_mirror_record(&boot, 0).unwrap();
    assert!(record.fixed.is_none());
    assert!(!record.issues.is_empty());
    assert!(raw.read_mft_map_from_mirror(&boot).is_err());

    // Gone: zeros where the mirror was.
    let mut zeroed = bytes.clone();
    zeroed[mirror..mirror + 4 * boot.file_record_size as usize].fill(0);
    let record = volume(zeroed).read_mirror_record(&boot, 1).unwrap();
    assert!(record.fixed.is_none() && !record.issues.is_empty());

    // Out of reach: a boot sector naming a cluster past the device, or one
    // whose offset overflows.
    for lcn in [boot.total_sectors, u64::MAX / 2] {
        let far = BootParams {
            mft_mirror_lcn: lcn,
            ..boot
        };
        let raw = volume(bytes.clone());
        assert!(raw.read_mirror_record(&far, 0).is_err());
        assert!(raw.read_mft_map_from_mirror(&far).is_err());
    }
}
