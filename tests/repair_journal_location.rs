//! An in-place repair refuses a journal location that is missing or on the
//! device being repaired, before anything is written (#921, part of #713).
//!
//! The target here is a copy of `windows-interrupted-6`, a volume Windows
//! was writing to when it was captured (see
//! `tests/windows_interrupted_fixture.rs`). Every refused location must
//! leave no journal file behind and the copy's SHA-256 unchanged.
//!
//! A journal on a filesystem the target device holds cannot be set up in
//! CI without root, so that comparison is tested over `stat` values.
#![cfg(all(feature = "plan-file", unix))]

mod common;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use fs_ntfs::repair_journal::{
    check_journal_location, directory_on_target, JournalLocationError, LocationStat,
};
use sha2::{Digest, Sha256};

/// Unpack a `test-disks/` image to a scratch path.
fn unpack(name: &str) -> String {
    let img = common::temp_image_path(format!(
        "journal_location_{}",
        name.replace(['.', '-'], "_")
    ));
    let out = File::create(&img).expect("create");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(out)
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    img
}

fn sha256(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).expect("read image")).to_vec()
}

/// A fresh scratch directory beside the unpacked image.
fn scratch_dir(img: &Path, name: &str) -> PathBuf {
    let dir = img.with_file_name(format!(
        "{}_{name}",
        img.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

/// Check `journal` against `target`, require the refusal `want` and a
/// message naming `reason`, and require that nothing was created at
/// `journal` nor written to the target.
fn refused(
    journal: &Path,
    target: &Path,
    before: &[u8],
    existed: bool,
    want: fn(&JournalLocationError) -> bool,
    reason: &str,
) {
    let err = match check_journal_location(journal, target) {
        Ok(()) => panic!("{}: accepted as a journal location", journal.display()),
        Err(e) => e,
    };
    assert!(want(&err), "{}: wrong refusal: {err:?}", journal.display());
    let message = err.to_string();
    assert!(
        message.contains(reason),
        "{}: refusal names the reason {reason:?}: {message}",
        journal.display()
    );
    assert_eq!(
        std::fs::symlink_metadata(journal).is_ok(),
        existed,
        "{}: the check created or removed nothing at the journal path",
        journal.display()
    );
    assert_eq!(
        sha256(target),
        before,
        "the refused location wrote nothing to the target"
    );
}

#[test]
fn a_journal_location_that_is_missing_the_target_or_taken_is_refused_with_nothing_written() {
    let img = PathBuf::from(unpack("windows-interrupted-6"));
    let before = sha256(&img);
    let dir = scratch_dir(&img, "journals");

    // A journal directory that does not exist: missing external storage.
    let missing = dir.join("not-mounted").join("repair.journal");
    refused(
        &missing,
        &img,
        &before,
        false,
        |e| matches!(e, JournalLocationError::MissingDirectory(_)),
        "does not exist",
    );

    // The target itself, by the same path.
    refused(
        &img,
        &img,
        &before,
        true,
        |e| matches!(e, JournalLocationError::IsTarget),
        "is the device being repaired",
    );

    // A hard link to the target: another name for the same file.
    let hard = dir.join("hard.journal");
    std::fs::hard_link(&img, &hard).expect("hard link to the target");
    refused(
        &hard,
        &img,
        &before,
        true,
        |e| matches!(e, JournalLocationError::IsTarget),
        "is the device being repaired",
    );

    // A symlink to the target.
    let soft = dir.join("soft.journal");
    std::os::unix::fs::symlink(
        std::fs::canonicalize(&img).expect("canonicalize target"),
        &soft,
    )
    .expect("symlink to the target");
    refused(
        &soft,
        &img,
        &before,
        true,
        |e| matches!(e, JournalLocationError::IsTarget),
        "is the device being repaired",
    );

    // A file that already exists: resuming a journal is not this check's.
    let taken = dir.join("taken.journal");
    std::fs::write(&taken, b"someone else's bytes").expect("write existing file");
    refused(
        &taken,
        &img,
        &before,
        true,
        |e| matches!(e, JournalLocationError::Exists),
        "already exists",
    );
    assert_eq!(
        std::fs::read(&taken).unwrap(),
        b"someone else's bytes",
        "the existing file is left as it was"
    );

    // A dangling symlink: creating the journal would follow it somewhere.
    let dangling = dir.join("dangling.journal");
    std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).expect("dangling symlink");
    refused(
        &dangling,
        &img,
        &before,
        true,
        |e| matches!(e, JournalLocationError::Exists),
        "already exists",
    );

    // A fresh name in an existing directory on another file is accepted,
    // and checking it creates nothing.
    let fresh = dir.join("repair.journal");
    check_journal_location(&fresh, &img).expect("a fresh journal path is accepted");
    assert!(
        std::fs::symlink_metadata(&fresh).is_err(),
        "the check created no journal"
    );
    assert_eq!(sha256(&img), before, "the accepted check wrote nothing");

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_file(&img).unwrap();
}

#[test]
fn a_journal_directory_on_a_filesystem_the_target_device_holds_is_on_the_target() {
    let directory = LocationStat {
        dev: 0x0100_0004,
        ino: 2,
        rdev: 0,
        is_device: false,
    };
    let holding = LocationStat {
        dev: 0x0300_0001,
        ino: 611,
        rdev: 0x0100_0004,
        is_device: true,
    };
    let other = LocationStat {
        rdev: 0x0100_0005,
        ..holding
    };
    assert!(
        directory_on_target(&directory, &holding),
        "a block device whose st_rdev is the directory's st_dev holds the journal directory"
    );
    assert!(
        !directory_on_target(&directory, &other),
        "a block device whose st_rdev differs does not hold the journal directory"
    );

    // An image file has no st_rdev to compare: a stale value in it must
    // not make a regular file look like the device under the directory.
    let image = LocationStat {
        is_device: false,
        ..holding
    };
    assert!(
        !directory_on_target(&directory, &image),
        "a regular file holds no filesystem"
    );

    let message = JournalLocationError::OnTargetDevice.to_string();
    assert!(
        message.contains("on a filesystem the device being repaired holds"),
        "the refusal names the reason: {message}"
    );
}
