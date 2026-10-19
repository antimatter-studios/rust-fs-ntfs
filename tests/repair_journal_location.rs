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
    bsd_device_record, check_journal_location, device_relation, directory_on_target,
    layered_refusal, sysfs_device_record, sysfs_lower_devices, DeviceRecord, DeviceRelation,
    JournalLocationError, LocationStat,
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

/// A device named `name` on the whole disk `whole_disk`.
fn device(name: &str, whole_disk: &str) -> DeviceRecord {
    DeviceRecord {
        name: name.into(),
        whole_disk: whole_disk.into(),
    }
}

#[test]
fn the_journal_device_is_related_to_the_target_device_by_disk_and_partition() {
    let disk2 = device("disk2", "disk2");
    let disk2s1 = device("disk2s1", "disk2");
    let disk2s2 = device("disk2s2", "disk2");
    let disk3s1 = device("disk3s1", "disk3");

    assert_eq!(
        device_relation(&disk2, &disk2),
        DeviceRelation::Same,
        "a journal on the whole disk being repaired"
    );
    assert_eq!(
        device_relation(&disk2s1, &disk2s1),
        DeviceRelation::Same,
        "a journal on the partition being repaired"
    );
    assert_eq!(
        device_relation(&disk2s1, &disk2),
        DeviceRelation::PartitionOfTarget,
        "a journal on /dev/disk2s1 while /dev/disk2 is repaired"
    );
    assert_eq!(
        device_relation(&disk2, &disk2s1),
        DeviceRelation::TargetOnJournalDisk,
        "a journal on /dev/disk2 while its partition /dev/disk2s1 is repaired"
    );
    assert_eq!(
        device_relation(&disk3s1, &disk2),
        DeviceRelation::Unrelated,
        "a journal on another disk"
    );
    assert_eq!(
        device_relation(&disk2s2, &disk2s1),
        DeviceRelation::Unrelated,
        "a journal on another partition of the disk that holds the target"
    );
}

#[test]
fn every_related_device_is_refused_naming_its_relationship() {
    assert_eq!(
        DeviceRelation::Same.refusal(),
        Some(JournalLocationError::OnTargetDevice)
    );
    assert_eq!(DeviceRelation::Unrelated.refusal(), None);

    let partition = DeviceRelation::PartitionOfTarget
        .refusal()
        .expect("a partition of the target is refused");
    assert_eq!(partition, JournalLocationError::OnTargetPartition);
    let message = partition.to_string();
    assert!(
        message.contains("on a partition of the disk being repaired"),
        "the refusal names the relationship: {message}"
    );

    let whole = DeviceRelation::TargetOnJournalDisk
        .refusal()
        .expect("the disk holding the target is refused");
    assert_eq!(whole, JournalLocationError::OnTargetWholeDisk);
    let message = whole.to_string();
    assert!(
        message.contains("on the disk that holds the partition being repaired"),
        "the refusal names the relationship: {message}"
    );
}

#[test]
fn a_bsd_device_name_resolves_to_its_whole_disk() {
    assert_eq!(bsd_device_record("disk2"), device("disk2", "disk2"));
    assert_eq!(bsd_device_record("disk2s1"), device("disk2s1", "disk2"));
    assert_eq!(bsd_device_record("disk10s3"), device("disk10s3", "disk10"));
    assert_eq!(bsd_device_record("disk3s1s1"), device("disk3s1s1", "disk3"));
    // The raw (character) device is the same device as the block one.
    assert_eq!(bsd_device_record("rdisk2"), device("disk2", "disk2"));
    assert_eq!(bsd_device_record("rdisk2s1"), device("disk2s1", "disk2"));
    // A name that is not a disk is its own whole disk.
    assert_eq!(bsd_device_record("vn0"), device("vn0", "vn0"));
}

