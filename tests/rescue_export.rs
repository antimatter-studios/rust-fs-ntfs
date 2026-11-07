//! Exporting a rescue archive to a new image (#993, part of #724).
//!
//! The sources are made here, in memory, so the expected image is known
//! before anything is archived: an export is graded against the SHA-256
//! of the source's own bytes, computed with the `sha2` crate, never
//! against this crate's reading of its archive.

mod common;

use fs_ntfs::rescue::{
    export_temp_path, export_to_path, scan, ReadFailure, RescueSource, ScanOptions,
};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

const SECTOR: u64 = 512;
const SIZE: u64 = 3 * 1024 * 1024 + 7 * SECTOR;

/// A source of `SIZE` patterned bytes; reads touching `bad` fail.
struct Memory {
    bytes: Vec<u8>,
    bad: Option<u64>,
}

impl Memory {
    fn new(bad: Option<u64>) -> Self {
        let bytes = (0..SIZE)
            .map(|i| (i.wrapping_mul(31) ^ (i >> 9)) as u8)
            .collect();
        Memory { bytes, bad }
    }
}

impl RescueSource for Memory {
    fn size(&self) -> u64 {
        SIZE
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        let end = offset + buf.len() as u64;
        if let Some(bad) = self.bad {
            if offset < (bad + 1) * SECTOR && bad * SECTOR < end {
                return Err(ReadFailure::Unreadable(format!(
                    "injected bad sector {bad}"
                )));
            }
        }
        buf.copy_from_slice(&self.bytes[offset as usize..end as usize]);
        Ok(())
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_of(path: &Path) -> String {
    hex(&Sha256::digest(fs::read(path).expect("read the file")))
}

fn archive_of(source: &mut Memory, name: &str) -> PathBuf {
    let archive = PathBuf::from(common::temp_image_path(name));
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&archive)
        .expect("create the archive");
    scan(source, &mut out, ScanOptions::default()).expect("scan the source");
    archive
}

#[test]
fn a_complete_archive_exports_to_an_image_equal_to_its_source() {
    let mut source = Memory::new(None);
    let expected = hex(&Sha256::digest(&source.bytes));
    let archive = archive_of(&mut source, "rescue_export_complete");
    let archived = sha256_of(&archive);
    let image = PathBuf::from(common::temp_image_path("rescue_export_complete_image"));

    let report = export_to_path(&archive, &image).expect("a complete archive exports");

    assert_eq!(report.bytes, SIZE);
    assert_eq!(hex(&report.sha256), expected, "the reported SHA-256");
    assert_eq!(sha256_of(&image), expected, "the exported image's SHA-256");
    assert!(
        !export_temp_path(&image).exists(),
        "the temporary file is left behind"
    );
    assert_eq!(sha256_of(&archive), archived, "the archive changed");
}

#[test]
fn an_archive_with_an_unreadable_span_is_refused_and_nothing_is_created() {
    let bad = 4100;
    let mut source = Memory::new(Some(bad));
    let archive = archive_of(&mut source, "rescue_export_incomplete");
    let archived = sha256_of(&archive);
    let image = PathBuf::from(common::temp_image_path("rescue_export_incomplete_image"));

    let err = export_to_path(&archive, &image).expect_err("an incomplete archive is refused");

    let span = format!("{}+{}", bad * SECTOR, SECTOR);
    assert!(
        err.contains(&span),
        "the refusal names the first missing span {span}: {err}"
    );
    assert!(
        !image.exists(),
        "an image was created for an incomplete archive"
    );
    assert!(
        !export_temp_path(&image).exists(),
        "a temporary file was created beside it"
    );
    assert_eq!(sha256_of(&archive), archived, "the archive changed");
}

#[test]
fn an_existing_destination_is_refused_and_left_unchanged() {
    let mut source = Memory::new(None);
    let archive = archive_of(&mut source, "rescue_export_existing");
    let image = PathBuf::from(common::temp_image_path("rescue_export_existing_image"));
    fs::write(&image, b"already here").expect("write the destination");

    let err = export_to_path(&archive, &image).expect_err("an existing destination is refused");

    assert!(err.contains("exists"), "{err}");
    assert_eq!(
        fs::read(&image).expect("read the destination"),
        b"already here"
    );
    assert!(
        !export_temp_path(&image).exists(),
        "a temporary file was created beside it"
    );
}
