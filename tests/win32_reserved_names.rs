//! A name is created exactly when Windows would create it (#498).
//!
//! Win32 path parsing turns the reserved DOS device names (`CON`, `NUL`,
//! `COM1` and the rest, and some of them followed by an extension) into
//! devices, so a file the crate wrote under one of them could not be
//! opened, renamed or deleted by an ordinary Windows program. The crate
//! refuses exactly the names Windows refuses and creates every name
//! Windows creates.
//!
//! Graded by Windows: `test-disks/win32-name-verdicts.tsv`, which
//! `test-disks/measure-win32-names.ps1` writes from what CreateFileW did
//! with each name on the Windows that ran it. Which spellings are devices
//! has changed between Windows releases, so the verdicts are measured
//! rather than written down here.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::error::Kind;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write;
use std::fs::File;
use std::path::Path;

const VOL_SIZE: u64 = 32 * 1024 * 1024;
const VERDICTS: &str = "test-disks/win32-name-verdicts.tsv";

/// (name, whether Windows created a file of exactly that name).
fn verdicts() -> Vec<(String, bool)> {
    assert!(
        Path::new(VERDICTS).exists(),
        "missing {VERDICTS}; run test-disks/measure-win32-names.ps1 on Windows"
    );
    let text = std::fs::read_to_string(VERDICTS).expect("read the verdicts");
    let verdicts: Vec<(String, bool)> = text
        .lines()
        .map(|line| {
            let (name, verdict) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("{VERDICTS}: no tab in {line:?}"));
            let created = match verdict {
                "file" => true,
                "refused" => false,
                other => panic!("{VERDICTS}: verdict {other:?} for {name:?}"),
            };
            (name.to_string(), created)
        })
        .collect();
    assert!(
        verdicts.iter().any(|(_, created)| *created)
            && verdicts.iter().any(|(_, created)| !*created),
        "{VERDICTS} holds only one verdict, so it measured nothing"
    );
    verdicts
}

/// What a create of each measured name did, by `create`, in a directory of
/// its own so case variants do not collide. A refusal must be
/// `Kind::Invalid`, as the other names Windows cannot address are.
fn disagreements(
    what: &str,
    create: fn(&mut PathIo, &str, &str) -> Result<(), fs_ntfs::error::Error>,
) -> Vec<String> {
    let img = common::temp_image_path(format!("win32names_{what}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let mut wrong = Vec::new();
    for (i, (name, windows_created)) in verdicts().iter().enumerate() {
        let dir = format!("d{i}");
        write::mkdir_io(&mut io, "/", &dir).expect("mkdir");
        let got = create(&mut io, &format!("/{dir}"), name);
        match (&got, windows_created) {
            (Ok(()), true) => {}
            (Err(e), false) if e.kind() == Kind::Invalid => {}
            (Ok(()), false) => wrong.push(format!("{what} {name:?}: created; Windows refuses it")),
            (Err(e), true) => wrong.push(format!("{what} {name:?}: {e}; Windows creates it")),
            (Err(e), false) => wrong.push(format!(
                "{what} {name:?}: refused with {:?} ({e}), not Invalid",
                e.kind()
            )),
        }
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    std::fs::remove_file(&img).ok();
    wrong
}

#[test]
fn a_file_is_created_exactly_when_windows_would_create_it() {
    let wrong = disagreements("create", |io, dir, name| {
        write::create_file_io(io, dir, name).map(drop)
    });
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_directory_is_made_exactly_when_windows_would_create_its_name() {
    let wrong = disagreements("mkdir", |io, dir, name| {
        write::mkdir_io(io, dir, name).map(drop)
    });
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_rename_lands_exactly_when_windows_would_create_the_new_name() {
    let wrong = disagreements("rename", |io, dir, name| {
        write::create_file_io(io, dir, "plain")?;
        write::rename_io(io, &format!("{dir}/plain"), name)
    });
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
