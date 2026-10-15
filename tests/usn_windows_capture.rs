//! Decodes USN_RECORD_V2 records Windows wrote, and checks them against
//! `fsutil`'s report of the same file (#609).
//!
//! The capture is made by test-disks/capture-usn-v2-records.ps1 in the
//! Windows CI job. A missing capture fails, naming that script.
#![cfg(windows)]

use fs_ntfs::usn::decode_usn_record_v2;

#[test]
fn windows_usn_v2_records_match_fsutil() {
    let bin = std::fs::read("test-disks/usn-v2-capture.bin")
        .expect("test-disks/usn-v2-capture.bin: run test-disks/capture-usn-v2-records.ps1");
    let manifest = std::fs::read_to_string("test-disks/usn-v2-capture.tsv")
        .expect("test-disks/usn-v2-capture.tsv: run test-disks/capture-usn-v2-records.ps1");
    let mut rows = 0;
    for line in manifest.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        let offset: usize = f[0].parse().unwrap();
        let length: u32 = f[1].parse().unwrap();
        let file_id = u64::from_str_radix(f[2].trim_start_matches("0x"), 16).unwrap();
        let rec = decode_usn_record_v2(&bin[offset..]).unwrap();
        assert_eq!(rec.record_length, length, "row {line}");
        assert_eq!(rec.file_reference, file_id, "row {line}");
        assert_eq!(rec.file_name_lossy(), f[3], "row {line}");
        assert_ne!(rec.reason, 0, "row {line}");
        rows += 1;
    }
    assert!(rows > 0, "the capture manifest names no records");
}
