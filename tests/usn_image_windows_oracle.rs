//! An image's change journal reads as Windows reported it (#609).
//!
//! `usn_image` reads `$Extend\$UsnJrnl` through this crate's own parsers,
//! with no Windows API: `$Max` for the journal's identity and sizes, `$J`'s
//! size and first allocated cluster for `NextUsn` and `FirstUsn`, and `$J`
//! itself, in bounded batches, for the records.
//!
//! Graded by Windows: `test-disks/ntfs-usnjrnl.img` is a volume whose journal
//! Windows created and wrote to, `ntfs-usnjrnl.query.txt` is what `fsutil usn
//! queryjournal` reported of it and `ntfs-usnjrnl.records.csv` every record
//! `fsutil usn readjournal` read back, all from
//! `test-disks/build-windows-native-read-fixtures.ps1`.

mod common;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::usn_cursor::{CursorAdmission, UsnCursor};
use fs_ntfs::usn_image::{
    journal_state_io, read_records_io, ImageJournal, JournalReadError, JournalState,
};
use sha2::{Digest, Sha256};
use std::path::Path;

const IMG: &str = "test-disks/ntfs-usnjrnl.img";
const QUERY: &str = "test-disks/ntfs-usnjrnl.query.txt";
const RECORDS: &str = "test-disks/ntfs-usnjrnl.records.csv";

fn require_fixtures() {
    for path in [IMG, QUERY, RECORDS] {
        assert!(
            Path::new(path).exists(),
            "missing {path}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
        );
    }
}

/// The number `fsutil usn queryjournal` printed for `field`, as in
/// `Maximum Size     : 0x0000000000100000 (  1.0 MB)`.
fn fsutil_field(query: &str, field: &str) -> u64 {
    let line = query
        .lines()
        .find(|l| l.split(':').next().is_some_and(|k| k.trim() == field))
        .unwrap_or_else(|| panic!("{QUERY} has no {field:?}:\n{query}"));
    let value = line.split(':').nth(1).unwrap().split_whitespace().next();
    let hex = value.and_then(|v| v.strip_prefix("0x"));
    u64::from_str_radix(hex.unwrap_or_else(|| panic!("{QUERY}: {line:?}")), 16)
        .unwrap_or_else(|e| panic!("{QUERY}: {line:?}: {e}"))
}

/// One record as `fsutil usn readjournal` listed it.
#[derive(Debug, PartialEq)]
struct Listed {
    usn: i64,
    name: String,
    reason: u128,
    attributes: u128,
    file: u128,
    parent: u128,
}

fn listed_records() -> Vec<Listed> {
    let csv = std::fs::read_to_string(RECORDS).expect("read the records");
    let mut lines = csv
        .lines()
        .skip_while(|l| !l.starts_with("Usn,"))
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            l.split(',')
                .map(|f| f.trim_matches('"').to_string())
                .collect::<Vec<_>>()
        });
    let header = lines.next().expect("the records' header");
    let column = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("{RECORDS} has no column {name:?}"))
    };
    let (c_usn, c_name, c_reason, c_attrs, c_id, c_parent) = (
        column("Usn"),
        column("File name"),
        column("Reason #"),
        column("File attributes #"),
        column("File ID"),
        column("Parent file ID"),
    );
    let hex = |s: &str| u128::from_str_radix(s.trim_start_matches("0x"), 16).expect(s);
    lines
        .map(|row| {
            assert!(
                row.len() > c_parent.max(c_name) && row.len() <= header.len(),
                "{RECORDS}: malformed record: {row:?}"
            );
            Listed {
                usn: row[c_usn].parse().expect("a USN"),
                name: row[c_name].clone(),
                reason: hex(&row[c_reason]),
                attributes: hex(&row[c_attrs]),
                file: hex(&row[c_id]),
                parent: hex(&row[c_parent]),
            }
        })
        .collect()
}

fn image_sha256() -> [u8; 32] {
    Sha256::digest(std::fs::read(IMG).expect("read the fixture")).into()
}

fn active_journal(io: &mut PathIo) -> ImageJournal {
    match journal_state_io(io).expect("journal state") {
        JournalState::Active(journal) => journal,
        other => panic!("the fixture's journal is {other:?}"),
    }
}

#[test]
fn the_journal_state_is_what_fsutil_queried() {
    require_fixtures();
    let query = std::fs::read_to_string(QUERY).expect("read the query");
    let before = image_sha256();
    let mut io = PathIo::open_ro(Path::new(IMG)).expect("open the fixture");
    let journal = active_journal(&mut io);
    let field = |name: &str| fsutil_field(&query, name);

    assert_eq!(journal.journal_id, field("Usn Journal ID"), "journal ID");
    assert_eq!(journal.first_usn as u64, field("First Usn"), "FirstUsn");
    assert_eq!(journal.next_usn as u64, field("Next Usn"), "NextUsn");
    assert_eq!(
        journal.lowest_valid_usn as u64,
        field("Lowest Valid Usn"),
        "LowestValidUsn"
    );
    assert_eq!(journal.maximum_size, field("Maximum Size"), "MaximumSize");
    assert_eq!(
        journal.allocation_delta,
        field("Allocation Delta"),
        "AllocationDelta"
    );
    assert_eq!(
        journal.journal_data().bound_violation(),
        None,
        "{journal:?} breaks a documented bound"
    );
    drop(io);
    assert_eq!(
        image_sha256(),
        before,
        "reading the journal changed the image"
    );
}

