//! A Windows-formatted volume, rescued and exported, is handed back to
//! Windows (#995).
//!
//! `test-disks/build-windows-native-read-fixtures.ps1` leaves
//! `ntfs-cli-read.img` and `ntfs-cli-read.sha256.tsv`, the size and the
//! SHA-256 `Get-FileHash` recorded for every file Windows wrote on that
//! volume. This test rescues the image into an archive, exports the archive
//! with `rescue::export_to_path`, and writes the export and a
//! `win-cli-verify.ps1` manifest built from Windows' record into
//! `tmp/rescue-export/`. The CI step after it attaches the export: Windows
//! hashes every file through ntfs.sys against that record, and read-only
//! `chkdsk` grades the volume. Nothing here grades the files itself.
#![cfg(windows)]

mod common;

use fs_ntfs::rescue::{
    export_manifest_path, export_to_path, export_with, scan, scan_to_path, ExportOptions,
    ReadFailure, RescueSource, ScanOptions,
};
use std::path::Path;

const IMAGE: &str = "test-disks/ntfs-cli-read.img";
const RECORD: &str = "test-disks/ntfs-cli-read.sha256.tsv";
const PROVIDER: &str = "test-disks/build-windows-native-read-fixtures.ps1";
const OUT: &str = "tmp/rescue-export";
const EXPORT: &str = "tmp/rescue-export/ntfs-cli-read.img";
const MANIFEST: &str = "tmp/rescue-export/ntfs-cli-read.manifest.tsv";

#[test]
fn the_windows_volume_is_rescued_and_exported_for_windows_to_grade() {
    let record = std::fs::read_to_string(RECORD)
        .unwrap_or_else(|e| panic!("{RECORD}: {e}; run {PROVIDER} to make it"));
    let mut manifest = String::new();
    for (n, line) in record.lines().filter(|l| !l.is_empty()).enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert!(
            fields.len() == 3 && fields[0].starts_with('/') && fields[2].len() == 64,
            "{RECORD} line {}: {line:?} is not /path<TAB>size<TAB>sha256",
            n + 1
        );
        manifest.push_str(&format!("file\t{line}\n"));
    }
    assert!(!manifest.is_empty(), "{RECORD} records no files");

    std::fs::create_dir_all(OUT).expect("make the export directory");
    for stale in [EXPORT, MANIFEST] {
        if Path::new(stale).exists() {
            std::fs::remove_file(stale).expect("remove an earlier run's export");
        }
    }

    let archive = common::temp_image_path("rescue_export_windows");
    let report = scan_to_path(IMAGE.as_ref(), archive.as_ref(), ScanOptions::default())
        .expect("rescue the Windows image");
    assert!(report.is_complete(), "the rescue lost bytes: {report:?}");
    let exported = export_to_path(archive.as_ref(), EXPORT.as_ref())
        .unwrap_or_else(|e| panic!("export the archive to {EXPORT}: {e}"));
    std::fs::remove_file(&archive).ok();
    assert_eq!(
        exported.bytes, report.source_size,
        "the export holds every byte of {IMAGE}"
    );

    std::fs::write(MANIFEST, manifest).expect("write the manifest");
}

/// The Windows image's bytes, its sectors at `bad` byte offsets unreadable.
struct Damaged {
    bytes: Vec<u8>,
    bad: u64,
    sector: u64,
}

impl RescueSource for Damaged {
    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        let end = offset + buf.len() as u64;
        if offset < self.bad + self.sector && self.bad < end {
            return Err(ReadFailure::Unreadable(format!(
                "injected bad sector at byte {}",
                self.bad
            )));
        }
        buf.copy_from_slice(&self.bytes[offset as usize..end as usize]);
        Ok(())
    }
}

#[test]
fn a_missing_span_inside_a_windows_mft_record_names_that_record() {
    let bytes =
        std::fs::read(IMAGE).unwrap_or_else(|e| panic!("{IMAGE}: {e}; run {PROVIDER} to make it"));
    // Record 3 ($Volume) placed from the boot sector Windows wrote: the
    // $MFT cluster and the record size, never this crate's MFT reader.
    let le = |at: usize, n: usize| {
        bytes[at..at + n]
            .iter()
            .rev()
            .fold(0u64, |v, &b| (v << 8) | u64::from(b))
    };
    let sector = le(0x0B, 2);
    let cluster = sector * le(0x0D, 1);
    let per = bytes[0x40] as i8;
    let record = if per > 0 {
        per as u64 * cluster
    } else {
        1u64 << -per
    };
    let bad = le(0x30, 8) * cluster + 3 * record;
    let mut source = Damaged { bytes, bad, sector };

    let archive = common::temp_image_path("rescue_export_windows_owner");
    let mut out = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&archive)
        .expect("create the archive");
    scan(&mut source, &mut out, ScanOptions::default()).expect("scan the damaged image");
    drop(out);
    let image = common::temp_image_path("rescue_export_windows_owner_image");
    let report = export_with(
        archive.as_ref(),
        image.as_ref(),
        ExportOptions {
            allow_incomplete: true,
        },
    )
    .expect("an incomplete archive exports when asked to");
    std::fs::remove_file(&archive).ok();

    assert_eq!(report.missing, vec![(bad, sector)]);
    assert_eq!(report.mft_records, vec![vec![3]], "the span's MFT records");
    let manifest = std::fs::read_to_string(export_manifest_path(image.as_ref()))
        .expect("read the error manifest");
    assert!(
        manifest
            .lines()
            .any(|l| l == format!("{bad} {sector} records=3")),
        "the manifest names record 3 for its span: {manifest}"
    );
    std::fs::remove_file(export_manifest_path(image.as_ref())).ok();
    std::fs::remove_file(&image).ok();
}
