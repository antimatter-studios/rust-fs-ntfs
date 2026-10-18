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

use fs_ntfs::rescue::{export_to_path, scan_to_path, ScanOptions};
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
