//! Grades cursor admission against what FSCTL_READ_USN_JOURNAL actually does
//! on the runner's own change journal (#609).
//!
//! test-disks/probe-usn-read-cursors.ps1 reads the journal, never writes it,
//! at three cursors: the current `NextUsn`, a USN beyond it, and `NextUsn`
//! under a journal ID that is not this journal's. Each row records whether
//! Windows answered, its `GetLastError` code, and the next USN it returned.
//! Admission may be stricter than Windows; it must never admit a cursor
//! Windows refuses. A missing probe fails, naming that script.
#![cfg(windows)]

use fs_ntfs::usn_cursor::{admit_cursor, CursorAdmission, UsnCursor, UsnJournalData};
use sha2::{Digest, Sha256};

const PROBE: &str = "test-disks/usn-read-probe.tsv";
const JOURNAL: &str = "test-disks/usn-journal-data.tsv";

struct Probe {
    name: String,
    cursor: UsnCursor,
    ok: bool,
    error: u32,
    returned_next_usn: Option<i64>,
}

fn hex(s: &str) -> u64 {
    u64::from_str_radix(s.trim().trim_start_matches("0x"), 16)
        .unwrap_or_else(|e| panic!("{s:?}: {e}"))
}

fn rows(text: &str, kind: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter(|l| l.split('\t').next() == Some(kind))
        .map(|l| l.split('\t').skip(1).map(str::to_string).collect())
        .collect()
}

fn probe() -> (String, UsnJournalData, Vec<Probe>) {
    let tsv = std::fs::read_to_string(PROBE)
        .unwrap_or_else(|_| panic!("{PROBE}: run test-disks/probe-usn-read-cursors.ps1"));
    let sha = rows(&tsv, "journal_capture_sha256")
        .first()
        .map(|r| r[0].clone())
        .expect("probe names the journal capture's sha256");
    let field = |name: &str| -> u64 {
        let r = rows(&tsv, "journal");
        let row = r
            .iter()
            .find(|r| r[0] == name)
            .unwrap_or_else(|| panic!("probe has no journal {name} row"));
        hex(&row[1])
    };
    let journal = UsnJournalData {
        journal_id: field("journal_id"),
        first_usn: field("first_usn") as i64,
        next_usn: field("next_usn") as i64,
        lowest_valid_usn: field("lowest_valid_usn") as i64,
        max_usn: field("max_usn") as i64,
        maximum_size: field("maximum_size"),
        allocation_delta: field("allocation_delta"),
    };
    let probes = rows(&tsv, "probe")
        .into_iter()
        .inspect(|r| {
            // A shifted row reads as a plausible cursor; refuse its shape.
            assert_eq!(r.len(), 7, "probe row is not seven fields: {r:?}");
            assert!(
                r[3] == "0" || r[3] == "1",
                "probe row's ok is {}: {r:?}",
                r[3]
            );
        })
        .map(|r| Probe {
            name: r[0].clone(),
            cursor: UsnCursor {
                journal_id: hex(&r[1]),
                next_usn: hex(&r[2]) as i64,
            },
            ok: r[3] == "1",
            error: r[4].parse().unwrap(),
            returned_next_usn: (r[3] == "1").then(|| hex(&r[6]) as i64),
        })
        .collect();
    (sha, journal, probes)
}

#[test]
fn probe_reads_the_captured_journal() {
    let (sha, journal, probes) = probe();
    let capture = std::fs::read(JOURNAL)
        .unwrap_or_else(|_| panic!("{JOURNAL}: run test-disks/capture-usn-journal-data.ps1"));
    assert_eq!(sha, format!("{:x}", Sha256::digest(&capture)));
    let fsutil_id = String::from_utf8(capture)
        .unwrap()
        .lines()
        .find(|l| l.starts_with("journal_id\t"))
        .map(|l| hex(l.split('\t').nth(1).unwrap()))
        .expect("capture has a journal_id row");
    assert_eq!(journal.journal_id, fsutil_id, "probe and fsutil disagree");
    let names: Vec<_> = probes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["current", "future", "wrong_journal_id"]);
}

#[test]
fn admission_never_admits_a_cursor_windows_refuses() {
    let (_, journal, probes) = probe();
    for p in &probes {
        let admission = admit_cursor(&p.cursor, &journal);
        let admits = matches!(
            admission,
            CursorAdmission::Admitted { .. } | CursorAdmission::UpToDate { .. }
        );
        assert!(
            p.ok || !admits,
            "{}: Windows refused (error {}) but admission is {admission:?}",
            p.name,
            p.error
        );
        if let Some(next) = p.returned_next_usn {
            assert!(next >= 0, "{}: Windows returned next USN {next}", p.name);
        }
    }
}
