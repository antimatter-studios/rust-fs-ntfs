//! NTFS volumes Windows was writing to when they were captured, and what
//! Windows recovered from them: the replay oracle #137 needs (#366).
//!
//! HOW THEY WERE MADE. `test-disks/capture-interrupted-logfile.ps1`, run by
//! `.github/workflows/logfile-oracle.yml` on `windows-latest` (run
//! 36668256057, 2026-09-30), formatted a fresh fixed VHD NTFS (4 KiB
//! clusters, label ORACLE366) and ran a workload on it that created,
//! renamed and deleted files. While it ran, VSS shadow copies of the VHD's
//! host volume were taken: point-in-time images of every block the inner
//! volume had written, and nothing it still held in memory -- what a power
//! cut at that instant leaves. Nothing was synthesised; no byte here was
//! written by this crate.
//!
//! * `windows-interrupted-{1,6}.img.gz`: the NTFS partition of snapshots 1
//!   (after 244 workload operations) and 6 (after 1,982), cut from the
//!   shadow's VHD at byte 65536. Never attached again after capture.
//! * `windows-interrupted-{1,6}.recovered.img.gz`: a separate copy of each,
//!   which Windows attached -- running its `$LogFile` restart pass -- and
//!   detached again.
//!   Windows' read-only chkdsk found no problems on either afterwards, and
//!   `fsutil dirty query` called both not dirty.
//! * `windows-interrupted-{1,6}.recovered.manifest`: every workload file on
//!   the recovered copy, as Windows listed it: path, size, and Windows' own
//!   `Get-FileHash` SHA-256.
//!
//! Every partition's SHA-256 is in [`PARTITIONS`] and checked on unpacking.
//!
//! WHAT WINDOWS' REPLAY CHANGED, read by ntfs-3g without replaying: on
//! snapshot 1 ntfs-3g cannot mount the volume at all ("Failed to open
//! $Secure"), and all 206 files exist only after recovery; on snapshot 6,
//! 195 names ntfs-3g sees are gone after recovery and 393 appear. So these
//! logs hold redo work, and the manifests are what a replay must produce.
//!
//! Until this crate replays (#137), what can be checked is that it knows
//! the pre-images' logs hold work, and that it reads what Windows recovered
//! exactly as Windows does. Neither pre-image has its dirty flag set, so
//! `fsck` refusing them is #376's to prove.

mod common;

use fs_ntfs::facade::{FileType, Filesystem};
use fs_ntfs::fsck;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::process::{Command, Stdio};

/// The SHA-256 of each decompressed partition, as captured.
const PARTITIONS: [(&str, &str); 4] = [
    (
        "windows-interrupted-1",
        "7236ffe64f5532b6f5cd976c1fa81c66e6be6cd31cfb8c22bc0bb588e8fad54f",
    ),
    (
        "windows-interrupted-1.recovered",
        "ba2efd60cf3de25f5e87e1adf11581826ccb3d3cfbb6c0fd5ac015bf8c308d2b",
    ),
    (
        "windows-interrupted-6",
        "5899daffd9e67bff8446c24647a4712092611cb00e20818ee771c20d63374238",
    ),
    (
        "windows-interrupted-6.recovered",
        "b6f8928649ec6239ac3e6fb40646c9bcd7128dca033a2da14d761a2f90fdc797",
    ),
];

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn unpack(name: &str) -> String {
    let path = common::temp_image_path(name.replace('.', "_"));
    let out = File::create(&path).expect("create image copy");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        133_103_616,
        "{name} is the 133,103,616-byte partition"
    );
    let want = PARTITIONS
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("{name} has no recorded SHA-256"))
        .1;
    assert_eq!(
        hex_sha256(&std::fs::read(&path).unwrap()),
        want,
        "{name} is not the partition that was captured"
    );
    path
}

/// Windows' manifest: path -> (size, SHA-256).
fn manifest(k: u32) -> BTreeMap<String, (u64, String)> {
    let path = format!("test-disks/windows-interrupted-{k}.recovered.manifest");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 3, "{path}: {l:?}");
            (f[0].to_string(), (f[1].parse().unwrap(), f[2].to_string()))
        })
        .collect()
}

