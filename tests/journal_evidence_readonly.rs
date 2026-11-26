//! Change journal and `$LogFile` evidence from a device that can only be
//! read (#1318, #1307).
//!
//! The device here implements `fs_core::BlockRead` and nothing else, so no
//! code path below can write it; its bytes are hashed before and after to
//! show that too.
//!
//! Graded by Windows:
//!
//! * `test-disks/ntfs-usnjrnl.img`, with `ntfs-usnjrnl.query.txt` and
//!   `ntfs-usnjrnl.records.csv`: a journal Windows created and wrote to, what
//!   `fsutil usn queryjournal` reported of it and every record `fsutil usn
//!   readjournal` read back (`test-disks/build-windows-native-read-fixtures.ps1`).
//! * The same journal with one record or its `$Max` stream damaged in memory:
//!   each damage is refused by name, after the undamaged image read cleanly.
//! * `test-disks/windows-interrupted-*.img.gz`: volumes Windows was writing
//!   to when they were captured, and `.recovered.manifest`, every file
//!   Windows listed after its own restart pass recovered a copy
//!   (`tests/windows_interrupted_fixture.rs` describes how they were made).
//!   The replay held in memory has to reach the same files, and the same
//!   bytes as the replay `fsck` writes to a copy, which that file grades
//!   against Windows' recovered metadata record for record.

mod common;

use fs_core::BlockRead;
use fs_ntfs::evidence::{
    logfile_state, read_usn_records, replay_logfile_in_memory, usn_journal_state, walk_logfile,
};
use fs_ntfs::facade::Filesystem;
use fs_ntfs::file::NtfsFileAPI;
use fs_ntfs::fsck;
use fs_ntfs::usn::{UsnDecodeError, UsnRecordV2};
use fs_ntfs::usn_cursor::{CursorAdmission, InvalidBound, UsnCursor};
use fs_ntfs::usn_image::{ImageJournal, JournalReadError, JournalState};
use fs_ntfs::volume::Volume;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

const USN_IMG: &str = "test-disks/ntfs-usnjrnl.img";
const USN_QUERY: &str = "test-disks/ntfs-usnjrnl.query.txt";
const USN_RECORDS: &str = "test-disks/ntfs-usnjrnl.records.csv";

/// The captures whose logs hold work, each a different shape of log.
const CAPTURES: [&str; 15] = [
    "1",
    "6",
    "wrap-span",
    "wrap-boundary",
    "wrap-mft-grows",
    "index-vcn",
    "small-cluster",
    "tail-continuation",
    "undo",
    "open-at-checkpoint",
    "quota",
    "fresh-log",
    "quota-spilled",
    "wrap-continuation",
    "freed",
];

/// A device that can only be read: the image file, read a span at a time.
type Image = common::PatchedImage;

/// Whether `device` reads, chunk for chunk, as the image file at `path`.
fn reads_as(device: &dyn BlockRead, path: &str) -> bool {
    let size = std::fs::metadata(path).unwrap().len();
    if device.size_bytes() != size {
        return false;
    }
    let mut chunk = vec![0; common::IMAGE_CHUNK_SIZE];
    let mut at = 0;
    while at < size {
        let count = (size - at).min(chunk.len() as u64) as usize;
        device.read_at(at, &mut chunk[..count]).unwrap();
        if chunk[..count] != common::read_image_region(path, at, count).unwrap()[..] {
            return false;
        }
        at += count as u64;
    }
    true
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The number `fsutil usn queryjournal` printed for `field`.
fn fsutil_field(query: &str, field: &str) -> u64 {
    let line = query
        .lines()
        .find(|l| l.split(':').next().is_some_and(|k| k.trim() == field))
        .unwrap_or_else(|| panic!("{USN_QUERY} has no {field:?}"));
    let value = line.split(':').nth(1).unwrap().split_whitespace().next();
    u64::from_str_radix(value.and_then(|v| v.strip_prefix("0x")).unwrap(), 16).unwrap()
}

/// fsutil's records: (USN, name, reason, attributes, file, parent).
fn fsutil_records() -> Vec<(i64, String, u128, u128, u128, u128)> {
    let csv = std::fs::read_to_string(USN_RECORDS).unwrap();
    let mut rows = csv
        .lines()
        .skip_while(|l| !l.starts_with("Usn,"))
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            l.split(',')
                .map(|f| f.trim_matches('"').to_string())
                .collect::<Vec<_>>()
        });
    let header = rows.next().unwrap();
    let column = |name: &str| header.iter().position(|h| h == name).unwrap();
    let columns = [
        column("Usn"),
        column("File name"),
        column("Reason #"),
        column("File attributes #"),
        column("File ID"),
        column("Parent file ID"),
    ];
    let hex = |s: &str| u128::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    rows.map(|row| {
        (
            row[columns[0]].parse().unwrap(),
            row[columns[1]].clone(),
            hex(&row[columns[2]]),
            hex(&row[columns[3]]),
            hex(&row[columns[4]]),
            hex(&row[columns[5]]),
        )
    })
    .collect()
}

