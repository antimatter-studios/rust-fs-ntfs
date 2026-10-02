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
//! exactly as Windows does, and that `fsck` refuses the pre-images and
//! writes nothing, though neither has its dirty flag set (#376).

mod common;

use fs_ntfs::facade::{FileType, Filesystem};
use fs_ntfs::fsck;
use fs_ntfs::{
    fs_ntfs_mount, fs_ntfs_mount_rw_with_fs_core_device, fs_ntfs_mount_with_fs_core_device,
    fs_ntfs_umount,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::CString;
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

/// The last LSN each pre-image's `$LogFile` holds, read by an
/// independent reader written for #137 (not this crate's): the newest
/// `last_end_lsn` of any record page whose update sequence checks out,
/// tail copies included.
const LOG_END: [(u32, u64); 2] = [(1, 0x11_d767), (6, 0x2b_48b3)];

/// Where, in each pre-image, a `$LogFile` record page the replay needs
/// sits: inside the walk from the oldest dirty page to the log's end, and
/// in no tail copy.
const NEEDED_LOG_PAGE: [(u32, u64); 2] = [(1, 0x8_0000), (6, 0x10_0000)];

#[test]
fn fsck_refuses_a_log_it_cannot_read_to_the_end_and_writes_nothing() {
    // A torn record page in the middle of the work: the log cannot be
    // replayed in full, so nothing may be replayed at all. A partial redo
    // would leave metadata that matches no state the volume was ever in.
    for (k, page) in NEEDED_LOG_PAGE {
        let img = unpack(&format!("windows-interrupted-{k}"));
        let logfile_lcn = 10_318u64; // $LogFile's one run, in both images
        let at = logfile_lcn * 4096 + page + 510; // the first sector's USN copy
        let mut bytes = std::fs::read(&img).unwrap();
        bytes[at as usize] ^= 0xFF;
        std::fs::write(&img, &bytes).unwrap();

        let err = fsck::fsck(&img).expect_err("fsck must not act on a log it cannot read");
        assert!(err.contains("$LogFile"), "snapshot {k}: {err}");
        assert!(
            std::fs::read(&img).unwrap() == bytes,
            "snapshot {k}: a refused fsck wrote to the volume"
        );
    }
}

#[test]
fn fsck_replays_a_volume_windows_left_mid_write_to_what_windows_recovered() {
    // Neither volume's dirty flag is set: Windows 8 and later record an
    // unclean shutdown in the log alone. Windows' own restart pass turned
    // each pre-image into the `.recovered` image; fsck must reach the same
    // metadata from the same log (#137).
    for (k, log_end) in LOG_END {
        let img = unpack(&format!("windows-interrupted-{k}"));
        fsck::fsck(&img).unwrap_or_else(|e| panic!("snapshot {k}: fsck: {e}"));

        // What Windows lists after recovery, file for file.
        let want = manifest(k);
        let got = walk(&img);
        assert!(
            got == want,
            "snapshot {k}: {} files after replay, {} in Windows' manifest",
            got.len(),
            want.len()
        );

        // The log no longer holds work, and the volume opens for writing.
        let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
        let state = fsck::logfile_state_io(&mut io).expect("read $LogFile");
        assert!(
            !state.needs_replay(),
            "snapshot {k} after replay: {state:?}"
        );
        drop(io);
        Filesystem::mount_rw(&img).unwrap_or_else(|e| panic!("snapshot {k}: mount_rw: {e}"));

        // A third reader: ntfs-3g refused both pre-images read-write (one
        // could not even open $Secure); it opens the replayed volumes.
        let (ok, err) = ntfs3g(&["ntfs-3g.probe", "--readwrite", &img]);
        assert!(
            ok,
            "snapshot {k}: ntfs-3g.probe --readwrite after replay: {err}"
        );

        let pre = Image::read(&unpack(&format!("windows-interrupted-{k}")));
        let win = Image::read(&unpack(&format!("windows-interrupted-{k}.recovered")));
        let ours = Image::read(&img);
        oracle::same_metadata_as_windows(k, &pre, &ours, &win, log_end);
    }
}

#[test]
fn a_read_write_mount_refuses_a_volume_windows_left_mid_write_and_writes_nothing() {
    // The dirty flag is clear on both, so a guard that reads only the flag
    // opens them for writing, and the first write -- the version upgrade --
    // lands on metadata whose committed changes are still in the log
    // (#137). A read-only mount stays allowed.
    for k in [1, 6] {
        let img = unpack(&format!("windows-interrupted-{k}"));
        let before = std::fs::read(&img).unwrap();

        Filesystem::mount(&img).expect("a read-only mount is allowed");
        let err = Filesystem::mount_rw(&img).expect_err("mount_rw over a log holding work");
        assert!(err.0.contains("$LogFile"), "snapshot {k}: {err}");

        let c_path = CString::new(img.as_str()).unwrap();
        assert!(
            fs_ntfs_mount(c_path.as_ptr()).is_null(),
            "snapshot {k}: fs_ntfs_mount opened a log holding work"
        );
        assert!(
            last_error().contains("$LogFile"),
            "snapshot {k}: {}",
            last_error()
        );

        let dev = unsafe { fs_core::ffi::fs_core_file_open(c_path.as_ptr(), true) };
        assert!(!dev.is_null(), "snapshot {k}: open fs-core device");
        let ro = fs_ntfs_mount_with_fs_core_device(dev);
        assert!(!ro.is_null(), "snapshot {k}: read-only: {}", last_error());
        fs_ntfs_umount(ro);
        let rw = fs_ntfs_mount_rw_with_fs_core_device(dev);
        let why = last_error();
        unsafe { fs_core::ffi::fs_core_device_close(dev) };
        assert!(
            rw.is_null(),
            "snapshot {k}: fs_ntfs_mount_rw_with_fs_core_device opened a log holding work"
        );
        assert!(why.contains("$LogFile"), "snapshot {k}: {why}");

        assert!(
            std::fs::read(&img).unwrap() == before,
            "snapshot {k}: a refused read-write mount wrote to the volume"
        );
    }
}

fn last_error() -> String {
    let p = fs_ntfs::fs_ntfs_last_error();
    if p.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
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

/// A raw reading of an NTFS image, written for this comparison from
/// MS-FSCC and nothing in this crate: the boot sector, `$MFT`'s runs, and
/// update-sequence fixups.
struct Image {
    bytes: Vec<u8>,
    cluster: u64,
    record: u64,
    mft_runs: Vec<(u64, u64)>,
}

fn le(b: &[u8], at: usize, n: usize) -> u64 {
    let mut v = 0u64;
    for i in (0..n).rev() {
        v = (v << 8) | u64::from(b[at + i]);
    }
    v
}

/// Mapping pairs of the attribute at `a` in `rec`: (lcn, clusters) runs.
fn runs_of(rec: &[u8], a: usize) -> Vec<(u64, u64)> {
    let mut at = a + le(rec, a + 0x20, 2) as usize;
    let end = a + le(rec, a + 4, 4) as usize;
    let mut lcn = 0i64;
    let mut out = Vec::new();
    while at < end && rec[at] != 0 {
        let (ln, on) = ((rec[at] & 15) as usize, (rec[at] >> 4) as usize);
        let len = le(rec, at + 1, ln);
        if on > 0 {
            let raw = le(rec, at + 1 + ln, on);
            let shift = 64 - 8 * on as u32;
            lcn += ((raw << shift) as i64) >> shift;
            out.push((lcn as u64, len));
        }
        at += 1 + ln + on;
    }
    out
}

/// Attributes of a record (fixups undone): (offset, type, non-resident).
fn attrs(rec: &[u8]) -> Vec<(usize, u32, bool)> {
    let mut at = le(rec, 0x14, 2) as usize;
    let mut out = Vec::new();
    while at + 8 <= rec.len() {
        let t = le(rec, at, 4) as u32;
        let len = le(rec, at + 4, 4) as usize;
        if t == 0xFFFF_FFFF || len == 0 || at + len > rec.len() {
            break;
        }
        out.push((at, t, rec[at + 8] != 0));
        at += len;
    }
    out
}

impl Image {
    fn read(path: &str) -> Image {
        let bytes = std::fs::read(path).unwrap();
        let cluster = le(&bytes, 0x0B, 2) * le(&bytes, 0x0D, 1);
        let c = bytes[0x40] as i8;
        let record = if c > 0 { c as u64 * cluster } else { 1 << -c };
        let mut img = Image {
            bytes,
            cluster,
            record,
            mft_runs: Vec::new(),
        };
        let mft_lcn = le(&img.bytes, 0x30, 8);
        let at = (mft_lcn * cluster) as usize;
        let rec0 = fixed(&img.bytes[at..at + record as usize], b"FILE").unwrap();
        let data = attrs(&rec0)
            .into_iter()
            .find(|&(_, t, nr)| t == 0x80 && nr)
            .unwrap();
        img.mft_runs = runs_of(&rec0, data.0);
        img
    }

    fn cluster_bytes(&self, lcn: u64, n: u64) -> &[u8] {
        &self.bytes[(lcn * self.cluster) as usize..((lcn + n) * self.cluster) as usize]
    }

    fn records(&self) -> u64 {
        self.mft_runs.iter().map(|r| r.1).sum::<u64>() * self.cluster / self.record
    }

    /// Record `n` as stored, before fixups.
    fn raw_record(&self, n: u64) -> &[u8] {
        let mut off = n * self.record;
        for &(lcn, len) in &self.mft_runs {
            if off < len * self.cluster {
                let at = (lcn * self.cluster + off) as usize;
                return &self.bytes[at..at + self.record as usize];
            }
            off -= len * self.cluster;
        }
        panic!("record {n} is past $MFT");
    }

    fn record(&self, n: u64) -> Option<Vec<u8>> {
        fixed(self.raw_record(n), b"FILE")
    }

    /// The unnamed stream of type `t` of record `n`, non-resident.
    fn stream(&self, n: u64, t: u32) -> Vec<u8> {
        let rec = self.record(n).unwrap();
        let (a, _, _) = attrs(&rec)
            .into_iter()
            .find(|&(_, ty, nr)| ty == t && nr)
            .unwrap();
        let size = le(&rec, a + 0x30, 8) as usize;
        let mut out = Vec::new();
        for (lcn, len) in runs_of(&rec, a) {
            out.extend_from_slice(self.cluster_bytes(lcn, len));
        }
        out.truncate(size);
        out
    }
}

/// Fixups undone, or `None` for a block that is not `magic` or is torn.
fn fixed(raw: &[u8], magic: &[u8; 4]) -> Option<Vec<u8>> {
    if &raw[..4] != magic {
        return None;
    }
    let mut b = raw.to_vec();
    let (uo, uc) = (le(&b, 4, 2) as usize, le(&b, 6, 2) as usize);
    for i in 1..uc {
        let end = i * 512 - 2;
        if b[end..end + 2] != b[uo..uo + 2] {
            return None;
        }
        b[end] = b[uo + 2 * i];
        b[end + 1] = b[uo + 2 * i + 1];
    }
    Some(b)
}

/// A FILE record or INDX block as content: fixups undone, the LSN and
/// the update sequence number (which every write moves) blanked, and only
/// the bytes in use.
fn content(raw: &[u8], magic: &[u8; 4]) -> Option<Vec<u8>> {
    let mut b = fixed(raw, magic)?;
    b[8..16].fill(0);
    let uo = le(&b, 4, 2) as usize;
    b[uo..uo + 2].fill(0);
    let used = if magic == b"FILE" {
        le(&b, 0x18, 4) as usize
    } else {
        0x18 + le(&b, 0x1C, 4) as usize
    };
    b.truncate(used.min(raw.len()));
    Some(b)
}

mod oracle {
    use super::*;
    use std::collections::BTreeSet;

    /// What Windows wrote AFTER its restart pass, which no replay of this
    /// log can produce: every record whose LSN is past the log's last
    /// record (new log records, written once the volume was mounted), and
    /// the transactional-NTFS metadata under `$Extend\$RmMetadata`, which
    /// Windows' resource manager rewrites at mount whether or not there
    /// was anything to replay.
    fn windows_after_recovery(win: &Image, log_end: u64) -> BTreeSet<u64> {
        let mut out = BTreeSet::new();
        let mut parent = std::collections::BTreeMap::new();
        for n in 0..win.records() {
            let Some(rec) = win.record(n) else { continue };
            if le(&rec, 8, 8) > log_end {
                out.insert(n);
            }
            for (a, t, nr) in attrs(&rec) {
                if t == 0x30 && !nr {
                    let v = a + le(&rec, a + 0x14, 2) as usize;
                    let name_len = rec[v + 0x40] as usize;
                    let name: Vec<u16> = (0..name_len)
                        .map(|i| le(&rec, v + 0x42 + 2 * i, 2) as u16)
                        .collect();
                    parent.insert(n, (le(&rec, v, 6), String::from_utf16_lossy(&name)));
                }
            }
        }
        let mut rm: BTreeSet<u64> = parent
            .iter()
            .filter(|(_, (_, name))| name == "$RmMetadata")
            .map(|(n, _)| *n)
            .collect();
        assert!(!rm.is_empty(), "no $RmMetadata in Windows' image");
        loop {
            let more: Vec<u64> = parent
                .iter()
                .filter(|(n, (p, _))| rm.contains(p) && !rm.contains(n))
                .map(|(n, _)| *n)
                .collect();
            if more.is_empty() {
                break;
            }
            rm.extend(more);
        }
        out.extend(rm);
        out
    }

    pub fn same_metadata_as_windows(k: u32, pre: &Image, ours: &Image, win: &Image, log_end: u64) {
        let after = windows_after_recovery(win, log_end);

        // Every MFT record, as content.
        let mut replayed = 0;
        let mut differ = Vec::new();
        for n in 0..win.records() {
            if after.contains(&n) {
                continue;
            }
            let w = content(win.raw_record(n), b"FILE");
            if content(pre.raw_record(n), b"FILE") != w {
                replayed += 1;
            }
            if content(ours.raw_record(n), b"FILE") != w {
                differ.push(n);
            }
        }
        assert!(
            differ.is_empty(),
            "snapshot {k}: {} MFT records differ from what Windows recovered: {:?}",
            differ.len(),
            &differ[..differ.len().min(20)]
        );
        assert!(
            replayed > 150,
            "snapshot {k}: Windows' replay changed only {replayed} records; the comparison \
             has lost its subject"
        );

        // Every index block of every directory Windows did not touch after.
        let mut blocks = 0;
        for n in 0..win.records() {
            if after.contains(&n) {
                continue;
            }
            let Some(rec) = win.record(n) else { continue };
            for (a, t, nr) in attrs(&rec) {
                if t != 0xA0 || !nr {
                    continue;
                }
                for (lcn, len) in runs_of(&rec, a) {
                    for c in lcn..lcn + len {
                        let w = content(win.cluster_bytes(c, 1), b"INDX");
                        if w.is_none() {
                            continue;
                        }
                        blocks += 1;
                        assert!(
                            content(ours.cluster_bytes(c, 1), b"INDX") == w,
                            "snapshot {k}: record {n}'s index block at LCN {c} differs from \
                             what Windows recovered"
                        );
                    }
                }
            }
        }
        assert!(
            blocks >= 3,
            "snapshot {k}: only {blocks} index blocks compared"
        );

        // $MFT's own bitmap, exactly.
        assert!(
            ours.stream(0, 0xB0) == win.stream(0, 0xB0),
            "snapshot {k}: $MFT's bitmap differs from what Windows recovered"
        );

        // $Bitmap: any cluster that differs is one Windows allocated, after
        // recovery, to a record it wrote after recovery.
        let (o, w, p) = (
            ours.stream(6, 0x80),
            win.stream(6, 0x80),
            pre.stream(6, 0x80),
        );
        let mut owned_after = BTreeSet::new();
        for &n in &after {
            let Some(rec) = win.record(n) else { continue };
            for (a, _, nr) in attrs(&rec) {
                if nr {
                    for (lcn, len) in runs_of(&rec, a) {
                        owned_after.extend(lcn..lcn + len);
                    }
                }
            }
        }
        let mut changed = 0;
        for c in 0..(w.len() as u64 * 8) {
            let bit = |b: &[u8]| (b[(c / 8) as usize] >> (c % 8)) & 1;
            if bit(&p) != bit(&w) && bit(&o) == bit(&w) {
                changed += 1;
            }
            if bit(&o) != bit(&w) {
                assert!(
                    owned_after.contains(&c),
                    "snapshot {k}: cluster {c} is {} in $Bitmap after replay, {} after Windows' \
                     recovery, and Windows did not allocate it afterwards",
                    bit(&o),
                    bit(&w)
                );
            }
        }
        assert!(
            changed > 1000,
            "snapshot {k}: replay matched only {changed} $Bitmap bits Windows changed"
        );
    }
}
