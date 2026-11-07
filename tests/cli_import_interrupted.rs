//! `fs.ntfs import` killed between the staged file's sync and its rename
//! (#1078): nothing appears at PATH, and the next import of the same PATH
//! exits 0 with the source's SHA-256 and leaves no staging name. ntfs-3g's
//! `ntfsls` and `ntfscat`, run at arm's length, read the volume, not this
//! crate.
//!
//! The kill is the binary's test-only hook (`--features harness`), which
//! aborts the process at that point: no destructor runs, as none would for
//! a SIGKILL or a power cut.

mod common;

use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::{Command, Output};

const PATH: &str = "/d/file.bin";

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn ntfs3g(tool: &str, args: &[&str]) -> Output {
    Command::new(tool).args(args).output().unwrap_or_else(|e| {
        panic!("{tool} could not be run ({e}): install ntfs-3g (`apt-get install ntfs-3g`)")
    })
}

/// The names ntfs-3g lists in /d.
fn listed(img: &str) -> Vec<String> {
    let out = ntfs3g("ntfsls", &["-p", "/d", img]);
    assert!(
        out.status.success(),
        "ntfsls -p /d: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn import(img: &str, src: &str, kill: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"));
    cmd.args(["fs", img, "import", src, PATH]);
    if kill {
        cmd.env("FS_NTFS_IMPORT_KILL_BEFORE_PUBLISH", "1");
    }
    cmd.output().expect("spawn fs.ntfs import")
}

#[test]
fn an_import_killed_before_its_rename_leaves_nothing_and_the_next_one_succeeds() {
    let img = common::temp_image_path("cli_import_killed");
    {
        let f = std::fs::File::create(&img).expect("create");
        f.set_len(64 << 20).expect("size");
    }
    let mut dev = fs_ntfs::block_io::PathIo::open_rw(Path::new(&img)).unwrap();
    fs_ntfs::mkfs::format_filesystem(&mut dev, 64 << 20, 4096, 4096, None, Some(1)).unwrap();
    drop(dev);
    fs_ntfs::write::mkdir(Path::new(&img), "/", "d").expect("mkdir /d");
    let src = format!("{img}.src");
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i * 31 + 7) as u8).collect();
    std::fs::write(&src, &bytes).unwrap();
    let want = hex(Sha256::digest(&bytes));

    let killed = import(&img, &src, true);
    assert!(
        !killed.status.success(),
        "the import was not killed before its rename: {}",
        String::from_utf8_lossy(&killed.stdout)
    );
    assert!(killed.stdout.is_empty(), "a killed import printed a report");
    assert!(
        !listed(&img).iter().any(|name| name == "file.bin"),
        "PATH appeared although the rename never ran: {:?}",
        listed(&img)
    );
    assert!(
        !ntfs3g("ntfscat", &[&img, PATH]).status.success(),
        "ntfscat reads {PATH} after the kill"
    );

    let done = import(&img, &src, false);
    assert_eq!(
        done.status.code(),
        Some(0),
        "the next import: {}",
        String::from_utf8_lossy(&done.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&done.stdout).expect("a JSON report");
    assert_eq!(report["sha256"], want.as_str(), "{report}");
    let cat = ntfs3g("ntfscat", &[&img, PATH]);
    assert!(
        cat.status.success(),
        "ntfscat {PATH}: {}",
        String::from_utf8_lossy(&cat.stderr)
    );
    assert_eq!(hex(Sha256::digest(&cat.stdout)), want, "ntfscat {PATH}");
    assert_eq!(
        listed(&img),
        ["file.bin"],
        "the next import left a staging name behind"
    );

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&img);
}
