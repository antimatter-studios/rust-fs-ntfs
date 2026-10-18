//! An in-place repair opens its device exclusively, and a device another
//! holder has locked is refused with nothing written (#920, part of #713).
//!
//! The device here is a copy of `windows-interrupted-6`, a volume Windows
//! was writing to when it was captured (see
//! `tests/windows_interrupted_fixture.rs`). The other holder is a separate
//! open of the same file that takes an exclusive lock. `flock` locks belong
//! to the open file description, not the process, so a second open in this
//! process conflicts exactly as another process's open does.

mod common;

use std::fs::File;
use std::process::Command;

use fs_ntfs::block_io::PathIo;
use sha2::{Digest, Sha256};

/// Unpack a `test-disks/` image to a scratch path.
fn unpack(name: &str) -> String {
    let img = common::temp_image_path(format!("open_exclusive_{}", name.replace(['.', '-'], "_")));
    let out = File::create(&img).expect("create");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(out)
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    img
}

fn sha256(path: &str) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).expect("read image")).to_vec()
}

#[test]
fn a_device_another_holder_locked_is_refused_with_nothing_written() {
    let img = unpack("windows-interrupted-6");
    let before = sha256(&img);

    let holder = File::options()
        .read(true)
        .write(true)
        .open(&img)
        .expect("open holder");
    holder.try_lock().expect("holder takes the exclusive lock");

    let err = match PathIo::open_exclusive(img.as_ref()) {
        Ok(_) => panic!("open_exclusive succeeded on a device another holder locked"),
        Err(e) => e,
    };
    assert!(
        err.contains("held by another process"),
        "refusal names the other holder: {err}"
    );
    assert_eq!(sha256(&img), before, "the refused open wrote nothing");

    drop(holder);
    std::fs::remove_file(&img).unwrap();
}

#[test]
fn the_lock_is_released_with_its_holder_and_held_against_a_second_exclusive_open() {
    let img = unpack("windows-interrupted-6");
    let before = sha256(&img);

    let holder = File::options()
        .read(true)
        .write(true)
        .open(&img)
        .expect("open holder");
    holder.try_lock().expect("holder takes the exclusive lock");
    drop(holder);

    let first = PathIo::open_exclusive(img.as_ref()).expect("open once the holder is gone");
    let err = match PathIo::open_exclusive(img.as_ref()) {
        Ok(_) => panic!("a second open_exclusive succeeded while the first is alive"),
        Err(e) => e,
    };
    assert!(
        err.contains("held by another process"),
        "refusal names the other holder: {err}"
    );
    drop(first);

    PathIo::open_exclusive(img.as_ref()).expect("open again once the first is dropped");
    assert_eq!(sha256(&img), before, "no open wrote anything");
    std::fs::remove_file(&img).unwrap();
}