#[test]
fn the_change_journal_reads_as_fsutil_reported_from_a_read_only_device() {
    for path in [USN_IMG, USN_QUERY, USN_RECORDS] {
        assert!(
            Path::new(path).exists(),
            "missing {path}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
        );
    }
    let before = common::image_digest(USN_IMG).unwrap();
    let device = Image::open(USN_IMG).unwrap();
    let query = std::fs::read_to_string(USN_QUERY).unwrap();

    let JournalState::Active(journal) = usn_journal_state(&device).unwrap() else {
        panic!("the fixture's journal is not active");
    };
    assert_eq!(journal.journal_id, fsutil_field(&query, "Usn Journal ID"));
    assert_eq!(journal.first_usn as u64, fsutil_field(&query, "First Usn"));
    let queried_next = fsutil_field(&query, "Next Usn") as i64;
    assert!(journal.next_usn >= queried_next);

    let mut cursor = journal.first_cursor();
    let mut records = Vec::new();
    loop {
        let batch = read_usn_records(&device, &journal, cursor, 1024)
            .unwrap_or_else(|error| panic!("batch from {cursor:?}: {error}"));
        records.extend(batch.records);
        cursor = batch.next;
        if batch.finished {
            break;
        }
    }
    let ours: Vec<_> = records
        .iter()
        .take_while(|record| record.usn < queried_next)
        .map(|record| {
            (
                record.usn,
                record.file_name_lossy(),
                u128::from(record.reason),
                u128::from(record.file_attributes),
                u128::from(record.file_reference),
                u128::from(record.parent_file_reference),
            )
        })
        .collect();
    let windows = fsutil_records();
    assert!(
        windows.len() > 20,
        "only {} records from fsutil",
        windows.len()
    );
    assert_eq!(ours, windows);
    assert_eq!(
        common::image_digest(USN_IMG).unwrap(),
        before,
        "the device changed"
    );
}

/// The capture, decompressed to a scratch file, never held whole.
fn unpack(name: &str) -> String {
    let path = common::temp_image_path(name.replace('.', "_"));
    let out = std::fs::File::create(&path).expect("create the image copy");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(std::process::Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        133_103_616,
        "{name} is the captured partition"
    );
    path
}

/// Windows' manifest: path -> (size, SHA-256).
fn manifest(capture: &str) -> BTreeMap<String, (u64, String)> {
    let path = format!("test-disks/windows-interrupted-{capture}.recovered.manifest");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (
                fields[0].to_owned(),
                (fields[1].parse().unwrap(), fields[2].to_owned()),
            )
        })
        .collect()
}

