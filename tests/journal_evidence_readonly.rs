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
use fs_ntfs::file::FileAPI;
use fs_ntfs::fsck;
use fs_ntfs::usn_image::JournalState;
use fs_ntfs::volume::Volume;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

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

/// A device that can only be read.
#[derive(Clone)]
struct Image(Arc<Vec<u8>>);

impl BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let range = usize::try_from(offset)
            .ok()
            .and_then(|start| Some(start..start.checked_add(output.len())?))
            .and_then(|range| self.0.get(range));
        output.copy_from_slice(range.ok_or(fs_core::Error::OutOfBounds {
            offset,
            len: output.len() as u64,
            size: self.0.len() as u64,
        })?);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
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
    let bytes = Arc::new(std::fs::read(USN_IMG).unwrap());
    let before = sha256(&bytes);
    let device = Image(bytes.clone());
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
    assert_eq!(sha256(&bytes), before, "the device changed");
}

fn unpack(name: &str) -> Vec<u8> {
    let output = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .output()
        .expect("run gzip");
    assert!(
        output.status.success(),
        "decompress test-disks/{name}.img.gz"
    );
    assert_eq!(
        output.stdout.len(),
        133_103_616,
        "{name} is the captured partition"
    );
    output.stdout
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
        let bytes = Arc::new(unpack(&format!("windows-interrupted-{capture}")));
        let device = Image(bytes.clone());
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
    }
}

#[test]
fn a_replay_held_in_memory_reaches_what_windows_recovered_and_writes_nothing() {
    for capture in CAPTURES {
        let name = format!("windows-interrupted-{capture}");
        let bytes = Arc::new(unpack(&name));
        let before = sha256(&bytes);

        let replayed = replay_logfile_in_memory(Image(bytes.clone()))
            .unwrap_or_else(|error| panic!("{capture}: replay: {error}"));
        let report = replayed.report().cloned().expect("a replay ran");
        assert!(report.records > 0, "{capture}: {report:?}");
        assert!(!replayed.changed_ranges().is_empty());
        assert!(
            !logfile_state(&replayed).unwrap().needs_replay(),
            "{capture}: the replayed view still holds work"
        );

        // The same bytes the replay fsck writes to a copy.
        let mut ours = vec![0; bytes.len()];
        replayed.read_at(0, &mut ours).unwrap();
        let copy = common::temp_image_path(format!("evidence-{capture}"));
        std::fs::write(&copy, bytes.as_slice()).unwrap();
        let written = fsck::replay_logfile(&copy)
            .unwrap_or_else(|error| panic!("{capture}: fsck replay: {error}"));
        assert_eq!(written, report, "{capture}");
        let on_disk = std::fs::read(&copy).unwrap();
        std::fs::remove_file(&copy).unwrap();
        assert!(
            ours == on_disk,
            "{capture}: the replay in memory differs from the one fsck wrote"
        );

        // The files Windows listed after its own restart pass.
        let volume = Volume::open(replayed).unwrap();
        assert_eq!(workload(&volume), manifest(capture), "{capture}");

        assert_eq!(sha256(&bytes), before, "{capture}: the device changed");
    }
}

#[test]
fn a_clean_log_replays_to_the_device_unchanged() {
    let recovered = Arc::new(unpack("windows-interrupted-1.recovered"));
    let replayed = replay_logfile_in_memory(Image(recovered.clone())).unwrap();
    assert!(replayed.report().is_none());
    assert!(replayed.changed_ranges().is_empty());
    let mut view = vec![0; recovered.len()];
    replayed.read_at(0, &mut view).unwrap();
    assert!(view == *recovered);
}
