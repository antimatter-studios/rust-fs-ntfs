//! CI-only regression: evidence must include a genuinely nonzero Windows link.

use std::{fs, path::PathBuf, process::Command};

use sha2::{Digest, Sha256};

#[test]
fn immutable_windows_nonzero_links_are_in_the_comparison_report() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let evidence = root.join("tmp/logfile-nonzero-links");
    fs::create_dir_all(&evidence).expect("create CI evidence directory");
    let capture = root.join("test-disks/windows-interrupted-logfile-1.bin.gz");
    let extracted = Command::new("gzip")
        .arg("-dc")
        .arg(&capture)
        .output()
        .expect("install gzip to read the immutable Windows capture");
    assert!(extracted.status.success(), "gzip failed: {extracted:?}");
    let bytes = extracted.stdout;
    let sha = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(
        sha,
        "e6a36b92ace6fa15378af53d9239b1df0aacdf931e4c721d10ce287259d638eb"
    );
    let decoded = ntfslog::parse(&bytes);
    let inventory = decoded
        .records
        .iter()
        .map(|record| {
            format!(
                "{sha}\tb88585646801ee2c1e2acf91a078da6ab57af778\t{:#x}\t{:#x}\t{:#x}\n",
                record.lsn, record.previous_lsn, record.undo_next_lsn
            )
        })
        .collect::<String>();
    fs::write(evidence.join("independent.tsv"), inventory).expect("retain independent links");
    fs::write(
        evidence.join("problems.txt"),
        format!("{:#?}\n", decoded.problems),
    )
    .expect("retain independent decoder limitations");
    // Only the live log is eligible: a record, and every record it links to,
    // no older than the independent NTFS client's oldest LSN. Older records
    // survive only in stale page copies from earlier passes of the log.
    let oldest = decoded
        .restart()
        .and_then(|restart| {
            restart
                .area
                .clients
                .iter()
                .find(|client| client.name == "NTFS")
        })
        .expect("fixture blocker: pinned decoder found no NTFS client in the restart area")
        .oldest_lsn;
    let live = |lsn: u64| lsn >= oldest;
    let selected = decoded
        .records
        .iter()
        .find(|record| {
            let links = [record.previous_lsn, record.undo_next_lsn];
            live(record.lsn) && links != [0, 0] && links.into_iter().all(|l| l == 0 || live(l))
        })
        .expect(
            "fixture blocker: pinned decoder found no live nonzero link in immutable Windows capture 1",
        );
    assert_eq!(
        decoded
            .records
            .iter()
            .filter(|r| r.lsn == selected.lsn)
            .count(),
        1,
        "independent nonzero record identity is ambiguous"
    );
    for link in [selected.previous_lsn, selected.undo_next_lsn] {
        if link != 0 {
            assert_eq!(
                decoded.records.iter().filter(|r| r.lsn == link).count(),
                1,
                "independent link target {link:#x} of {:#x} is missing or ambiguous",
                selected.lsn
            );
        }
    }
    let raw = evidence.join("windows-interrupted-1.bin");
    fs::write(&raw, &bytes).expect("retain original Windows bytes for helper");
    let output = Command::new(env!("CARGO_BIN_EXE_logfile-record-oracle"))
        .arg(&raw)
        .arg(&sha)
        .output()
        .expect("run comparison helper in CI");
    fs::write(evidence.join("comparisons.tsv"), &output.stdout).expect("retain comparisons");
    fs::write(evidence.join("helper.stderr"), &output.stderr).expect("retain helper refusal");
    assert_eq!(
        fs::read(&raw).expect("reread capture"),
        bytes,
        "capture changed"
    );
    assert!(
        output.status.success(),
        "helper refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).expect("UTF-8 comparison report");
    for (field, expected) in [
        ("client_previous_lsn", selected.previous_lsn),
        ("client_undo_next_lsn", selected.undo_next_lsn),
    ] {
        let suffix = format!(
            "\t{:#x}\t{field}\t{expected:#x}\t{expected:#x}",
            selected.lsn
        );
        assert!(
            report.lines().any(|line| line.ends_with(&suffix)),
            "missing independent nonzero-record comparison: {suffix}"
        );
    }
}
