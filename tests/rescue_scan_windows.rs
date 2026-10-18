//! The rescue scan's per-class byte totals (#970), graded against the
//! cluster counts Windows reported for a volume it formatted and wrote.
//!
//! `test-disks/build-windows-native-read-fixtures.ps1` leaves
//! `ntfs-cli-read.img` and `ntfs-cli-read.ntfsinfo.txt`, which is
//! `fsutil fsinfo ntfsinfo` of that volume while Windows had it mounted,
//! bound to the image by its SHA-256. The scan's archive is read by the
//! layout `docs/rescue-archive.md` documents, with every CRC-32 checked by
//! `crc32fast`, and its records are summed by class:
//!
//! - metadata and allocated bytes inside the clusters equal
//!   `(Total Clusters - Free Clusters) x Bytes Per Cluster`;
//! - free bytes equal `Free Clusters x Bytes Per Cluster`;
//! - every byte past `Total Clusters x Bytes Per Cluster` is reported apart,
//!   as the tail (or the backup boot sector the scan keeps as metadata).
#![cfg(windows)]

mod common;

use fs_ntfs::rescue::{scan_to_path, RangeClass, ScanOptions};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};

const IMAGE: &str = "test-disks/ntfs-cli-read.img";
const RECORD: &str = "test-disks/ntfs-cli-read.ntfsinfo.txt";
const PROVIDER: &str = "test-disks/build-windows-native-read-fixtures.ps1";

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().unwrap())
}

fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().unwrap())
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

fn file_sha256(path: &str) -> String {
    let mut f = File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).expect("hash the image");
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Bytes the archive's data records hold, by class code, split at `boundary`
/// into (inside the clusters, past them). Every CRC-32 is checked.
fn class_bytes(archive: &str, boundary: u64) -> [(u64, u64); 5] {
    let mut r = BufReader::new(File::open(archive).expect("open the archive"));
    let mut head = [0u8; 32];
    r.read_exact(&mut head).expect("the archive header");
    assert_eq!(&head[0..8], b"NTRESCUE", "the archive's magic");
    assert_eq!(le32(&head[8..12]), 2, "the archive's version");
    assert_eq!(
        le32(&head[28..32]),
        crc32fast::hash(&head[..28]),
        "the header's CRC-32"
    );
    let mut totals = [(0u64, 0u64); 5];
    let mut payload = Vec::new();
    loop {
        let mut rh = [0u8; 28];
        match r.read(&mut rh[..1]).expect("read the archive") {
            0 => break,
            _ => r.read_exact(&mut rh[1..]).expect("a record header"),
        }
        let (offset, len, kind) = (le64(&rh[0..8]), le64(&rh[8..16]), le32(&rh[16..20]));
        let class = le32(&rh[20..24]);
        assert_eq!(kind, 1, "the record at {offset}+{len} is not data");
        assert!(class <= 4, "the record at {offset} has class {class}");
        payload.resize(len as usize, 0);
        r.read_exact(&mut payload).expect("a data record's payload");
        let mut crc = crc32fast::Hasher::new();
        crc.update(&rh[..24]);
        crc.update(&payload);
        assert_eq!(
            le32(&rh[24..28]),
            crc.finalize(),
            "the CRC-32 of the record at {offset}+{len}"
        );
        let end = offset + len;
        let inside = end.min(boundary).saturating_sub(offset);
        totals[class as usize].0 += inside;
        totals[class as usize].1 += len - inside;
    }
    totals
}

#[test]
fn the_scans_class_totals_match_windows_cluster_counts() {
    let text = std::fs::read_to_string(RECORD)
        .unwrap_or_else(|e| panic!("{RECORD}: {e}; run {PROVIDER} to make it"));
    let fields = record_fields(&text);
    let digest = fields
        .get("image sha256")
        .unwrap_or_else(|| panic!("{RECORD} has no 'image sha256' line"));
    assert_eq!(
        &file_sha256(IMAGE),
        digest,
        "{IMAGE} is not the image {RECORD} describes"
    );
    let cluster = number(&fields, "Bytes Per Cluster");
    let total = number(&fields, "Total Clusters");
    let free = number(&fields, "Free Clusters");
    let boundary = total * cluster;
    let size = std::fs::metadata(IMAGE).expect("stat the image").len();
    assert!(
        boundary <= size,
        "Total Clusters x Bytes Per Cluster ({boundary}) is past the image's {size} bytes"
    );

    let archive = common::temp_image_path("rescue_windows_counts");
    let report = scan_to_path(IMAGE.as_ref(), archive.as_ref(), ScanOptions::default())
        .expect("scan the Windows image");
    assert!(report.is_complete(), "the scan lost bytes: {report:?}");
    let totals = class_bytes(&archive, boundary);
    std::fs::remove_file(&archive).ok();

    let of = |c: RangeClass| totals[c.code() as usize];
    assert_eq!(
        of(RangeClass::Unclassified),
        (0, 0),
        "the scan classified none of the image as NTFS"
    );
    let allocated = of(RangeClass::Metadata).0 + of(RangeClass::Allocated).0;
    assert_eq!(
        allocated,
        (total - free) * cluster,
        "Total Clusters - Free Clusters: the scan's metadata and allocated bytes \
         ({} metadata + {} allocated) disagree with Windows' {total} - {free} clusters of {cluster}",
        of(RangeClass::Metadata).0,
        of(RangeClass::Allocated).0,
    );
    assert_eq!(
        of(RangeClass::Free).0,
        free * cluster,
        "Free Clusters: the scan's free bytes disagree with Windows' {free} clusters of {cluster}"
    );
    for class in [RangeClass::Allocated, RangeClass::Free] {
        assert_eq!(
            of(class).1,
            0,
            "Total Clusters: the scan put {class:?} bytes past Windows' {total} clusters"
        );
    }
    assert_eq!(
        of(RangeClass::Tail).0,
        0,
        "Total Clusters: the scan's tail starts inside Windows' {total} clusters"
    );
    let past = of(RangeClass::Metadata).1 + of(RangeClass::Tail).1;
    assert_eq!(
        past,
        size - boundary,
        "the tail: the scan reports {past} bytes past the last whole cluster of {size}"
    );
}
