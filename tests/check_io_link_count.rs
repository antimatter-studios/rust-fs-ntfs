//! `check_io` compares each base record's hard-link count with its
//! `$FILE_NAME` attributes (#583).
//!
//! The header's count is what Windows trusts when it takes a name away:
//! one too high and the last unlink leaves an orphan no directory names;
//! one too low and it frees a file another name still reaches. chkdsk
//! recounts the names and reports either. `check_io` read neither.
//!
//! Each case changes one count on a volume `check_io` first reports
//! clean, and requires a `LinkCountDrift` for that record. Volumes Windows
//! wrote, and its chkdsk left, are checked too: the count must not be
//! called wrong where Windows itself wrote it.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::fsck::{self, CheckFinding};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{mft_io, write};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const VOL_SIZE: u64 = 16 * 1024 * 1024;
const LINK_COUNT: usize = 0x12;

/// A fresh volume holding `/f`, and `/g` a second name for it when
/// `linked`, that `check_io` reports clean. Returns the image and `/f`'s
/// record.
fn volume_with_a_file(tag: &str, linked: bool) -> (String, u64) {
    let img = common::temp_image_path(format!("checklinks_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let rec = write::create_file_io(&mut io, "/", "f").expect("create /f");
    if linked {
        write::link_io(&mut io, "/f", "/", "g").expect("link /g to /f");
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let report = fsck::check_io(&mut io).expect("check the untouched volume");
    assert!(
        report.findings.is_empty(),
        "the untouched volume is not clean: {:?}",
        report.findings
    );
    (img, rec)
}

fn set_link_count(img: &str, rec: u64, links: u16) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    mft_io::update_mft_record_io(&mut io, rec, |r| {
        r[LINK_COUNT..LINK_COUNT + 2].copy_from_slice(&links.to_le_bytes());
        Ok(())
    })
    .expect("set the link count");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

fn drift(img: &str) -> Vec<CheckFinding> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    fsck::check_io(&mut io)
        .expect("check")
        .findings
        .into_iter()
        .filter(|f| matches!(f, CheckFinding::LinkCountDrift { .. }))
        .collect()
}

#[test]
fn a_link_count_above_the_names_is_reported() {
    let (img, rec) = volume_with_a_file("above", false);
    set_link_count(&img, rec, 2);
    assert_eq!(
        drift(&img),
        vec![CheckFinding::LinkCountDrift {
            record: rec,
            links: 2,
            names: 1,
        }]
    );
    std::fs::remove_file(&img).ok();
}

#[test]
fn a_link_count_below_the_names_is_reported() {
    let (img, rec) = volume_with_a_file("below", true);
    set_link_count(&img, rec, 1);
    assert_eq!(
        drift(&img),
        vec![CheckFinding::LinkCountDrift {
            record: rec,
            links: 1,
            names: 2,
        }]
    );
    std::fs::remove_file(&img).ok();
}

#[test]
fn a_link_count_of_zero_on_a_named_file_is_reported() {
    let (img, rec) = volume_with_a_file("zero", false);
    set_link_count(&img, rec, 0);
    assert_eq!(
        drift(&img),
        vec![CheckFinding::LinkCountDrift {
            record: rec,
            links: 0,
            names: 1,
        }]
    );
    std::fs::remove_file(&img).ok();
}

fn no_link_count_drift(img: &str, name: &str) {
    let found = drift(img);
    assert!(
        found.is_empty(),
        "{name}, which Windows wrote, has link counts check_io calls wrong: {found:?}"
    );
}

/// Graded by Windows: the volumes its chkdsk left after recovering an
/// interrupted log, and the ones the native-read fixtures build has
/// Windows write (an `$ATTRIBUTE_LIST`, compressed files, symlinks).
#[test]
fn volumes_windows_wrote_have_no_link_count_drift() {
    let mut recovered: Vec<String> = std::fs::read_dir("test-disks")
        .expect("list test-disks")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".recovered.img.gz"))
        .collect();
    recovered.sort();
    assert!(
        !recovered.is_empty(),
        "no test-disks/*.recovered.img.gz; they are committed with the repository"
    );
    for name in &recovered {
        let img = common::temp_image_path(format!("checklinks_{}", name.replace('.', "_")));
        let out = File::create(&img).expect("create image copy");
        let status = Command::new("gzip")
            .args(["-dc", &format!("test-disks/{name}")])
            .stdout(Stdio::from(out))
            .status()
            .expect("run gzip");
        assert!(status.success(), "decompress test-disks/{name}");
        no_link_count_drift(&img, name);
        std::fs::remove_file(&img).ok();
    }
    for fixture in [
        "ntfs-attrlist.img",
        "ntfs-compressed.img",
        "ntfs-symlink.img",
        "ntfs-cli-read.img",
    ] {
        let path = format!("test-disks/{fixture}");
        assert!(
            Path::new(&path).exists(),
            "missing {path}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
        );
        no_link_count_drift(&path, fixture);
    }
}