/// Every file under the workload's `dNNN` directories, as this crate reads
/// it: path -> (size read, SHA-256 of the bytes).
fn walk(img: &str) -> BTreeMap<String, (u64, String)> {
    let fs = Filesystem::mount(img).unwrap_or_else(|e| panic!("mount {img}: {e:?}"));
    let mut out = BTreeMap::new();
    for dir in fs.read_dir("/").expect("read /") {
        let workload = dir.name.len() == 4
            && dir.name.starts_with('d')
            && dir.name[1..].bytes().all(|b| b.is_ascii_digit());
        if !workload || dir.file_type != FileType::Directory {
            continue;
        }
        for entry in fs.read_dir(&format!("/{}", dir.name)).expect("read dir") {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let path = format!("{}/{}", dir.name, entry.name);
            let mut buf = vec![0u8; 64 * 1024];
            let n = fs
                .read_file(&format!("/{path}"), 0, &mut buf)
                .unwrap_or_else(|e| panic!("read /{path}: {e:?}"));
            let hash = hex_sha256(&buf[..n]);
            out.insert(path, (n as u64, hash));
        }
    }
    out
}

fn ntfs3g(args: &[&str]) -> (bool, String) {
    let out = Command::new(args[0])
        .args(&args[1..])
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "{} could not be run ({e}): install ntfs-3g (`apt-get install ntfs-3g`)",
                args[0]
            )
        });
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_logs_windows_left_mid_write_are_read_as_holding_work() {
    for k in [1, 6] {
        let img = unpack(&format!("windows-interrupted-{k}"));
        let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
        let state = fsck::logfile_state_io(&mut io).expect("read $LogFile");
        assert!(state.needs_replay(), "snapshot {k}: {state:?}");

        // The independent reading: ntfs-3g refuses both read-write. On
        // snapshot 6 it reads the log and calls the volume unclean; on
        // snapshot 1 it gets no further than `$Secure`, which Windows had
        // logged but not yet written home.
        let (ok, err) = ntfs3g(&["ntfs-3g.probe", "--readwrite", &img]);
        let why = if k == 1 {
            "Failed to open $Secure"
        } else {
            "unclean"
        };
        assert!(
            !ok && err.contains(why),
            "snapshot {k}: ntfs-3g.probe succeeded={ok}, said {err}"
        );
    }
}

#[test]
fn what_windows_recovered_reads_here_as_windows_lists_it() {
    for k in [1, 6] {
        let img = unpack(&format!("windows-interrupted-{k}.recovered"));
        let want = manifest(k);
        assert!(
            want.len() > 200,
            "snapshot {k}: the manifest lost its lines"
        );
        let got = walk(&img);
        let missing: Vec<_> = want
            .keys()
            .filter(|p| !got.contains_key(*p))
            .take(5)
            .collect();
        let extra: Vec<_> = got
            .keys()
            .filter(|p| !want.contains_key(*p))
            .take(5)
            .collect();
        let differ: Vec<_> = want
            .iter()
            .filter(|(p, v)| got.get(*p).is_some_and(|g| g != *v))
            .take(5)
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty() && differ.is_empty(),
            "snapshot {k}: {} files here, {} in Windows' manifest; missing {missing:?}, \
             extra {extra:?}, differing {differ:?}",
            got.len(),
            want.len()
        );
        // And the recovered copy's log is one this crate calls clean.
        let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
        let state = fsck::logfile_state_io(&mut io).expect("read $LogFile");
        assert!(!state.needs_replay(), "snapshot {k} recovered: {state:?}");
    }
}

/// The oracle is only worth committing if the replay did something: on
/// the pre-images ntfs-3g, which does not replay, sees a different set of
/// files from Windows after recovery -- or cannot mount the volume at all.
#[test]
fn windows_replay_changed_what_the_volumes_hold() {
    let (mounted, err) = ntfs3g(&["ntfsls", "-f", &unpack("windows-interrupted-1")]);
    assert!(
        !mounted,
        "snapshot 1 should not mount without a replay; ntfs-3g said {err}"
    );

    let img = unpack("windows-interrupted-6");
    let want: std::collections::BTreeSet<String> = manifest(6).into_keys().collect();
    let mut seen = std::collections::BTreeSet::new();
    for dir in 0..64 {
        let out = Command::new("ntfsls")
            .args(["-f", "-p", &format!("/d{dir:03}"), &img])
            .output()
            .expect("run ntfsls");
        for name in String::from_utf8_lossy(&out.stdout).lines() {
            if name.ends_with(".bin") || name.ends_with(".renamed") {
                seen.insert(format!("d{dir:03}/{name}"));
            }
        }
    }
    assert!(!seen.is_empty(), "ntfs-3g listed nothing on snapshot 6");
    let gone = seen.difference(&want).count();
    let appeared = want.difference(&seen).count();
    assert!(
        gone > 0 && appeared > 0,
        "Windows' replay should both remove and add names on snapshot 6 \
         (removed {gone}, added {appeared})"
    );
}
