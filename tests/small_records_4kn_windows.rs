//! A volume Windows formatted on 4096-byte sectors with 1024-byte file
//! records is writable with the native 512-byte update-sequence stride (#1396).
//!
//! `test-disks/build-windows-4kn-fixture.ps1` has Windows format a disk
//! presenting 4096-byte logical sectors with `Format-Volume` and its
//! defaults, write a file set to it, run a read-only `chkdsk` (clean), and
//! copy the volume out to `ntfs-4kn-read.img`, with
//! `ntfs-4kn-read.sha256.tsv`, each file's path, size and `Get-FileHash`
//! SHA-256, `ntfs-4kn-read.index.tsv`, the same for the 400 files of a
//! directory large enough for index blocks, and `ntfs-4kn-read.ntfsinfo.txt`,
//! `fsutil fsinfo ntfsinfo`.
//! Windows chose 1024-byte file records for that disk, and its update
//! sequence arrays have one entry per 512 bytes whatever the sector size:
//! three words in a file record, nine in a 4096-byte index block. Readers
//! and writers take the stride from each record's array.
#![cfg(windows)]

use fs_core::BlockRead;
use fs_ntfs::evidence::{logfile_state, read_logfile};
use fs_ntfs::file::NtfsFileAPI;
use fs_ntfs::mft_io::apply_fixup_on_read_magic;
use fs_ntfs::primitives::NtfsPrimitiveAPI;
use fs_ntfs::volume::{NtfsVolumeAPI, RawVolume, Volume};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const IMAGE: &str = "test-disks/ntfs-4kn-read.img";
const HASHES: &str = "test-disks/ntfs-4kn-read.sha256.tsv";
/// The files of `/index-blocks`, a directory large enough for index blocks.
const INDEX_HASHES: &str = "test-disks/ntfs-4kn-read.index.tsv";
const INDEX_DIRECTORY: &str = "/index-blocks";
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

/// `(path, size, sha256)` of each file a report of Windows' names.
fn report(path: &str) -> Vec<(String, u64, String)> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{path}: {e}; {PROVIDER} builds it"));
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

/// `(path, size, sha256)` of every file Windows wrote: the file set of
/// ntfs-cli-read.img and the files of [`INDEX_DIRECTORY`].
fn windows_files() -> Vec<(String, u64, String)> {
    let mut files = report(HASHES);
    assert!(!files.is_empty(), "{HASHES} names no file");
    let index = report(INDEX_HASHES);
    assert!(
        index.len() >= 400,
        "{INDEX_HASHES} names {} files; {PROVIDER} writes 400",
        index.len()
    );
    files.extend(index);
    files
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
        .read_directory(INDEX_DIRECTORY)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .filter(|name| name != "." && name != "..")
        .collect();
    let windows: BTreeSet<String> = report(INDEX_HASHES)
        .into_iter()
        .filter_map(|(path, ..)| {
            let rest = path.strip_prefix(INDEX_DIRECTORY)?.strip_prefix('/')?;
            Some(rest.split('/').next().unwrap().to_owned())
        })
        .collect();
    assert!(
        windows.len() >= 400,
        "{INDEX_HASHES} names {}",
        windows.len()
    );
    assert_eq!(listed, windows);
}