/// Every file under the workload's `dNNN` directories: path -> (size, SHA-256).
fn workload<D: BlockRead>(volume: &Volume<D>) -> BTreeMap<String, (u64, String)> {
    let mut files = BTreeMap::new();
    for directory in volume.read_directory("/").unwrap() {
        let name = &directory.name;
        let numbered = name.len() == 4
            && name.starts_with('d')
            && name[1..].bytes().all(|byte| byte.is_ascii_digit());
        if !numbered || !directory.is_dir {
            continue;
        }
        for entry in volume.read_directory(&format!("/{name}")).unwrap() {
            if entry.is_dir || entry.name == "." || entry.name == ".." {
                continue;
            }
            let path = format!("{name}/{}", entry.name);
            let file = volume.open_file(&format!("/{path}")).unwrap();
            let (mut bytes, mut chunk) = (Vec::new(), vec![0; 64 * 1024]);
            loop {
                let count = volume
                    .read_file_at(&file, bytes.len() as u64, &mut chunk)
                    .unwrap_or_else(|error| panic!("read /{path}: {error}"));
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            files.insert(path, (bytes.len() as u64, sha256(&bytes)));
        }
    }
    files
}

#[test]
fn the_log_windows_left_is_walked_from_a_read_only_device() {
    for capture in CAPTURES {
        let path = unpack(&format!("windows-interrupted-{capture}"));
        let device = Image::open(&path).unwrap();
        assert!(
            logfile_state(&device).unwrap().needs_replay(),
            "{capture}: the log holds no work"
        );
        let walk = walk_logfile(&device, None, 1_000_000, 1024 * 1024).unwrap();
        assert!(!walk.records.is_empty(), "{capture}: no records walked");
        assert!(
            walk.records
                .windows(2)
                .all(|pair| pair[0].view.lsn < pair[1].view.lsn),
            "{capture}: the walk is not in LSN order"
        );
        drop(device);
        std::fs::remove_file(&path).unwrap();
    }
}

#[test]
fn a_replay_held_in_memory_reaches_what_windows_recovered_and_writes_nothing() {
    for capture in CAPTURES {
        let name = format!("windows-interrupted-{capture}");
        let path = unpack(&name);
        let before = common::image_digest(&path).unwrap();

        let replayed = replay_logfile_in_memory(Image::open(&path).unwrap())
            .unwrap_or_else(|error| panic!("{capture}: replay: {error}"));
        let report = replayed.report().cloned().expect("a replay ran");
        assert!(report.records > 0, "{capture}: {report:?}");
        assert!(!replayed.changed_ranges().is_empty());
        assert!(
            !logfile_state(&replayed).unwrap().needs_replay(),
            "{capture}: the replayed view still holds work"
        );

        // The same bytes the replay fsck writes to a copy.
        let copy = common::temp_image_path(format!("evidence-{capture}"));
        std::fs::copy(&path, &copy).unwrap();
        let written = fsck::replay_logfile(&copy)
            .unwrap_or_else(|error| panic!("{capture}: fsck replay: {error}"));
        assert_eq!(written, report, "{capture}");
        assert!(
            reads_as(&replayed, &copy),
            "{capture}: the replay in memory differs from the one fsck wrote"
        );
        std::fs::remove_file(&copy).unwrap();

        // The files Windows listed after its own restart pass.
        let volume = Volume::open(replayed).unwrap();
        assert_eq!(workload(&volume), manifest(capture), "{capture}");

        assert_eq!(
            common::image_digest(&path).unwrap(),
            before,
            "{capture}: the device changed"
        );
        std::fs::remove_file(&path).unwrap();
    }
}

#[test]
fn a_clean_log_replays_to_the_device_unchanged() {
    let recovered = unpack("windows-interrupted-1.recovered");
    let replayed = replay_logfile_in_memory(Image::open(&recovered).unwrap()).unwrap();
    assert!(replayed.report().is_none());
    assert!(replayed.changed_ranges().is_empty());
    assert!(reads_as(&replayed, &recovered));
    drop(replayed);
    std::fs::remove_file(&recovered).unwrap();
}

/// Larger than the fixture's whole `$J`, so one batch reads all of it.
const WHOLE_JOURNAL: usize = 16 * 1024 * 1024;

fn active_journal(device: &Image) -> ImageJournal {
    match usn_journal_state(device).unwrap() {
        JournalState::Active(journal) => journal,
        other => panic!("the journal is {other:?}"),
    }
}

/// The undamaged fixture, its journal and every record in it: the control each
/// damaged copy is compared with.
fn undamaged_journal() -> (Image, ImageJournal, Vec<UsnRecordV2>) {
    assert!(
        Path::new(USN_IMG).exists(),
        "missing {USN_IMG}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let device = Image::open(USN_IMG).unwrap();
    let journal = active_journal(&device);
    let batch = read_usn_records(&device, &journal, journal.first_cursor(), WHOLE_JOURNAL).unwrap();
    assert!(batch.finished && batch.records.len() > 2);
    (device, journal, batch.records)
}

/// Every 8-byte-aligned offset of the image at `path` that holds `needle`.
fn occurrences(path: &str, needle: &[u8]) -> Vec<u64> {
    // Chunks advance short of their length, so a needle of up to `OVERLAP`
    // bytes across a boundary is seen whole, and seen once.
    const OVERLAP: usize = 64;
    const STEP: usize = common::IMAGE_CHUNK_SIZE - OVERLAP;
    assert!(needle.len() <= OVERLAP);
    let size = std::fs::metadata(path).unwrap().len();
    let mut found = Vec::new();
    let mut at = 0;
    while at < size {
        let count = (size - at).min(common::IMAGE_CHUNK_SIZE as u64) as usize;
        let chunk = common::read_image_region(path, at, count).unwrap();
        for offset in (0..STEP).step_by(8) {
            if chunk.get(offset..offset + needle.len()) == Some(needle) {
                found.push(at + offset as u64);
            }
        }
        at += STEP as u64;
    }
    found
}

/// The fixture with `bytes` laid `within` every copy of `needle`, in memory.
/// A copy `$LogFile` happens to hold is changed too, which no journal read
/// looks at.
fn damaged(needle: &[u8], within: u64, bytes: &[u8]) -> Image {
    let found = occurrences(USN_IMG, needle);
    assert!(!found.is_empty(), "{USN_IMG} does not hold {needle:02x?}");
    let mut device = Image::open(USN_IMG).unwrap();
    for at in found {
        device.write(at + within, bytes);
    }
    device
}

/// The first 32 bytes of `record` on disk: its length, version 2.0, file and
/// parent references and USN.
fn record_head(record: &UsnRecordV2) -> Vec<u8> {
    let mut head = record.record_length.to_le_bytes().to_vec();
    head.extend([2, 0, 0, 0]);
    head.extend(record.file_reference.to_le_bytes());
    head.extend(record.parent_file_reference.to_le_bytes());
    head.extend(record.usn.to_le_bytes());
    head
}

/// `$Max` as the disk holds it.
fn max_stream(journal: &ImageJournal) -> Vec<u8> {
    [
        journal.maximum_size,
        journal.allocation_delta,
        journal.journal_id,
        journal.lowest_valid_usn as u64,
    ]
    .iter()
    .flat_map(|field| field.to_le_bytes())
    .collect()
}

#[test]
fn a_damaged_record_in_the_windows_journal_is_refused_not_skipped() {
    let before = common::image_digest(USN_IMG).unwrap();
    let (_, journal, records) = undamaged_journal();
    let target = &records[1];
    let usn = target.usn;
    let head = record_head(target);
    // The error each damage to the second record has to name.
    let read = |within: u64, bytes: &[u8]| {
        let device = damaged(&head, within, bytes);
        match read_usn_records(&device, &journal, journal.first_cursor(), WHOLE_JOURNAL) {
            Err(error) => error,
            Ok(batch) => panic!(
                "damage at byte {within} of the record at USN {usn} read as {} records",
                batch.records.len()
            ),
        }
    };
    let decode_error = |within: u64, bytes: &[u8]| match read(within, bytes) {
        JournalReadError::Record { usn: at, error } if at == usn => error,
        other => panic!("damage at byte {within}: {other:?}"),
    };

    assert_eq!(
        decode_error(4, &9u16.to_le_bytes()),
        UsnDecodeError::UnsupportedVersion { major: 9, minor: 0 }
    );
    for length in [0u32, 8] {
        assert_eq!(
            decode_error(0, &length.to_le_bytes()),
            UsnDecodeError::RecordLengthTooSmall {
                record_length: length
            }
        );
    }
    assert!(matches!(
        decode_error(58, &u16::MAX.to_le_bytes()),
        UsnDecodeError::BadFileName {
            offset: u16::MAX,
            ..
        }
    ));
    match read(0, &0xffff_fff8u32.to_le_bytes()) {
        JournalReadError::PastNextUsn {
            usn: at,
            record_length: 0xffff_fff8,
            ..
        } if at == usn => {}
        other => panic!("an out-of-range length: {other:?}"),
    }
    match read(24, &(usn + 8).to_le_bytes()) {
        JournalReadError::Misplaced { usn: at, recorded } if at == usn && recorded == usn + 8 => {}
        other => panic!("another record's USN: {other:?}"),
    }
    assert_eq!(common::image_digest(USN_IMG).unwrap(), before);
}

#[test]
fn a_cursor_the_windows_journal_cannot_answer_is_refused_not_empty() {
    let (device, journal, records) = undamaged_journal();
    let saved = journal.first_cursor();
    let second = records[1].usn;
    let max = max_stream(&journal);
    let refusal = |device: &Image, journal: &ImageJournal, cursor: UsnCursor| match read_usn_records(
        device,
        journal,
        cursor,
        WHOLE_JOURNAL,
    ) {
        Err(JournalReadError::NotAdmitted(admission)) => admission,
        other => panic!("{cursor:?}: {other:?}"),
    };
    let at = |next_usn| UsnCursor {
        journal_id: journal.journal_id,
        next_usn,
    };

    let past = journal.next_usn + 8;
    assert_eq!(
        refusal(&device, &journal, at(past)),
        CursorAdmission::Future {
            cursor_usn: past,
            next_usn: journal.next_usn
        }
    );
    assert_eq!(
        refusal(&device, &journal, at(-8)),
        CursorAdmission::InvalidCursor { cursor_usn: -8 }
    );
    // The bounds a journal reports once its front is gone.
    let wrapped = ImageJournal {
        first_usn: second,
        ..journal
    };
    assert_eq!(
        refusal(&device, &wrapped, saved),
        CursorAdmission::Wrapped {
            cursor_usn: saved.next_usn,
            first_usn: second
        }
    );

    // A journal deleted and created again has another identity.
    let new_id = journal.journal_id ^ 1;
    let reset = damaged(&max, 16, &new_id.to_le_bytes());
    let state = active_journal(&reset);
    assert_eq!(state.journal_id, new_id);
    assert_eq!(
        refusal(&reset, &state, saved),
        CursorAdmission::JournalReset {
            cursor_id: journal.journal_id,
            journal_id: new_id
        }
    );

    let raised = damaged(&max, 24, &second.to_le_bytes());
    let state = active_journal(&raised);
    assert_eq!((state.lowest_valid_usn, state.first_usn), (second, second));
    assert_eq!(
        refusal(&raised, &state, saved),
        CursorAdmission::BeforeLowestValid {
            cursor_usn: saved.next_usn,
            lowest_valid_usn: second
        }
    );

    let beyond = damaged(&max, 24, &past.to_le_bytes());
    let state = active_journal(&beyond);
    assert_eq!(
        refusal(&beyond, &state, saved),
        CursorAdmission::InvalidJournal(InvalidBound::LowestValidAfterNext)
    );
}

#[test]
fn inspecting_a_log_neither_clears_its_work_nor_admits_a_writer() {
    for capture in CAPTURES {
        let path = unpack(&format!("windows-interrupted-{capture}"));
        let before = common::image_digest(&path).unwrap();

        let device = Image::open(&path).unwrap();
        walk_logfile(&device, None, 1, 1024 * 1024).unwrap();
        drop(replay_logfile_in_memory(device).unwrap());

        assert!(
            logfile_state(&Image::open(&path).unwrap())
                .unwrap()
                .needs_replay(),
            "{capture}: inspection cleared the log's work"
        );
        let refused = match Filesystem::mount_rw(&path) {
            Ok(_) => panic!("{capture}: a read-write mount was admitted after inspection"),
            Err(error) => error.0,
        };
        assert!(
            refused.contains("read-write mount refused"),
            "{capture}: {refused}"
        );
        assert_eq!(
            common::image_digest(&path).unwrap(),
            before,
            "{capture}: the device changed"
        );
        std::fs::remove_file(&path).unwrap();
    }
}
