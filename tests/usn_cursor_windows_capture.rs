//! Admits cursors against the change journal Windows reports through
//! FSCTL_QUERY_USN_JOURNAL on the runner's own volume (#609).
//!
//! The capture is made by test-disks/capture-usn-journal-data.ps1 in the
//! Windows CI job. A missing capture fails, naming that script.
#![cfg(windows)]

use fs_ntfs::usn_cursor::{admit_cursor, CursorAdmission, UsnCursor, UsnJournalData};

fn captured() -> UsnJournalData {
    let tsv = std::fs::read_to_string("test-disks/usn-journal-data.tsv")
        .expect("test-disks/usn-journal-data.tsv: run test-disks/capture-usn-journal-data.ps1");
    let field = |name: &str| -> u64 {
        let line = tsv
            .lines()
            .find(|l| l.split('\t').next() == Some(name))
            .unwrap_or_else(|| panic!("capture has no {name} row"));
        let hex = line.split('\t').nth(1).unwrap().trim();
        u64::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap()
    };
    UsnJournalData {
        journal_id: field("journal_id"),
        first_usn: field("first_usn") as i64,
        next_usn: field("next_usn") as i64,
        lowest_valid_usn: field("lowest_valid_usn") as i64,
        max_usn: field("max_usn") as i64,
        maximum_size: field("maximum_size"),
        allocation_delta: field("allocation_delta"),
    }
}

#[test]
fn windows_journal_data_satisfies_the_documented_bounds() {
    let j = captured();
    assert_eq!(j.bound_violation(), None, "{j:?}");
    assert_ne!(j.journal_id, 0, "{j:?}");
}

#[test]
fn windows_journal_admits_cursors_by_its_own_bounds() {
    let j = captured();
    let at = |usn| UsnCursor {
        journal_id: j.journal_id,
        next_usn: usn,
    };
    assert_eq!(
        admit_cursor(&at(j.next_usn), &j),
        CursorAdmission::UpToDate {
            next_usn: j.next_usn
        }
    );
    if j.first_usn < j.next_usn {
        assert_eq!(
            admit_cursor(&at(j.first_usn), &j),
            CursorAdmission::Admitted {
                from: j.first_usn,
                to: j.next_usn
            }
        );
    }
    assert_eq!(
        admit_cursor(&at(j.next_usn + 8), &j),
        CursorAdmission::Future {
            cursor_usn: j.next_usn + 8,
            next_usn: j.next_usn
        }
    );
    let other = UsnCursor {
        journal_id: j.journal_id ^ 1,
        next_usn: j.next_usn,
    };
    assert_eq!(
        admit_cursor(&other, &j),
        CursorAdmission::JournalReset {
            cursor_id: j.journal_id ^ 1,
            journal_id: j.journal_id
        }
    );
    if j.first_usn > j.lowest_valid_usn {
        assert!(matches!(
            admit_cursor(&at(j.first_usn - 1), &j),
            CursorAdmission::Wrapped { .. }
        ));
    }
}