#[test]
fn a_torn_stride_of_a_record_smaller_than_its_sector_is_still_detected() {
    let device = image();
    let volume = Volume::open(device.clone()).unwrap();
    let (path, ..) = &report(HASHES)[0];
    let number = volume.open_file(path).unwrap().record_number();
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
fn a_malformed_array_of_a_windows_record_is_refused() {
    let device = image();
    let volume = Volume::open(device.clone()).unwrap();
    let (path, ..) = &report(HASHES)[0];
    let number = volume.open_file(path).unwrap().record_number();
    let offset = volume.inspect_mft_record(number).unwrap().offset;
    // The array's offset is at byte 4 of the record, its word count at byte 6.
    for (field, value) in [(4u64, 508u16), (4, 1020), (6, 2), (6, 4)] {
        let mut damaged = device.clone();
        damaged.write(offset + field, &value.to_le_bytes());
        let refused = Volume::open_for_salvage(damaged)
            .unwrap()
            .inspect_mft_record(number)
            .unwrap();
        assert!(
            refused.fixed.is_none(),
            "{value} at byte {field} of the record was accepted"
        );
    }
}

#[test]
fn every_log_page_windows_wrote_decodes_with_512_byte_strides() {
    let device = image();
    let state = logfile_state(&device).unwrap();
    assert!(!state.needs_replay(), "{}", state.describe());
    let log = read_logfile(&device).unwrap();
    // The system and log page sizes the first restart page names.
    assert_eq!(&log.bytes[0x10..0x18], &[0, 0x10, 0, 0, 0, 0x10, 0, 0]);
    let (mut restart_pages, mut record_pages) = (0, 0);
    for (index, page) in log.bytes.chunks_exact(4096).enumerate() {
        let magic: &[u8; 4] = match &page[..4] {
            b"RSTR" => {
                restart_pages += 1;
                b"RSTR"
            }
            b"RCRD" => {
                record_pages += 1;
                b"RCRD"
            }
            _ => continue,
        };
        assert_eq!(le_u16(page, 6), 9, "log page {index}");
        apply_fixup_on_read_magic(&mut page.to_vec(), 512, magic)
            .unwrap_or_else(|error| panic!("log page {index}: {error}"));
        for tail in (510..4096).step_by(512) {
            let mut torn = page.to_vec();
            torn[tail] ^= 0xff;
            assert!(
                apply_fixup_on_read_magic(&mut torn, 512, magic).is_err(),
                "log page {index}: a torn stride ending at {tail} was accepted"
            );
        }
    }
    assert_eq!(restart_pages, 2);
    assert!(record_pages > 0, "Windows' log holds no record page");

    // One torn restart page leaves the other to be read; two leave none.
    let mut damaged = device.clone();
    damaged.update(log.offset + 510, |byte| byte ^ 0xff);
    let one = logfile_state(&damaged).unwrap().describe();
    assert!(!one.contains("no readable restart page"), "{one}");
    damaged.update(log.offset + 4096 + 510, |byte| byte ^ 0xff);
    let state = logfile_state(&damaged).unwrap();
    assert!(
        state.describe().contains("no readable restart page"),
        "{}",
        state.describe()
    );
}

/// The offsets of the 4096-byte index blocks that hold a name of
/// [`INDEX_DIRECTORY`], `f000.txt` to `f399.txt`.
fn index_blocks_of_the_directory(device: &Image) -> Vec<u64> {
    let utf16 =
        |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let names: Vec<Vec<u8>> = (0..400).map(|i| utf16(&format!("f{i:03}.txt"))).collect();
    let mut blocks = Vec::new();
    let mut chunk = vec![0; 1 << 20];
    let mut at = 0;
    while at < device.size_bytes() {
        let count = (device.size_bytes() - at).min(chunk.len() as u64) as usize;
        device.read_at(at, &mut chunk[..count]).unwrap();
        for (index, block) in chunk[..count].chunks_exact(4096).enumerate() {
            if &block[..4] == b"INDX"
                && block
                    .windows(16)
                    .any(|window| names.iter().any(|name| window == name.as_slice()))
            {
                blocks.push(at + index as u64 * 4096);
            }
        }
        at += count as u64;
    }
    blocks
}

#[test]
fn a_torn_stride_of_an_index_block_windows_wrote_is_refused() {
    let device = image();
    let blocks = index_blocks_of_the_directory(&device);
    assert!(!blocks.is_empty(), "no index block holds a name");
    for &block in &blocks {
        assert_eq!(le_u16(&device.read(block, 8), 6), 9, "block at {block:#x}");
    }
    for tail in (510..4096u64).step_by(512) {
        let mut damaged = device.clone();
        for &block in &blocks {
            damaged.update(block + tail, |byte| byte ^ 0xff);
        }
        let error = match Volume::open(damaged)
            .unwrap()
            .read_directory(INDEX_DIRECTORY)
        {
            Ok(entries) => panic!(
                "a torn stride ending at {tail} listed {} names",
                entries.len()
            ),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("USN mismatch"), "{error}");
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
    for (path, size, digest) in &windows_files() {
        let number = volume.open_file(path).unwrap().record_number();
        let base = raw.read_mapped_record(&map, number).unwrap();
        assert!(base.issues.is_empty(), "{path}: {:?}", base.issues);
        let resolved = raw.resolve_salvage_record(&base, &map).unwrap();
        let name = path.rsplit('/').next().unwrap();
        let names = resolved.file_names();
        assert!(
            names.iter().any(|found| found.name == name),
            "{path}: the record names {names:?}"
        );
        let stream = resolved.stream("").unwrap();
        // One byte more than Windows' length, so a longer stream shows.
        let mut bytes = vec![0; *size as usize + 1];
        let mut filled = 0;
        loop {
            let count = raw
                .read_resolved_stream_at(
                    &stream,
                    boot.cluster_size,
                    filled as u64,
                    &mut bytes[filled..],
                )
                .unwrap_or_else(|error| panic!("{path}: {error}"));
            if count == 0 {
                break;
            }
            filled += count;
        }
        assert_eq!(filled as u64, *size, "{path}");
        assert_eq!(&sha256(&bytes[..filled]), digest, "{path}");
    }
}

#[test]
fn a_writable_mount_of_records_smaller_than_a_sector_is_admitted() {
    let device = image();
    Volume::open_writable(device).expect("512-byte USA strides make these records writable");
}

#[test]
fn the_shared_layers_read_the_volume_windows_formatted() {
    use fs_core::layers::conformance::{self, Limits};
    use fs_core::layers::{FileAPI, OpenVolume};
    use fs_ntfs::layers::NtfsVolume;
    let volume = NtfsVolume::open(image()).unwrap();
    let limits = Limits {
        max_lookups: 256,
        ..Limits::default()
    };
    conformance::check_healthy(&volume, &[], &limits).assert_passed();
    for (path, size, digest) in windows_files() {
        let node = volume.lookup(&path).unwrap();
        let mut bytes = Vec::new();
        volume.copy_to(&node, &mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, size, "{path}");
        assert_eq!(sha256(&bytes), digest, "{path}");
    }
}
