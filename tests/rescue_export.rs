//! Exporting a rescue archive to a new image (#993, part of #724).
//!
//! The sources are made here, in memory, so the expected image is known
//! before anything is archived: an export is graded against the SHA-256
//! of the source's own bytes, computed with the `sha2` crate, never
//! against this crate's reading of its archive.

mod common;

use fs_ntfs::rescue::{
    export_manifest_path, export_temp_path, export_to_path, export_with, scan, ExportOptions,
    ReadFailure, RescueSource, ScanOptions,
};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

const SECTOR: u64 = 512;
const SIZE: u64 = 3 * 1024 * 1024 + 7 * SECTOR;

/// A source of `SIZE` patterned bytes; reads touching a `bad` sector fail.
struct Memory {
    bytes: Vec<u8>,
    bad: Vec<u64>,
}

impl Memory {
    fn new(bad: Option<u64>) -> Self {
        Self::with_bad(bad.into_iter().collect())
    }

    fn with_bad(bad: Vec<u64>) -> Self {
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
        for &bad in &self.bad {
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

/// The `(offset, length)` spans an error manifest lists, one per line
/// that is not a `#` comment.
fn manifest_spans(path: &Path) -> Vec<(u64, u64)> {
    fs::read_to_string(path)
        .expect("read the manifest")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split_whitespace();
            let mut next = || {
                fields
                    .next()
                    .and_then(|f| f.parse::<u64>().ok())
                    .unwrap_or_else(|| panic!("a manifest line is not 'offset length': {line}"))
            };
            (next(), next())
        })
        .collect()
}

#[test]
fn an_incomplete_archive_exported_on_request_zeroes_its_missing_spans_and_lists_them() {
    let bad = vec![300, 4100, 5000];
    let mut source = Memory::with_bad(bad.clone());
    let mut expected_bytes = source.bytes.clone();
    for &sector in &bad {
        let at = (sector * SECTOR) as usize;
        expected_bytes[at..at + SECTOR as usize].fill(0);
    }
    let expected = hex(&Sha256::digest(&expected_bytes));
    let spans: Vec<(u64, u64)> = bad.iter().map(|&s| (s * SECTOR, SECTOR)).collect();
    let archive = archive_of(&mut source, "rescue_export_on_request");
    let archived = sha256_of(&archive);
    let image = PathBuf::from(common::temp_image_path("rescue_export_on_request_image"));
    let manifest = export_manifest_path(&image);

    let report = export_with(
        &archive,
        &image,
        ExportOptions {
            allow_incomplete: true,
        },
    )
    .expect("an incomplete archive exports when asked to");

    assert!(!report.is_complete(), "the report calls the image complete");
    assert_eq!(report.missing, spans, "the reported missing spans");
    assert_eq!(report.bytes, SIZE);
    assert_eq!(hex(&report.sha256), expected, "the reported SHA-256");
    assert_eq!(
        sha256_of(&image),
        expected,
        "the image is not the source with exactly the injected sectors zeroed"
    );
    assert_eq!(manifest_spans(&manifest), spans, "the manifest's spans");
    assert!(
        !export_temp_path(&image).exists(),
        "the temporary file is left behind"
    );
    assert_eq!(sha256_of(&archive), archived, "the archive changed");
}

#[test]
fn a_complete_archive_exported_with_the_opt_in_reports_complete_and_writes_no_manifest() {
    let mut source = Memory::new(None);
    let expected = hex(&Sha256::digest(&source.bytes));
    let archive = archive_of(&mut source, "rescue_export_opt_in_complete");
    let image = PathBuf::from(common::temp_image_path(
        "rescue_export_opt_in_complete_image",
    ));

    let report = export_with(
        &archive,
        &image,
        ExportOptions {
            allow_incomplete: true,
        },
    )
    .expect("a complete archive exports");

    assert!(
        report.is_complete(),
        "a complete export reports missing spans"
    );
    assert!(report.missing.is_empty());
    assert_eq!(sha256_of(&image), expected, "the exported image's SHA-256");
    assert!(
        !export_manifest_path(&image).exists(),
        "a complete export wrote an error manifest"
    );
}