#[test]
fn a_sysfs_block_tree_resolves_a_partition_to_its_disk() {
    let root = PathBuf::from(common::temp_image_path("journal_location_sysfs"));
    let _ = std::fs::remove_dir_all(&root);
    let sda = root.join("devices/pci0000:00/block/sda");
    std::fs::create_dir_all(sda.join("sda1")).unwrap();
    std::fs::write(sda.join("sda1/partition"), "1\n").unwrap();
    std::fs::create_dir_all(root.join("dev/block")).unwrap();
    std::os::unix::fs::symlink(
        "../../devices/pci0000:00/block/sda",
        root.join("dev/block/8:0"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "../../devices/pci0000:00/block/sda/sda1",
        root.join("dev/block/8:1"),
    )
    .unwrap();

    assert_eq!(sysfs_device_record(&root, 8, 0), Some(device("sda", "sda")));
    assert_eq!(
        sysfs_device_record(&root, 8, 1),
        Some(device("sda1", "sda"))
    );
    assert_eq!(
        sysfs_device_record(&root, 0, 42),
        None,
        "a device sysfs does not list (tmpfs, NFS) has no record"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// The devices directly under each layered device in `stack`, as an
/// APFS container's physical stores or a device-mapper or md device's
/// slaves would be read (#952).
fn under<'a>(stack: &'a [(&'a str, Vec<DeviceRecord>)]) -> impl Fn(&str) -> Vec<DeviceRecord> + 'a {
    move |name| {
        stack
            .iter()
            .find(|(layered, _)| *layered == name)
            .map(|(_, lower)| lower.clone())
            .unwrap_or_default()
    }
}

#[test]
fn a_journal_on_a_layered_device_is_traced_to_the_disks_under_it() {
    let stack = [
        // An APFS volume disk3s1 on the synthesized disk3, whose physical
        // store is the partition disk2s2.
        ("disk3", vec![device("disk2s2", "disk2")]),
        // A device-mapper device on the whole disk sda.
        ("dm-0", vec![device("sda", "sda")]),
        // An md array across a partition of sdb and one of sdc.
        ("md0", vec![device("sdb1", "sdb"), device("sdc1", "sdc")]),
        // A device-mapper device on that array: two layers down.
        ("dm-1", vec![device("md0", "md0")]),
        // A device-mapper device on an unrelated disk.
        ("dm-2", vec![device("sdd1", "sdd")]),
    ];
    let lower = under(&stack);

    let refused = |journal: DeviceRecord, target: DeviceRecord, why: &str| {
        let refusal =
            layered_refusal(&journal, &target, &lower).unwrap_or_else(|| panic!("{why}: accepted"));
        assert!(
            matches!(refusal, JournalLocationError::OnTargetStack { .. }),
            "{why}: {refusal:?}"
        );
        refusal
    };

    let apfs = refused(
        device("disk3s1", "disk3"),
        device("disk2", "disk2"),
        "an APFS volume whose container is on a partition of the target disk",
    );
    assert_eq!(
        apfs,
        JournalLocationError::OnTargetStack {
            journal_device: "disk2s2".into(),
            target_device: "disk2".into(),
        },
        "the refusal names the device under the journal and the target it shares a disk with"
    );
    let message = apfs.to_string();
    assert!(
        message.contains("'disk2s2'") && message.contains("'disk2'"),
        "the refusal names both devices: {message}"
    );
    refused(
        device("disk3s1", "disk3"),
        device("disk2s2", "disk2"),
        "an APFS volume whose container is the partition being repaired",
    );
    refused(
        device("dm-0", "dm-0"),
        device("sda", "sda"),
        "a device-mapper device on the target disk",
    );
    refused(
        device("dm-0", "dm-0"),
        device("sda1", "sda"),
        "a device-mapper device on the disk holding the target partition",
    );
    refused(
        device("md0", "md0"),
        device("sdc", "sdc"),
        "an md array across the target disk and another",
    );
    refused(
        device("md0p1", "md0"),
        device("sdb", "sdb"),
        "a partition of an md array across the target disk",
    );
    refused(
        device("dm-1", "dm-1"),
        device("sdb1", "sdb"),
        "a device-mapper device on an md array across the target partition",
    );
    refused(
        device("sda2", "sda"),
        device("dm-0", "dm-0"),
        "a journal on the disk under the device-mapper device being repaired",
    );

    for (journal, target, why) in [
        (
            device("dm-2", "dm-2"),
            device("sda", "sda"),
            "a stack on an unrelated disk",
        ),
        (
            device("disk3s1", "disk3"),
            device("disk2s1", "disk2"),
            "an APFS container on another partition of the disk holding the target",
        ),
        (
            device("sdd2", "sdd"),
            device("dm-2", "dm-2"),
            "another partition of the disk under the device being repaired",
        ),
        (
            device("sde1", "sde"),
            device("sda", "sda"),
            "no layers and an unrelated disk",
        ),
    ] {
        assert_eq!(layered_refusal(&journal, &target, &lower), None, "{why}");
    }

    assert_eq!(
        layered_refusal(
            &device("disk2s1", "disk2"),
            &device("disk2", "disk2"),
            &lower
        ),
        Some(JournalLocationError::OnTargetPartition),
        "a direct relationship keeps its own refusal"
    );

    // A topology that loops (a damaged or racing registry) still ends.
    let looped = [
        ("dm-5", vec![device("dm-6", "dm-6")]),
        ("dm-6", vec![device("dm-5", "dm-5")]),
    ];
    assert_eq!(
        layered_refusal(
            &device("dm-5", "dm-5"),
            &device("sda", "sda"),
            &under(&looped)
        ),
        None
    );
}

#[test]
fn a_sysfs_block_tree_lists_the_slaves_under_a_layered_device() {
    let root = PathBuf::from(common::temp_image_path("journal_location_sysfs_slaves"));
    let _ = std::fs::remove_dir_all(&root);
    let sda = root.join("devices/pci0000:00/block/sda");
    std::fs::create_dir_all(sda.join("sda1")).unwrap();
    std::fs::write(sda.join("sda1/partition"), "1\n").unwrap();
    let sdb = root.join("devices/pci0000:00/block/sdb");
    std::fs::create_dir_all(&sdb).unwrap();
    let dm0 = root.join("devices/virtual/block/dm-0");
    std::fs::create_dir_all(dm0.join("slaves")).unwrap();
    std::os::unix::fs::symlink(sda.join("sda1"), dm0.join("slaves/sda1")).unwrap();
    std::os::unix::fs::symlink(&sdb, dm0.join("slaves/sdb")).unwrap();
    std::fs::create_dir_all(root.join("class/block")).unwrap();
    std::os::unix::fs::symlink(&dm0, root.join("class/block/dm-0")).unwrap();
    std::os::unix::fs::symlink(&sda, root.join("class/block/sda")).unwrap();

    let mut slaves = sysfs_lower_devices(&root, "dm-0");
    slaves.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(
        slaves,
        vec![device("sda1", "sda"), device("sdb", "sdb")],
        "a device-mapper device's slaves, a partition resolved to its disk"
    );
    assert_eq!(
        sysfs_lower_devices(&root, "sda"),
        Vec::new(),
        "a disk with no slaves directory is on nothing"
    );
    assert_eq!(
        sysfs_lower_devices(&root, "nvme9n1"),
        Vec::new(),
        "a device sysfs does not list is on nothing"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
