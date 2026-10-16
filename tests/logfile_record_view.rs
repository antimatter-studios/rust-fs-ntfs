//! Narrow independent evidence: Windows wrote each restart area's client
//! restart LSN. Compare that value with the record found there, including a
//! checkpoint present only in a tail copy. This does NOT independently grade
//! the other RecordView fields or establish replay eligibility (#607).
//! Capture commands, Windows CI runs and extraction provenance are recorded
//! in tests/logfile_state.rs; no independent decoder source is used here.

use std::path::Path;
use std::process::Command;

use fs_ntfs::logfile_replay::record_at_lsn;
use sha2::{Digest, Sha256};

const CAPTURES: [(&str, &str, u64); 3] = [
    (
        "windows-clean-logfile",
        "b5b8d7ed7dacde1c0a6a7304d8d13038706efedd78a066fc67f5c3c624ba8a54",
        0x184415,
    ),
    (
        "windows-interrupted-logfile-1",
        "e6a36b92ace6fa15378af53d9239b1df0aacdf931e4c721d10ce287259d638eb",
        0x33e791,
    ),
    (
        "windows-interrupted-logfile-5",
        "fa15e8340582367d5c36f7e75e9e3de19d5c35580d1091cc72e85054967ad1ec",
        0xcac252,
    ),
];

fn capture(name: &str, sha: &str) -> Vec<u8> {
    let path = format!("test-disks/{name}.bin.gz");
    assert!(
        Path::new(&path).is_file(),
        "missing immutable Windows capture {path}: restore it from git; producer: test-disks/capture-interrupted-logfile.ps1"
    );
    let out = Command::new("gzip")
        .args(["-dc", &path])
        .output()
        .expect("install gzip to read the Windows captures");
    assert!(out.status.success(), "gzip -dc {path}: {:?}", out.stderr);
    assert_eq!(format!("{:x}", Sha256::digest(&out.stdout)), sha, "{path}");
    out.stdout
}

#[test]
fn record_lsn_agrees_with_windows_written_restart_lsn_without_changing_source() {
    // The independent selector validates this same manifest and capture in CI.
    let mut manifest = include_str!("../test-disks/logfile-record-checkpoint.tsv").lines();
    assert_eq!(
        manifest.next().unwrap(),
        "capture\tsha256\toracle_revision\tlfs_major\tcheckpoint_lsn"
    );
    let row: Vec<_> = manifest
        .next()
        .expect("missing supported checkpoint identity")
        .split('\t')
        .collect();
    assert_eq!(row.len(), 5);
    assert!(manifest.next().is_none(), "ambiguous checkpoint manifest");
    assert_eq!(row[2], "b88585646801ee2c1e2acf91a078da6ab57af778");
    assert_eq!(row[3], "2");
    let lsn = u64::from_str_radix(row[4].strip_prefix("0x").unwrap(), 16).unwrap();
    assert_eq!((row[0], row[1], lsn), CAPTURES[1]);
    // Keep the original second supported capture and both identity assertions.
    for (name, sha, restart_lsn) in [(row[0], row[1], lsn), CAPTURES[2]] {
        let log = capture(name, sha);
        let before = log.clone();
        let view = record_at_lsn(&log, restart_lsn, log.len())
            .unwrap_or_else(|e| panic!("{name}: Windows restart LSN {restart_lsn:#x}: {e}"));
        assert_eq!(view.lsn, restart_lsn, "{name}");
        assert_eq!(log, before, "inspection changed {name}");
    }
}

#[test]
fn original_windows_lfs1_checkpoint_is_strictly_refused_without_changing_source() {
    let (name, sha, restart_lsn) = CAPTURES[0];
    let log = capture(name, sha);
    let before = log.clone();
    let error = record_at_lsn(&log, restart_lsn, log.len()).unwrap_err();
    assert!(
        error.to_string().contains("LFS version 1"),
        "{name}: {error}"
    );
    assert_eq!(log, before, "failed inspection changed {name}");
}

#[test]
fn invalid_lsns_short_logs_and_exhausted_record_budgets_are_refused() {
    for (name, sha, restart_lsn) in CAPTURES {
        let log = capture(name, sha);
        let before = log.clone();
        // Zero, a restart-page address, a page header, a shifted record and
        // an impossible sequence number must not resolve to another record.
        for lsn in [0, 1, restart_lsn & !0x1ff, restart_lsn + 1, u64::MAX] {
            assert!(
                record_at_lsn(&log, lsn, log.len()).is_err(),
                "{name}: {lsn:#x}"
            );
        }
        assert!(record_at_lsn(&log, restart_lsn, 0).is_err(), "{name}");
        assert_eq!(log, before, "failed inspection changed {name}");
    }
    for len in [0, 16, 512, 4096] {
        assert!(record_at_lsn(&vec![0; len], 0, 1024).is_err());
    }
}
