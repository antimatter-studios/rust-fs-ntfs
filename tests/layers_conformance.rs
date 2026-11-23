//! rust-fs-core's salvage conformance set (`fs_core::layers::conformance::
//! salvage`) run against [`NtfsAdapter`], and the strict API it is judged
//! beside: [`NtfsVolume::open`] and the file layer reading every file (#1385).
//!
//! The images are volumes this crate's `mkfs` makes and its write path
//! fills, in memory: a healthy one holding a deleted file, as Windows
//! leaves one (the record kept, its in-use flag clear), and damaged copies,
//! each with the evidence the set requires it to show:
//!
//! - a boot sector with its signature broken: a `Signature` finding;
//! - a file's record with a torn update sequence: a candidate whose
//!   integrity failed;
//! - a file's record on sectors that cannot be read: an `Unreadable`
//!   finding.
//!
//! The Windows-made volume is judged by `tests/layers_windows.rs`.

use fs_core::layers::conformance::salvage::{run, Damaged, Driver, Evidence};
use fs_core::layers::{DamageKind, Extent, FileAPI, NodeKind, OpenVolume};
use fs_core::{BlockDevice, BlockRead};
use fs_ntfs::block_io::BlockIo;
use fs_ntfs::file::NtfsFileAPI;
use fs_ntfs::layers::{LayerError, NtfsAdapter, NtfsVolume};
use fs_ntfs::mft_io::parse_boot_sector;
use fs_ntfs::volume::{NtfsVolumeAPI, Volume};
use std::sync::{Arc, Mutex};

const SIZE: usize = 16 * 1024 * 1024;

#[derive(Clone)]
struct Memory(Arc<Mutex<Vec<u8>>>);

impl BlockRead for Memory {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let data = self.0.lock().unwrap();
        output.copy_from_slice(&data[offset as usize..offset as usize + output.len()]);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.0.lock().unwrap().len() as u64
    }
}

impl BlockDevice for Memory {
    fn write_at(&self, offset: u64, input: &[u8]) -> fs_core::Result<()> {
        let mut data = self.0.lock().unwrap();
        data[offset as usize..offset as usize + input.len()].copy_from_slice(input);
        Ok(())
    }
    fn is_writable(&self) -> bool {
        true
    }
}

impl BlockIo for Memory {
    fn read_exact_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), String> {
        BlockRead::read_at(self, offset, output).map_err(|e| e.to_string())
    }
    fn write_all_at(&mut self, offset: u64, input: &[u8]) -> Result<(), String> {
        BlockDevice::write_at(self, offset, input).map_err(|e| e.to_string())
    }
    fn size(&self) -> u64 {
        self.size_bytes()
    }
}

/// An owned copy of a source, for the strict API, which takes its source by
/// value.
struct Owned(Vec<u8>);

impl BlockRead for Owned {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let end = offset
            .checked_add(output.len() as u64)
            .filter(|&end| end <= self.0.len() as u64)
            .ok_or(fs_core::Error::OutOfBounds {
                offset,
                len: output.len() as u64,
                size: self.0.len() as u64,
            })?;
        output.copy_from_slice(&self.0[offset as usize..end as usize]);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
}

fn pattern(seed: u8, length: usize) -> Vec<u8> {
    (0..length)
        .map(|i| (i as u8).wrapping_mul(37) ^ seed)
        .collect()
}

/// The healthy volume, and the record numbers of a live file and of the
/// deleted one.
fn healthy() -> (Vec<u8>, u64) {
    let mut memory = Memory(Arc::new(Mutex::new(vec![0; SIZE])));
    fs_ntfs::mkfs::format_filesystem(
        &mut memory,
        SIZE as u64,
        4096,
        1024,
        Some("LAYERS"),
        Some(7),
    )
    .unwrap();
    let volume = Volume::open_writable(memory.clone()).unwrap();
    volume.create_directory("/", "docs").unwrap();
    let kept = volume.create_file("/docs", "kept.bin").unwrap();
    volume
        .write_file_at("/docs/kept.bin", 0, &pattern(1, 20000))
        .unwrap();
    volume.create_file("/", "small.txt").unwrap();
    volume
        .write_file_at("/small.txt", 0, b"resident bytes")
        .unwrap();
    volume.create_file("/", "gone.bin").unwrap();
    volume
        .write_file_at("/gone.bin", 0, &pattern(2, 9000))
        .unwrap();
    volume.remove_file("/gone.bin").unwrap();
    NtfsVolumeAPI::flush(&volume).unwrap();
    drop(volume);
    let bytes = memory.0.lock().unwrap().clone();
    (bytes, kept)
}

fn record_offset(image: &[u8], number: u64) -> usize {
    let boot = parse_boot_sector(&image[..512]).unwrap();
    (boot.mft_lcn * boot.cluster_size + number * boot.file_record_size) as usize
}

struct NtfsDriver;

impl Driver for NtfsDriver {
    type Adapter = NtfsAdapter;
    type StrictError = String;

    fn salvage(&self, source: &dyn BlockRead) -> NtfsAdapter {
        NtfsAdapter::from_source(source)
    }

    fn strict(&self, source: &dyn BlockRead) -> Result<(), String> {
        let mut bytes = vec![0; source.size_bytes() as usize];
        source.read_at(0, &mut bytes).map_err(|e| e.to_string())?;
        let volume = NtfsVolume::open(Owned(bytes)).map_err(|e| e.to_string())?;
        let mut pending = vec![volume.root().map_err(|e: LayerError| e.to_string())?];
        while let Some(directory) = pending.pop() {
            for entry in volume.read_dir(&directory).map_err(|e| e.to_string())? {
                if entry.kind == NodeKind::Directory {
                    pending.push(entry.node);
                } else {
                    let mut sink = Vec::new();
                    volume
                        .copy_to(&entry.node, &mut sink)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    fn healthy(&self) -> Vec<u8> {
        healthy().0
    }

    fn damaged(&self) -> Vec<Damaged> {
        let (image, kept) = healthy();
        let mut boot = image.clone();
        boot[3..11].copy_from_slice(b"NOTNTFS ");
        let mut torn = image.clone();
        let at = record_offset(&image, kept);
        torn[at + 510] ^= 0xff;
        let unreadable = image.clone();
        vec![
            Damaged::new(
                "boot signature",
                boot,
                Evidence::Damage(DamageKind::Signature),
            ),
            Damaged::new("torn record", torn, Evidence::FailedCandidate),
            Damaged::new(
                "unreadable record",
                unreadable,
                Evidence::Damage(DamageKind::Unreadable),
            )
            .unreadable(Extent::new(at as u64, 1024)),
        ]
    }
}

#[test]
fn the_salvage_adapter_passes_the_core_conformance_set() {
    run(&NtfsDriver).assert_conforms();
}

#[test]
fn the_healthy_volume_passes_the_core_checks() {
    use fs_core::layers::conformance::{self, Expected, Limits};
    let (image, _) = healthy();
    let volume = NtfsVolume::open(Owned(image)).unwrap();
    conformance::check_healthy(
        &volume,
        &[
            Expected::file("/docs/kept.bin", &pattern(1, 20000)),
            Expected::file("/small.txt", b"resident bytes"),
            Expected::directory("/docs"),
            Expected::absent("/gone.bin"),
        ],
        &Limits::default(),
    )
    .assert_passed();
    conformance::check_files_against_candidates(&volume, &Limits::default()).assert_passed();
}
