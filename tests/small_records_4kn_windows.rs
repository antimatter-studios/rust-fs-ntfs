//! A volume Windows formatted on 4096-byte sectors with 1024-byte file
//! records is read, and only read (#1384).
//!
//! `test-disks/build-windows-4kn-fixture.ps1` has Windows format a disk
//! presenting 4096-byte logical sectors with `Format-Volume` and its
//! defaults, write a file set to it, run a read-only `chkdsk` (clean), and
//! copy the volume out to `ntfs-4kn-read.img`, with
//! `ntfs-4kn-read.sha256.tsv`, each file's path, size and `Get-FileHash`
//! SHA-256, and `ntfs-4kn-read.ntfsinfo.txt`, `fsutil fsinfo ntfsinfo`.
//! Windows chose 1024-byte file records for that disk, and its update
//! sequence arrays have one entry per 512 bytes whatever the sector size:
//! three words in a file record, nine in a 4096-byte index block. The
//! reader takes the stride from the record's own array; the writers build
//! arrays from the sector size, so a writable mount of this geometry is
//! refused by name.
#![cfg(windows)]

use fs_ntfs::file::FileAPI;
use fs_ntfs::primitives::PrimitiveAPI;
use fs_ntfs::volume::{RawVolume, Volume, VolumeAPI};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const IMAGE: &str = "test-disks/ntfs-4kn-read.img";
const HASHES: &str = "test-disks/ntfs-4kn-read.sha256.tsv";
const RECORD: &str = "test-disks/ntfs-4kn-read.ntfsinfo.txt";
const PROVIDER: &str = "test-disks/build-windows-4kn-fixture.ps1";

mod common;

/// The image file read a span at a time; writes go to memory, never to it.
type Image = common::PatchedImage;

fn image() -> Image {
    Image::open(IMAGE).unwrap_or_else(|e| {
        panic!("{IMAGE}: {e}; {PROVIDER} builds it on an elevated Windows host")
    })
}

/// `(path, size, sha256)` of every file Windows wrote.
fn windows_files() -> Vec<(String, u64, String)> {
    let text = std::fs::read_to_string(HASHES)
        .unwrap_or_else(|e| panic!("{HASHES}: {e}; {PROVIDER} builds it"));
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (
                fields[0].to_owned(),
                fields[1].parse().unwrap(),
                fields[2].to_owned(),
            )
        })
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn le_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

#[test]
fn the_geometry_is_what_fsutil_reported() {
    let record = std::fs::read_to_string(RECORD)
        .unwrap_or_else(|e| panic!("{RECORD}: {e}; {PROVIDER} builds it"));
    let field = |key: &str| {
        record
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                (k.trim() == key).then(|| v.split_whitespace().next().unwrap().replace(',', ""))
            })
            .unwrap_or_else(|| panic!("{RECORD} has no {key:?}"))
    };
    assert_eq!(field("Bytes Per Sector"), "4096");
    assert_eq!(field("Bytes Per FileRecord Segment"), "1024");

    let volume = Volume::open(image()).unwrap();
    assert_eq!(volume.boot().bytes_per_sector, 4096);
    assert_eq!(volume.boot().file_record_size, 1024);

    // Windows' own arrays: one entry per 512 bytes of the structure.
    let mft = volume.boot().mft_lcn * volume.boot().cluster_size;
    let mut raw = vec![0; 1024];
    volume.raw().read_volume_bytes(mft, &mut raw).unwrap();
    assert_eq!(&raw[..4], b"FILE");
    assert_eq!(
        le_u16(&raw, 6),
        3,
        "a 1024-byte record's array has three words"
    );
}

#[test]
fn every_file_windows_wrote_reads_back() {
    let volume = Volume::open(image()).unwrap();
    let files = windows_files();
    assert!(files.len() > 400, "{HASHES} names {} files", files.len());
    for (path, size, digest) in &files {
        let file = volume
            .open_file(path)
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        let mut bytes = vec![0; *size as usize + 1];
        let mut filled = 0;
        loop {
            let count = volume
                .read_file_at(&file, filled as u64, &mut bytes[filled..])
                .unwrap_or_else(|error| panic!("{path}: {error}"));
            if count == 0 {
                break;
            }
            filled += count;
        }
        assert_eq!(filled as u64, *size, "{path}");
        assert_eq!(
            &sha256(&bytes[..filled]),
            digest,
            "{path} differs from Windows"
        );
    }
}

#[test]
fn a_directory_held_in_index_blocks_lists_what_windows_wrote() {
    let volume = Volume::open(image()).unwrap();
    let listed: BTreeSet<String> = volume
        .read_directory("/dir")
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .filter(|name| name != "." && name != "..")
        .collect();
    let windows: BTreeSet<String> = windows_files()
        .into_iter()
        .filter_map(|(path, ..)| {
            let rest = path.strip_prefix("/dir/")?;
            Some(rest.split('/').next().unwrap().to_owned())
        })
        .collect();
    assert!(windows.len() > 400);
    assert_eq!(listed, windows);
}

#[test]
fn a_torn_stride_of_a_record_smaller_than_its_sector_is_still_detected() {
    let device = image();
    let volume = Volume::open(device.clone()).unwrap();
    let number = volume.open_file("/large.bin").unwrap().record_number();
    let record = volume.inspect_mft_record(number).unwrap();
    assert!(record.issues.is_empty(), "{:?}", record.issues);
    for stride_end in [510usize, 1022] {
        let mut damaged = device.clone();
        damaged.update(record.offset + stride_end as u64, |byte| byte ^ 0xff);
        let torn = Volume::open_for_salvage(damaged)
            .unwrap()
            .inspect_mft_record(number)
            .unwrap();
        assert!(
            torn.fixed.is_none(),
            "a torn stride ending at {stride_end} was accepted"
        );
    }
}

#[test]
fn a_salvage_read_follows_the_same_records() {
    let device = image();
    let volume = Volume::open(device.clone()).unwrap();
    let boot = *volume.boot();
    let raw = RawVolume::open(device);
    let map = raw
        .read_mft_map(
            boot.mft_lcn * boot.cluster_size,
            boot.file_record_size as usize,
            boot.bytes_per_sector,
            boot.cluster_size,
        )
        .unwrap();
    for (path, size, digest) in windows_files().iter().take(4) {
        let number = volume.open_file(path).unwrap().record_number();
        let base = raw.read_mapped_record(&map, number).unwrap();
        assert!(base.issues.is_empty(), "{path}: {:?}", base.issues);
        let stream = raw
            .resolve_salvage_record(&base, &map)
            .unwrap()
            .stream("")
            .unwrap();
        let mut bytes = vec![0; *size as usize];
        let count = raw
            .read_resolved_stream_at(&stream, boot.cluster_size, 0, &mut bytes)
            .unwrap();
        assert_eq!(count as u64, *size);
        assert_eq!(&sha256(&bytes), digest, "{path}");
    }
}

#[test]
fn a_writable_mount_of_records_smaller_than_a_sector_is_refused_and_writes_nothing() {
    let device = image();
    let before = device.digest();
    let error = match Volume::open_writable(device.clone()) {
        Ok(_) => panic!("a writable mount was admitted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("smaller than"), "{error}");
    assert_eq!(device.digest(), before);
}
