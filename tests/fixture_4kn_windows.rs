//! The Windows-authored 4096-byte logical sector fixture (#1150).
//!
//! `test-disks/build-windows-4kn-fixture.ps1` has Windows format an NTFS
//! volume on a disk presenting 4096-byte logical sectors, write the file set
//! of `ntfs-cli-read.img` to it, and copy the volume out to
//! `ntfs-4kn-read.img`. Beside the image it leaves
//! `ntfs-4kn-read.ntfsinfo.txt`, `fsutil fsinfo ntfsinfo` of the mounted
//! volume bound to the image by its SHA-256, and `ntfs-4kn-read.sha256.tsv`,
//! each file's path, size and `Get-FileHash` SHA-256.
//!
//! Windows' own account, not this crate's reader, says the sectors are 4096
//! bytes. The image is then held to it: its boot sector is read by the
//! published offsets alone and must report the sector size, cluster size
//! and `$MFT` cluster fsutil reported for the mounted volume.
#![cfg(windows)]

use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};

const IMAGE: &str = "test-disks/ntfs-4kn-read.img";
const RECORD: &str = "test-disks/ntfs-4kn-read.ntfsinfo.txt";
const HASHES: &str = "test-disks/ntfs-4kn-read.sha256.tsv";
const PROVIDER: &str = "test-disks/build-windows-4kn-fixture.ps1";
const CLI_READ_HASHES: &str = "test-disks/ntfs-cli-read.sha256.tsv";
const CLI_READ_PROVIDER: &str = "test-disks/build-windows-native-read-fixtures.ps1";
const SECTOR: u64 = 4096;

fn read(path: &str, provider: &str) -> Vec<u8> {
    std::fs::read(path)
        .unwrap_or_else(|e| panic!("{path}: {e}; {provider} builds it on an elevated Windows host"))
}

fn text(path: &str, provider: &str) -> String {
    String::from_utf8(read(path, provider)).unwrap_or_else(|e| panic!("{path} is not UTF-8: {e}"))
}

/// `key : value` lines of the record, keys as fsutil prints them.
fn record_fields(text: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = value.split_whitespace().next() else {
            continue;
        };
        fields
            .entry(key.trim().to_string())
            .or_insert_with(|| value.to_string());
    }
    fields
}

fn number(fields: &HashMap<String, String>, key: &str) -> u64 {
    let raw = fields
        .get(key)
        .unwrap_or_else(|| panic!("{RECORD} has no '{key}' line"))
        .replace(',', "");
    let parsed = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => raw.parse(),
    };
    parsed.unwrap_or_else(|e| panic!("{RECORD}: '{key}' is {raw:?}: {e}"))
}

/// The (path, size) of every line of a `.sha256.tsv`, each line
/// `/path<TAB>size<TAB>sha256`.
fn file_set(path: &str, text: &str) -> BTreeSet<(String, u64)> {
    let mut set = BTreeSet::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let columns: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            columns.len(),
            3,
            "{path}: {line:?} is not path, size and sha256"
        );
        assert!(
            columns[0].starts_with('/'),
            "{path}: {:?} is not absolute",
            columns[0]
        );
        assert!(
            columns[2].len() == 64 && columns[2].bytes().all(|b| b.is_ascii_hexdigit()),
            "{path}: {:?} is not a SHA-256",
            columns[2]
        );
        let size = columns[1]
            .parse()
            .unwrap_or_else(|e| panic!("{path}: size {:?}: {e}", columns[1]));
        assert!(
            set.insert((columns[0].to_string(), size)),
            "{path}: {line:?} twice"
        );
    }
    set
}

fn le16(b: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u64
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[test]
fn windows_formats_the_4kn_fixture_in_4096_byte_sectors() {
    let image = read(IMAGE, PROVIDER);
    let fields = record_fields(&text(RECORD, PROVIDER));

    let digest: String = Sha256::digest(&image)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        fields.get("image sha256").map(String::as_str),
        Some(digest.as_str()),
        "{RECORD} does not describe this {IMAGE}"
    );
    assert_eq!(
        number(&fields, "Bytes Per Sector"),
        SECTOR,
        "fsutil's Bytes Per Sector for the volume Windows formatted"
    );
    assert!(
        !image.is_empty() && image.len() as u64 % SECTOR == 0,
        "{IMAGE} is {} bytes, not a whole number of 4096-byte sectors",
        image.len()
    );

    // The boot sector by its published offsets: OEM ID at 3, bytes per
    // sector at 11, sectors per cluster at 13, total sectors at 40, the
    // $MFT cluster at 48, and the 0x55 0xAA signature at 510.
    let boot = &image[..512];
    assert_eq!(
        &boot[3..11],
        b"NTFS    ",
        "{IMAGE} does not start with an NTFS boot sector"
    );
    assert_eq!(
        &boot[510..512],
        b"\x55\xAA",
        "{IMAGE}'s boot sector signature"
    );
    let sector = le16(boot, 11);
    assert_eq!(
        sector,
        number(&fields, "Bytes Per Sector"),
        "the boot sector's bytes per sector against fsutil's"
    );
    // A sectors-per-cluster byte above 0x80 is a negative power of two.
    let per_cluster = boot[13];
    let cluster = if per_cluster <= 0x80 {
        sector * per_cluster as u64
    } else {
        sector << (256 - per_cluster as u32)
    };
    assert_eq!(
        cluster,
        number(&fields, "Bytes Per Cluster"),
        "the boot sector's cluster size against fsutil's"
    );
    assert_eq!(
        le64(boot, 48),
        number(&fields, "Mft Start Lcn"),
        "the boot sector's $MFT cluster against fsutil's"
    );
    let sectors = le64(boot, 40);
    assert!(
        sectors > 0 && sectors * sector <= image.len() as u64,
        "the boot sector's {sectors} sectors do not fit in {IMAGE}"
    );

    // Every file Windows wrote, with the file set of ntfs-cli-read.img.
    let files = file_set(HASHES, &text(HASHES, PROVIDER));
    assert!(!files.is_empty(), "{HASHES} records no file");
    let cli_read = file_set(CLI_READ_HASHES, &text(CLI_READ_HASHES, CLI_READ_PROVIDER));
    assert_eq!(
        files, cli_read,
        "{HASHES} does not hold the file set of {CLI_READ_HASHES}"
    );
}
