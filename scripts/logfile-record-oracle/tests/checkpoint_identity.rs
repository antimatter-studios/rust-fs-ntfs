//! CI-only independent identity controls for the unit regression's input.
use std::{fs, path::PathBuf, process::Command};

use sha2::{Digest, Sha256};

#[test]
fn checkpoint_selection_is_independent_and_lfs1_is_refused() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let evidence = root.join("tmp/logfile-record-oracle/identity-controls");
    fs::create_dir_all(&evidence).unwrap();
    let manifest = include_str!("../../../test-disks/logfile-record-checkpoint.tsv");
    let mut lines = manifest.lines();
    assert_eq!(
        lines.next().unwrap(),
        "capture\tsha256\toracle_revision\tlfs_major\tcheckpoint_lsn"
    );
    let identity: Vec<_> = lines
        .next()
        .expect("missing supported checkpoint identity")
        .split('\t')
        .collect();
    assert_eq!(identity.len(), 5);
    assert!(lines.next().is_none(), "ambiguous checkpoint manifest");
    fs::write(evidence.join("checkpoint-manifest.tsv"), manifest).unwrap();
    for (name, hash, supported) in [
        (identity[0], identity[1], true),
        (
            "windows-clean-logfile",
            "b5b8d7ed7dacde1c0a6a7304d8d13038706efedd78a066fc67f5c3c624ba8a54",
            false,
        ),
    ] {
        let capture = root.join(format!("test-disks/{name}.bin.gz"));
        assert!(capture.is_file(), "missing immutable Windows capture {}: restore it from git; producer: test-disks/capture-interrupted-logfile.ps1", capture.display());
        let output = Command::new("gzip")
            .arg("-dc")
            .arg(capture)
            .output()
            .unwrap();
        assert!(output.status.success(), "gzip: {:?}", output.stderr);
        let bytes = output.stdout;
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), hash);
        let raw = evidence.join(format!("{name}.bin"));
        fs::write(&raw, &bytes).unwrap();
        let selected = Command::new(env!("CARGO_BIN_EXE_logfile-record-oracle"))
            .arg("--select-checkpoint")
            .arg(&raw)
            .arg(hash)
            .output()
            .unwrap();
        fs::write(evidence.join(format!("{name}.tsv")), &selected.stdout).unwrap();
        fs::write(evidence.join(format!("{name}.stderr")), &selected.stderr).unwrap();
        assert_eq!(fs::read(&raw).unwrap(), bytes, "selection changed source");
        assert_eq!(selected.status.success(), supported, "{:?}", selected);
        if supported {
            let independent = ntfslog::parse(&bytes);
            let restart = independent.restart().expect("independent restart page");
            assert_eq!(restart.version.major, 2);
            let client = restart
                .area
                .clients
                .iter()
                .find(|c| c.name == "NTFS")
                .unwrap();
            assert_eq!(client.restart_lsn, 0x33e791);
            let records: Vec<_> = independent
                .records
                .iter()
                .filter(|r| r.lsn == client.restart_lsn)
                .collect();
            assert_eq!(records.len(), 1);
            assert!(matches!(records[0].body, ntfslog::Body::Checkpoint(_)));
            let report = String::from_utf8(selected.stdout).unwrap();
            let rows: Vec<_> = report.lines().collect();
            assert_eq!(rows.len(), 2);
            let cells: Vec<_> = rows[1].split('\t').collect();
            assert_eq!(cells[0], hash);
            assert_eq!(cells[1], "b88585646801ee2c1e2acf91a078da6ab57af778");
            assert_eq!(cells[2], "2");
            assert_eq!(cells[4], "0x33e791");
            assert_eq!(cells[1], identity[2]);
            assert_eq!(cells[2], identity[3]);
            assert_eq!(cells[4], identity[4]);
        } else {
            assert!(String::from_utf8_lossy(&selected.stderr).contains("requires LFS 2.x, found 1"));
        }
    }
}
