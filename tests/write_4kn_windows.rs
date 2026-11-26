//! The 4Kn volume Windows formatted is written by this crate, for Windows to
//! grade (#1396, WRITE4K-02).
//!
//! `test-disks/build-windows-4kn-fixture.ps1` builds `ntfs-4kn-read.img` on
//! 4096-byte logical sectors with 1024-byte file records. This test copies it
//! to `ntfs-4kn-write.img`, then creates, writes, renames and removes files
//! and directories in the copy, and writes `ntfs-4kn-write.expected.tsv`:
//! every file and directory the volume must then hold, each file with its
//! length and SHA-256. `test-disks/grade-windows-4kn-write.ps1` puts the
//! image on a 4096-byte sector disk, has Windows' `chkdsk` judge it, and
//! compares Windows' own listing and hashes with that report.
#![cfg(windows)]

use fs_core::FileDevice;
use fs_ntfs::file::NtfsFileAPI;
use fs_ntfs::volume::{NtfsVolumeAPI, Volume};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const SOURCE: &str = "test-disks/ntfs-4kn-read.img";
const HASHES: &str = "test-disks/ntfs-4kn-read.sha256.tsv";
const INDEX_HASHES: &str = "test-disks/ntfs-4kn-read.index.tsv";
const IMAGE: &str = "test-disks/ntfs-4kn-write.img";
const EXPECTED: &str = "test-disks/ntfs-4kn-write.expected.tsv";
const PROVIDER: &str = "test-disks/build-windows-4kn-fixture.ps1";

/// What the volume must hold: each file's length and SHA-256, and every
/// directory below the root.
#[derive(Default)]
struct Expected {
    files: BTreeMap<String, (u64, String)>,
    directories: BTreeSet<String>,
}

impl Expected {
    /// The files Windows wrote, and the directories that hold them.
    fn from_windows() -> Self {
        let mut expected = Self::default();
        for report in [HASHES, INDEX_HASHES] {
            let text = std::fs::read_to_string(report)
                .unwrap_or_else(|e| panic!("{report}: {e}; {PROVIDER} builds it"));
            for line in text.lines().filter(|line| !line.is_empty()) {
                let fields: Vec<&str> = line.split('\t').collect();
                let path = fields[0].to_owned();
                let mut parent = path.as_str();
                while let Some((above, _)) = parent.rsplit_once('/') {
                    if above.is_empty() {
                        break;
                    }
                    expected.directories.insert(above.to_owned());
                    parent = above;
                }
                expected
                    .files
                    .insert(path, (fields[1].parse().unwrap(), fields[2].to_owned()));
            }
        }
        assert!(
            expected.files.len() > 400,
            "{HASHES} and {INDEX_HASHES} name {} files",
            expected.files.len()
        );
        expected
    }

    fn holds(&mut self, path: &str, bytes: &[u8]) {
        self.files
            .insert(path.to_owned(), (bytes.len() as u64, sha256(bytes)));
    }