#[test]
fn bounded_batches_read_every_record_fsutil_read() {
    require_fixtures();
    let listed = listed_records();
    assert!(
        listed.len() > 20,
        "only {} records in {RECORDS}",
        listed.len()
    );
    let mut io = PathIo::open_ro(Path::new(IMG)).expect("open the fixture");
    let journal = active_journal(&mut io);

    const BOUND: usize = 512;
    let mut cursor = journal.first_cursor();
    let mut read = Vec::new();
    loop {
        let batch = read_records_io(&mut io, &journal, cursor, BOUND)
            .unwrap_or_else(|e| panic!("batch from {cursor:?}: {e}"));
        assert_eq!(batch.next.journal_id, journal.journal_id);
        let examined = batch.next.next_usn - cursor.next_usn;
        assert!(
            examined > 0 || batch.finished,
            "a batch from {cursor:?} examined nothing and did not finish"
        );
        assert!(
            examined <= BOUND as i64,
            "a batch from {cursor:?} examined {examined} bytes, over {BOUND}"
        );
        read.extend(batch.records);
        cursor = batch.next;
        if batch.finished {
            break;
        }
    }
    assert_eq!(
        cursor.next_usn, journal.next_usn,
        "the batches stopped early"
    );

    let decoded: Vec<Listed> = read
        .iter()
        .map(|r| Listed {
            usn: r.usn,
            name: r.file_name_lossy(),
            reason: r.reason.into(),
            attributes: r.file_attributes.into(),
            file: r.file_reference.into(),
            parent: r.parent_file_reference.into(),
        })
        .collect();
    assert_eq!(decoded, listed, "the records differ from fsutil's");

    // One unbounded batch reads the same records the bounded ones did.
    let whole = read_records_io(
        &mut io,
        &journal,
        journal.first_cursor(),
        journal.next_usn as usize,
    )
    .expect("one batch");
    assert!(whole.finished);
    assert_eq!(whole.records, read, "resumed batches differ from one batch");
}

#[test]
fn cursors_the_journal_cannot_answer_are_refused() {
    require_fixtures();
    let mut io = PathIo::open_ro(Path::new(IMG)).expect("open the fixture");
    let journal = active_journal(&mut io);
    let at = |journal_id, next_usn| UsnCursor {
        journal_id,
        next_usn,
    };

    let other = at(journal.journal_id ^ 1, journal.first_usn);
    match read_records_io(&mut io, &journal, other, 4096) {
        Err(JournalReadError::NotAdmitted(CursorAdmission::JournalReset { .. })) => {}
        other => panic!("another journal's cursor: {other:?}"),
    }
    let future = at(journal.journal_id, journal.next_usn + 8);
    match read_records_io(&mut io, &journal, future, 4096) {
        Err(JournalReadError::NotAdmitted(CursorAdmission::Future { .. })) => {}
        other => panic!("a cursor past NextUsn: {other:?}"),
    }

    // At NextUsn there is nothing new, and that is not a refusal.
    let current = at(journal.journal_id, journal.next_usn);
    let batch = read_records_io(&mut io, &journal, current, 4096).expect("an up-to-date cursor");
    assert!(batch.records.is_empty() && batch.finished);
    assert_eq!(batch.next, current);

    // A bound smaller than the first record cannot make progress.
    match read_records_io(&mut io, &journal, journal.first_cursor(), 16) {
        Err(JournalReadError::BoundTooSmall { .. }) => {}
        other => panic!("a 16-byte bound: {other:?}"),
    }

    // A cursor inside a record is not where any record starts.
    let inside = at(journal.journal_id, journal.first_usn + 8);
    match read_records_io(&mut io, &journal, inside, 4096) {
        Err(JournalReadError::NotAdmitted(_)) | Ok(_) => {
            panic!("a cursor inside a record was read or not admitted")
        }
        Err(_) => {}
    }
}

#[test]
fn a_volume_with_no_journal_reports_none() {
    const SIZE: u64 = 32 * 1024 * 1024;
    let img = common::temp_image_path("usn_image_none");
    let f = std::fs::File::create(&img).expect("create image");
    f.set_len(SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, SIZE, 4096, 1024, Some("NOUSN"), None).expect("format");
    drop(io);
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
    assert_eq!(
        journal_state_io(&mut io).expect("journal state"),
        JournalState::Absent
    );
    drop(io);
    let _ = std::fs::remove_file(&img);
}