    fn tsv(&self) -> String {
        let directories = self.directories.iter().map(|path| format!("dir\t{path}\n"));
        let files = self
            .files
            .iter()
            .map(|(path, (size, digest))| format!("file\t{path}\t{size}\t{digest}\n"));
        directories.chain(files).collect()
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `length` bytes that differ between seeds and along the file.
fn pattern(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn read_all<V: NtfsFileAPI>(volume: &V, path: &str, size: u64) -> Vec<u8> {
    let file = volume
        .open_file(path)
        .unwrap_or_else(|error| panic!("open {path}: {error}"));
    let mut bytes = vec![0; size as usize + 1];
    let mut filled = 0;
    loop {
        let count = volume
            .read_file_at(&file, filled as u64, &mut bytes[filled..])
            .unwrap_or_else(|error| panic!("read {path}: {error}"));
        if count == 0 {
            break;
        }
        filled += count;
    }
    bytes.truncate(filled);
    bytes
}

/// Creates `parent/name` and writes `bytes` to it in pieces of `piece`.
fn create<V: NtfsFileAPI>(
    volume: &V,
    expected: &mut Expected,
    parent: &str,
    name: &str,
    bytes: &[u8],
    piece: usize,
) -> String {
    let path = format!("{}/{name}", parent.trim_end_matches('/'));
    volume
        .create_file(parent, name)
        .unwrap_or_else(|error| panic!("create {path}: {error}"));
    for (index, chunk) in bytes.chunks(piece).enumerate() {
        volume
            .write_file_at(&path, (index * piece) as u64, chunk)
            .unwrap_or_else(|error| panic!("write {path}: {error}"));
    }
    expected.holds(&path, bytes);
    path
}

fn make_directory<V: NtfsFileAPI>(
    volume: &V,
    expected: &mut Expected,
    parent: &str,
    name: &str,
) -> String {
    let path = format!("{}/{name}", parent.trim_end_matches('/'));
    volume
        .create_directory(parent, name)
        .unwrap_or_else(|error| panic!("mkdir {path}: {error}"));
    expected.directories.insert(path.clone());
    path
}

fn remove<V: NtfsFileAPI>(volume: &V, expected: &mut Expected, path: &str) {
    volume
        .remove_file(path)
        .unwrap_or_else(|error| panic!("remove {path}: {error}"));
    assert!(expected.files.remove(path).is_some(), "{path} was not held");
}

fn rename<V: NtfsFileAPI>(volume: &V, expected: &mut Expected, path: &str, name: &str) {
    volume
        .rename(path, name)
        .unwrap_or_else(|error| panic!("rename {path} to {name}: {error}"));
    let (parent, _) = path.rsplit_once('/').unwrap();
    let held = expected
        .files
        .remove(path)
        .unwrap_or_else(|| panic!("{path} was not held"));
    expected.files.insert(format!("{parent}/{name}"), held);
}

/// New files and directories: resident and non-resident contents, a
/// directory large enough to build index blocks, and every removal.
fn change_new_entries<V: NtfsFileAPI>(volume: &V, expected: &mut Expected) {
    let top = make_directory(volume, expected, "/", "write4k");

    create(volume, expected, &top, "empty.bin", &[], 1);
    create(
        volume,
        expected,
        &top,
        "resident.bin",
        &pattern(1, 100),
        100,
    );
    // Appends that move a resident value into clusters and grow its run.
    create(
        volume,
        expected,
        &top,
        "grown.bin",
        &pattern(2, 3 * 1024 * 1024 + 17),
        65_521,
    );
    let before = create(
        volume,
        expected,
        &top,
        "before.bin",
        &pattern(3, 9000),
        9000,
    );
    rename(volume, expected, &before, "after the rename.bin");
    let doomed = create(
        volume,
        expected,
        &top,
        "doomed.bin",
        &pattern(4, 20_000),
        20_000,
    );
    remove(volume, expected, &doomed);

    let gone = make_directory(volume, expected, &top, "gone");
    volume
        .remove_directory(&gone)
        .unwrap_or_else(|error| panic!("rmdir {gone}: {error}"));
    expected.directories.remove(&gone);

    let many = make_directory(volume, expected, &top, "many");
    let paths: Vec<String> = (0..300u64)
        .map(|number| {
            let size = (number as usize * 131) % 1800 + 1;
            create(
                volume,
                expected,
                &many,
                &format!("entry-{number:04}.bin"),
                &pattern(100 + number, size),
                size,
            )
        })
        .collect();
    for path in paths.iter().step_by(3) {
        remove(volume, expected, path);
    }
}

/// The files and the index blocks Windows built.
fn change_windows_entries<V: NtfsFileAPI>(volume: &V, expected: &mut Expected) {
    for number in 0..50 {
        remove(volume, expected, &format!("/index-blocks/f{number:03}.txt"));
    }
    for number in 100..120 {
        rename(
            volume,
            expected,
            &format!("/index-blocks/f{number:03}.txt"),
            &format!("renamed-{number:03}.txt"),
        );
    }
    for number in 0..50u64 {
        create(
            volume,
            expected,
            "/index-blocks",
            &format!("added-{number:03}.bin"),
            &pattern(1000 + number, 700),
            700,
        );
    }

    // The two largest files of the set Windows hashed: one overwritten
    // across a cluster boundary and appended to, one cut short.
    let mut largest: Vec<(u64, String)> = expected
        .files
        .iter()
        .filter(|(path, _)| !path.starts_with("/index-blocks/") && !path.starts_with("/write4k/"))
        .map(|(path, (size, _))| (*size, path.clone()))
        .collect();
    largest.sort();
    let (size, overwritten) = largest.pop().expect("a file Windows wrote");
    assert!(size > 8192, "{overwritten} is only {size} bytes");
    let mut bytes = read_all(volume, &overwritten, size);
    let patch = pattern(5, 6000);
    bytes[3000..9000].copy_from_slice(&patch);
    volume
        .write_file_at(&overwritten, 3000, &patch)
        .unwrap_or_else(|error| panic!("overwrite {overwritten}: {error}"));
    let tail = pattern(6, 12_345);
    volume
        .write_file_at(&overwritten, size, &tail)
        .unwrap_or_else(|error| panic!("append {overwritten}: {error}"));
    bytes.extend_from_slice(&tail);
    expected.holds(&overwritten, &bytes);

    let (size, cut) = largest.pop().expect("a second file Windows wrote");
    assert!(size > 100, "{cut} is only {size} bytes");
    let mut bytes = read_all(volume, &cut, size);
    bytes.truncate(size as usize / 2);
    volume
        .set_len(&cut, size / 2)
        .unwrap_or_else(|error| panic!("truncate {cut}: {error}"));
    expected.holds(&cut, &bytes);
}

#[test]
fn files_and_directories_are_changed_for_windows_to_grade() {
    std::fs::copy(SOURCE, IMAGE).unwrap_or_else(|e| panic!("{SOURCE}: {e}; {PROVIDER} builds it"));
    let mut expected = Expected::from_windows();

    let volume = Volume::open_writable(FileDevice::open_rw(IMAGE).unwrap()).unwrap();
    assert_eq!(volume.boot().bytes_per_sector, 4096);
    assert_eq!(volume.boot().file_record_size, 1024);
    change_new_entries(&volume, &mut expected);
    change_windows_entries(&volume, &mut expected);
    volume.flush().unwrap();
    drop(volume);

    // This crate's own reading of what it wrote; Windows' is the verdict.
    let volume = Volume::open(FileDevice::open(IMAGE).unwrap()).unwrap();
    for (path, (size, digest)) in &expected.files {
        let bytes = read_all(&volume, path, *size);
        assert_eq!(bytes.len() as u64, *size, "{path}");
        assert_eq!(&sha256(&bytes), digest, "{path}");
    }

    std::fs::write(EXPECTED, expected.tsv()).unwrap();
}
