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
//! written by this crate. Formatter bootstrap code was subsequently zeroed
//! in primary/backup boot code and `$Boot` sectors 1–15 (#487); BPBs,
//! signatures and every byte of filesystem state remain as captured.
//! `windows-interrupted-bootstrap.json` records original and scrubbed hashes;
//! CI compares the original git blobs byte for byte outside those regions.
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
//! * `windows-interrupted-wrap-{span,boundary,mft-grows}*`: the same three
//!   files for three later captures whose `$LogFile` wrapped between the
//!   last checkpoint and the snapshot. See [`WRAPPED_LOG_END`].
//!
//! * `windows-interrupted-index-vcn*`: the same three files for a capture
//!   whose log holds `SetIndexEntryVcnAllocation` redo. See
//!   [`INDEX_VCN_LOG_END`].
//!
//! * `windows-interrupted-small-cluster*`: the same three files for a
//!   capture on 1 KiB clusters, whose logged pages span several clusters.
//!   See [`SMALL_CLUSTER_LOG_END`].
//!
//! * `windows-interrupted-tail-continuation*`: the same three files for a
//!   capture whose newest log page reached only the tail-copy area. See
//!   [`TAIL_CONTINUATION_LOG_END`].
//!
//! * `windows-interrupted-undo*`: the same three files for a capture whose
//!   log ends inside a transaction, which a replay must roll back. See
//!   [`UNDO_LOG_END`].
//!
//! * `windows-interrupted-open-at-checkpoint*`: the same three files for a
//!   capture whose checkpoint lists a transaction still open. See
//!   [`OPEN_AT_CHECKPOINT_LOG_END`].
//!
//! * `windows-interrupted-quota*`: the same three files for a capture with
//!   quota tracking on, whose log updates `$Quota` entries in place. See
//!   [`QUOTA_LOG_END`].
//!
//! * `windows-interrupted-fresh-log*`: the same three files for a capture
//!   whose last checkpoint is the first after the log was emptied, and
//!   names no start and no tables. See [`FRESH_LOG_END`].
//!
//! * `windows-interrupted-quota-spilled*`: the same three files for a
//!   capture whose `$Quota` index has outgrown its root, so the log updates
//!   its entries in index blocks. See [`QUOTA_SPILLED_LOG_END`].
//!
//! Every partition's SHA-256 is in [`PARTITIONS`] and checked on unpacking.
//!
//! WHAT WINDOWS' REPLAY CHANGED, read by ntfs-3g without replaying: on
//! snapshot 1 ntfs-3g cannot mount the volume at all ("Failed to open
//! $Secure"), and all 206 files exist only after recovery; on snapshot 6,
//! 195 names ntfs-3g sees are gone after recovery and 393 appear. So these
//! logs hold redo work, and the manifests are what a replay must produce.
//!
//! What is checked: this crate knows the pre-images' logs hold work,
//! though neither has its dirty flag set (#376); it reads what Windows
//! recovered exactly as Windows does; and `fsck`, and every read-write
//! mount, replay each pre-image to the metadata Windows' own restart
//! reached, or refuse with nothing written a log they cannot replay in
//! full (#137).

mod common;

use fs_ntfs::facade::{FileType, Filesystem};
use fs_ntfs::fsck;
use fs_ntfs::{
    fs_ntfs_create_file, fs_ntfs_fsck_with_callbacks, fs_ntfs_fsck_with_fs_core_device,
    fs_ntfs_mount, fs_ntfs_mount_replaying_log, fs_ntfs_mount_rw_with_fs_core_device,
    fs_ntfs_mount_rw_with_fs_core_device_replaying_log, fs_ntfs_mount_with_callbacks,
    fs_ntfs_mount_with_callbacks_replaying_log, fs_ntfs_mount_with_fs_core_device, fs_ntfs_umount,
    FsNtfsBlockdevCfg,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::raw::{c_int, c_void};
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// The SHA-256 of each decompressed partition after bootstrap-only removal.
const PARTITIONS: [(&str, &str); 26] = [
    (
        "windows-interrupted-1",
        "c45659e95104eca5f793c3f21061f42705a01783e62d1c7a46c63e4089db0b9a",
    ),
    (
        "windows-interrupted-1.recovered",
        "16beeb97f2177b8abd02287c8803bf4faa460a7ee701ab842f8ef552ef3bc14a",
    ),
    (
        "windows-interrupted-6",
        "7393334b8b7c1faba047d8fd4081d0cd07e9c9de0d59d9e21e6b954845170442",
    ),
    (
        "windows-interrupted-6.recovered",
        "c4b18810f6aa6130073583d8343b355c31a9f4f94cf1740cb59b76af10b50308",
    ),
    (
        "windows-interrupted-wrap-span",
        "4fd202e11e53453f7b00a9f89a9984bc1ad2224a3c2245df8916b9e24a75ac17",
    ),
    (
        "windows-interrupted-wrap-span.recovered",
        "ac543acf876dbd75eef55c62f456cbeab9d26bef72a628d5cbf01fcbe8d9e005",
    ),
    (
        "windows-interrupted-wrap-boundary",
        "12d76f82972eb091b00b29efa89cb61f93563fbf3b7125bd2a22095d85985bc6",
    ),
    (
        "windows-interrupted-wrap-boundary.recovered",
        "31130affa16175885d8f4d886e5221d315e6a1b0aec8c485e977e9eb4ec719c3",
    ),
    (
        "windows-interrupted-wrap-mft-grows",
        "5e04c86858dd03495d7d2127ffe90c9baa74c22eac8614c838874e33b053e02b",
    ),
    (
        "windows-interrupted-wrap-mft-grows.recovered",
        "70bae65f266631ec8bc301a02dcaa88f9122f7cf9f0487da9fd22bfc0e796b3f",
    ),
    (
        "windows-interrupted-index-vcn",
        "b47ab06b0397e86b2076e49560c68f70556fb13140a239c0167795e91d6204bc",
    ),
    (
        "windows-interrupted-index-vcn.recovered",
        "7e57ee7cbf5b7ec111df94b840b43e3b31fe5641c98e96d7907aecb87602386e",
    ),
    (
        "windows-interrupted-small-cluster",
        "683ed42d2481c01ea65c9db36ff01b77075b2c7d7502746cc8b4e9119749e98e",
    ),
    (
        "windows-interrupted-open-at-checkpoint",
        "8f0e24cf12cec66a5cd723baba9ab0cebd29b537d6f4bfc6a32c7dbe9766f6c2",
    ),
    (
        "windows-interrupted-open-at-checkpoint.recovered",
        "3d5e4d5c2527736d8fadbe89fd06324361cd4335f5c4f4fc3305a4985f05b214",
    ),
    (
        "windows-interrupted-undo",
        "a46c9285fbaacbd1253b14e01444ea4764df5f43696dda7630d7a6b0ca6272e7",
    ),
    (
        "windows-interrupted-undo.recovered",
        "8799d511bc4d776d410b2e3c50e1c147a518de980e4ff0c72b0e1f13d4d9c667",
    ),
    (
        "windows-interrupted-tail-continuation",
        "786c75d7c15b37a38dd965cc1145cea7cdf9c326d493c0530c9310c8d9630223",
    ),
    (
        "windows-interrupted-tail-continuation.recovered",
        "ebfa2421bbdb56adc33f870f795f710ede3aea8d89c4714405262b839f4df0af",
    ),
    (
        "windows-interrupted-small-cluster.recovered",
        "239e26b581b120d88d212bd3cba6ebc87c21949d6f6b10bec55c3ba49b312d23",
    ),
    (
        "windows-interrupted-quota",
        "10e4128f474961b9e00a55b106ccd9ec431813f6ab901b4006dda2261e06feb4",
    ),
    (
        "windows-interrupted-quota.recovered",
        "579b728110d674e37eaa4a97ba31843a3f08ac56a0ee7cb66624d46fb195c74b",
    ),
    (
        "windows-interrupted-fresh-log",
        "e5d5b35e83ca2f14656f24806b39af9bf4313a9013a69eb9a74a59a4e8103470",
    ),
    (
        "windows-interrupted-fresh-log.recovered",
        "1c8f516f0396e53ac9051dac2e37db00b816f37677d644cd6349156bc4751426",
    ),
    (
        "windows-interrupted-quota-spilled",
        "f1ce96af31cb69afe1f7b1c4c8592738fb7486c2cd454cfd2968ebb55d9168b0",
    ),
    (
        "windows-interrupted-quota-spilled.recovered",
        "6c3caf4281b34494f05a661a1895059473f6b0d3a542d888516d69423437e2db",
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
        "{name} is not the captured partition with only bootstrap code removed"
    );
    path
}

/// Windows' manifest: path -> (size, SHA-256).
fn manifest(k: &str) -> BTreeMap<String, (u64, String)> {
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
            // Windows' manifest lists files. The junctions `quota`'s
            // workload makes beside them are directories in the index, and
            // their MFT records are compared with the rest.
            if entry.name == "." || entry.name == ".." || entry.file_type == FileType::Directory {
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
    for k in ["1", "6"] {
        let img = unpack(&format!("windows-interrupted-{k}"));
        let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
        let state = fsck::logfile_state_io(&mut io).expect("read $LogFile");
        assert!(state.needs_replay(), "snapshot {k}: {state:?}");

        // The independent reading: ntfs-3g refuses both read-write. On
        // snapshot 6 it reads the log and calls the volume unclean; on
        // snapshot 1 it gets no further than `$Secure`, which Windows had
        // logged but not yet written home.
        let (ok, err) = ntfs3g(&["ntfs-3g.probe", "--readwrite", &img]);
        let why = if k == "1" {
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
const LOG_END: [(&str, u64); 2] = [("1", 0x11_d767), ("6", 0x2b_48b3)];

/// Volumes whose `$LogFile` wrapped -- the writer passed its last page and
/// went on at the first page of its record area, one sequence number up --
/// between the last checkpoint and the capture, so a replay must follow it
/// across the end (#137). Made by `.github/workflows/logfile-oracle.yml`
/// with the same capture, and recovered by Windows the same way, as the
/// two snapshots above; each last LSN read by the same independent reader:
///
/// * `wrap-span` (run 37077649836, one writer for 240 s, snapshot 2): a
///   record starts on the log's last page and ends on the record area's
///   first.
/// * `wrap-boundary` (same run, snapshot 5): a record ends at the last
///   page, and the next starts the new lap.
/// * `wrap-mft-grows` (run 37077643885, eight writers for 60 s, snapshot
///   3): a record spans the end, and the log also grows `$MFT` and fills
///   the records it adds, so the records it names are where `$MFT` has
///   them only once the replay is written.
const WRAPPED_LOG_END: [(&str, u64); 3] = [
    ("wrap-span", 0x21_4840),
    ("wrap-boundary", 0x50_c32d),
    ("wrap-mft-grows", 0x52_13e3),
];

/// A volume whose `$LogFile` holds `SetIndexEntryVcnAllocation` redo
/// records -- an entry inside an index block pointed at another child
/// block -- which replay refused before it performed them (#137). Made by
/// the same capture, and recovered by Windows the same way, as the
/// snapshots above (run 37077649836, one writer for 240 s, snapshot 14:
/// refused at LSN `0xd26ed7`); its last LSN read by the same independent
/// reader.
const INDEX_VCN_LOG_END: [(&str, u64); 1] = [("index-vcn", 0xd3_21e9)];

/// A volume formatted with 1 KiB clusters, so the 4 KiB pages NTFS logs
/// by span four clusters each, which replay refused before it handled them
/// (#137). Its checkpoint's dirty page table lists a page by its first
/// VCN with an LCN per cluster (0 for one not yet mapped), and a record
/// names the VCN of the 1 KiB MFT record it changes, which may be any
/// cluster of that page, with LCNs only as far as it writes. Made by the
/// same capture, and recovered by Windows the same way, as the snapshots
/// above (run 37077655517, four writers for 60 s on 1 KiB clusters,
/// snapshot 8: refused for "a dirty page spanning more than one
/// cluster"); its last LSN read by the same independent reader.
const SMALL_CLUSTER_LOG_END: [(&str, u64); 1] = [("small-cluster", 0x82_967f)];

/// A volume whose last log record starts near the end of one page and ends
/// on the next, where only a tail copy holds the newest version of that
/// next page. A tail copy's last LSN names the record that ends on it, so
/// this copy names a record that starts on the page before; filed under
/// that earlier page, it either clashed with the earlier page's own tail
/// copy ("two different tail copies ... are equally new") or left the
/// record's end unread ("no valid copy ... holds LSN") (#137). Made by the
/// same capture, and recovered by Windows the same way, as the snapshots
/// above (run 37161976800, one writer for 40 s, snapshot 6); its last LSN
/// read by the same independent reader.
const TAIL_CONTINUATION_LOG_END: [(&str, u64); 1] = [("tail-continuation", 0x32_4e8a)];

/// A volume whose log ends inside a transaction: its last three records,
/// `SetNewAttributeSizes`, `UpdateResidentValue` and
/// `UpdateFileNameAllocation`, belong to transaction 344, which has no
/// `ForgetTransaction`, so Windows' restart rolls them back. Replay refused
/// it before undo was implemented (#137). Made by the same capture, and
/// recovered by Windows the same way, as the snapshots above (run
/// 37077655517, four writers for 60 s on 1 KiB clusters, snapshot 3); its
/// last LSN read by the same independent reader.
const UNDO_LOG_END: [(&str, u64); 1] = [("undo", 0x32_74ad)];

/// A volume whose last checkpoint lists transaction 464 as still open, so
/// its undo chain starts before the checkpoint, and which the log ends
/// without forgetting. Windows' restart rolls it back: among its
/// compensation records is `0x63423e`, a 24-byte
/// `InitializeFileRecordSegment` that undoes the deallocation of MFT
/// record 2559 by rewriting the record's header and nothing else. Replay
/// refused any checkpoint listing an open transaction (#137). Made by the
/// same capture, and recovered by Windows the same way, as the snapshots
/// above (run 37173622839, eight writers for 120 s, snapshot 4); its last
/// LSN read by the same independent reader.
const OPEN_AT_CHECKPOINT_LOG_END: [(&str, u64); 1] = [("open-at-checkpoint", 0x63_4233)];

/// A volume with quota tracking on, so every allocation charges the file's
/// owner in `$Quota`'s `$Q` index, whose few entries live in the index
/// root: the log holds `UpdateRecordDataRoot` redo records, each rewriting
/// the start of an entry's data (version, flags, bytes charged and, in the
/// 24-byte ones, the change time), which replay refused before it
/// performed them (#137). The workload also gives every file a security
/// descriptor of its own, some an object id, and makes junctions beside
/// them. Made by the same capture, and recovered by Windows the same way,
/// as the snapshots above (run 37600787594, `mix: views`, one writer for
/// 60 s with a 2 MiB log, snapshot 3: refused at LSN `0x30ae35`); its last
/// LSN read by the same independent reader.
const QUOTA_LOG_END: [(&str, u64); 1] = [("quota", 0x33_eced)];

/// A volume captured 5 s into its workload, just after the capture had
/// `chkdsk /L` resize -- and so empty -- its `$LogFile`: the last
/// checkpoint is the first one written since, and its restart area names
/// no start of checkpoint (`0`) and no open attribute, dirty page or
/// transaction table. Replay read that `0` as an LSN and refused the log
/// ("no valid copy of the log page at 0x0 holds LSN 0x0") (#137). Made by
/// the same capture, and recovered by Windows the same way, as `quota`
/// (run 37600787594, snapshot 1); its last LSN read by the same
/// independent reader.
const FRESH_LOG_END: [(&str, u64); 1] = [("fresh-log", 0x22_578d)];

/// A volume with quota tracking on whose files belong to 48 owners, so
/// `$Quota`'s `$Q` index has outgrown its root into two index blocks (in
/// extension record 122): the log holds `UpdateRecordDataAllocation` redo
/// records, each rewriting the start of an entry's data in an index block
/// as `UpdateRecordDataRoot` does in the root, which replay refused before
/// it performed them (#137). Made by the same capture, and recovered by
/// Windows the same way, as the snapshots above (run 38535425189,
/// `mix: views` with an owner per file, one writer for 120 s, snapshot 2:
/// refused at LSN `0x1bfc19`); its last LSN read by the same independent
/// reader.
const QUOTA_SPILLED_LOG_END: [(&str, u64); 1] = [("quota-spilled", 0x20_754f)];

/// Where, in each pre-image, a `$LogFile` record page the replay needs
/// sits: inside the walk from the oldest dirty page to the log's end, and
/// in no tail copy.
const NEEDED_LOG_PAGE: [(&str, u64); 2] = [("1", 0x8_0000), ("6", 0x10_0000)];

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
fn a_log_that_wrapped_is_replayed_across_its_end_to_what_windows_recovered() {
    // The writer reached the log's last page and went on at the first page
    // of its record area between the checkpoint and the capture. Windows'
    // restart follows it there; so must fsck, and so must a read-write
    // mount, or the newest committed work is lost (#137).
    for (k, log_end) in WRAPPED_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

#[test]
fn an_index_entry_vcn_set_in_an_index_block_is_replayed_to_what_windows_recovered() {
    // SetIndexEntryVcnAllocation points an entry inside an index block at
    // another child block. Windows' restart redoes it; a replay that
    // refuses it sends the volume back to Windows, and one that skips it
    // leaves a directory whose index reaches the wrong block (#137).
    for (k, log_end) in INDEX_VCN_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

#[test]
fn a_log_whose_pages_span_several_clusters_is_replayed_to_what_windows_recovered() {
    // On clusters smaller than the 4 KiB page NTFS logs by, a dirty page
    // and the records that change it name several clusters, and a record
    // may name a cluster in the middle of its page. A replay that refuses
    // them sends the volume back to Windows; one that looks a record's
    // page up by the record's own VCN alone skips it, and leaves an MFT
    // record unwritten that the log created (#137).
    for (k, log_end) in SMALL_CLUSTER_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

#[test]
fn a_record_whose_end_only_a_tail_copy_holds_is_replayed_to_what_windows_recovered() {
    // The newest page of a log may have reached only the tail-copy area,
    // and the record ending on it may have started on the page before. The
    // copy belongs to the page it ends on; a replay that files it under
    // the page the record starts on cannot read the record, and refuses a
    // volume Windows recovers (#137).
    for (k, log_end) in TAIL_CONTINUATION_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

#[test]
fn a_transaction_the_log_ends_inside_is_rolled_back_to_what_windows_recovered() {
    // A log may stop before a transaction's ForgetTransaction. Windows'
    // restart redoes everything and then undoes that transaction, newest
    // record first, whether or not its changes reached the disk. A replay
    // that refuses it sends the volume back to Windows; one that only
    // redoes leaves half a transaction on the volume (#137).
    for (k, log_end) in UNDO_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

/// What Windows' own log says record 2072 held right after its restart on
/// `undo`. Its restart rolled transaction 344 back with compensation
/// records whose redo is exactly the original records' undo -- `0x3274f1`
/// rewrites the `$DATA` sizes at record offset `0x110`, `0x3274de` the 64
/// bytes at `0x38 + 0x20` -- and then rewrote the record again after
/// mounting (transaction 24), so the comparison above cannot reach it: its
/// LSN is past the log's end. Transaction 24's own undo data, though,
/// records the state it found: `0x3276ce`'s the sizes, `0x3276c1`'s the
/// mapping pairs at `0x110 + 0x40`. Read from the recovered image's
/// `$LogFile` by the same independent reader.
const UNDO_RECORD_2072: [(usize, &str); 3] = [
    (0x110 + 0x40, "2103622800000000"),
    (0x110 + 0x28, "000c000000000000"),
    (
        0x38 + 0x20,
        "46b50aaec552dd0146b50aaec552dd0146b50aaec552dd012000000000000000\
         0000000000000000000000000801000000000000000000000000000000000000",
    ),
];

#[test]
fn a_transaction_open_at_the_checkpoint_is_rolled_back_to_what_windows_recovered() {
    // The checkpoint's transaction table lists a transaction still open,
    // whose records before the checkpoint must be undone along with the
    // rest. A replay that refuses it sends the volume back to Windows (#137).
    for (k, log_end) in OPEN_AT_CHECKPOINT_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

#[test]
fn a_checkpoint_that_names_no_start_is_replayed_from_itself_to_what_windows_recovered() {
    // The first checkpoint after the log was emptied has nothing before it
    // to start from and no tables to dump: analysis and redo start at the
    // checkpoint itself. A replay that reads its `0` as an LSN refuses a
    // volume Windows recovers (#137).
    for (k, log_end) in FRESH_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

#[test]
fn a_quota_entry_updated_in_an_index_root_is_replayed_to_what_windows_recovered() {
    // With quota tracking on, NTFS charges each allocation to the owner's
    // `$Quota` entry in place, logged as UpdateRecordDataRoot. Windows'
    // restart redoes it; a replay that refuses it sends every volume with
    // quotas on back to Windows, and one that skips it leaves the owner
    // charged for what the log had already undone or added (#137).
    for (k, log_end) in QUOTA_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

#[test]
fn a_quota_entry_updated_in_an_index_block_is_replayed_to_what_windows_recovered() {
    // Once a volume's owners outnumber what `$Quota`'s index root holds,
    // the charges land in its index blocks, logged as
    // UpdateRecordDataAllocation. A replay that refuses it sends every such
    // volume back to Windows (#137).
    for (k, log_end) in QUOTA_SPILLED_LOG_END {
        fsck_and_mount_rw_replay_to_what_windows_recovered(k, log_end);
    }
}

/// Changes Windows made after its restart that moved no LSN, so the
/// comparison cannot tell them from the restart's own by the LSN of the
/// record or block they are in. `UpdateRecordDataRoot` and
/// `UpdateFileNameAllocation` write in place and leave it as it was.
/// Each is undone in Windows' copy before the comparison, with the undo
/// data of Windows' first change to those bytes after its restart: what
/// they held once the restart was over. Read from the recovered image's
/// `$LogFile` by the same independent reader.
///
/// On `quota`, Windows' transaction 24, after its restart, charges the
/// owner's `$Q` entry in record 24 (`$Quota`), whose LSN stays `0x2047c6`
/// on the pre-image and on Windows' copy alike, and updates the root
/// directory's entry for `System Volume Information` in its index block
/// at VCN 2, whose LSN stays `0x216e53`:
///
/// * the entry's data (record offset `0x1b8 + 0x88`, data `0x14` into
///   it): version, flags and bytes charged from the undo of `0x33eed5`,
///   the first charge after the restart; the change time from the last 8
///   bytes of the undo of `0x33fb16`, the first after it to log one (the
///   16-byte charges between leave it);
/// * the index entry at `0xd70`, its `$FILE_NAME` from the creation time
///   on (`0x18` into the entry): the undo of `0x33ef83`.
///
/// On `fresh-log`, the same transaction does the same to the same entries,
/// and to the root's entry at `0x378` of its index block at VCN 0:
///
/// * the `$Q` entry's data: the undo of `0x225986`, then the change time
///   from the undo of `0x226386`;
/// * the entry at `0xd70` of VCN 2: the undo of `0x225a3c`;
/// * the entry at `0x378` of VCN 0: the undo of `0x2257e1`.
///
/// On `quota-spilled`, the `$Q` entry Windows charges after its restart is
/// in `$Q`'s index block at VCN 0, in record 122 (`$Quota`'s extension
/// record), whose LSN stays `0x11676c` on the pre-image and on Windows'
/// copy alike. Windows' transaction 24 also updates the root directory's
/// entry at `0xd70` of its index block at VCN 2 again:
///
/// * the `$Q` entry at `0x88`, its data `0x14` into it: version, flags and
///   bytes charged from the undo of `0x20781e`, the first charge after the
///   restart; the change time from the last 8 bytes of the undo of
///   `0x208229`, the first after it to log one;
/// * the entry at `0xd70` of VCN 2: the undo of `0x2076c9`.
///
/// Fields: fixture, MFT record, the index block in that record's
/// `$INDEX_ALLOCATION` (or `None` for the record itself), offset, bytes.
const AFTER_RESTART_UNSTAMPED: [(&str, u64, Option<IndexBlock>, usize, &str); 7] = [
    (
        "quota",
        24,
        None,
        0x1b8 + 0x88 + 0x14,
        "0200000001000000000097000000000032f20f033f56dd01",
    ),
    (
        "quota",
        5,
        Some(("$I30", 2)),
        0xd70 + 0x18,
        "5307c6f53e56dd0150a5c6f53e56dd0150a5c6f53e56dd0150a5c6f53e56dd01\
         000000000000000000000000000000000600001000000000",
    ),
    (
        "fresh-log",
        24,
        None,
        0x1b8 + 0x88 + 0x14,
        "020000000100000000142500000000000ec11cfa3e56dd01",
    ),
    (
        "fresh-log",
        5,
        Some(("$I30", 2)),
        0xd70 + 0x18,
        "5307c6f53e56dd0150a5c6f53e56dd0150a5c6f53e56dd0150a5c6f53e56dd01\
         000000000000000000000000000000000600001000000000",
    ),
    (
        "fresh-log",
        5,
        Some(("$I30", 0)),
        0x378 + 0x18,
        "8534aef53e56dd018534aef53e56dd018534aef53e56dd018534aef53e56dd01\
         000000000000000000000000000000000600002000000000",
    ),
    (
        "quota-spilled",
        122,
        Some(("$Q", 0)),
        0x88 + 0x14,
        "020000000100000000c402000000000009471525b65bdd01",
    ),
    (
        "quota-spilled",
        5,
        Some(("$I30", 2)),
        0xd70 + 0x18,
        "56fc2425b65bdd0156fc2425b65bdd0156fc2425b65bdd0156fc2425b65bdd01\
         000000000000000000000000000000000600001000000000",
    ),
];

/// An index block: the name of the `$INDEX_ALLOCATION` it is in, and its
/// VCN there. `$Quota`'s `$O` and `$Q` each have a block at VCN 0, so the
/// name tells them apart.
type IndexBlock<'a> = (&'a str, u64);

/// Windows' content of MFT record `n`, or of the index block `vcn` of its
/// `$INDEX_ALLOCATION`, with [`AFTER_RESTART_UNSTAMPED`] undone.
fn after_restart(k: &str, n: u64, vcn: Option<IndexBlock>, c: Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mut c = c?;
    for &(f, rec, v, at, hex) in &AFTER_RESTART_UNSTAMPED {
        if f != k || rec != n || v != vcn {
            continue;
        }
        let hex: String = hex.split_whitespace().collect();
        for i in 0..hex.len() / 2 {
            c[at + i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
    }
    Some(c)
}

/// Record 24's LSN on `quota`, the pre-image and Windows' copy alike.
const QUOTA_RECORD_LSN: u64 = 0x20_47c6;

#[test]
fn a_quota_entry_update_leaves_the_record_lsn_where_windows_left_it() {
    // Windows charges a `$Quota` entry in place and leaves the record's
    // LSN as it was, so the pre-image and Windows' copy both have record
    // 24 at `0x2047c6` though the log charges it many times after. A
    // replay leaves it there too, or it writes a record no Windows run
    // would have (#137). The comparison blanks LSNs, so this is checked
    // here.
    let want = QUOTA_RECORD_LSN;
    let win = Image::read(&unpack("windows-interrupted-quota.recovered"));
    assert_eq!(le(&fixed(win.raw_record(24), b"FILE").unwrap(), 8, 8), want);
    let replay: [(&str, Mount); 2] = [
        ("fsck", |img| {
            fsck::fsck(img).map(|_| ()).map_err(|e| e.to_string())
        }),
        (
            "Filesystem::mount_rw_replaying_log",
            RW_MOUNTS_REPLAYING[0].1,
        ),
    ];
    for (how, run) in replay {
        let img = unpack("windows-interrupted-quota");
        run(&img).unwrap_or_else(|e| panic!("quota: {how}: {e}"));
        let ours = Image::read(&img);
        assert_eq!(
            le(&fixed(ours.raw_record(24), b"FILE").unwrap(), 8, 8),
            want,
            "quota: {how}: record 24's LSN moved"
        );
        std::fs::remove_file(&img).unwrap();
    }
}

/// `$Q`'s two index blocks on `quota-spilled`, by LCN, and the LSN each
/// carries on the pre-image and on Windows' copy alike, though the log
/// updates entries in both many times after it.
const QUOTA_SPILLED_BLOCK_LSN: [(u64, u64); 2] = [(0x7e3, 0x11_676c), (0x895, 0x11_a1b2)];

#[test]
fn a_quota_entry_update_in_an_index_block_leaves_the_block_lsn_where_windows_left_it() {
    // Windows charges a `$Q` entry in an index block in place and leaves
    // the block's LSN as it was, as it does in the index root. A replay
    // leaves it there too, or it writes a block no Windows run would have
    // (#137). The comparison blanks LSNs, so this is checked here.
    let lsn = |img: &Image, lcn: u64| {
        le(
            &fixed(img.cluster_bytes(lcn, 1), b"INDX").expect("an index block"),
            8,
            8,
        )
    };
    let win = Image::read(&unpack("windows-interrupted-quota-spilled.recovered"));
    for (lcn, want) in QUOTA_SPILLED_BLOCK_LSN {
        assert_eq!(lsn(&win, lcn), want, "Windows' block at LCN {lcn:#x}");
    }
    let replay: [(&str, Mount); 2] = [
        ("fsck", |img| {
            fsck::fsck(img).map(|_| ()).map_err(|e| e.to_string())
        }),
        (
            "Filesystem::mount_rw_replaying_log",
            RW_MOUNTS_REPLAYING[0].1,
        ),
    ];
    for (how, run) in replay {
        let img = unpack("windows-interrupted-quota-spilled");
        run(&img).unwrap_or_else(|e| panic!("quota-spilled: {how}: {e}"));
        let ours = Image::read(&img);
        for (lcn, want) in QUOTA_SPILLED_BLOCK_LSN {
            assert_eq!(
                lsn(&ours, lcn),
                want,
                "quota-spilled: {how}: the block at LCN {lcn:#x} moved its LSN"
            );
        }
        std::fs::remove_file(&img).unwrap();
    }
}

/// MFT record 2559 on `open-at-checkpoint`, whose LSN on Windows' image is
/// `0x63423e`: the restart's own compensation record, which no later
/// record names, so Windows' copy is the state right after its restart.
/// The comparison above skips it, its LSN being past the log's end.
#[test]
fn a_deallocation_undone_keeps_the_record_windows_kept() {
    let win = Image::read(&unpack("windows-interrupted-open-at-checkpoint.recovered"));
    let want = content(win.raw_record(2559), b"FILE").expect("Windows' record 2559");
    let replay: [(&str, Mount); 2] = [
        ("fsck", |img| {
            fsck::fsck(img).map(|_| ()).map_err(|e| e.to_string())
        }),
        (
            "Filesystem::mount_rw_replaying_log",
            RW_MOUNTS_REPLAYING[0].1,
        ),
    ];
    for (how, run) in replay {
        let img = unpack("windows-interrupted-open-at-checkpoint");
        run(&img).unwrap_or_else(|e| panic!("open-at-checkpoint: {how}: {e}"));
        assert!(
            content(Image::read(&img).raw_record(2559), b"FILE").as_ref() == Some(&want),
            "open-at-checkpoint: {how}: record 2559 is not what Windows' restart left"
        );
        std::fs::remove_file(&img).unwrap();
    }
}

#[test]
fn a_rolled_back_record_holds_what_windows_own_log_says_it_held_after_restart() {
    let replay: [(&str, Mount); 2] = [
        ("fsck", |img| {
            fsck::fsck(img).map(|_| ()).map_err(|e| e.to_string())
        }),
        (
            "Filesystem::mount_rw_replaying_log",
            RW_MOUNTS_REPLAYING[0].1,
        ),
    ];
    for (how, run) in replay {
        let img = unpack("windows-interrupted-undo");
        run(&img).unwrap_or_else(|e| panic!("undo: {how}: {e}"));
        let rec = Image::read(&img).record(2072).expect("record 2072");
        for (at, want) in UNDO_RECORD_2072 {
            let want: Vec<u8> = (0..want.len() / 2)
                .map(|i| u8::from_str_radix(&want[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            assert_eq!(
                &rec[at..at + want.len()],
                &want[..],
                "undo: {how}: record 2072 at {at:#x} is not what Windows' log says it held \
                 after restart"
            );
        }
        std::fs::remove_file(&img).unwrap();
    }
}

/// `fsck` and `Filesystem::mount_rw` must each bring the pre-image `k` to
/// the files, MFT records, index blocks and bitmaps Windows' own restart
/// produced from it.
fn fsck_and_mount_rw_replay_to_what_windows_recovered(k: &str, log_end: u64) {
    let pre_img = unpack(&format!("windows-interrupted-{k}"));
    let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&pre_img)).unwrap();
    let state = fsck::logfile_state_io(&mut io).expect("read $LogFile");
    assert!(state.needs_replay(), "{k}: {state:?}");
    drop(io);
    let pre = Image::read(&pre_img);
    let win = Image::read(&unpack(&format!("windows-interrupted-{k}.recovered")));
    let want = manifest(k);
    // `fresh-log` was captured 5 s in: Windows listed 125 files;
    // `quota-spilled` after 343 workload operations: 339.
    let lines = match k {
        "fresh-log" => 100,
        "quota-spilled" => 300,
        _ => 500,
    };
    assert!(want.len() > lines, "{k}: the manifest lost its lines");

    let replay: [(&str, Mount); 2] = [
        ("fsck", |img| {
            fsck::fsck(img).map(|_| ()).map_err(|e| e.to_string())
        }),
        (
            "Filesystem::mount_rw_replaying_log",
            RW_MOUNTS_REPLAYING[0].1,
        ),
    ];
    for (how, run) in replay {
        let img = unpack(&format!("windows-interrupted-{k}"));
        run(&img).unwrap_or_else(|e| panic!("{k}: {how}: {e}"));
        let got = walk(&img);
        let missing: Vec<_> = want
            .keys()
            .filter(|p| !got.contains_key(*p))
            .take(5)
            .collect();
        let differ: Vec<_> = want
            .iter()
            .filter(|(p, v)| got.get(*p).is_some_and(|g| g != *v))
            .take(5)
            .collect();
        assert!(
            got == want,
            "{k}: {how}: {} files after replay, {} in Windows' manifest; missing \
             {missing:?}, differing {differ:?}",
            got.len(),
            want.len()
        );
        let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
        let state = fsck::logfile_state_io(&mut io).expect("read $LogFile");
        assert!(!state.needs_replay(), "{k}: {how} left {state:?}");
        drop(io);
        oracle::same_metadata_as_windows(k, &pre, &Image::read(&img), &win, log_end);
        std::fs::remove_file(&img).unwrap();
    }
}

/// Every way this crate opens a volume for writing, each over an image
/// path: `Ok` once it mounted (and unmounted again), or the reason it
/// refused.
type Mount = fn(&str) -> Result<(), String>;

const RW_MOUNTS: [(&str, Mount); 4] = [
    ("Filesystem::mount_rw", |img| {
        Filesystem::mount_rw(img).map(|_| ()).map_err(|e| e.0)
    }),
    ("fs_ntfs_mount", |img| {
        let c_path = CString::new(img).unwrap();
        let h = fs_ntfs_mount(c_path.as_ptr());
        if h.is_null() {
            return Err(last_error());
        }
        fs_ntfs_umount(h);
        Ok(())
    }),
    ("fs_ntfs_mount_with_callbacks", |img| {
        callback_mount(img, true)
    }),
    ("fs_ntfs_mount_rw_with_fs_core_device", |img| {
        let c_path = CString::new(img).unwrap();
        let dev = unsafe { fs_core::ffi::fs_core_file_open(c_path.as_ptr(), true) };
        assert!(!dev.is_null(), "open fs-core device on {img}");
        let h = fs_ntfs_mount_rw_with_fs_core_device(dev);
        let why = last_error();
        if !h.is_null() {
            fs_ntfs_umount(h);
        }
        unsafe { fs_core::ffi::fs_core_device_close(dev) };
        if h.is_null() {
            Err(why)
        } else {
            Ok(())
        }
    }),
];

/// Every way this crate asks a read-write mount to replay a `$LogFile`
/// holding work first, each over an image path, as [`RW_MOUNTS`].
const RW_MOUNTS_REPLAYING: [(&str, Mount); 3] = [
    ("Filesystem::mount_rw_replaying_log", |img| {
        Filesystem::mount_rw_replaying_log(img)
            .map(|_| ())
            .map_err(|e| e.0)
    }),
    ("fs_ntfs_mount_replaying_log", |img| {
        let c_path = CString::new(img).unwrap();
        let h = fs_ntfs_mount_replaying_log(c_path.as_ptr());
        if h.is_null() {
            return Err(last_error());
        }
        fs_ntfs_umount(h);
        Ok(())
    }),
    // fs_ntfs_mount_with_callbacks_replaying_log is not here: the
    // read/write callback pair cannot flush, so it refuses to replay (#518),
    // as `a_replay_through_callbacks_that_cannot_flush_is_refused` pins.
    (
        "fs_ntfs_mount_rw_with_fs_core_device_replaying_log",
        |img| {
            let c_path = CString::new(img).unwrap();
            let dev = unsafe { fs_core::ffi::fs_core_file_open(c_path.as_ptr(), true) };
            assert!(!dev.is_null(), "open fs-core device on {img}");
            let h = fs_ntfs_mount_rw_with_fs_core_device_replaying_log(dev);
            let why = last_error();
            if !h.is_null() {
                fs_ntfs_umount(h);
            }
            unsafe { fs_core::ffi::fs_core_device_close(dev) };
            if h.is_null() {
                Err(why)
            } else {
                Ok(())
            }
        },
    ),
];

struct FileCtx(Mutex<File>);

unsafe extern "C" fn read_cb(ctx: *mut c_void, buf: *mut c_void, at: u64, len: u64) -> c_int {
    let ctx = unsafe { &*(ctx as *const FileCtx) };
    let mut f = ctx.0.lock().unwrap();
    let buf = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, len as usize) };
    c_int::from(f.seek(SeekFrom::Start(at)).is_err() || f.read_exact(buf).is_err())
}

unsafe extern "C" fn write_cb(ctx: *mut c_void, buf: *const c_void, at: u64, len: u64) -> c_int {
    let ctx = unsafe { &*(ctx as *const FileCtx) };
    let mut f = ctx.0.lock().unwrap();
    let buf = unsafe { std::slice::from_raw_parts(buf as *const u8, len as usize) };
    c_int::from(f.seek(SeekFrom::Start(at)).is_err() || f.write_all(buf).is_err())
}

/// Mount `img` through host callbacks, with a write callback or without.
fn callback_mount(img: &str, writable: bool) -> Result<(), String> {
    callback_mount_as(img, writable, false)
}

/// [`callback_mount`], through the mount that replays a log holding work
/// when `replay` asks for it.
fn callback_mount_as(img: &str, writable: bool, replay: bool) -> Result<(), String> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(img)
        .unwrap();
    let size = f.metadata().unwrap().len();
    let ctx = FileCtx(Mutex::new(f));
    let cfg = FsNtfsBlockdevCfg {
        read: read_cb,
        context: &ctx as *const FileCtx as *mut c_void,
        size_bytes: size,
        write: if writable { Some(write_cb) } else { None },
    };
    let h = if replay {
        fs_ntfs_mount_with_callbacks_replaying_log(&cfg)
    } else {
        fs_ntfs_mount_with_callbacks(&cfg)
    };
    if h.is_null() {
        return Err(last_error());
    }
    fs_ntfs_umount(h);
    Ok(())
}

#[test]
fn every_read_write_mount_asked_to_replay_reaches_what_windows_recovered() {
    // Windows replays `$LogFile` when it mounts a volume after a crash, and
    // so does a read-write mount here when it is asked to (#137). Each
    // entry point must reach the metadata Windows' own restart reached from
    // the same pre-image.
    for (k, log_end) in LOG_END {
        let pre_img = unpack(&format!("windows-interrupted-{k}"));
        let pre = Image::read(&pre_img);
        let win = Image::read(&unpack(&format!("windows-interrupted-{k}.recovered")));
        let want = manifest(k);
        for (name, rw_mount) in RW_MOUNTS_REPLAYING {
            let img = unpack(&format!("windows-interrupted-{k}"));
            rw_mount(&img).unwrap_or_else(|e| panic!("snapshot {k}: {name}: {e}"));

            let got = walk(&img);
            assert!(
                got == want,
                "snapshot {k}: {name}: {} files after the mount, {} in Windows' manifest",
                got.len(),
                want.len()
            );
            let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
            let state = fsck::logfile_state_io(&mut io).expect("read $LogFile");
            assert!(
                !state.needs_replay(),
                "snapshot {k}: {name} left the log holding work: {state:?}"
            );
            drop(io);
            oracle::same_metadata_as_windows(k, &pre, &Image::read(&img), &win, log_end);
            std::fs::remove_file(&img).unwrap();
        }
    }
}

/// Every way this crate writes to a volume over an image path with no
/// read-write mount first: each creates the file `/{name}` and returns its
/// MFT record number, or the reason it refused.
type PathWrite = fn(&str, &str) -> Result<u64, String>;

const PATH_WRITES: [(&str, PathWrite); 3] = [
    ("Filesystem::mount, then create_file", |img, name| {
        let fs = Filesystem::mount(img).map_err(|e| e.0)?;
        fs.create_file("/", name).map_err(|e| e.0)
    }),
    ("write::create_file", |img, name| {
        fs_ntfs::write::create_file(std::path::Path::new(img), "/", name).map_err(|e| e.to_string())
    }),
    ("fs_ntfs_create_file", |img, name| {
        let c_img = CString::new(img).unwrap();
        let c_parent = CString::new("/").unwrap();
        let c_name = CString::new(name).unwrap();
        let rn = fs_ntfs_create_file(c_img.as_ptr(), c_parent.as_ptr(), c_name.as_ptr());
        u64::try_from(rn).map_err(|_| last_error())
    }),
];

#[test]
fn every_write_without_a_read_write_mount_replays_the_log_first_or_writes_nothing() {
    // A read-write mount refuses a log holding work, or replays it when
    // asked to, before anything is written (#137). These write over an
    // image path with no read-write mount, and skipped that check: on
    // snapshot 6 each put its file in MFT record 1069 and left the log
    // holding work, where a write after the replay takes 1277. Windows'
    // restart, run on that volume later, gives 1069 to a file of its own,
    // and one of the two is lost. So each must refuse with nothing
    // written, or replay first: the log holding no work, every file
    // Windows recovered in place, and the new file in the record a write
    // after `mount_rw_replaying_log` takes.
    for (k, _) in LOG_END {
        let want = manifest(k);
        let after_mount_rw = {
            let img = unpack(&format!("windows-interrupted-{k}"));
            let fs = Filesystem::mount_rw_replaying_log(&img)
                .unwrap_or_else(|e| panic!("snapshot {k}: mount_rw_replaying_log: {e}"));
            let rn = fs
                .create_file("/", "written")
                .unwrap_or_else(|e| panic!("snapshot {k}: create_file after mount_rw: {e}"));
            std::fs::remove_file(&img).unwrap();
            rn
        };
        for (how, write) in PATH_WRITES {
            let img = unpack(&format!("windows-interrupted-{k}"));
            let before = std::fs::read(&img).unwrap();
            match write(&img, "written") {
                Err(why) => assert!(
                    std::fs::read(&img).unwrap() == before,
                    "snapshot {k}: {how} refused ({why}) and wrote to the volume anyway"
                ),
                Ok(rn) => {
                    let mut io =
                        fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
                    let state = fsck::logfile_state_io(&mut io).expect("read $LogFile");
                    assert!(
                        !state.needs_replay(),
                        "snapshot {k}: {how} wrote MFT record {rn} and left the log holding \
                         work: {state:?}"
                    );
                    drop(io);
                    let got = walk(&img);
                    assert!(
                        got == want,
                        "snapshot {k}: {how}: {} files after the write, {} in Windows' manifest",
                        got.len(),
                        want.len()
                    );
                    assert_eq!(
                        rn, after_mount_rw,
                        "snapshot {k}: {how} wrote MFT record {rn}; after mount_rw the same \
                         file takes {after_mount_rw}"
                    );
                }
            }
            std::fs::remove_file(&img).unwrap();
        }
    }
}

#[test]
fn a_read_only_mount_of_a_volume_windows_left_mid_write_replays_nothing() {
    // A read-only mount reads the metadata as it is on disk, a little
    // behind the log, and writes nothing -- what every other driver here
    // does with a volume it is not allowed to change.
    for k in ["1", "6"] {
        let img = unpack(&format!("windows-interrupted-{k}"));
        let before = std::fs::read(&img).unwrap();

        Filesystem::mount(&img).expect("a read-only mount is allowed");
        callback_mount(&img, false)
            .unwrap_or_else(|e| panic!("snapshot {k}: read-only callback mount: {e}"));
        let c_path = CString::new(img.as_str()).unwrap();
        let dev = unsafe { fs_core::ffi::fs_core_file_open(c_path.as_ptr(), true) };
        assert!(!dev.is_null(), "snapshot {k}: open fs-core device");
        let ro = fs_ntfs_mount_with_fs_core_device(dev);
        assert!(!ro.is_null(), "snapshot {k}: read-only: {}", last_error());
        fs_ntfs_umount(ro);
        unsafe { fs_core::ffi::fs_core_device_close(dev) };

        assert!(
            std::fs::read(&img).unwrap() == before,
            "snapshot {k}: a read-only mount wrote to the volume"
        );
    }
}

#[test]
fn a_read_write_mount_refuses_a_log_it_cannot_replay_in_full_and_writes_nothing() {
    // A torn record page in the middle of the work: the replay cannot be
    // done whole, so none of it may be done, and the mount is refused as
    // it was before replay existed. The volume is left exactly as found.
    for (k, page) in NEEDED_LOG_PAGE {
        for (name, rw_mount) in RW_MOUNTS_REPLAYING {
            let img = unpack(&format!("windows-interrupted-{k}"));
            let logfile_lcn = 10_318u64; // $LogFile's one run, in both images
            let at = logfile_lcn * 4096 + page + 510; // the first sector's USN copy
            let mut bytes = std::fs::read(&img).unwrap();
            bytes[at as usize] ^= 0xFF;
            std::fs::write(&img, &bytes).unwrap();

            let err = rw_mount(&img).expect_err("a mount over a log it cannot replay");
            assert!(
                err.contains("$LogFile") && err.contains("read-write mount refused"),
                "snapshot {k}: {name}: {err}"
            );
            assert!(
                std::fs::read(&img).unwrap() == bytes,
                "snapshot {k}: {name}: a refused mount wrote to the volume"
            );
            std::fs::remove_file(&img).unwrap();
        }
    }
}

#[test]
fn a_read_write_mount_refuses_a_log_holding_work_unless_asked_to_replay_it() {
    // Windows leaves a log holding work after a crash, which replaying it
    // recovers, and after a Fast Startup shutdown or a hibernation, which
    // replaying it corrupts: Windows resumes from what it cached and does
    // not see the replay's changes. Only `hiberfil.sys` tells the two
    // apart, and only on the volume Windows boots from; a data volume Fast
    // Startup left looks exactly like a crashed one. So a read-write mount
    // does not replay unasked. It refuses, with nothing written, and names
    // `fsck`, which replays on request (#137).
    for (k, _) in LOG_END {
        for (name, rw_mount) in RW_MOUNTS {
            let img = unpack(&format!("windows-interrupted-{k}"));
            let before = std::fs::read(&img).unwrap();

            let err = rw_mount(&img).expect_err("a read-write mount of a log holding work");
            assert!(
                err.contains("$LogFile") && err.contains("read-write mount refused"),
                "snapshot {k}: {name}: {err}"
            );
            assert!(
                err.contains("fsck"),
                "snapshot {k}: {name}: the refusal does not say how to replay: {err}"
            );
            assert!(
                std::fs::read(&img).unwrap() == before,
                "snapshot {k}: {name}: a refused mount wrote to the volume"
            );
            std::fs::remove_file(&img).unwrap();
        }
    }
}

#[test]
fn a_dirty_volume_is_refused_before_its_log_is_replayed() {
    // The dirty flag asks for a check of the whole volume, which a replay
    // is not. A read-write mount refuses it as before, and does not replay
    // first: nothing is written to a volume the mount then declines.
    for k in ["1", "6"] {
        for (name, rw_mount) in RW_MOUNTS.iter().chain(&RW_MOUNTS_REPLAYING) {
            let img = unpack(&format!("windows-interrupted-{k}"));
            fsck::set_dirty(&img).expect("mark dirty");
            let before = std::fs::read(&img).unwrap();

            let err = rw_mount(&img).expect_err("a read-write mount of a dirty volume");
            assert!(err.contains("dirty"), "snapshot {k}: {name}: {err}");
            assert!(
                std::fs::read(&img).unwrap() == before,
                "snapshot {k}: {name}: a refused mount wrote to the volume"
            );
            std::fs::remove_file(&img).unwrap();
        }
    }
}

/// A replay through the read/write callback pair is refused, with nothing
/// written (#518). The pair has no flush callback, so nothing orders the
/// replayed metadata before the log's "clean": a host whose write callback
/// buffers could land the second first, and a power cut then leaves a log
/// saying there is nothing to replay over metadata the replay never
/// finished. The mount that replays and fsck are both refused.
#[test]
fn a_replay_through_callbacks_that_cannot_flush_is_refused() {
    let img = unpack("windows-interrupted-1");
    let before = std::fs::read(&img).unwrap();
    let why = callback_mount_as(&img, true, true)
        .expect_err("a replaying mount through callbacks with no flush callback");
    assert!(why.contains("flush"), "the refusal names the flush: {why}");
    assert!(
        std::fs::read(&img).unwrap() == before,
        "the refused mount wrote to the volume"
    );

    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&img)
        .unwrap();
    let size = f.metadata().unwrap().len();
    let ctx = FileCtx(Mutex::new(f));
    let cfg = FsNtfsBlockdevCfg {
        read: read_cb,
        context: &ctx as *const FileCtx as *mut c_void,
        size_bytes: size,
        write: Some(write_cb),
    };
    let rc = fs_ntfs_fsck_with_callbacks(&cfg, None, std::ptr::null_mut(), std::ptr::null_mut(), 0);
    let why = last_error();
    assert_eq!(
        rc, -1,
        "fsck through callbacks with no flush callback replayed"
    );
    assert!(why.contains("flush"), "the refusal names the flush: {why}");
    drop(ctx);
    assert!(
        std::fs::read(&img).unwrap() == before,
        "the refused fsck wrote to the volume"
    );
}

/// What a device that can flush saw: each write's offset and length, and
/// each flush, in order.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Seen {
    Write(u64, usize),
    Flush,
}

struct Recording {
    file: Mutex<File>,
    seen: Mutex<Vec<Seen>>,
}

unsafe extern "C" fn rec_read(ctx: *mut c_void, at: u64, buf: *mut u8, len: usize) -> c_int {
    let r = unsafe { &*(ctx as *const Recording) };
    let mut f = r.file.lock().unwrap();
    let buf = unsafe { std::slice::from_raw_parts_mut(buf, len) };
    c_int::from(f.seek(SeekFrom::Start(at)).is_err() || f.read_exact(buf).is_err())
}

unsafe extern "C" fn rec_write(ctx: *mut c_void, at: u64, buf: *const u8, len: usize) -> c_int {
    let r = unsafe { &*(ctx as *const Recording) };
    r.seen.lock().unwrap().push(Seen::Write(at, len));
    let mut f = r.file.lock().unwrap();
    let buf = unsafe { std::slice::from_raw_parts(buf, len) };
    c_int::from(f.seek(SeekFrom::Start(at)).is_err() || f.write_all(buf).is_err())
}

unsafe extern "C" fn rec_flush(ctx: *mut c_void) -> c_int {
    let r = unsafe { &*(ctx as *const Recording) };
    r.seen.lock().unwrap().push(Seen::Flush);
    c_int::from(r.file.lock().unwrap().sync_all().is_err())
}

/// Through a device with a flush callback, the replay flushes at each point
/// the path backend calls `fsync` (#518): the replayed metadata, a flush,
/// the restart pages marked clean, a flush, the rest of the log erased, a
/// flush, and then the restart pages themselves erased, each followed by a
/// flush.
#[test]
fn a_replay_through_a_device_that_flushes_orders_its_writes() {
    let img = unpack("windows-interrupted-1");
    let (log_start, log_len) = {
        let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
        fs_ntfs::read::nonresident_contiguous_disk_range(
            &mut io,
            2,
            fs_ntfs::attr_io::AttrType::Data,
            None,
        )
        .expect("$LogFile is one extent")
    };
    // The restart pages: the first two system pages of the log, whose size
    // the restart page header records at 0x10.
    let page = {
        let mut f = File::open(&img).unwrap();
        let mut b = [0u8; 4];
        f.seek(SeekFrom::Start(log_start + 0x10)).unwrap();
        f.read_exact(&mut b).unwrap();
        u64::from(u32::from_le_bytes(b))
    };
    assert!(
        page.is_power_of_two() && page >= 512,
        "restart page size {page}"
    );

    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&img)
        .unwrap();
    let size = f.metadata().unwrap().len();
    let rec = Recording {
        file: Mutex::new(f),
        seen: Mutex::new(Vec::new()),
    };
    let cfg = fs_core::ffi::FsCoreCallbackCfg {
        read: Some(rec_read),
        write: Some(rec_write),
        flush: Some(rec_flush),
        ctx: &rec as *const Recording as *mut c_void,
        size,
    };
    let dev = unsafe { fs_core::ffi::fs_core_device_from_callbacks(&cfg) };
    assert!(!dev.is_null(), "fs_core_device_from_callbacks");
    let rc =
        fs_ntfs_fsck_with_fs_core_device(dev, None, std::ptr::null_mut(), std::ptr::null_mut(), 0);
    let why = last_error();
    unsafe { fs_core::ffi::fs_core_device_close(dev) };
    assert_eq!(rc, 0, "fsck through a device that flushes: {why}");

    let seen = rec.seen.lock().unwrap().clone();
    let restart = log_start..log_start + 2 * page;
    let log = log_start..log_start + log_len;
    let kind = |s: &Seen| match *s {
        Seen::Flush => 'F',
        Seen::Write(at, _) if restart.contains(&at) => 'R',
        Seen::Write(at, _) if log.contains(&at) => 'E',
        Seen::Write(..) => 'M',
    };
    let kinds: String = seen.iter().map(kind).collect();
    let (Some(m), Some(r), Some(e)) = (kinds.rfind('M'), kinds.find('R'), kinds.find('E')) else {
        panic!("writes seen: {kinds}");
    };
    assert!(
        m < r && r < e,
        "the replayed metadata, then the restart pages, then the log: {kinds}"
    );
    // The restart pages marked clean are the R writes before the first E;
    // the R writes after the last E erase them once the log is empty.
    let marked = kinds[..e].rfind('R').expect("an R before the first E");
    let erased = kinds.rfind('E').expect("an E");
    assert!(
        kinds[m..r].contains('F'),
        "no flush between the replayed metadata and the restart pages: {kinds}"
    );
    assert!(
        kinds[marked..e].contains('F'),
        "no flush between the restart pages marked clean and the erased log: {kinds}"
    );
    if let Some(after) = kinds[erased..].find('R') {
        assert!(
            kinds[erased..erased + after].contains('F'),
            "no flush between the erased log and the erased restart pages: {kinds}"
        );
    }
    assert!(
        kinds.ends_with('F'),
        "the last write is not followed by a flush: {kinds}"
    );
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
    let wrapped = WRAPPED_LOG_END.map(|(k, _)| k);
    let index_vcn = INDEX_VCN_LOG_END.map(|(k, _)| k);
    let small_cluster = SMALL_CLUSTER_LOG_END.map(|(k, _)| k);
    let tail_continuation = TAIL_CONTINUATION_LOG_END.map(|(k, _)| k);
    let undo = UNDO_LOG_END.map(|(k, _)| k);
    let open_at_checkpoint = OPEN_AT_CHECKPOINT_LOG_END.map(|(k, _)| k);
    for k in ["1", "6"]
        .into_iter()
        .chain(wrapped)
        .chain(index_vcn)
        .chain(small_cluster)
        .chain(tail_continuation)
        .chain(undo)
        .chain(open_at_checkpoint)
    {
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
    let want: std::collections::BTreeSet<String> = manifest("6").into_keys().collect();
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

/// How much each fixture's comparison must have to compare, so a
/// comparison that has lost its subject fails rather than passing on
/// nothing: MFT records Windows' replay changed, index blocks compared,
/// and `$Bitmap` bits the replay set as Windows did. Measured, 2026-10-03:
///
/// | fixture        | records | blocks | bits  |
/// |----------------|---------|--------|-------|
/// | 1              | 247     | 3      | 2,990 |
/// | 6              | 206     | 67     | 2,607 |
/// | wrap-span      | 210     | 67     | 954   |
/// | wrap-boundary  | 147     | 131    | 439   |
/// | wrap-mft-grows | 516     | 212    | 1,493 |
/// | index-vcn      | 134     | 418    | <= 99 |
/// | small-cluster  | 217     | 259    | 4,760 |
/// | tail-continuation | 57   | 99     | 1,493 |
/// | undo           | 342     | 99     | 5,089 |
/// | open-at-checkpoint | 15  | 250    | 2,771 |
/// | quota          | 43      | 73     | 106   |
/// | fresh-log      | 213     | 4      | 535   |
/// | quota-spilled  | 19      | 34     | <= 48 |
///
/// `quota`'s and `fresh-log`'s were measured on 2026-10-07, with Windows'
/// changes after its restart that moved no LSN undone
/// ([`AFTER_RESTART_UNSTAMPED`]).
///
/// `quota-spilled`'s were measured on 2026-10-14 from the pre-image and
/// Windows' copy alone, by a separate reader, with the same changes
/// undone; its bits are at most the 48 `$Bitmap` differs in between them
/// on clusters no record Windows wrote after its restart holds.
///
/// `index-vcn`'s records and blocks were measured from the pre-image and
/// Windows' recovered copy alone; its bits are at most the 99 `$Bitmap`
/// differs in between them, before discounting clusters Windows allocated
/// after its restart.
fn floors(k: &str) -> (usize, usize, usize) {
    match k {
        "1" | "6" => (150, 3, 1000),
        "wrap-span" => (150, 50, 800),
        "wrap-boundary" => (100, 100, 350),
        "wrap-mft-grows" => (400, 150, 1200),
        "index-vcn" => (100, 300, 40),
        "small-cluster" => (150, 200, 4000),
        "tail-continuation" => (40, 70, 1100),
        "undo" => (250, 70, 4000),
        "open-at-checkpoint" => (10, 180, 2000),
        "quota" => (30, 50, 80),
        "fresh-log" => (150, 3, 400),
        "quota-spilled" => (14, 25, 30),
        other => panic!("no floors measured for fixture {other}"),
    }
}

/// A raw reading of an NTFS image, written for this comparison from
/// MS-FSCC and nothing in this crate: the boot sector, `$MFT`'s runs, and
/// update-sequence fixups.
struct Image {
    bytes: Vec<u8>,
    cluster: u64,
    record: u64,
    /// Index block size, which spans several clusters when they are small.
    index: u64,
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

/// The name of the attribute at `a` in `rec`, empty for an unnamed one.
fn attr_name(rec: &[u8], a: usize) -> String {
    let (len, at) = (rec[a + 9] as usize, a + le(rec, a + 0x0A, 2) as usize);
    let name: Vec<u16> = (0..len).map(|i| le(rec, at + 2 * i, 2) as u16).collect();
    String::from_utf16_lossy(&name)
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
        Image::of(std::fs::read(path).unwrap())
    }

    fn of(bytes: Vec<u8>) -> Image {
        let cluster = le(&bytes, 0x0B, 2) * le(&bytes, 0x0D, 1);
        let c = bytes[0x40] as i8;
        let record = if c > 0 { c as u64 * cluster } else { 1 << -c };
        let c = bytes[0x44] as i8;
        let index = if c > 0 { c as u64 * cluster } else { 1 << -c };
        let mut img = Image {
            bytes,
            cluster,
            record,
            index,
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
        self.raw_record_if_mapped(n)
            .unwrap_or_else(|| panic!("record {n} is past $MFT"))
    }

    /// Record `n` as stored, or `None` past this image's `$MFT`: a record
    /// the log adds after growing `$MFT` has no place before the replay.
    fn raw_record_if_mapped(&self, n: u64) -> Option<&[u8]> {
        let mut off = n * self.record;
        for &(lcn, len) in &self.mft_runs {
            if off < len * self.cluster {
                let at = (lcn * self.cluster + off) as usize;
                return Some(&self.bytes[at..at + self.record as usize]);
            }
            off -= len * self.cluster;
        }
        None
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
    /// was anything to replay. A file's extension records hold its
    /// attributes too, so they go with it: on `quota-spilled`, Windows adds
    /// security descriptors after its restart, stamping `$Secure`'s record
    /// 9 past the log's end and adding entries to the `$SDH` and `$SII`
    /// index blocks that extension record 212 holds.
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
        let extensions: Vec<u64> = (0..win.records())
            .filter(|&n| {
                win.record(n)
                    .is_some_and(|rec| out.contains(&le(&rec, 0x20, 6)))
            })
            .collect();
        out.extend(extensions);
        out
    }

    /// The LSN a FILE record or INDX block carries, or `None` for a block
    /// that is not `magic` or is torn.
    fn lsn(raw: &[u8], magic: &[u8; 4]) -> Option<u64> {
        fixed(raw, magic).map(|b| le(&b, 8, 8))
    }

    /// Whether the replay left the LSN Windows' restart left. `content`
    /// blanks it, so it is compared here: redo decides from it whether a
    /// block already holds a record, so a replay leaves it where Windows'
    /// restart does, not merely the bytes around it (#137). Two are not
    /// compared: one Windows wrote after its restart (past the log's end),
    /// and the stamp undo gives a block it rolled back, one past the log's
    /// end, so a rerun can tell it from one it did not (#514); Windows logs
    /// compensation records instead.
    fn same_lsn(ours: Option<u64>, win: Option<u64>, log_end: u64) -> bool {
        match (ours, win) {
            (Some(o), Some(w)) => o == w || w > log_end || o == log_end + 1,
            _ => true,
        }
    }

    pub fn same_metadata_as_windows(k: &str, pre: &Image, ours: &Image, win: &Image, log_end: u64) {
        let (min_records, min_blocks, min_bits) = super::floors(k);
        let after = windows_after_recovery(win, log_end);

        // Every MFT record, as content, and its LSN.
        let mut replayed = 0;
        let mut differ = Vec::new();
        let mut lsn_differ = Vec::new();
        for n in 0..win.records() {
            if after.contains(&n) {
                continue;
            }
            let w = after_restart(k, n, None, content(win.raw_record(n), b"FILE"));
            if pre
                .raw_record_if_mapped(n)
                .and_then(|r| content(r, b"FILE"))
                != w
            {
                replayed += 1;
            }
            if content(ours.raw_record(n), b"FILE") != w {
                differ.push(n);
            }
            let (o, w) = (
                lsn(ours.raw_record(n), b"FILE"),
                lsn(win.raw_record(n), b"FILE"),
            );
            if !same_lsn(o, w, log_end) {
                lsn_differ.push(format!("record {n}: {o:#x?}, Windows {w:#x?}"));
            }
        }
        assert!(
            differ.is_empty(),
            "snapshot {k}: {} MFT records differ from what Windows recovered: {:?}",
            differ.len(),
            &differ[..differ.len().min(20)]
        );
        assert!(
            replayed > min_records,
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
                // Block by block through the allocation, which a block
                // spans several clusters of when clusters are small.
                let runs = runs_of(&rec, a);
                let name = attr_name(&rec, a);
                let (w_alloc, o_alloc): (Vec<u8>, Vec<u8>) = (
                    runs.iter()
                        .flat_map(|&(l, n)| win.cluster_bytes(l, n).to_vec())
                        .collect(),
                    runs.iter()
                        .flat_map(|&(l, n)| ours.cluster_bytes(l, n).to_vec())
                        .collect(),
                );
                let size = win.index as usize;
                for at in (0..w_alloc.len() / size * size).step_by(size) {
                    let w = after_restart(
                        k,
                        n,
                        Some((&name, at as u64 / win.cluster)),
                        content(&w_alloc[at..at + size], b"INDX"),
                    );
                    if w.is_none() {
                        continue;
                    }
                    blocks += 1;
                    assert!(
                        content(&o_alloc[at..at + size], b"INDX") == w,
                        "snapshot {k}: record {n}'s {name} index block at VCN {} differs from \
                         what Windows recovered",
                        at as u64 / win.cluster
                    );
                    let (o, w) = (
                        lsn(&o_alloc[at..at + size], b"INDX"),
                        lsn(&w_alloc[at..at + size], b"INDX"),
                    );
                    if !same_lsn(o, w, log_end) {
                        lsn_differ.push(format!(
                            "record {n}'s {name} index block at VCN {}: {o:#x?}, Windows {w:#x?}",
                            at as u64 / win.cluster
                        ));
                    }
                }
            }
        }
        assert!(
            blocks >= min_blocks,
            "snapshot {k}: only {blocks} index blocks compared"
        );
        assert!(
            lsn_differ.is_empty(),
            "snapshot {k}: {} MFT records and index blocks carry an LSN Windows' restart did \
             not leave: {:?}",
            lsn_differ.len(),
            &lsn_differ[..lsn_differ.len().min(10)]
        );

        // $MFT's own bitmap: any record whose bit differs is one Windows
        // wrote after its restart (on `quota`, a file it created under
        // `System Volume Information` once mounted).
        let (o, w) = (ours.stream(0, 0xB0), win.stream(0, 0xB0));
        assert_eq!(
            o.len(),
            w.len(),
            "snapshot {k}: $MFT's bitmap is not the length Windows left"
        );
        for n in 0..(w.len() as u64 * 8) {
            let bit = |b: &[u8]| (b[(n / 8) as usize] >> (n % 8)) & 1;
            assert!(
                bit(&o) == bit(&w) || after.contains(&n),
                "snapshot {k}: MFT record {n} is {} in $MFT's bitmap after replay, {} after \
                 Windows' recovery, and Windows did not write it afterwards",
                bit(&o),
                bit(&w)
            );
        }

        // $Bitmap: any cluster that differs is one Windows allocated, after
        // recovery, to a record it wrote after recovery.
        let (o, w, p) = (
            ours.stream(6, 0x80),
            win.stream(6, 0x80),
            pre.stream(6, 0x80),
        );
        // A record Windows rewrote after its restart may have taken clusters
        // or given them back (a zero-length file's allocation released at
        // mount, on `undo`): either way the bit is Windows' later work, so
        // the record's runs count as Windows has them and as replay left
        // them.
        let mut owned_after = BTreeSet::new();
        for &n in &after {
            for img in [win, ours] {
                let Some(rec) = img.raw_record_if_mapped(n).and_then(|r| fixed(r, b"FILE")) else {
                    continue;
                };
                for (a, _, nr) in attrs(&rec) {
                    if nr {
                        for (lcn, len) in runs_of(&rec, a) {
                            owned_after.extend(lcn..lcn + len);
                        }
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
            changed > min_bits,
            "snapshot {k}: replay matched only {changed} $Bitmap bits Windows changed"
        );
    }
}

/// How many records `$MFT` itself holds on what Windows recovered from
/// snapshot 6, read from `$MFT`'s own `$DATA` run list.
const SNAPSHOT_6_MFT_RECORDS: u64 = 1280;

/// Windows sizes `$MFT:$Bitmap` well ahead of `$MFT`: on snapshot 6 it
/// describes 32,832 records while `$MFT`'s `$DATA` holds 1,280, and when
/// Windows itself grew `$MFT` on the `wrap-mft-grows` capture it extended
/// `$DATA` and left the bitmap's length alone. A clear bit past the last
/// record names a record with no storage, so creating files here must grow
/// `$MFT` once its own records are used up, not hand out a record past its
/// end ("MFT record 1280 is not mapped", #415).
///
/// Windows grades the same workload: the `cli-windows-6-grow-mft` scenario.
#[test]
fn creating_files_on_a_volume_windows_wrote_grows_its_mft_once_its_records_run_out() {
    let img = unpack("windows-interrupted-6.recovered");
    let before = Image::read(&img);
    assert_eq!(before.records(), SNAPSHOT_6_MFT_RECORDS);
    let bitmap = before.stream(0, 0xB0);
    let bit = |b: &[u8], n: u64| (b[(n / 8) as usize] >> (n % 8)) & 1 != 0;
    assert!(
        bitmap.len() as u64 * 8 > SNAPSHOT_6_MFT_RECORDS,
        "the bitmap no longer outruns $MFT, so this volume no longer shows #415"
    );
    let free_before = (0..SNAPSHOT_6_MFT_RECORDS)
        .filter(|&n| !bit(&bitmap, n))
        .count();

    // Enough files to use every free record, and then two growths of 64.
    let files = free_before + 70;
    let fs = Filesystem::mount_rw(&img).unwrap();
    fs.mkdir("/", "grown").expect("mkdir /grown");
    let mut want = BTreeMap::new();
    for i in 0..files {
        // 32 files to a directory: 32 names fit one index block, so each
        // directory stays one level deep and this grades $MFT's growth,
        // not a deep index B-tree (#432).
        let dir = format!("/grown/b{:03}", i / 32);
        if i % 32 == 0 {
            fs.mkdir("/grown", &dir["/grown/".len()..])
                .unwrap_or_else(|e| panic!("mkdir {dir}: {e:?}"));
        }
        let name = format!("f{i:04}.bin");
        let path = format!("{dir}/{name}");
        let record = fs
            .create_file(&dir, &name)
            .unwrap_or_else(|e| panic!("create {path} (file {i} of {files}): {e:?}"));
        let data: Vec<u8> = (0..300 + i).map(|b| (b * 31 + i) as u8).collect();
        fs.write_file_contents(&path, &data)
            .unwrap_or_else(|e| panic!("write {path} (record {record}): {e:?}"));
        want.insert(path, data);
    }

    let after = Image::read(&img);
    assert!(
        after.records() > SNAPSHOT_6_MFT_RECORDS,
        "{files} files created and $MFT still holds {} records",
        after.records()
    );
    // No record past `$MFT`'s storage is marked in use.
    let bitmap = after.stream(0, 0xB0);
    let past_end: Vec<u64> = (after.records()..bitmap.len() as u64 * 8)
        .filter(|&n| bit(&bitmap, n))
        .take(5)
        .collect();
    assert!(
        past_end.is_empty(),
        "records past $MFT marked in use: {past_end:?}"
    );
    // The volume's record count is `$MFT`'s, not the bitmap's capacity.
    let stats = fs.volume_stats().unwrap();
    assert_eq!(stats.mft_total_records, after.records());

    let fs = Filesystem::mount(&img).unwrap();
    for (path, data) in &want {
        let mut buf = vec![0u8; data.len() + 1];
        let n = fs
            .read_file(path, 0, &mut buf)
            .unwrap_or_else(|e| panic!("read {path}: {e:?}"));
        assert_eq!(&buf[..n], &data[..], "{path} reads back as written");
    }
    // What Windows wrote before is untouched.
    let windows = manifest("6");
    let got = walk(&img);
    assert!(
        windows.iter().all(|(p, v)| got.get(p) == Some(v)),
        "a file Windows wrote no longer reads as Windows listed it"
    );
    let mut io = fs_ntfs::block_io::PathIo::open_ro(std::path::Path::new(&img)).unwrap();
    let report = fsck::check_io(&mut io).expect("check");
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    std::fs::remove_file(&img).unwrap();
}

/// A volume image under a copy-on-write overlay, so a run can be stopped
/// and run again without copying the image: what a run writes lands in
/// `written`, sector by sector, and `base` is never changed. Once
/// `stop_after` writes have landed, every later write is dropped and
/// fails, as a power cut leaves them. Each write lands whole or not at
/// all.
struct Overlay<'a> {
    base: &'a [u8],
    written: BTreeMap<u64, Vec<u8>>,
    writes: usize,
    stop_after: Option<usize>,
}

impl<'a> Overlay<'a> {
    fn new(base: &'a [u8], stop_after: Option<usize>) -> Self {
        Overlay {
            base,
            written: BTreeMap::new(),
            writes: 0,
            stop_after,
        }
    }

    fn sector(&self, n: u64) -> &[u8] {
        self.written.get(&n).map_or_else(
            || &self.base[(n * 512) as usize..(n * 512 + 512) as usize],
            |s| s.as_slice(),
        )
    }

    fn bytes(&self, at: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for n in at / 512..(at + len as u64).div_ceil(512) {
            out.extend_from_slice(self.sector(n));
        }
        let skip = (at % 512) as usize;
        out[skip..skip + len].to_vec()
    }
}

impl fs_ntfs::block_io::BlockIo for Overlay<'_> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        if offset + buf.len() as u64 > self.base.len() as u64 {
            return Err(format!("read past the end at {offset:#x}"));
        }
        buf.copy_from_slice(&self.bytes(offset, buf.len()));
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        if self.stop_after.is_some_and(|n| self.writes >= n) {
            return Err("the power was cut".into());
        }
        assert!(
            offset.is_multiple_of(512) && buf.len().is_multiple_of(512),
            "a write of {} bytes at {offset:#x} is not whole sectors",
            buf.len()
        );
        for (i, s) in buf.chunks(512).enumerate() {
            self.written.insert(offset / 512 + i as u64, s.to_vec());
        }
        self.writes += 1;
        Ok(())
    }

    fn size(&self) -> u64 {
        self.base.len() as u64
    }
}

/// The volumes whose logs need undo, the part of a replay that a second
/// run could repeat.
const RERUN: [&str; 2] = ["undo", "open-at-checkpoint"];

#[test]
fn a_replay_stopped_after_any_write_reaches_the_same_volume_when_run_again() {
    // A replay writes the blocks its log changes, then empties the log. A
    // power cut can stop it after any of those writes, and the volume must
    // then come back right when `fsck` runs again: every block holding
    // what an uninterrupted replay leaves there, and the log holding
    // nothing to replay. Nothing else can be expected of the user (#137).
    //
    // An undo applied twice is not undone once: on `open-at-checkpoint`,
    // a stop from the 70th write of 101 on left index block 0x511f000 with
    // two entries for record 2559's name, where Windows' restart left one.
    // And a stop while the log is being emptied left no restart page in
    // it: no replay could read it, so the volume could no longer be
    // written.
    let mut failed = Vec::new();
    for k in RERUN {
        let img = unpack(&format!("windows-interrupted-{k}"));
        let base = std::fs::read(&img).unwrap();
        std::fs::remove_file(&img).unwrap();

        let mut whole = Overlay::new(&base, None);
        fsck::fsck_io(&mut whole, None).unwrap_or_else(|e| panic!("{k}: fsck: {e}"));
        let writes = whole.writes;

        // Where the replay writes: the blocks its plan names, $MFTMirr's
        // copies of the records among them, and the log.
        let mut io = Overlay::new(&base, None);
        let params = fs_ntfs::mft_io::read_boot_params_io(&mut io).unwrap();
        let (log_at, log_len) = fs_ntfs::read::nonresident_contiguous_disk_range(
            &mut io,
            2,
            fs_ntfs::attr_io::AttrType::Data,
            None,
        )
        .unwrap();
        let log = io.bytes(log_at, log_len as usize);
        let plan = fs_ntfs::logfile_replay::plan(&log, &params, &mut |at, buf| {
            buf.copy_from_slice(&io.bytes(at, buf.len()));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let in_log = |s: &u64| (log_at / 512..(log_at + log_len) / 512).contains(s);

        let mut bad = Vec::new();
        for stop in 0..writes {
            let mut cut = Overlay::new(&base, Some(stop));
            if fsck::fsck_io(&mut cut, None).is_ok() {
                bad.push(format!("stop after {stop}: the cut run reported success"));
                continue;
            }
            cut.stop_after = None;
            if let Err(e) = fsck::fsck_io(&mut cut, None) {
                bad.push(format!("stop after {stop}: fsck again refused: {e}"));
                continue;
            }

            // Every block the plan writes holds what it holds after an
            // uninterrupted replay: fixups undone and the update sequence
            // number blanked, which a block written once more moves.
            let as_content = |raw: &[u8]| {
                let magic: [u8; 4] = raw[..4].try_into().unwrap();
                if &magic != b"FILE" && &magic != b"INDX" {
                    return raw.to_vec();
                }
                let Some(mut b) = fixed(raw, &magic) else {
                    return raw.to_vec();
                };
                let uo = le(&b, 4, 2) as usize;
                b[uo..uo + 2].fill(0);
                b
            };
            let differ: Vec<String> = plan
                .writes
                .iter()
                .filter(|(&at, b)| {
                    as_content(&cut.bytes(at, b.len())) != as_content(&whole.bytes(at, b.len()))
                })
                .map(|(&at, _)| format!("{at:#x}"))
                .collect();
            // Anything else either run wrote outside the log: $MFTMirr's
            // copies, compared the same way, record by record.
            let rs = params.file_record_size;
            let elsewhere: std::collections::BTreeSet<u64> = cut
                .written
                .keys()
                .chain(whole.written.keys())
                .filter(|&&s| !in_log(&s))
                .filter(|&&s| {
                    !plan
                        .writes
                        .iter()
                        .any(|(&at, b)| (at / 512..(at + b.len() as u64) / 512).contains(&s))
                })
                .map(|&s| s * 512 / rs * rs)
                .collect();
            let stray: Vec<String> = elsewhere
                .iter()
                .filter(|&&at| {
                    as_content(&cut.bytes(at, rs as usize))
                        != as_content(&whole.bytes(at, rs as usize))
                })
                .map(|at| format!("{at:#x}"))
                .collect();
            if !differ.is_empty() || !stray.is_empty() {
                bad.push(format!(
                    "stop after {stop}: blocks {differ:?} and records {stray:?} differ from an \
                     uninterrupted replay"
                ));
                continue;
            }
            let state = fsck::logfile_state_io(&mut cut).expect("read $LogFile");
            if state.needs_replay() {
                bad.push(format!(
                    "stop after {stop}: the log still holds work: {state:?}"
                ));
            }
        }
        if !bad.is_empty() {
            failed.push(format!(
                "{k}: of {writes} stops, {} did not come back to the uninterrupted replay:\n{}",
                bad.len(),
                bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
            ));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[path = "replay_stop/mod.rs"]
mod replay_stop;

#[test]
fn a_replay_stopped_after_its_first_undo_write_leaves_the_log_holding_work() {
    // The image the Windows scenarios attach (#755) is what a power cut
    // right after the replay's first undo write leaves (#754): that write
    // and every one before it landed, none after, and the log still holds
    // the work Windows' own restart will then find (#648).
    for k in RERUN {
        let img = unpack(&format!("windows-interrupted-{k}"));
        let base = std::fs::read(&img).unwrap();
        std::fs::remove_file(&img).unwrap();

        let stamp = replay_stop::undo_stamp(&mut Overlay::new(&base, None))
            .unwrap_or_else(|e| panic!("{k}: undo's stamp: {e}"));
        let mut stopped = Overlay::new(&base, None);
        let n = replay_stop::stop_after_first_undo(&mut stopped)
            .unwrap_or_else(|e| panic!("{k}: stop: {e}"));

        // Exactly what the power cut after `n` writes leaves.
        let mut cut = Overlay::new(&base, Some(n));
        assert!(
            fsck::fsck_io(&mut cut, None).is_err(),
            "{k}: a replay cut after {n} writes reported success"
        );
        assert!(
            stopped.written == cut.written,
            "{k}: the stop after {n} writes differs from a power cut after {n}"
        );

        // The last write landed is a block undo rolled back.
        let undone: Vec<u64> = stopped
            .written
            .iter()
            .filter(|(_, s)| matches!(&s[..4], b"FILE" | b"INDX") && le(s, 8, 8) == stamp)
            .map(|(&n, _)| n * 512)
            .collect();
        assert!(
            !undone.is_empty(),
            "{k}: no block written carries undo's stamp {stamp:#x}"
        );

        let state = fsck::logfile_state_io(&mut stopped).expect("read $LogFile");
        assert!(
            state.needs_replay(),
            "{k}: after the stop the log no longer holds work: {state:?}"
        );
    }
}

/// Every capture whose `$LogFile` holds work, with the last LSN it holds.
fn captures_holding_work() -> Vec<(&'static str, u64)> {
    [
        &LOG_END[..],
        &WRAPPED_LOG_END,
        &INDEX_VCN_LOG_END,
        &SMALL_CLUSTER_LOG_END,
        &TAIL_CONTINUATION_LOG_END,
        &UNDO_LOG_END,
        &OPEN_AT_CHECKPOINT_LOG_END,
        &QUOTA_LOG_END,
        &FRESH_LOG_END,
        &QUOTA_SPILLED_LOG_END,
    ]
    .concat()
}

/// One write as a plan reports it: offset, length, and the SHA-256 of the
/// bytes there before it and of the bytes it leaves.
type PlannedWrite = (u64, u64, String, String);

/// A volume held in memory that records every write made to it, grouped
/// into the units a flush closes, and makes each one.
struct RecordedRepair {
    bytes: Vec<u8>,
    units: Vec<Vec<PlannedWrite>>,
    open: Vec<PlannedWrite>,
}

impl fs_ntfs::block_io::BlockIo for RecordedRepair {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let at = offset as usize;
        let there = self
            .bytes
            .get(at..at + buf.len())
            .ok_or("read past the end")?;
        buf.copy_from_slice(there);
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let at = offset as usize;
        let there = self
            .bytes
            .get_mut(at..at + buf.len())
            .ok_or("write past the end")?;
        self.open
            .push((offset, buf.len() as u64, hex_sha256(there), hex_sha256(buf)));
        there.copy_from_slice(buf);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn sync(&mut self) -> Result<(), String> {
        if !self.open.is_empty() {
            self.units.push(std::mem::take(&mut self.open));
        }
        Ok(())
    }
}

#[test]
fn a_dry_run_plans_the_writes_that_reach_what_windows_recovered_and_makes_none() {
    // `fsck.ntfs --dry-run` works out every write `fsck.ntfs -y` would
    // make, and makes none of them (#709). A plan is only worth reading if
    // it is the repair: the same writes, in the same order, each between
    // the same flushes, over the same bytes, leaving the same bytes. The
    // repair's own writes are recorded here, by this test, as it makes
    // them; the volume they leave must be what Windows' own restart
    // reached from the same capture.
    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();

        let mut repair = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        fsck::fsck_io(&mut repair, None).unwrap_or_else(|e| panic!("{k}: fsck: {e}"));
        fs_ntfs::block_io::BlockIo::sync(&mut repair).unwrap();
        // A replay writes its blocks, marks the log clean, then empties
        // it, each step flushed before the next.
        assert!(
            repair.units.len() > 1,
            "{k}: a replay is several durable steps; fsck made {}",
            repair.units.len()
        );
        let repaired = format!("{src}.repaired");
        std::fs::write(&repaired, &repair.bytes).unwrap();
        let want = manifest(k);
        let got = walk(&repaired);
        assert!(
            got == want,
            "{k}: {} files after the repair, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &Image::read(&repaired),
            &Image::read(&recovered),
            log_end,
        );
        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_file(&repaired).unwrap();

        // The dry run, on a source it may not write to, exits as -n does,
        // reports those writes, and leaves the file as it was.
        let mut readonly = std::fs::metadata(&src).unwrap().permissions();
        readonly.set_readonly(true);
        std::fs::set_permissions(&src, readonly).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
            .args(["fsck", "--dry-run", &src])
            .output()
            .expect("spawn fsck.ntfs");
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{k}: fsck.ntfs --dry-run printed no JSON report ({e}): stderr {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert_eq!(out.status.code(), Some(4), "{k}: exit as -n: {json}");
        assert_eq!(json["mode"], "dry-run", "{k}: {json}");
        let reported: Vec<Vec<PlannedWrite>> = json["plan"]["units"]
            .as_array()
            .unwrap_or_else(|| panic!("{k}: no plan.units in {}", json["plan"]))
            .iter()
            .map(|unit| {
                unit["writes"]
                    .as_array()
                    .expect("a unit's writes")
                    .iter()
                    .map(|w| {
                        (
                            w["offset"].as_u64().expect("offset"),
                            w["length"].as_u64().expect("length"),
                            w["before_sha256"].as_str().expect("before").to_string(),
                            w["after_sha256"].as_str().expect("after").to_string(),
                        )
                    })
                    .collect()
            })
            .collect();
        let first_differing = reported.iter().zip(&repair.units).position(|(r, m)| r != m);
        assert!(
            reported == repair.units,
            "{k}: fsck.ntfs --dry-run planned {} units, the repair made {}; first differing \
             unit {first_differing:?}",
            reported.len(),
            repair.units.len()
        );
        assert!(
            std::fs::read(&src).unwrap() == pristine,
            "{k}: fsck.ntfs --dry-run wrote to the source"
        );
        // Windows refuses to delete a read-only file.
        let mut writable = std::fs::metadata(&src).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        std::fs::set_permissions(&src, writable).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
/// Where each section of a saved plan starts, read from the layout
/// docs/repair-plan-file.md documents, not from the reader under test:
/// the units' starts, then the trailer's, then the file's end.
fn plan_file_sections(units: &[fs_ntfs::repair_plan::PlanUnit]) -> (Vec<usize>, usize, usize) {
    // Header: magic, version, unit count, file length. Identity: source
    // size, volume serial number.
    let mut at = 8 + 4 + 4 + 8 + 8 + 8;
    let mut starts = Vec::new();
    for unit in units {
        starts.push(at);
        // Write count, the before-image's SHA-256, then each write's
        // offset, length, before- and after-image.
        at += 4 + 32;
        at += unit
            .writes
            .iter()
            .map(|w| 8 + 4 + w.before.len() + w.after.len())
            .sum::<usize>();
    }
    (starts, at, at + 32)
}

#[cfg(feature = "plan-file")]
/// Applies a loaded plan onto `bytes` by this test's own writes, refusing
/// any write whose before-image is not what `bytes` hold.
fn apply_plan(k: &str, bytes: &mut [u8], units: &[fs_ntfs::repair_plan::PlanUnit]) {
    for (u, unit) in units.iter().enumerate() {
        for w in &unit.writes {
            let at = w.offset as usize;
            let there = &mut bytes[at..at + w.after.len()];
            assert!(
                there == &w.before[..],
                "{k}: unit {u}: the write at {at} finds other bytes than its before-image"
            );
            there.copy_from_slice(&w.after);
        }
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_saved_plan_loads_back_to_what_windows_recovered_and_only_when_intact() {
    // A repair plan is saved to a file of its own and loads back only when
    // intact (#905). Saved from each Windows capture, loaded and applied
    // by this test's own writes, it must reach what Windows' own restart
    // reached; truncated or changed anywhere, it must load as nothing.
    use fs_ntfs::repair_plan::{load_plan, plan_fsck_io, save_plan};
    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let mut volume = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_fsck_io(&mut volume, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        assert!(
            volume.bytes == pristine,
            "{k}: planning wrote to the volume"
        );
        let saved = save_plan(&plan, &mut volume).unwrap_or_else(|e| panic!("{k}: save: {e}"));

        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        assert_eq!(
            loaded.source_size,
            pristine.len() as u64,
            "{k}: source size"
        );
        let serial = u64::from_le_bytes(pristine[0x48..0x50].try_into().unwrap());
        assert_eq!(loaded.volume_serial, serial, "{k}: volume serial number");
        assert!(
            loaded.units == plan.units,
            "{k}: the units loaded back differ"
        );

        let mut copy = pristine.clone();
        apply_plan(k, &mut copy, &loaded.units);
        let repaired = format!("{src}.repaired");
        std::fs::write(&repaired, &copy).unwrap();
        let want = manifest(k);
        let got = walk(&repaired);
        assert!(
            got == want,
            "{k}: {} files after the loaded plan, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &Image::read(&repaired),
            &Image::read(&recovered),
            log_end,
        );
        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_file(&repaired).unwrap();

        let (starts, trailer, end) = plan_file_sections(&plan.units);
        assert_eq!(saved.len(), end, "{k}: the saved file's length");
        let pristine_sha = hex_sha256(&pristine);
        let refused = |what: &str, bytes: &[u8], names: &str| {
            let mut copy = pristine.clone();
            match load_plan(bytes) {
                Ok(p) => {
                    apply_plan(k, &mut copy, &p.units);
                    panic!("{k}: {what}: loaded");
                }
                Err(e) => assert!(
                    e.to_string().contains(names),
                    "{k}: {what}: refused as {e:?}, not naming {names:?}"
                ),
            }
            assert_eq!(hex_sha256(&copy), pristine_sha, "{k}: {what}: copy changed");
        };
        for &cut in starts.iter().chain([&trailer]) {
            refused(&format!("cut at {cut}"), &saved[..cut], "truncated");
            refused(
                &format!("cut at {}", cut + 1),
                &saved[..cut + 1],
                "truncated",
            );
        }
        let sections = [("header", 0), ("identity", 32), ("trailer", end - 1)]
            .into_iter()
            .map(|(s, at)| (s.to_string(), at))
            .chain(starts.iter().enumerate().flat_map(|(u, &at)| {
                [
                    (format!("unit {u} write count"), at),
                    (format!("unit {u} before-image hash"), at + 4),
                    (format!("unit {u} first write"), at + 4 + 32),
                ]
            }));
        for (section, at) in sections {
            let mut flipped = saved.clone();
            flipped[at] ^= 0x01;
            refused(&format!("{section} byte {at} flipped"), &flipped, "");
        }
        let mut newer = saved.clone();
        newer[8..12].copy_from_slice(&2u32.to_le_bytes());
        let sum = Sha256::digest(&newer[..trailer]);
        newer[trailer..].copy_from_slice(&sum);
        refused("format version 2", &newer, "version 2");

        std::fs::remove_file(&src).unwrap();
    }
}

/// A byte of write `w` in unit `u` that no earlier write of the plan
/// covers, so the source alone decides what the plan expects there.
#[cfg(feature = "plan-file")]
fn uncovered_byte(units: &[fs_ntfs::repair_plan::PlanUnit], u: usize, w: usize) -> Option<u64> {
    let earlier: Vec<&fs_ntfs::repair_plan::PlannedWrite> = units[..u]
        .iter()
        .flat_map(|x| &x.writes)
        .chain(&units[u].writes[..w])
        .collect();
    let me = &units[u].writes[w];
    (me.offset..me.offset + me.before.len() as u64).find(|&b| {
        !earlier
            .iter()
            .any(|e| (e.offset..e.offset + e.after.len() as u64).contains(&b))
    })
}

#[cfg(feature = "plan-file")]
#[test]
fn a_loaded_plan_is_checked_against_its_source_and_refused_on_the_first_mismatch() {
    // A loaded plan is checked against the volume it is about to be
    // applied to before anything is written (#908): the source's size,
    // its volume serial number, and every write's before-image at that
    // write's turn. A capture of Windows' own must check clean; the same
    // capture with one byte changed, one sector shorter, or with another
    // serial must be refused naming what differs, and never written.
    use fs_ntfs::repair_plan::{check_plan_source, load_plan, plan_fsck_io, save_plan};
    let volume = |bytes: Vec<u8>| RecordedRepair {
        bytes,
        units: Vec::new(),
        open: Vec::new(),
    };
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let pristine_sha = hex_sha256(&pristine);
        let mut v = volume(pristine.clone());
        let plan = plan_fsck_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        assert!(
            !loaded.units.is_empty(),
            "{k}: a log holding work planned nothing"
        );

        check_plan_source(&loaded, &mut v)
            .unwrap_or_else(|e| panic!("{k}: the capture it was made from was refused: {e}"));
        assert_eq!(
            hex_sha256(&v.bytes),
            pristine_sha,
            "{k}: the check wrote to the capture"
        );

        let refused = |what: &str, bytes: Vec<u8>, names: &str| {
            let before = hex_sha256(&bytes);
            let mut v = volume(bytes);
            match check_plan_source(&loaded, &mut v) {
                Ok(()) => panic!("{k}: {what}: checked clean"),
                Err(e) => assert!(
                    e.to_string().contains(names),
                    "{k}: {what}: refused as {e}, not naming {names:?}"
                ),
            }
            assert_eq!(hex_sha256(&v.bytes), before, "{k}: {what}: the check wrote");
        };

        let writes: Vec<(usize, usize)> = loaded
            .units
            .iter()
            .enumerate()
            .flat_map(|(u, unit)| (0..unit.writes.len()).map(move |w| (u, w)))
            .collect();
        let first = writes[0];
        let last = *writes
            .iter()
            .rev()
            .find(|&&(u, w)| uncovered_byte(&loaded.units, u, w).is_some())
            .unwrap_or_else(|| panic!("{k}: every write is covered by an earlier one"));
        for (u, w) in [first, last] {
            let at = uncovered_byte(&loaded.units, u, w).unwrap() as usize;
            let mut changed = pristine.clone();
            changed[at] ^= 0xff;
            refused(
                &format!("byte {at} changed"),
                changed,
                &format!("unit {u} write {w}"),
            );
        }

        let mut shorter = pristine.clone();
        shorter.truncate(pristine.len() - 512);
        refused("one sector shorter", shorter, "size");

        let mut other = pristine.clone();
        other[0x48] ^= 0xff;
        refused("another volume serial", other, "serial");

        std::fs::remove_file(&src).unwrap();
    }
}

/// A byte of write `w` that no earlier write of its own unit covers, so
/// the device alone decides what the unit expects there at its turn.
#[cfg(feature = "plan-file")]
fn uncovered_in_unit(unit: &fs_ntfs::repair_plan::PlanUnit, w: usize) -> Option<u64> {
    let me = &unit.writes[w];
    (me.offset..me.offset + me.before.len() as u64).find(|&b| {
        !unit.writes[..w]
            .iter()
            .any(|e| (e.offset..e.offset + e.after.len() as u64).contains(&b))
    })
}

#[cfg(feature = "plan-file")]
#[test]
fn a_plan_unit_is_checked_on_the_device_about_to_receive_it_at_its_turn() {
    // Each unit is checked on the device about to receive it, with the
    // plan's earlier units already written there (#913). Applied unit by
    // unit to a copy of Windows' own capture, every unit must check clean
    // at its turn; one byte changed inside the first, a middle or the last
    // unit's ranges must be refused naming that write's range, and the
    // check must never write.
    use fs_ntfs::repair_plan::{check_unit_on_device, load_plan, plan_fsck_io, save_plan};
    let volume = |bytes: Vec<u8>| RecordedRepair {
        bytes,
        units: Vec::new(),
        open: Vec::new(),
    };
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let mut v = volume(pristine.clone());
        let plan = plan_fsck_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        let n = loaded.units.len();
        assert!(n > 0, "{k}: a log holding work planned nothing");

        let mut copy = volume(pristine.clone());
        for u in 0..n {
            let before = hex_sha256(&copy.bytes);
            check_unit_on_device(&loaded, u, &mut copy)
                .unwrap_or_else(|e| panic!("{k}: unit {u} was refused at its turn: {e}"));
            assert_eq!(
                hex_sha256(&copy.bytes),
                before,
                "{k}: unit {u}: the check wrote"
            );
            apply_plan(k, &mut copy.bytes, &loaded.units[u..u + 1]);
        }

        let mut positions = vec![0, n / 2, n - 1];
        positions.dedup();
        for u in positions {
            let unit = &loaded.units[u];
            let (w, at) = (0..unit.writes.len())
                .find_map(|w| uncovered_in_unit(unit, w).map(|b| (w, b as usize)))
                .unwrap_or_else(|| panic!("{k}: unit {u} has no byte its own writes do not cover"));
            let mut copy = volume(pristine.clone());
            apply_plan(k, &mut copy.bytes, &loaded.units[..u]);
            copy.bytes[at] ^= 0xff;
            let before = hex_sha256(&copy.bytes);
            let write = &unit.writes[w];
            let range = format!(
                "{}..{}",
                write.offset,
                write.offset + write.before.len() as u64
            );
            match check_unit_on_device(&loaded, u, &mut copy) {
                Ok(()) => panic!("{k}: unit {u}: byte {at} changed, checked clean"),
                Err(e) => {
                    let e = e.to_string();
                    assert!(
                        e.contains(&format!("unit {u} write {w}")) && e.contains(&range),
                        "{k}: unit {u}: byte {at} changed, refused as {e}, not naming write {w} \
                         and bytes {range}"
                    );
                }
            }
            assert_eq!(
                hex_sha256(&copy.bytes),
                before,
                "{k}: unit {u}: the refused check wrote"
            );
        }

        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_copy_repair_reaches_what_windows_recovered_and_never_writes_its_source() {
    // A checked plan is applied by copy-repair (#909): the source is opened
    // read-only and checked, and only then is a new target created, the
    // source copied into it and the plan applied there. From each Windows
    // capture the target must reach what Windows' own restart reached and
    // the capture must keep its SHA-256; a source the check refuses must
    // leave no target behind, and a target that already exists is refused
    // and kept as it was.
    use fs_ntfs::repair_plan::{copy_repair, load_plan, plan_fsck_io, save_plan};
    use std::path::Path;
    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let pristine_sha = hex_sha256(&pristine);
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_fsck_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        assert!(
            !loaded.units.is_empty(),
            "{k}: a log holding work planned nothing"
        );

        let target = format!("{src}.copy-repaired");
        let _ = std::fs::remove_file(&target);
        copy_repair(&loaded, Path::new(&src), Path::new(&target))
            .unwrap_or_else(|e| panic!("{k}: copy-repair: {e}"));
        assert_eq!(
            hex_sha256(&std::fs::read(&src).unwrap()),
            pristine_sha,
            "{k}: copy-repair wrote to its source"
        );
        let want = manifest(k);
        let got = walk(&target);
        assert!(
            got == want,
            "{k}: {} files after copy-repair, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &Image::read(&target),
            &Image::read(&recovered),
            log_end,
        );
        std::fs::remove_file(&recovered).unwrap();

        // The target now exists: a second copy-repair onto it is refused
        // and leaves it as it was.
        let kept = hex_sha256(&std::fs::read(&target).unwrap());
        match copy_repair(&loaded, Path::new(&src), Path::new(&target)) {
            Ok(()) => panic!("{k}: copy-repair onto an existing target went ahead"),
            Err(e) => assert!(
                e.to_string().contains("exists"),
                "{k}: an existing target refused as {e}, not as existing"
            ),
        }
        assert_eq!(
            hex_sha256(&std::fs::read(&target).unwrap()),
            kept,
            "{k}: the refused copy-repair changed the existing target"
        );
        std::fs::remove_file(&target).unwrap();

        // One before-image byte changed in the source: the check refuses
        // it, and no target is created.
        let at = uncovered_byte(&loaded.units, 0, 0)
            .unwrap_or_else(|| panic!("{k}: the first write has no byte of its own"))
            as usize;
        let mut changed = pristine.clone();
        changed[at] ^= 0xff;
        let changed_src = format!("{src}.changed");
        std::fs::write(&changed_src, &changed).unwrap();
        let changed_sha = hex_sha256(&changed);
        let refused_target = format!("{src}.refused");
        let _ = std::fs::remove_file(&refused_target);
        match copy_repair(&loaded, Path::new(&changed_src), Path::new(&refused_target)) {
            Ok(()) => panic!("{k}: byte {at} changed, copy-repair went ahead"),
            Err(e) => assert!(
                e.to_string().contains("unit 0 write 0"),
                "{k}: byte {at} changed, refused as {e}, not naming unit 0 write 0"
            ),
        }
        assert!(
            !Path::new(&refused_target).exists(),
            "{k}: a refused copy-repair left a target behind"
        );
        assert_eq!(
            hex_sha256(&std::fs::read(&changed_src).unwrap()),
            changed_sha,
            "{k}: the refused copy-repair wrote to its source"
        );
        std::fs::remove_file(&changed_src).unwrap();

        std::fs::remove_file(&src).unwrap();
    }
}

/// A target that records each write's offset, and each sync as `None`, in
/// the order they reach it.
#[cfg(feature = "plan-file")]
struct CountingTarget {
    bytes: Vec<u8>,
    events: Vec<Option<u64>>,
}

#[cfg(feature = "plan-file")]
impl fs_ntfs::block_io::BlockIo for CountingTarget {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let at = offset as usize;
        let there = self
            .bytes
            .get(at..at + buf.len())
            .ok_or("read past the end")?;
        buf.copy_from_slice(there);
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let at = offset as usize;
        self.bytes
            .get_mut(at..at + buf.len())
            .ok_or("write past the end")?
            .copy_from_slice(buf);
        self.events.push(Some(offset));
        Ok(())
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn sync(&mut self) -> Result<(), String> {
        self.events.push(None);
        Ok(())
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_copy_repair_syncs_after_every_unit_and_never_inside_one() {
    // A unit is what is applied and flushed as a whole (#909): applied to a
    // counting target, every unit's writes must land in order with one
    // sync after them and none among them, and the target must end holding
    // what this test's own writes of the plan leave.
    use fs_ntfs::repair_plan::{apply_plan_units, load_plan, plan_fsck_io, save_plan};
    let mut most_units = 0;
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_fsck_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        most_units = most_units.max(loaded.units.len());

        let mut target = CountingTarget {
            bytes: pristine.clone(),
            events: Vec::new(),
        };
        apply_plan_units(&loaded, &mut target)
            .unwrap_or_else(|e| panic!("{k}: applying the plan: {e}"));
        let want: Vec<Option<u64>> = loaded
            .units
            .iter()
            .flat_map(|unit| unit.writes.iter().map(|w| Some(w.offset)).chain([None]))
            .collect();
        assert!(
            target.events == want,
            "{k}: {} writes and syncs reached the target, {} expected: a sync missing after a \
             unit, or one inside it",
            target.events.len(),
            want.len()
        );
        let mut expected = pristine.clone();
        apply_plan(k, &mut expected, &loaded.units);
        assert!(
            target.bytes == expected,
            "{k}: the target holds other bytes than the plan leaves"
        );

        std::fs::remove_file(&src).unwrap();
    }
    assert!(
        most_units > 1,
        "no capture planned more than one unit, so no boundary between units was tested"
    );
}

/// A target that changes one byte once it has seen `after` syncs, that
/// is between unit `after - 1` and unit `after`; with `after` 0 the byte
/// is changed before anything is written.
#[cfg(feature = "plan-file")]
struct ChangingTarget {
    bytes: Vec<u8>,
    syncs: usize,
    after: usize,
    at: usize,
}

#[cfg(feature = "plan-file")]
impl ChangingTarget {
    fn new(bytes: Vec<u8>, after: usize, at: usize) -> Self {
        let mut t = ChangingTarget {
            bytes,
            syncs: 0,
            after,
            at,
        };
        if after == 0 {
            t.bytes[at] ^= 0xff;
        }
        t
    }
}

#[cfg(feature = "plan-file")]
impl fs_ntfs::block_io::BlockIo for ChangingTarget {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let at = offset as usize;
        let there = self
            .bytes
            .get(at..at + buf.len())
            .ok_or("read past the end")?;
        buf.copy_from_slice(there);
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let at = offset as usize;
        self.bytes
            .get_mut(at..at + buf.len())
            .ok_or("write past the end")?
            .copy_from_slice(buf);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn sync(&mut self) -> Result<(), String> {
        self.syncs += 1;
        if self.syncs == self.after {
            self.bytes[self.at] ^= 0xff;
        }
        Ok(())
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn applying_a_plan_rechecks_each_unit_on_the_target_and_stops_with_earlier_units_intact() {
    // Each unit is re-checked on the target immediately before it is
    // written (#914). One byte of the first, a middle or the last unit's
    // ranges changed on the target after the previous unit's sync must
    // stop the apply before that unit, naming the unit and the range: the
    // earlier units stay written and synced, nothing of the refused unit
    // is written, and the source keeps its SHA-256. A plan made again from
    // the target as it now stands applies without refusal.
    use fs_ntfs::repair_plan::{apply_plan_units, load_plan, plan_fsck_io, save_plan};
    let volume = |bytes: Vec<u8>| RecordedRepair {
        bytes,
        units: Vec::new(),
        open: Vec::new(),
    };
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let pristine_sha = hex_sha256(&pristine);
        let mut v = volume(pristine.clone());
        let plan = plan_fsck_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        let n = loaded.units.len();
        assert!(n > 0, "{k}: a log holding work planned nothing");

        let mut positions = vec![0, n / 2, n - 1];
        positions.dedup();
        for u in positions {
            let unit = &loaded.units[u];
            let (w, at) = (0..unit.writes.len())
                .find_map(|w| uncovered_in_unit(unit, w).map(|b| (w, b as usize)))
                .unwrap_or_else(|| panic!("{k}: unit {u} has no byte its own writes do not cover"));
            let write = &unit.writes[w];
            let range = format!(
                "{}..{}",
                write.offset,
                write.offset + write.before.len() as u64
            );
            let mut target = ChangingTarget::new(pristine.clone(), u, at);
            match apply_plan_units(&loaded, &mut target) {
                Ok(()) => panic!("{k}: unit {u}: byte {at} changed on the target, applied anyway"),
                Err(e) => {
                    let e = e.to_string();
                    assert!(
                        e.contains(&format!("unit {u} write {w}")) && e.contains(&range),
                        "{k}: unit {u}: byte {at} changed, refused as {e}, not naming unit {u} \
                         write {w} and bytes {range}"
                    );
                }
            }
            assert_eq!(
                target.syncs, u,
                "{k}: unit {u} refused after {} syncs, not one per earlier unit",
                target.syncs
            );
            let mut expected = pristine.clone();
            apply_plan(k, &mut expected, &loaded.units[..u]);
            expected[at] ^= 0xff;
            assert!(
                target.bytes == expected,
                "{k}: unit {u} refused: the target holds other bytes than the earlier units \
                 and the changed byte, so the refused unit was written or an earlier one lost"
            );
            assert_eq!(
                hex_sha256(&std::fs::read(&src).unwrap()),
                pristine_sha,
                "{k}: unit {u}: the source changed"
            );

            // A plan made again from the target as it now stands applies.
            let mut now = volume(target.bytes.clone());
            let again = plan_fsck_io(&mut now, fsck::FsckOptions::default())
                .unwrap_or_else(|e| panic!("{k}: unit {u}: planning again: {e}"));
            let again = save_plan(&again, &mut now)
                .unwrap_or_else(|e| panic!("{k}: unit {u}: saving again: {e}"));
            let again =
                load_plan(&again).unwrap_or_else(|e| panic!("{k}: unit {u}: loading again: {e}"));
            let mut retry = ChangingTarget {
                bytes: target.bytes.clone(),
                syncs: 0,
                after: usize::MAX,
                at,
            };
            apply_plan_units(&again, &mut retry)
                .unwrap_or_else(|e| panic!("{k}: unit {u}: the plan made again was refused: {e}"));
        }

        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn applying_a_plan_refuses_a_target_of_another_size_or_serial_before_the_first_unit() {
    // The device about to be written is checked against the plan's source
    // identity before the first unit (#915), not only the source before
    // the copy. A copy one sector short, and a copy whose boot-sector
    // serial at bytes 0x48..0x50 changed after the copy, must each be
    // refused naming size or serial, with no write of any unit reaching
    // the target, and the source keeping its SHA-256.
    use fs_ntfs::repair_plan::{apply_plan_units, load_plan, plan_fsck_io, save_plan};
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let pristine_sha = hex_sha256(&pristine);
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_fsck_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        assert!(
            !loaded.units.is_empty(),
            "{k}: a log holding work planned nothing"
        );

        let sector = u16::from_le_bytes([pristine[0x0b], pristine[0x0c]]) as usize;
        let short = pristine[..pristine.len() - sector].to_vec();
        let mut reserialled = pristine.clone();
        reserialled[0x48] ^= 0xff;
        let serial = u64::from_le_bytes(reserialled[0x48..0x50].try_into().unwrap());
        for (what, copy, named) in [
            ("size", short, format!("{}", pristine.len() - sector)),
            ("serial", reserialled, format!("{serial:#018x}")),
        ] {
            let mut target = CountingTarget {
                bytes: copy.clone(),
                events: Vec::new(),
            };
            match apply_plan_units(&loaded, &mut target) {
                Ok(()) => panic!("{k}: a target of another {what} was applied to"),
                Err(e) => {
                    let e = e.to_string();
                    assert!(
                        e.contains(what) && e.contains(&named),
                        "{k}: a target of another {what} refused as {e}, not naming its {what} \
                         {named}"
                    );
                }
            }
            assert!(
                target.events.is_empty() && target.bytes == copy,
                "{k}: a target of another {what} was refused after {} writes and syncs reached \
                 it",
                target.events.len()
            );
        }
        assert_eq!(
            hex_sha256(&std::fs::read(&src).unwrap()),
            pristine_sha,
            "{k}: the source changed"
        );

        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_repair_journal_reads_back_to_its_last_whole_record_and_refuses_the_rest() {
    // A repair journal is an append-only external file of checksummed
    // records (#918). Written for a plan from each Windows capture, it
    // must read back record for record, each intent carrying its unit's
    // before- and after-images; cut at a record boundary it must read back
    // the records before the cut, and one byte past it name the torn tail.
    // A changed record that is not the last, an applied record with no
    // intent, a skipped unit and another capture's plan must be refused,
    // naming the record.
    use fs_ntfs::repair_journal::{
        applied_record, header_record, intent_record, read_journal, JournalBinding, JournalRecord,
        TornRecord,
    };
    use fs_ntfs::repair_plan::{load_plan, plan_fsck_io, save_plan};
    let mut plans = Vec::new();
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let mut v = RecordedRepair {
            bytes: std::fs::read(&src).unwrap(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_fsck_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        std::fs::remove_file(&src).unwrap();
        plans.push((k, saved));
    }
    assert!(
        plans.len() > 1,
        "only one capture holds work, so no journal could be read against another plan"
    );
    for (i, (k, saved)) in plans.iter().enumerate() {
        let loaded = load_plan(saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        let binding = JournalBinding::of(saved).unwrap_or_else(|e| panic!("{k}: bind: {e}"));
        let mut journal = header_record(&binding);
        let mut bounds = vec![0, journal.len()];
        for (u, unit) in loaded.units.iter().enumerate() {
            journal.extend(intent_record(u as u32, unit));
            bounds.push(journal.len());
            journal.extend(applied_record(u as u32));
            bounds.push(journal.len());
        }

        let read = read_journal(&journal, &binding).unwrap_or_else(|e| panic!("{k}: read: {e}"));
        assert_eq!(read.torn, None, "{k}: a whole journal read as torn");
        assert_eq!(
            read.records.len(),
            1 + 2 * loaded.units.len(),
            "{k}: records read back"
        );
        assert_eq!(
            read.records[0],
            JournalRecord::Header(binding.clone()),
            "{k}: header"
        );
        for (u, unit) in loaded.units.iter().enumerate() {
            match &read.records[1 + 2 * u] {
                JournalRecord::Intent { unit: n, writes } => {
                    assert_eq!(*n as usize, u, "{k}: intent {u}'s unit index");
                    assert_eq!(writes.len(), unit.writes.len(), "{k}: intent {u}'s writes");
                    for (w, (got, want)) in writes.iter().zip(&unit.writes).enumerate() {
                        assert_eq!(got.offset, want.offset, "{k}: unit {u} write {w} offset");
                        assert!(
                            got.before == want.before,
                            "{k}: unit {u} write {w}: preimage is not the plan's before-image"
                        );
                        assert!(
                            got.after == want.after,
                            "{k}: unit {u} write {w}: redo is not the plan's after-image"
                        );
                    }
                }
                other => panic!(
                    "{k}: record {} is {other:?}, not unit {u}'s intent",
                    1 + 2 * u
                ),
            }
            assert_eq!(
                read.records[2 + 2 * u],
                JournalRecord::Applied { unit: u as u32 },
                "{k}: unit {u}'s applied record"
            );
        }

        for (r, &cut) in bounds.iter().enumerate() {
            let back = read_journal(&journal[..cut], &binding)
                .unwrap_or_else(|e| panic!("{k}: cut at {cut}: {e}"));
            assert!(
                back.records == read.records[..r] && back.torn.is_none(),
                "{k}: cut at {cut}: read {} records, torn {:?}, not the {r} before it",
                back.records.len(),
                back.torn
            );
            if cut < journal.len() {
                let back = read_journal(&journal[..cut + 1], &binding)
                    .unwrap_or_else(|e| panic!("{k}: cut at {}: {e}", cut + 1));
                assert!(
                    back.records == read.records[..r],
                    "{k}: cut at {}: read {} records, not the {r} before it",
                    cut + 1,
                    back.records.len()
                );
                assert_eq!(
                    back.torn,
                    Some(TornRecord {
                        record: r,
                        offset: cut as u64
                    }),
                    "{k}: cut at {}: the torn tail",
                    cut + 1
                );
            }
        }

        let refused =
            |what: &str, bytes: &[u8], against: &JournalBinding, record: usize| match read_journal(
                bytes, against,
            ) {
                Ok(j) => panic!("{k}: {what}: read back {} records", j.records.len()),
                Err(e) => assert!(
                    e.to_string().contains(&format!("journal record {record}:")),
                    "{k}: {what}: refused as {e}, not naming record {record}"
                ),
            };
        for r in 0..bounds.len() - 2 {
            let (start, end) = (bounds[r], bounds[r + 1]);
            for at in [start, start + 8, end - 33, end - 1] {
                let mut flipped = journal.clone();
                flipped[at] ^= 0x01;
                refused(
                    &format!("record {r} byte {at} flipped"),
                    &flipped,
                    &binding,
                    r,
                );
            }
        }
        let mut orphan = header_record(&binding);
        orphan.extend(applied_record(0));
        refused("an applied record with no intent", &orphan, &binding, 1);
        let mut skipped = journal[..bounds[3]].to_vec();
        skipped.extend(intent_record(2, &loaded.units[0]));
        refused("unit 1 skipped", &skipped, &binding, 3);
        let (other, other_saved) = &plans[(i + 1) % plans.len()];
        let other_binding =
            JournalBinding::of(other_saved).unwrap_or_else(|e| panic!("{other}: bind: {e}"));
        refused(
            &format!("read against {other}'s plan"),
            &journal,
            &other_binding,
            0,
        );
    }
}

/// Each capture's plan, loaded back from its file, with the SHA-256 of
/// that file and the capture's bytes.
#[cfg(feature = "plan-file")]
fn saved_plan(k: &str, src: &str) -> (fs_ntfs::repair_plan::SavedPlan, [u8; 32], Vec<u8>) {
    use fs_ntfs::repair_plan::{load_plan, plan_in_place_io, save_plan};
    let pristine = std::fs::read(src).unwrap();
    let mut v = RecordedRepair {
        bytes: pristine.clone(),
        units: Vec::new(),
        open: Vec::new(),
    };
    let plan = plan_in_place_io(&mut v, fsck::FsckOptions::default())
        .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
    let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
    let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
    assert!(
        !loaded.units.is_empty(),
        "{k}: a log holding work planned nothing"
    );
    (loaded, Sha256::digest(&saved).into(), pristine)
}

/// An in-place-repaired `device` as Windows' recovery is compared with
/// it. An in-place repair leaves the volume dirty until it is audited
/// (#716), and Windows' recovery leaves it clean, so the flag must be set
/// and is cleared on a copy in memory before the comparison: `$Volume`'s
/// `VOLUME_IS_DIRTY` bit, and its `$MFTMirr` copy, are the only bytes that
/// differ from the device (#1127).
#[cfg(feature = "plan-file")]
fn in_place_image(k: &str, device: &str) -> Image {
    let mut v = RecordedRepair {
        bytes: std::fs::read(device).unwrap(),
        units: Vec::new(),
        open: Vec::new(),
    };
    assert!(
        fsck::is_dirty_io(&mut v).unwrap_or_else(|e| panic!("{k}: dirty flag: {e}")),
        "{k}: the in-place repair left the dirty flag clear"
    );
    fsck::clear_dirty_io(&mut v).unwrap_or_else(|e| panic!("{k}: clear dirty: {e}"));
    Image::of(v.bytes)
}

#[cfg(feature = "plan-file")]
#[test]
fn an_in_place_repair_reaches_what_windows_recovered_and_equals_the_copy_repair() {
    // A plan applied in place through a journal (#919) must leave the
    // device byte for byte where copy-repair leaves its target from the
    // same plan, and so reach the files and metadata Windows' own restart
    // reached. The journal lives in another directory than the device.
    use fs_ntfs::block_io::PathIo;
    use fs_ntfs::repair_journal::{apply_in_place, read_journal, JournalBinding, JournalRecord};
    use fs_ntfs::repair_plan::copy_repair;
    use std::path::Path;
    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);

        let target = format!("{src}.copy-repaired");
        let _ = std::fs::remove_file(&target);
        copy_repair(&loaded, Path::new(&src), Path::new(&target))
            .unwrap_or_else(|e| panic!("{k}: copy-repair: {e}"));

        let device = format!("{src}.in-place");
        std::fs::write(&device, &pristine).unwrap();
        let journal_dir =
            std::env::temp_dir().join(format!("rust-fs-ntfs-in-place-{}-{k}", std::process::id()));
        let _ = std::fs::remove_dir_all(&journal_dir);
        std::fs::create_dir_all(&journal_dir).unwrap();
        let journal = journal_dir.join("repair.journal");
        std::fs::write(&journal, b"").unwrap();
        {
            let mut dev = PathIo::open_rw(Path::new(&device)).unwrap();
            let mut jrn = PathIo::open_rw(&journal).unwrap();
            apply_in_place(&loaded, plan_sha256, &mut dev, &mut jrn)
                .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        }
        assert!(
            std::fs::read(&device).unwrap() == std::fs::read(&target).unwrap(),
            "{k}: the in-place repair left other bytes than copy-repair's target"
        );
        let binding = JournalBinding {
            plan_sha256,
            source_size: loaded.source_size,
            volume_serial: loaded.volume_serial,
        };
        let read = read_journal(&std::fs::read(&journal).unwrap(), &binding)
            .unwrap_or_else(|e| panic!("{k}: the journal reads back as {e}"));
        let applied = read
            .records
            .iter()
            .filter(|r| matches!(r, JournalRecord::Applied { .. }))
            .count();
        assert_eq!(
            applied,
            loaded.units.len(),
            "{k}: the journal marks {applied} of {} units applied",
            loaded.units.len()
        );
        assert_eq!(read.torn, None, "{k}: the journal ends in a torn record");

        let want = manifest(k);
        let got = walk(&device);
        assert!(
            got == want,
            "{k}: {} files after the in-place repair, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &in_place_image(k, &device),
            &Image::read(&recovered),
            log_end,
        );
        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_dir_all(&journal_dir).unwrap();
        std::fs::remove_file(&device).unwrap();
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

/// What reached a device or a journal, in the order it reached either.
#[cfg(feature = "plan-file")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IoEvent {
    Write,
    Sync,
    Read,
}

/// Every write, sync and read that reached a recording pair, tagged
/// `true` for the journal.
#[cfg(feature = "plan-file")]
type IoLog = std::rc::Rc<std::cell::RefCell<Vec<(bool, IoEvent)>>>;

/// One side of a recording pair: in-memory bytes, and every write, sync
/// and read pushed to a log shared with the other side, tagged
/// `journal` or not. `drop_write` names one write, by its order among
/// this side's writes, that is recorded but never lands. `cut_after`
/// cuts the power once that many writes have landed on either side: every
/// later write fails and lands nothing. `tear` makes the first write the
/// cut stops land in part before it fails: the half of its bytes it
/// names, the other half left as the device held it.
#[cfg(feature = "plan-file")]
struct RecordingIo {
    bytes: Vec<u8>,
    journal: bool,
    log: IoLog,
    writes: usize,
    drop_write: Option<usize>,
    cut_after: Option<usize>,
    tear: Option<Tear>,
}

/// Which half of a write torn by a power cut lands: the first half, as a
/// device writing in order leaves it, or the second, as one writing its
/// sectors out of order can.
#[cfg(feature = "plan-file")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tear {
    FirstHalf,
    SecondHalf,
}

#[cfg(feature = "plan-file")]
impl fs_ntfs::block_io::BlockIo for RecordingIo {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let at = offset as usize;
        let there = self
            .bytes
            .get(at..at + buf.len())
            .ok_or("read past the end")?;
        buf.copy_from_slice(there);
        self.log.borrow_mut().push((self.journal, IoEvent::Read));
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        if let Some(n) = self.cut_after {
            let landed = self
                .log
                .borrow()
                .iter()
                .filter(|(_, e)| *e == IoEvent::Write)
                .count();
            if landed >= n {
                let at = offset as usize;
                let half = buf.len() / 2;
                let landing = match self.tear.take() {
                    Some(Tear::FirstHalf) => Some(at..at + half),
                    Some(Tear::SecondHalf) => Some(at + half..at + buf.len()),
                    None => None,
                };
                if let Some(range) = landing {
                    let from = range.start - at;
                    if let Some(there) = self.bytes.get_mut(range.clone()) {
                        there.copy_from_slice(&buf[from..from + range.len()]);
                    }
                }
                return Err("the power was cut".into());
            }
        }
        let at = offset as usize;
        if self.journal && self.bytes.len() < at + buf.len() {
            self.bytes.resize(at + buf.len(), 0);
        }
        if self.drop_write != Some(self.writes) {
            self.bytes
                .get_mut(at..at + buf.len())
                .ok_or("write past the end")?
                .copy_from_slice(buf);
        }
        self.writes += 1;
        self.log.borrow_mut().push((self.journal, IoEvent::Write));
        Ok(())
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn sync(&mut self) -> Result<(), String> {
        self.log.borrow_mut().push((self.journal, IoEvent::Sync));
        Ok(())
    }
}

#[cfg(feature = "plan-file")]
fn recording_pair(pristine: &[u8], drop_write: Option<usize>) -> (RecordingIo, RecordingIo, IoLog) {
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let device = RecordingIo {
        bytes: pristine.to_vec(),
        journal: false,
        log: log.clone(),
        writes: 0,
        drop_write,
        cut_after: None,
        tear: None,
    };
    let journal = RecordingIo {
        bytes: Vec::new(),
        journal: true,
        log: log.clone(),
        writes: 0,
        drop_write: None,
        cut_after: None,
        tear: None,
    };
    (device, journal, log)
}

#[cfg(feature = "plan-file")]
#[test]
fn an_in_place_repair_makes_each_unit_durable_in_the_journal_before_writing_it() {
    // For every unit (#919): the device is checked, the intent record is
    // written, flushed and read back from the journal, and only then does
    // the unit reach the device; the device is flushed and every write read
    // back before the applied record is written and flushed; and no device
    // write falls between units. The expected order is built here from the
    // plan, one unit at a time, and must be exactly what reached the pair.
    // After the last unit's applied record, the audit (#941) reads the
    // device and does nothing else: no write and no sync on either side.
    use fs_ntfs::repair_journal::apply_in_place;
    use IoEvent::{Read, Sync, Write};
    const J: bool = true;
    const D: bool = false;
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let (mut device, mut journal, log) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));

        let mut want = vec![(J, Write), (J, Sync), (J, Read)];
        for unit in &loaded.units {
            let n = unit.writes.len();
            want.extend(std::iter::repeat_n((D, Read), n));
            want.extend([(J, Write), (J, Sync), (J, Read)]);
            want.extend(std::iter::repeat_n((D, Write), n));
            want.push((D, Sync));
            want.extend(std::iter::repeat_n((D, Read), n));
            want.extend([(J, Write), (J, Sync)]);
        }
        let got = log.borrow().clone();
        let first = got.iter().zip(&want).position(|(a, b)| a != b);
        assert!(
            got.len() > want.len() && got[..want.len()] == want[..],
            "{k}: {} events reached the device and journal, the units' {} expected first and \
             the audit's after them; first difference at event {first:?}",
            got.len(),
            want.len()
        );
        assert!(
            got[want.len()..].iter().all(|&e| e == (D, Read)),
            "{k}: the audit after the last unit did more than read the device"
        );
        let mut expected = pristine.clone();
        apply_plan(k, &mut expected, &loaded.units);
        assert!(
            device.bytes == expected,
            "{k}: the device holds other bytes than the plan leaves"
        );
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_completed_in_place_apply_returns_an_audit_finding_only_the_dirty_flag() {
    // After the last unit (#941), an in-place apply and a resume run the
    // read-only check over the whole repaired device and return its report.
    // The repair leaves the dirty flag set, so the report's flag is set and
    // nothing else is found; the device still reads dirty afterwards.
    use fs_ntfs::repair_journal::{apply_in_place, resume_in_place};
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let (mut device, mut journal, _) = recording_pair(&pristine, None);
        let audit = apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        assert!(audit.dirty, "{k}: the audit read the dirty flag clear");
        assert!(
            audit.findings.is_empty(),
            "{k}: the audit found more than the dirty flag: {:?}",
            audit.findings
        );
        assert!(
            fsck::is_dirty_io(&mut device).unwrap_or_else(|e| panic!("{k}: dirty flag: {e}")),
            "{k}: the in-place repair left the dirty flag clear"
        );
        let audit = resume_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: resuming the finished repair: {e}"));
        assert!(
            audit.dirty && audit.findings.is_empty(),
            "{k}: the resume's audit: dirty {}, findings {:?}",
            audit.dirty,
            audit.findings
        );
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn an_in_place_apply_audit_reports_a_mirror_record_changed_outside_every_plan_range() {
    // A byte of a `$MFTMirr` record that no unit writes is changed on the
    // device before the apply (#941). The units never read it, so only an
    // audit that reads the device rather than the planned bytes can see it:
    // the report names the mirror mismatch, and the flag stays set.
    use fs_ntfs::repair_journal::apply_in_place;
    let (k, _) = captures_holding_work()[0];
    let src = unpack(&format!("windows-interrupted-{k}"));
    let (loaded, plan_sha256, mut pristine) = saved_plan(k, &src);
    let sector = u64::from(u16::from_le_bytes([pristine[0x0B], pristine[0x0C]]));
    let cluster = sector * u64::from(pristine[0x0D]);
    let mirror_lcn = u64::from_le_bytes(pristine[0x38..0x40].try_into().unwrap());
    let record_size = match pristine[0x40] as i8 {
        n if n < 0 => 1u64 << -n,
        n => n as u64 * cluster,
    };
    let planned = |at: u64| {
        loaded.units.iter().any(|u| {
            u.writes
                .iter()
                .any(|w| (w.offset..w.offset + w.after.len() as u64).contains(&at))
        })
    };
    let (record, at) = [1u64, 2, 0, 3]
        .into_iter()
        .map(|r| (r, mirror_lcn * cluster + r * record_size + 0x100))
        .find(|&(_, at)| !planned(at))
        .unwrap_or_else(|| panic!("{k}: the plan writes every mirror record's byte 0x100"));
    pristine[at as usize] ^= 0xFF;
    let (mut device, mut journal, _) = recording_pair(&pristine, None);
    let audit = apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
        .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
    assert!(
        audit
            .findings
            .contains(&fsck::CheckFinding::MirrorMismatch { record }),
        "{k}: the audit did not report mirror record {record}: {:?}",
        audit.findings
    );
    assert!(audit.dirty, "{k}: the audit read the dirty flag clear");
    assert!(
        fsck::is_dirty_io(&mut device).unwrap_or_else(|e| panic!("{k}: dirty flag: {e}")),
        "{k}: the in-place repair left the dirty flag clear"
    );
    std::fs::remove_file(&src).unwrap();
}

#[cfg(feature = "plan-file")]
#[test]
fn an_in_place_repair_stops_at_a_unit_the_device_reads_back_wrong_and_never_marks_it_applied() {
    // A device that drops one write (#919) reads that unit back different
    // from its redo: the apply stops there, naming the unit and the range,
    // and the journal holds the unit's intent but no applied record for it.
    use fs_ntfs::repair_journal::{apply_in_place, read_journal, JournalBinding, JournalRecord};
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let last = loaded.units.len() - 1;
        let first_of_last: usize = loaded.units[..last].iter().map(|u| u.writes.len()).sum();
        let dropped = &loaded.units[last].writes[0];
        let (start, end) = (dropped.offset, dropped.offset + dropped.after.len() as u64);
        assert!(
            dropped.before != dropped.after,
            "{k}: unit {last}'s first write changes nothing, so dropping it proves nothing"
        );
        let (mut device, mut journal, _) = recording_pair(&pristine, Some(first_of_last));
        match apply_in_place(&loaded, plan_sha256, &mut device, &mut journal) {
            Ok(_) => panic!("{k}: unit {last}'s first write was dropped, the apply went ahead"),
            Err(e) => {
                let e = e.to_string();
                assert!(
                    e.contains(&format!("unit {last} ")) && e.contains(&format!("{start}..{end}")),
                    "{k}: a dropped write refused as {e}, not naming unit {last} and bytes \
                     {start}..{end}"
                );
            }
        }
        let binding = JournalBinding {
            plan_sha256,
            source_size: loaded.source_size,
            volume_serial: loaded.volume_serial,
        };
        let read = read_journal(&journal.bytes, &binding)
            .unwrap_or_else(|e| panic!("{k}: the journal reads back as {e}"));
        let last_u32 = last as u32;
        assert!(
            matches!(read.records.last(), Some(JournalRecord::Intent { unit, .. }) if *unit == last_u32),
            "{k}: the journal does not end at unit {last}'s intent: {:?}",
            read.records.last().map(std::mem::discriminant)
        );
        assert!(
            !read
                .records
                .iter()
                .any(|r| matches!(r, JournalRecord::Applied { unit } if *unit == last_u32)),
            "{k}: unit {last} is marked applied though the device read it back wrong"
        );
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_repair_journal_reports_the_units_applied_and_the_unit_left_open_at_every_cut() {
    // Wherever an in-place apply's journal is cut (#926), its progress
    // names the units with an applied record wholly before the cut, and
    // the unit whose intent is whole before the cut with no applied record
    // after it. Every byte inside one record reads back alike, so the cuts
    // are each record boundary, one byte either side of it and the middle
    // of each record. The expected answer is built from the plan's own
    // record lengths, not from the reader.
    use fs_ntfs::repair_journal::{
        applied_record, apply_in_place, header_record, intent_record, progress, read_journal,
        JournalBinding, Progress, ProgressError,
    };
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let (mut device, mut journal, _) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        let bytes = journal.bytes;
        let binding = JournalBinding {
            plan_sha256,
            source_size: loaded.source_size,
            volume_serial: loaded.volume_serial,
        };

        // Each record's end, and the progress a journal cut there holds.
        let mut at = header_record(&binding).len();
        let mut ends = vec![(
            at,
            Progress {
                applied: 0,
                open: None,
            },
        )];
        for (u, unit) in loaded.units.iter().enumerate() {
            let u = u as u32;
            at += intent_record(u, unit).len();
            ends.push((
                at,
                Progress {
                    applied: u,
                    open: Some(u),
                },
            ));
            at += applied_record(u).len();
            ends.push((
                at,
                Progress {
                    applied: u + 1,
                    open: None,
                },
            ));
        }
        assert_eq!(at, bytes.len(), "{k}: the journal is not its records");

        let mut cuts = vec![0, 1];
        let mut start = 0;
        for &(end, _) in &ends {
            cuts.extend([(start + end) / 2, end - 1, end, end + 1]);
            start = end;
        }
        cuts.retain(|&c| c <= bytes.len());
        for cut in cuts {
            let want = ends.iter().rev().find(|(end, _)| *end <= cut).map_or(
                Progress {
                    applied: 0,
                    open: None,
                },
                |(_, p)| *p,
            );
            let read = read_journal(&bytes[..cut], &binding)
                .unwrap_or_else(|e| panic!("{k}: cut at byte {cut}: the journal reads as {e}"));
            let got = progress(&read, &loaded)
                .unwrap_or_else(|e| panic!("{k}: cut at byte {cut}: progress refused: {e}"));
            assert_eq!(got, want, "{k}: cut at byte {cut}");
        }

        // One byte of unit 0's first redo changed, its checksum whole.
        let mut changed = loaded.units[0].clone();
        changed.writes[0].after[0] ^= 0xff;
        let head = header_record(&binding).len();
        let first = intent_record(0, &loaded.units[0]).len();
        let mut forged = bytes[..head].to_vec();
        forged.extend(intent_record(0, &changed));
        forged.extend(&bytes[head + first..]);
        let read = read_journal(&forged, &binding)
            .unwrap_or_else(|e| panic!("{k}: the forged journal reads as {e}"));
        let refusal = progress(&read, &loaded);
        assert_eq!(
            refusal,
            Err(ProgressError::WriteDiffers { unit: 0, write: 0 }),
            "{k}: an intent whose redo is not the plan's"
        );
        assert!(
            refusal
                .unwrap_err()
                .to_string()
                .contains("plan unit 0 write 0"),
            "{k}: the refusal does not name the unit and write"
        );
        std::fs::remove_file(&src).unwrap();
    }
}

/// Cut the power on both sides of a recording pair once `n` writes have
/// landed, or restore it with `None`.
#[cfg(feature = "plan-file")]
fn cut_pair(device: &mut RecordingIo, journal: &mut RecordingIo, n: Option<usize>) {
    device.cut_after = n;
    journal.cut_after = n;
}

#[cfg(feature = "plan-file")]
#[test]
fn an_in_place_repair_stopped_after_any_write_resumes_to_the_uninterrupted_volume() {
    // A power cut can stop an in-place apply after any device write or
    // journal append (#927). Resumed on the same device and journal, it
    // must leave the device byte for byte where an uninterrupted apply
    // leaves it, and the journal holding exactly one intent and one
    // applied record per unit, in order. The cuts fall between whole
    // writes, as the replay's do in
    // a_replay_stopped_after_any_write_reaches_the_same_volume_when_run_again;
    // the resumed device must then reach the files and metadata Windows'
    // own restart reached.
    use fs_ntfs::repair_journal::{
        apply_in_place, read_journal, resume_in_place, JournalBinding, JournalRecord,
    };
    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let (mut device, mut journal, log) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        let whole = device.bytes;
        let writes = log
            .borrow()
            .iter()
            .filter(|(_, e)| *e == IoEvent::Write)
            .count();
        let binding = JournalBinding {
            plan_sha256,
            source_size: loaded.source_size,
            volume_serial: loaded.volume_serial,
        };
        let every_unit: Vec<u32> = (0..loaded.units.len() as u32).collect();

        let mut bad = Vec::new();
        let mut resumed = None;
        for stop in 0..writes {
            let (mut device, mut journal, _) = recording_pair(&pristine, None);
            cut_pair(&mut device, &mut journal, Some(stop));
            if apply_in_place(&loaded, plan_sha256, &mut device, &mut journal).is_ok() {
                bad.push(format!("stop after {stop}: the cut apply reported success"));
                continue;
            }
            cut_pair(&mut device, &mut journal, None);
            if let Err(e) = resume_in_place(&loaded, plan_sha256, &mut device, &mut journal) {
                bad.push(format!("stop after {stop}: the resume refused: {e}"));
                continue;
            }
            if device.bytes != whole {
                bad.push(format!(
                    "stop after {stop}: the device holds other bytes than an uninterrupted apply"
                ));
                continue;
            }
            let read = match read_journal(&journal.bytes, &binding) {
                Ok(read) => read,
                Err(e) => {
                    bad.push(format!("stop after {stop}: the journal reads back as {e}"));
                    continue;
                }
            };
            let intents: Vec<u32> = read
                .records
                .iter()
                .filter_map(|r| match r {
                    JournalRecord::Intent { unit, .. } => Some(*unit),
                    _ => None,
                })
                .collect();
            let applied: Vec<u32> = read
                .records
                .iter()
                .filter_map(|r| match r {
                    JournalRecord::Applied { unit } => Some(*unit),
                    _ => None,
                })
                .collect();
            if intents != every_unit || applied != every_unit || read.torn.is_some() {
                bad.push(format!(
                    "stop after {stop}: the journal holds intents {intents:?}, applied \
                     {applied:?} and torn {:?}",
                    read.torn
                ));
                continue;
            }
            resumed = Some(device.bytes);
        }
        assert!(
            bad.is_empty(),
            "{k}: of {writes} stops, {} did not resume to the uninterrupted apply:\n{}",
            bad.len(),
            bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
        );

        let device = format!("{src}.resumed");
        std::fs::write(&device, resumed.expect("no stop resumed")).unwrap();
        let want = manifest(k);
        let got = walk(&device);
        assert!(
            got == want,
            "{k}: {} files after the resumed repair, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &in_place_image(k, &device),
            &Image::read(&recovered),
            log_end,
        );
        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_file(&device).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

/// An observer of an in-place apply that keeps every progress report and
/// asks for a stop once `stop` says so, given how many times it has been
/// asked and how many device writes have landed.
#[cfg(feature = "plan-file")]
struct Watching<F: FnMut(usize, usize) -> bool> {
    reports: Vec<fs_ntfs::repair_journal::ApplyProgress>,
    asked: usize,
    log: IoLog,
    stop: F,
}

#[cfg(feature = "plan-file")]
impl<F: FnMut(usize, usize) -> bool> fs_ntfs::repair_journal::ApplyObserver for Watching<F> {
    fn progress(&mut self, at: fs_ntfs::repair_journal::ApplyProgress) {
        self.reports.push(at);
    }

    fn cancel_requested(&mut self) -> bool {
        let landed = self
            .log
            .borrow()
            .iter()
            .filter(|(journal, e)| !journal && *e == IoEvent::Write)
            .count();
        self.asked += 1;
        (self.stop)(self.asked - 1, landed)
    }
}

/// Whether `reports` start at `from` units and bytes done, rise without
/// falling to `to`, and name `total` throughout; the first report that
/// does not is named.
#[cfg(feature = "plan-file")]
fn rising(
    reports: &[fs_ntfs::repair_journal::ApplyProgress],
    from: (u32, u64),
    to: (u32, u64),
    total: (u32, u64),
) -> Result<(), String> {
    let first = reports.first().ok_or("no progress was reported")?;
    if (first.units, first.bytes) != from {
        return Err(format!("the first report is {first:?}, not {from:?} done"));
    }
    for pair in reports.windows(2) {
        if pair[1].units <= pair[0].units || pair[1].bytes < pair[0].bytes {
            return Err(format!("{:?} follows {:?}", pair[1], pair[0]));
        }
    }
    if let Some(r) = reports
        .iter()
        .find(|r| (r.units_total, r.bytes_total) != total)
    {
        return Err(format!("{r:?} names another total than {total:?}"));
    }
    let last = reports.last().unwrap();
    if (last.units, last.bytes) != to {
        return Err(format!("the last report is {last:?}, not {to:?} done"));
    }
    Ok(())
}

#[cfg(feature = "plan-file")]
#[test]
fn an_in_place_repair_cancelled_anywhere_stops_after_a_whole_unit_and_resumes() {
    // A cancellation is honoured only at a unit boundary (#956): asked at
    // each boundary, or requested while any device write of a unit is
    // landing, the apply stops with every unit it began written, read
    // back and marked applied, and none open. Resumed, it must reach the
    // bytes of an uninterrupted apply, and the files and metadata Windows'
    // own restart reached. Progress, in the apply and in the resume, must
    // rise without falling and end at the plan's units and bytes.
    use fs_ntfs::repair_journal::{
        apply_in_place, apply_in_place_observed, read_journal, resume_in_place_observed,
        InPlaceError, JournalBinding, JournalRecord,
    };
    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let units = loaded.units.len();
        let bytes_of = |n: usize| -> u64 {
            loaded.units[..n]
                .iter()
                .flat_map(|u| &u.writes)
                .map(|w| w.after.len() as u64)
                .sum()
        };
        let total = (units as u32, bytes_of(units));
        let binding = JournalBinding {
            plan_sha256,
            source_size: loaded.source_size,
            volume_serial: loaded.volume_serial,
        };

        let (mut device, mut journal, log) = recording_pair(&pristine, None);
        let mut watch = Watching {
            reports: Vec::new(),
            asked: 0,
            log,
            stop: |_, _| false,
        };
        apply_in_place_observed(&loaded, plan_sha256, &mut device, &mut journal, &mut watch)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        rising(&watch.reports, (0, 0), total, total)
            .unwrap_or_else(|e| panic!("{k}: uninterrupted apply: {e}"));
        assert_eq!(watch.reports.len(), units + 1, "{k}: one report per unit");
        let whole = device.bytes;

        // Unit u's device writes are the ones numbered after every earlier
        // unit's: a request after write n is honoured after its unit.
        let mut ends = Vec::new();
        let mut landed = 0;
        for unit in &loaded.units {
            landed += unit.writes.len();
            ends.push(landed);
        }
        let mut stops: Vec<(String, usize)> = (0..units)
            .map(|b| (format!("at boundary {b}"), b))
            .collect();
        for n in 1..=landed {
            let after = ends.iter().position(|&end| end >= n).unwrap() + 1;
            stops.push((format!("after device write {n}"), after));
        }

        let mut bad = Vec::new();
        let mut resumed = None;
        for (i, (when, want)) in stops.iter().enumerate() {
            let boundary = i < units;
            let (mut device, mut journal, log) = recording_pair(&pristine, None);
            let mut watch = Watching {
                reports: Vec::new(),
                asked: 0,
                log,
                stop: |asked: usize, landed: usize| {
                    if boundary {
                        asked >= *want
                    } else {
                        landed >= i + 1 - units
                    }
                },
            };
            match apply_in_place_observed(
                &loaded,
                plan_sha256,
                &mut device,
                &mut journal,
                &mut watch,
            ) {
                Err(InPlaceError::Cancelled { applied }) if applied as usize == *want => {}
                other => {
                    bad.push(format!(
                        "{when}: the apply returned {other:?}, not cancelled after {want} units"
                    ));
                    continue;
                }
            }
            let done = (*want as u32, bytes_of(*want));
            if let Err(e) = rising(&watch.reports, (0, 0), done, total) {
                bad.push(format!("{when}: the cancelled apply's progress: {e}"));
                continue;
            }
            let read = match read_journal(&journal.bytes, &binding) {
                Ok(read) => read,
                Err(e) => {
                    bad.push(format!("{when}: the journal reads back as {e}"));
                    continue;
                }
            };
            let intents: Vec<u32> = read
                .records
                .iter()
                .filter_map(|r| match r {
                    JournalRecord::Intent { unit, .. } => Some(*unit),
                    _ => None,
                })
                .collect();
            let applied: Vec<u32> = read
                .records
                .iter()
                .filter_map(|r| match r {
                    JournalRecord::Applied { unit } => Some(*unit),
                    _ => None,
                })
                .collect();
            let first: Vec<u32> = (0..*want as u32).collect();
            if intents != first || applied != first || read.torn.is_some() {
                bad.push(format!(
                    "{when}: the journal holds intents {intents:?}, applied {applied:?} and \
                     torn {:?}",
                    read.torn
                ));
                continue;
            }
            let mut again = Watching {
                reports: Vec::new(),
                asked: 0,
                log: watch.log.clone(),
                stop: |_, _| false,
            };
            if let Err(e) = resume_in_place_observed(
                &loaded,
                plan_sha256,
                &mut device,
                &mut journal,
                &mut again,
            ) {
                bad.push(format!("{when}: the resume refused: {e}"));
                continue;
            }
            if let Err(e) = rising(&again.reports, done, total, total) {
                bad.push(format!("{when}: the resume's progress: {e}"));
                continue;
            }
            if device.bytes != whole {
                bad.push(format!(
                    "{when}: the resumed device holds other bytes than an uninterrupted apply"
                ));
                continue;
            }
            resumed = Some(device.bytes);
        }
        assert!(
            bad.is_empty(),
            "{k}: of {} cancellations, {} did not stop after a whole unit and resume:\n{}",
            stops.len(),
            bad.len(),
            bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
        );

        // The plain apply is the observed one with nobody watching.
        let (mut device, mut journal, _) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: unobserved in-place repair: {e}"));
        assert!(device.bytes == whole, "{k}: the unobserved apply differs");

        let device = format!("{src}.cancelled");
        std::fs::write(&device, resumed.expect("no cancellation resumed")).unwrap();
        let want = manifest(k);
        let got = walk(&device);
        assert!(
            got == want,
            "{k}: {} files after the cancelled repair resumed, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &in_place_image(k, &device),
            &Image::read(&recovered),
            log_end,
        );
        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_file(&device).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

/// What the C ABI's repair callbacks saw (#957): every progress report,
/// how many times a stop was asked about, and the ask that answers yes.
#[cfg(all(unix, feature = "plan-file"))]
struct CWatch {
    reports: Vec<fs_ntfs::repair_journal::ApplyProgress>,
    asked: usize,
    stop_at: Option<usize>,
}

#[cfg(all(unix, feature = "plan-file"))]
extern "C" fn c_repair_progress(
    ctx: *mut c_void,
    units: u32,
    units_total: u32,
    bytes: u64,
    bytes_total: u64,
) {
    let watch = unsafe { &mut *(ctx as *mut CWatch) };
    watch.reports.push(fs_ntfs::repair_journal::ApplyProgress {
        units,
        bytes,
        units_total,
        bytes_total,
    });
}

#[cfg(all(unix, feature = "plan-file"))]
extern "C" fn c_repair_cancel(ctx: *mut c_void) -> c_int {
    let watch = unsafe { &mut *(ctx as *mut CWatch) };
    let stop = watch.stop_at == Some(watch.asked);
    watch.asked += 1;
    c_int::from(stop)
}

#[cfg(all(unix, feature = "plan-file"))]
#[test]
fn a_c_abi_in_place_repair_cancelled_by_its_callback_resumes_through_the_c_abi() {
    // The C ABI's in-place apply and resume take a progress callback and
    // a cancellation callback over the library observer (#957). Cancelled
    // from the callback halfway through a Windows interrupted capture's
    // plan, the apply must return the cancelled status naming the units
    // applied; resumed through the C ABI it must reach the bytes of an
    // uninterrupted apply, and the files and metadata Windows' own restart
    // reached. Progress must rise to the stop, then from there to the end.
    use fs_ntfs::repair_journal::apply_in_place;
    use fs_ntfs::repair_plan::{load_plan, plan_in_place_io, save_plan};
    use fs_ntfs::{
        fs_ntfs_repair_apply_in_place, fs_ntfs_repair_resume_in_place, FS_NTFS_REPAIR_CANCELLED,
    };
    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_in_place_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        let plan_sha256: [u8; 32] = Sha256::digest(&saved).into();
        let units = loaded.units.len();
        assert!(units > 0, "{k}: a log holding work planned nothing");
        let bytes_of = |n: usize| -> u64 {
            loaded.units[..n]
                .iter()
                .flat_map(|u| &u.writes)
                .map(|w| w.after.len() as u64)
                .sum()
        };
        let total = (units as u32, bytes_of(units));

        let (mut device, mut journal, _) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        let whole = device.bytes;

        let device = format!("{src}.capi");
        let plan_file = format!("{src}.capi.plan");
        let journal_dir =
            std::env::temp_dir().join(format!("ntfs-capi-journal-{k}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&journal_dir);
        std::fs::create_dir_all(&journal_dir).unwrap();
        let journal_file = journal_dir.join("repair.journal");
        std::fs::write(&device, &pristine).unwrap();
        std::fs::write(&plan_file, &saved).unwrap();
        let c_device = CString::new(device.as_str()).unwrap();
        let c_plan = CString::new(plan_file.as_str()).unwrap();
        let c_journal = CString::new(journal_file.to_str().unwrap()).unwrap();

        let stop = units / 2;
        let mut watch = CWatch {
            reports: Vec::new(),
            asked: 0,
            stop_at: Some(stop),
        };
        let mut applied = u32::MAX;
        let rc = fs_ntfs_repair_apply_in_place(
            c_device.as_ptr(),
            c_plan.as_ptr(),
            c_journal.as_ptr(),
            Some(c_repair_progress),
            Some(c_repair_cancel),
            &mut watch as *mut CWatch as *mut c_void,
            &mut applied,
        );
        assert_eq!(
            rc, FS_NTFS_REPAIR_CANCELLED,
            "{k}: the apply cancelled at boundary {stop} of {units} returned {rc}"
        );
        assert_eq!(applied as usize, stop, "{k}: units applied before the stop");
        let done = (stop as u32, bytes_of(stop));
        rising(&watch.reports, (0, 0), done, total)
            .unwrap_or_else(|e| panic!("{k}: the cancelled apply's progress: {e}"));

        let mut again = CWatch {
            reports: Vec::new(),
            asked: 0,
            stop_at: None,
        };
        let mut finished = u32::MAX;
        let rc = fs_ntfs_repair_resume_in_place(
            c_device.as_ptr(),
            c_plan.as_ptr(),
            c_journal.as_ptr(),
            Some(c_repair_progress),
            Some(c_repair_cancel),
            &mut again as *mut CWatch as *mut c_void,
            &mut finished,
        );
        assert_eq!(rc, 0, "{k}: the resume through the C ABI returned {rc}");
        assert_eq!(finished as usize, units, "{k}: units applied by the resume");
        rising(&again.reports, done, total, total)
            .unwrap_or_else(|e| panic!("{k}: the resume's progress: {e}"));
        assert!(
            std::fs::read(&device).unwrap() == whole,
            "{k}: the device resumed through the C ABI holds other bytes than an \
             uninterrupted apply"
        );

        let want = manifest(k);
        let got = walk(&device);
        assert!(
            got == want,
            "{k}: {} files after the C ABI repair resumed, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &in_place_image(k, &device),
            &Image::read(&recovered),
            log_end,
        );
        std::fs::remove_dir_all(&journal_dir).unwrap();
        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_file(&plan_file).unwrap();
        std::fs::remove_file(&device).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_device_write_torn_by_a_cut_resumes_to_the_uninterrupted_volume() {
    // A power cut can stop a device write half done (#928): the device
    // holds part of the write's new bytes and its old bytes in the rest.
    // For every device write of each capture's apply, the cut lands the
    // first half of that write's new bytes and leaves the rest unchanged,
    // and then, as a device writing its sectors out of order can, only
    // the second half. Resumed on the same device and journal, the device
    // must hold exactly what an uninterrupted apply leaves.
    use fs_ntfs::repair_journal::{apply_in_place, resume_in_place};
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let (mut device, mut journal, log) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        let whole = device.bytes;
        // The order, among the writes reaching either side, of each write
        // that reached the device.
        let device_writes: Vec<usize> = log
            .borrow()
            .iter()
            .filter(|(_, e)| *e == IoEvent::Write)
            .enumerate()
            .filter(|(_, (journal, _))| !journal)
            .map(|(n, _)| n)
            .collect();
        assert!(!device_writes.is_empty(), "{k}: the apply wrote nothing");

        let mut bad = Vec::new();
        for tear in [Tear::FirstHalf, Tear::SecondHalf] {
            for &stop in &device_writes {
                let (mut device, mut journal, _) = recording_pair(&pristine, None);
                cut_pair(&mut device, &mut journal, Some(stop));
                device.tear = Some(tear);
                if apply_in_place(&loaded, plan_sha256, &mut device, &mut journal).is_ok() {
                    bad.push(format!(
                        "{tear:?} of write {stop}: the cut apply reported success"
                    ));
                    continue;
                }
                if device.tear.is_some() {
                    bad.push(format!("{tear:?} of write {stop}: the cut tore no write"));
                    continue;
                }
                cut_pair(&mut device, &mut journal, None);
                if let Err(e) = resume_in_place(&loaded, plan_sha256, &mut device, &mut journal) {
                    bad.push(format!("{tear:?} of write {stop}: the resume refused: {e}"));
                    continue;
                }
                if device.bytes != whole {
                    bad.push(format!(
                        "{tear:?} of write {stop}: the device holds other bytes than an \
                         uninterrupted apply"
                    ));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "{k}: of {} torn device writes, {} did not resume to the uninterrupted apply:\n{}",
            2 * device_writes.len(),
            bad.len(),
            bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
        );
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_resumed_in_place_repair_writes_nothing_to_the_device_for_a_unit_marked_applied() {
    // A resume (#927) reads back each unit its journal marks applied and
    // writes none of them again. Cut right after each unit's applied
    // record, the resume writes exactly the later units' writes to the
    // device; resuming a finished apply writes nothing to the device or
    // the journal.
    use fs_ntfs::repair_journal::{apply_in_place, resume_in_place};
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let units = loaded.units.len();
        // The header, then each unit's intent, writes and applied record.
        let mut cut = 1;
        for done in 0..=units {
            let (mut device, mut journal, log) = recording_pair(&pristine, None);
            cut_pair(&mut device, &mut journal, Some(cut));
            let stopped = apply_in_place(&loaded, plan_sha256, &mut device, &mut journal);
            assert_eq!(
                stopped.is_ok(),
                done == units,
                "{k}: cut after {cut} writes, {done} of {units} units applied"
            );
            cut_pair(&mut device, &mut journal, None);
            let from = log.borrow().len();
            resume_in_place(&loaded, plan_sha256, &mut device, &mut journal)
                .unwrap_or_else(|e| panic!("{k}: {done} units applied: the resume refused: {e}"));
            let count = |side: bool| {
                log.borrow()[from..]
                    .iter()
                    .filter(|&&e| e == (side, IoEvent::Write))
                    .count()
            };
            let later: usize = loaded.units[done..].iter().map(|u| u.writes.len()).sum();
            assert_eq!(
                count(false),
                later,
                "{k}: {done} of {units} units applied: the resume's device writes"
            );
            if done == units {
                assert_eq!(
                    count(true),
                    0,
                    "{k}: a finished apply resumed wrote the journal"
                );
            } else {
                cut += loaded.units[done].writes.len() + 2;
            }
        }
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_resume_refuses_a_unit_whose_range_changed_while_stopped_and_names_the_byte() {
    // While an in-place apply is stopped, something else can write the
    // device (#929). For every device write of each capture's apply, the
    // cut stops that write; then one byte is changed to a value that is
    // neither what the journal says belongs there nor, for the open unit,
    // its preimage: first in the open unit's range, then in a unit the
    // journal marks applied. The resume must refuse, naming that unit,
    // write and byte, and leave the device and the journal exactly as the
    // cut and the change left them.
    use fs_ntfs::repair_journal::{
        apply_in_place, progress, read_journal, resume_in_place, InPlaceError, JournalBinding,
    };
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let (mut device, mut journal, log) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        let device_writes: Vec<usize> = log
            .borrow()
            .iter()
            .filter(|(_, e)| *e == IoEvent::Write)
            .enumerate()
            .filter(|(_, (journal, _))| !journal)
            .map(|(n, _)| n)
            .collect();
        let binding = JournalBinding {
            plan_sha256,
            source_size: loaded.source_size,
            volume_serial: loaded.volume_serial,
        };

        let mut bad = Vec::new();
        let mut applied_cases = 0;
        for &stop in &device_writes {
            for in_applied in [false, true] {
                let (mut device, mut journal, _) = recording_pair(&pristine, None);
                cut_pair(&mut device, &mut journal, Some(stop));
                if apply_in_place(&loaded, plan_sha256, &mut device, &mut journal).is_ok() {
                    bad.push(format!("write {stop}: the cut apply reported success"));
                    continue;
                }
                cut_pair(&mut device, &mut journal, None);
                let read = read_journal(&journal.bytes, &binding)
                    .unwrap_or_else(|e| panic!("{k}: write {stop}: the journal: {e}"));
                let stopped = progress(&read, &loaded)
                    .unwrap_or_else(|e| panic!("{k}: write {stop}: the journal: {e}"));
                let Some(open) = stopped.open else {
                    bad.push(format!("write {stop}: the cut left no unit open"));
                    continue;
                };
                let open_writes = &loaded.units[open as usize].writes;
                let in_open = |byte: u64| {
                    open_writes
                        .iter()
                        .any(|o| (o.offset..o.offset + o.after.len() as u64).contains(&byte))
                };
                // The unit, write and byte to change, and the values the
                // change must avoid.
                let target = if in_applied {
                    (0..stopped.applied as usize).find_map(|u| {
                        loaded.units[u]
                            .writes
                            .iter()
                            .enumerate()
                            .find_map(|(w, write)| {
                                (0..write.after.len() as u64)
                                    .map(|i| write.offset + i)
                                    .find(|&byte| !in_open(byte))
                                    .map(|byte| (u as u32, w, byte, Vec::new()))
                            })
                    })
                } else {
                    let write = &open_writes[0];
                    let later: Vec<u8> = open_writes[1..]
                        .iter()
                        .filter(|o| {
                            (o.offset..o.offset + o.after.len() as u64).contains(&write.offset)
                        })
                        .map(|o| o.after[(write.offset - o.offset) as usize])
                        .collect();
                    let mut avoid = vec![write.before[0], write.after[0]];
                    avoid.extend(later);
                    Some((open, 0, write.offset, avoid))
                };
                let Some((unit, w, byte, mut avoid)) = target else {
                    continue;
                };
                if in_applied {
                    applied_cases += 1;
                }
                avoid.push(device.bytes[byte as usize]);
                let value = (0..=255u8).find(|v| !avoid.contains(v)).unwrap();
                device.bytes[byte as usize] = value;
                let device_sha256 = hex_sha256(&device.bytes);
                let journal_len = journal.bytes.len();

                let what = if in_applied {
                    "an applied unit"
                } else {
                    "the open unit"
                };
                match resume_in_place(&loaded, plan_sha256, &mut device, &mut journal) {
                    Err(InPlaceError::OpenUnitRange {
                        unit: u,
                        write,
                        start,
                        end,
                    }) if !in_applied && (u, write, start, end) == (unit, w, byte, byte + 1) => {}
                    Err(InPlaceError::DeviceReadBack {
                        unit: u,
                        write,
                        start,
                        end,
                    }) if in_applied && (u, write, start, end) == (unit, w, byte, byte + 1) => {}
                    other => {
                        bad.push(format!(
                            "write {stop}: byte {byte} of unit {unit} write {w} ({what}) changed: \
                             the resume gave {other:?}"
                        ));
                        continue;
                    }
                }
                if hex_sha256(&device.bytes) != device_sha256 {
                    bad.push(format!(
                        "write {stop}: {what}: the refused resume wrote the device"
                    ));
                }
                if journal.bytes.len() != journal_len {
                    bad.push(format!(
                        "write {stop}: {what}: the refused resume appended to the journal"
                    ));
                }
            }
        }
        assert!(applied_cases > 0, "{k}: no cut left a unit applied");
        assert!(
            bad.is_empty(),
            "{k}: of {} changed bytes, {} were not refused as they must be:\n{}",
            device_writes.len() + applied_cases,
            bad.len(),
            bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
        );
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_torn_journal_tail_resumes_only_as_a_prefix_of_the_record_the_plan_expects_next() {
    // A cut append leaves the journal's last record short: a prefix of
    // the record being appended (#934). The plan fixes which record that
    // is, so a short tail holding anything else -- a whole record whose
    // length field was raised, a torn tail with a byte changed, or a tail
    // after the last unit's applied record -- is corruption. The resume
    // must refuse it, naming the record, with the device and the journal
    // as it found them; a true prefix must still resume to the
    // uninterrupted apply.
    use fs_ntfs::repair_journal::{
        applied_record, apply_in_place, header_record, intent_record, resume_in_place,
        InPlaceError, JournalBinding,
    };
    let raised = |record: &[u8]| {
        let mut r = record.to_vec();
        let length = u32::from_le_bytes(r[4..8].try_into().unwrap()) + 1;
        r[4..8].copy_from_slice(&length.to_le_bytes());
        r
    };
    let changed = |record: &[u8], keep: usize| {
        let mut r = record[..keep].to_vec();
        r[8] ^= 0x01;
        r
    };
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let (mut device, mut journal, _) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        let whole = device.bytes;
        let full = journal.bytes;
        let binding = JournalBinding {
            plan_sha256,
            source_size: loaded.source_size,
            volume_serial: loaded.volume_serial,
        };
        let header = header_record(&binding);
        let intent = intent_record(0, &loaded.units[0]);
        let applied = applied_record(0);
        let mut unit0 = pristine.clone();
        for w in &loaded.units[0].writes {
            let at = w.offset as usize;
            unit0[at..at + w.after.len()].copy_from_slice(&w.after);
        }
        let joined = |parts: &[&[u8]]| parts.concat();
        let after_all = 1 + 2 * loaded.units.len();

        let mut bad = Vec::new();
        let corrupt: Vec<(&str, &[u8], Vec<u8>, usize)> = vec![
            ("the header's length raised", &pristine, raised(&header), 0),
            (
                "unit 0's intent length raised",
                &pristine,
                joined(&[&header, &raised(&intent)]),
                1,
            ),
            (
                "unit 0's applied record length raised",
                &unit0,
                joined(&[&header, &intent, &raised(&applied)]),
                2,
            ),
            (
                "a byte changed in a torn intent",
                &pristine,
                joined(&[&header, &changed(&intent, intent.len() / 2)]),
                1,
            ),
            (
                "a byte changed in a torn applied record",
                &unit0,
                joined(&[&header, &intent, &changed(&applied, applied.len() - 1)]),
                2,
            ),
            (
                "a tail after the last unit's applied record",
                &whole,
                joined(&[&full, &intent[..intent.len() / 2]]),
                after_all,
            ),
        ];
        for (what, held, bytes, record) in corrupt {
            let (mut device, mut journal, _) = recording_pair(held, None);
            journal.bytes = bytes.clone();
            match resume_in_place(&loaded, plan_sha256, &mut device, &mut journal) {
                Ok(_) => bad.push(format!("{what}: the resume reported success")),
                Err(InPlaceError::Journal(e)) if e.record == record => {
                    if device.bytes != held {
                        bad.push(format!("{what}: refused as {e}, but the device changed"));
                    }
                    if journal.bytes != bytes {
                        bad.push(format!("{what}: refused as {e}, but the journal changed"));
                    }
                }
                Err(e) => bad.push(format!(
                    "{what}: refused as {e}, not as journal record {record}"
                )),
            }
        }

        let prefixes: Vec<(&str, &[u8], Vec<u8>)> = vec![
            (
                "half the header",
                &pristine,
                header[..header.len() / 2].to_vec(),
            ),
            (
                "half unit 0's intent",
                &pristine,
                joined(&[&header, &intent[..intent.len() / 2]]),
            ),
            (
                "all but the last byte of unit 0's applied record",
                &unit0,
                joined(&[&header, &intent, &applied[..applied.len() - 1]]),
            ),
        ];
        for (what, held, bytes) in prefixes {
            let (mut device, mut journal, _) = recording_pair(held, None);
            journal.bytes = bytes;
            match resume_in_place(&loaded, plan_sha256, &mut device, &mut journal) {
                Ok(_) if device.bytes == whole => {}
                Ok(_) => bad.push(format!(
                    "{what}: the device holds other bytes than an uninterrupted apply"
                )),
                Err(e) => bad.push(format!("{what}: the resume refused: {e}")),
            }
        }
        assert!(bad.is_empty(), "{k}:\n{}", bad.join("\n"));
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "plan-file")]
#[test]
fn a_resume_refuses_a_corrupt_or_foreign_journal_with_nothing_written() {
    // While an in-place apply is stopped, its journal can be damaged or
    // swapped for the journal of another repair (#935). At every stop of
    // each capture's cut apply, a byte flipped in the header, in an intent
    // and in an applied record, and the journal another capture's apply
    // left, must each refuse the resume, naming the record and the reason,
    // before anything is written: no write reaches the device or the
    // journal, and the journal holds the bytes it was given. Resumed
    // afterwards from an intact copy of the journal, the device reaches
    // the uninterrupted apply, and so the files and metadata Windows' own
    // restart reached.
    use fs_ntfs::repair_journal::{
        apply_in_place, read_journal, resume_in_place, InPlaceError, JournalBinding, JournalError,
        JournalFault, JournalRecord,
    };
    let captures = captures_holding_work();
    // The journal each capture's whole apply leaves, which no other
    // capture's plan may resume from.
    let finished: Vec<Vec<u8>> = captures
        .iter()
        .map(|&(k, _)| {
            let src = unpack(&format!("windows-interrupted-{k}"));
            let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
            let (mut device, mut journal, _) = recording_pair(&pristine, None);
            apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
                .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
            std::fs::remove_file(&src).unwrap();
            journal.bytes
        })
        .collect();
    for (i, &(k, log_end)) in captures.iter().enumerate() {
        let foreign = &finished[(i + 1) % finished.len()];
        let src = unpack(&format!("windows-interrupted-{k}"));
        let (loaded, plan_sha256, pristine) = saved_plan(k, &src);
        let (mut device, mut journal, log) = recording_pair(&pristine, None);
        apply_in_place(&loaded, plan_sha256, &mut device, &mut journal)
            .unwrap_or_else(|e| panic!("{k}: in-place repair: {e}"));
        let whole = device.bytes;
        let writes = log
            .borrow()
            .iter()
            .filter(|(_, e)| *e == IoEvent::Write)
            .count();
        let binding = JournalBinding {
            plan_sha256,
            source_size: loaded.source_size,
            volume_serial: loaded.volume_serial,
        };
        let kinds = ["header", "intent", "applied record"];

        let mut bad = Vec::new();
        let mut flipped = [0usize; 3];
        let mut resumed = None;
        for stop in 0..writes {
            let (mut device, mut journal, log) = recording_pair(&pristine, None);
            cut_pair(&mut device, &mut journal, Some(stop));
            if apply_in_place(&loaded, plan_sha256, &mut device, &mut journal).is_ok() {
                bad.push(format!("stop after {stop}: the cut apply reported success"));
                continue;
            }
            cut_pair(&mut device, &mut journal, None);
            let intact = journal.bytes.clone();
            let read = match read_journal(&intact, &binding) {
                Ok(read) => read,
                Err(e) => {
                    bad.push(format!(
                        "stop after {stop}: the cut journal reads back as {e}"
                    ));
                    continue;
                }
            };
            // The first whole record of each kind: its index, where it
            // starts and its length (type, payload length, payload, sum).
            let mut first = [None; 3];
            let mut at = 0usize;
            for (n, record) in read.records.iter().enumerate() {
                let kind = match record {
                    JournalRecord::Header(_) => 0,
                    JournalRecord::Intent { .. } => 1,
                    JournalRecord::Applied { .. } => 2,
                };
                let payload = u32::from_le_bytes(intact[at + 4..at + 8].try_into().unwrap());
                let length = 4 + 4 + payload as usize + 32;
                first[kind].get_or_insert((n, at, length));
                at += length;
            }

            let mut cases = Vec::new();
            for (kind, what) in kinds.iter().enumerate() {
                let Some((record, offset, length)) = first[kind] else {
                    continue;
                };
                let mut bytes = intact.clone();
                bytes[offset + length / 2] ^= 0xff;
                flipped[kind] += 1;
                let want = JournalError {
                    record,
                    offset: offset as u64,
                    fault: JournalFault::ChecksumMismatch,
                };
                cases.push((format!("a byte flipped in the first {what}"), bytes, want));
            }
            let want = JournalError {
                record: 0,
                offset: 0,
                fault: JournalFault::OtherPlan,
            };
            cases.push((
                "another capture's journal".to_string(),
                foreign.clone(),
                want,
            ));

            for (what, bytes, want) in cases {
                journal.bytes = bytes.clone();
                let from = log.borrow().len();
                match resume_in_place(&loaded, plan_sha256, &mut device, &mut journal) {
                    Err(InPlaceError::Journal(e)) if e == want => {}
                    other => bad.push(format!(
                        "stop after {stop}: {what}: the resume gave {other:?}, not {want}"
                    )),
                }
                let wrote = log.borrow()[from..]
                    .iter()
                    .filter(|(_, e)| *e == IoEvent::Write)
                    .count();
                if wrote > 0 || journal.bytes != bytes {
                    bad.push(format!(
                        "stop after {stop}: {what}: the refused resume made {wrote} writes"
                    ));
                }
            }

            journal.bytes = intact;
            if let Err(e) = resume_in_place(&loaded, plan_sha256, &mut device, &mut journal) {
                bad.push(format!(
                    "stop after {stop}: from an intact copy, the resume refused: {e}"
                ));
                continue;
            }
            if device.bytes != whole {
                bad.push(format!(
                    "stop after {stop}: from an intact copy, the device holds other bytes than \
                     an uninterrupted apply"
                ));
                continue;
            }
            resumed = Some(device.bytes);
        }
        for (kind, what) in kinds.iter().enumerate() {
            assert!(flipped[kind] > 0, "{k}: no stop left a whole {what}");
        }
        assert!(
            bad.is_empty(),
            "{k}: of {writes} stops, {} did not refuse or resume as they must:\n{}",
            bad.len(),
            bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
        );

        let device = format!("{src}.resumed");
        std::fs::write(&device, resumed.expect("no stop resumed")).unwrap();
        let want = manifest(k);
        let got = walk(&device);
        assert!(
            got == want,
            "{k}: {} files after the resumed repair, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &in_place_image(k, &device),
            &Image::read(&recovered),
            log_end,
        );
        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_file(&device).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

#[test]
fn a_plan_for_an_in_place_apply_opens_by_setting_the_dirty_flag() {
    // An in-place repair keeps the volume dirty until it has been audited
    // (#716), so the flag is set before the first repair unit, and set by
    // a unit of the plan itself (#939): the in-place apply journals,
    // checks and resumes it like any other, and nothing reaches the device
    // outside the plan's ranges. Unit 0 must be exactly what `set_dirty_io`
    // writes on a copy of the source, and every later unit what
    // `plan_fsck_io` plans over that dirtied copy. On a source already
    // dirty, nothing is added.
    use fs_ntfs::repair_plan::{plan_fsck_io, plan_in_place_io};
    let recorded = |units: &[fs_ntfs::repair_plan::PlanUnit]| -> Vec<Vec<PlannedWrite>> {
        units
            .iter()
            .map(|unit| {
                unit.writes
                    .iter()
                    .map(|w| {
                        (
                            w.offset,
                            w.after.len() as u64,
                            hex_sha256(&w.before),
                            hex_sha256(&w.after),
                        )
                    })
                    .collect()
            })
            .collect()
    };
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        std::fs::remove_file(&src).unwrap();
        let mut dirtied = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let was_dirty = fsck::is_dirty_io(&mut dirtied).unwrap();
        fsck::set_dirty_io(&mut dirtied).unwrap_or_else(|e| panic!("{k}: set dirty: {e}"));
        fs_ntfs::block_io::BlockIo::sync(&mut dirtied).unwrap();
        let set_dirty = std::mem::take(&mut dirtied.units);
        let repair = plan_fsck_io(&mut dirtied, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan over the dirtied copy: {e}"));

        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_in_place_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: in-place plan: {e}"));
        assert_eq!(v.bytes, pristine, "{k}: planning wrote to the source");
        assert!(
            v.units.is_empty() && v.open.is_empty(),
            "{k}: planning wrote"
        );
        let got = recorded(&plan.units);
        if was_dirty {
            assert_eq!(
                got,
                recorded(&repair.units),
                "{k}: a source already dirty gained units"
            );
        } else {
            // The flag, then its `$MFTMirr` copy, each flushed.
            let n = set_dirty.len();
            assert!(n > 0, "{k}: set_dirty_io wrote nothing to a clean volume");
            assert_eq!(
                got[..n.min(got.len())],
                set_dirty[..],
                "{k}: the plan does not open with set_dirty_io's writes"
            );
            assert_eq!(
                got[n..],
                recorded(&repair.units)[..],
                "{k}: the units after the dirty flag are not the repair's"
            );
            assert_eq!(plan.report, repair.report, "{k}: report");
        }

        // A source whose flag is already set gains nothing.
        let mut already = RecordedRepair {
            bytes: dirtied.bytes.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_in_place_io(&mut already, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: in-place plan of a dirty source: {e}"));
        assert_eq!(
            recorded(&plan.units),
            recorded(&repair.units),
            "{k}: a source already dirty gained units"
        );
    }
}

/// Run the binary's `fsck` verb; the exit status, and stdout with stderr.
#[cfg(feature = "cli")]
fn fsck_cli(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
        .arg("fsck")
        .args(args)
        .output()
        .expect("spawn fsck.ntfs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().expect("exit status"), said)
}

#[cfg(feature = "cli")]
#[test]
fn fsck_apply_copy_repairs_a_read_only_capture_into_a_new_target() {
    // `fsck.ntfs --apply PLAN --to TARGET SOURCE` (#910): the plan file is
    // loaded and SOURCE copy-repaired into TARGET through the library. The
    // capture is made read-only, so a write to it would fail, and its
    // SHA-256 is checked after every run. TARGET must reach the files
    // Windows' own restart left. Each refusal must name its reason, exit
    // non-zero, and create nothing. The plan is saved through the library
    // (#905) until `--dry-run --plan FILE` (#906) writes it from the CLI.
    use fs_ntfs::repair_plan::{plan_fsck_io, save_plan};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let pristine_sha = hex_sha256(&pristine);
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_fsck_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        drop(pristine);
        let plan_file = format!("{src}.plan");
        std::fs::write(&plan_file, &saved).unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
        let unchanged = |what: &str| {
            assert_eq!(
                hex_sha256(&std::fs::read(&src).unwrap()),
                pristine_sha,
                "{k}: {what} changed the capture"
            );
        };

        let target = format!("{src}.applied");
        let _ = std::fs::remove_file(&target);
        let (code, said) = fsck_cli(&["--apply", &plan_file, "--to", &target, &src]);
        assert_eq!(code, 0, "{k}: --apply --to exited {code}: {said}");
        unchanged("--apply --to");
        let want = manifest(k);
        let got = walk(&target);
        assert!(
            got == want,
            "{k}: {} files after --apply --to, {} in Windows' manifest",
            got.len(),
            want.len()
        );

        // Each refusal names its reason, exits non-zero and creates nothing.
        let refused = format!("{src}.refused");
        let link = format!("{src}.link");
        let _ = std::fs::remove_file(&refused);
        let _ = std::fs::remove_file(&link);
        std::fs::hard_link(&src, &link).unwrap();
        let cases: [(&str, Vec<&str>, &str, &str); 6] = [
            (
                "--apply without --to",
                vec!["--apply", &plan_file, &src],
                "--to",
                &refused,
            ),
            (
                "--to naming the source's path",
                vec!["--apply", &plan_file, "--to", &src, &src],
                "is the source",
                &refused,
            ),
            (
                "--to naming the source through a hard link",
                vec!["--apply", &plan_file, "--to", &link, &src],
                "is the source",
                &refused,
            ),
            (
                "--to naming a file that exists",
                vec!["--apply", &plan_file, "--to", &target, &src],
                "already exists",
                &refused,
            ),
            (
                "--apply with --dry-run",
                vec!["--apply", &plan_file, "--to", &refused, "--dry-run", &src],
                "--dry-run",
                &refused,
            ),
            (
                "--apply with -y",
                vec!["--apply", &plan_file, "--to", &refused, "-y", &src],
                "--yes",
                &refused,
            ),
        ];
        let kept = hex_sha256(&std::fs::read(&target).unwrap());
        for (what, args, reason, not_created) in cases {
            let (code, said) = fsck_cli(&args);
            assert_ne!(code, 0, "{k}: {what} went ahead: {said}");
            assert!(
                said.contains(reason),
                "{k}: {what} refused without naming '{reason}': {said}"
            );
            unchanged(what);
            assert!(
                !Path::new(not_created).exists(),
                "{k}: {what} created {not_created}"
            );
        }
        assert_eq!(
            hex_sha256(&std::fs::read(&target).unwrap()),
            kept,
            "{k}: a refused --apply changed the existing target"
        );

        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_file(&plan_file).unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "cli")]
#[test]
fn fsck_dry_run_plan_writes_the_plan_to_an_external_file_never_its_target() {
    // `fsck.ntfs --dry-run --plan FILE SOURCE` (#906): the dry run's plan is
    // saved to FILE, never onto SOURCE. The capture is made read-only, so a
    // write to it would fail, and its SHA-256 is checked after every run.
    // The run exits as `-n` does, FILE loads back through the library, and
    // its units are the report's plan. `--plan` naming the capture, by path
    // or hard link, naming a file that exists, or without `--dry-run`, is
    // refused naming its reason, and nothing is written.
    use fs_ntfs::repair_plan::load_plan;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    type Writes = Vec<Vec<(u64, u64, String, String)>>;
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine_sha = hex_sha256(&std::fs::read(&src).unwrap());
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
        let unchanged = |what: &str| {
            assert_eq!(
                hex_sha256(&std::fs::read(&src).unwrap()),
                pristine_sha,
                "{k}: {what} changed the capture"
            );
        };

        let (check_code, said) = fsck_cli(&["-n", &src]);
        unchanged("-n");
        let plan_file = format!("{src}.plan");
        let _ = std::fs::remove_file(&plan_file);
        let out = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
            .args(["fsck", "--dry-run", "--plan", &plan_file, &src])
            .output()
            .expect("spawn fsck.ntfs");
        let code = out.status.code().expect("exit status");
        assert_eq!(
            code,
            check_code,
            "{k}: --dry-run --plan exited {code}, -n {check_code} ({said}): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        unchanged("--dry-run --plan");
        let report: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{k}: the report is not JSON: {e}"));
        let reported: Writes = report["plan"]["units"]
            .as_array()
            .unwrap_or_else(|| panic!("{k}: no plan in the report: {report}"))
            .iter()
            .map(|unit| {
                unit["writes"]
                    .as_array()
                    .expect("a unit's writes")
                    .iter()
                    .map(|w| {
                        (
                            w["offset"].as_u64().expect("offset"),
                            w["length"].as_u64().expect("length"),
                            w["before_sha256"].as_str().expect("before").to_string(),
                            w["after_sha256"].as_str().expect("after").to_string(),
                        )
                    })
                    .collect()
            })
            .collect();
        let saved = load_plan(&std::fs::read(&plan_file).unwrap())
            .unwrap_or_else(|e| panic!("{k}: the plan file does not load: {e}"));
        let loaded: Writes = saved
            .units
            .iter()
            .map(|unit| {
                unit.writes
                    .iter()
                    .map(|w| {
                        (
                            w.offset,
                            w.after.len() as u64,
                            hex_sha256(&w.before),
                            hex_sha256(&w.after),
                        )
                    })
                    .collect()
            })
            .collect();
        assert!(
            !loaded.is_empty(),
            "{k}: a capture holding work saved no units"
        );
        assert_eq!(
            loaded, reported,
            "{k}: the plan file is not the report's plan"
        );
        assert_eq!(
            saved.source_size,
            std::fs::metadata(&src).unwrap().len(),
            "{k}: the plan file is bound to another size"
        );

        // Each refusal names its reason, exits non-zero and writes nothing.
        let refused = format!("{src}.refused");
        let link = format!("{src}.link");
        let _ = std::fs::remove_file(&refused);
        let _ = std::fs::remove_file(&link);
        std::fs::hard_link(&src, &link).unwrap();
        let cases: [(&str, Vec<&str>, &str); 4] = [
            (
                "--plan naming the capture's path",
                vec!["--dry-run", "--plan", &src, &src],
                "is the volume",
            ),
            (
                "--plan naming the capture through a hard link",
                vec!["--dry-run", "--plan", &link, &src],
                "is the volume",
            ),
            (
                "--plan naming a file that exists",
                vec!["--dry-run", "--plan", &plan_file, &src],
                "already exists",
            ),
            (
                "--plan without --dry-run",
                vec!["--plan", &refused, &src],
                "--dry-run",
            ),
        ];
        let kept = hex_sha256(&std::fs::read(&plan_file).unwrap());
        for (what, args, reason) in cases {
            let (code, said) = fsck_cli(&args);
            assert_ne!(code, 0, "{k}: {what} went ahead: {said}");
            assert!(
                said.contains(reason),
                "{k}: {what} refused without naming '{reason}': {said}"
            );
            unchanged(what);
            assert!(
                !Path::new(&refused).exists(),
                "{k}: {what} created {refused}"
            );
        }
        assert_eq!(
            hex_sha256(&std::fs::read(&plan_file).unwrap()),
            kept,
            "{k}: a refused --plan changed the existing plan file"
        );

        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(&plan_file).unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "cli")]
#[test]
fn fsck_apply_in_place_repairs_a_copy_through_an_external_journal() {
    // `fsck.ntfs --apply PLAN --in-place --journal FILE TARGET` (#922): the
    // journal location is checked, TARGET opened exclusively and checked
    // against the plan, and the plan applied in place through the journal.
    // The copy must reach the files Windows' own restart left, and the
    // journal must read back through the library with every unit applied.
    // Each refusal must name its reason, exit non-zero, leave the copy's
    // SHA-256 unchanged and create no journal. The plan is saved through the
    // library (#905) until `--dry-run --plan FILE` (#906) writes it from the
    // CLI.
    use fs_ntfs::repair_journal::{progress, read_journal, JournalBinding};
    use fs_ntfs::repair_plan::{load_plan, plan_in_place_io, save_plan};
    use std::path::Path;
    for (k, _) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let pristine_sha = hex_sha256(&pristine);
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_in_place_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        let plan_file = format!("{src}.plan");
        std::fs::write(&plan_file, &saved).unwrap();
        let journal_dir = format!("{src}.journal.d");
        let _ = std::fs::remove_dir_all(&journal_dir);
        std::fs::create_dir(&journal_dir).unwrap();
        let journal = format!("{journal_dir}/repair.journal");

        // A second copy that no longer holds the first write's before-image.
        let first = &loaded.units[0].writes[0];
        let mut changed = pristine.clone();
        changed[first.offset as usize] ^= 0xff;
        let other = format!("{src}.other");
        std::fs::write(&other, &changed).unwrap();
        let other_sha = hex_sha256(&changed);
        drop(changed);
        drop(pristine);

        // Each refusal names its reason, exits non-zero, leaves its target
        // unchanged and creates no journal.
        let missing = format!("{journal_dir}/no-such-dir/repair.journal");
        let elsewhere = format!("{src}.to");
        let _ = std::fs::remove_file(&elsewhere);
        let cases: [(&str, Vec<&str>, &str, &str, &str); 9] = [
            (
                "--in-place without --journal",
                vec!["--apply", &plan_file, "--in-place", &src],
                "--journal",
                &src,
                &journal,
            ),
            (
                "--journal without --in-place",
                vec!["--apply", &plan_file, "--journal", &journal, &src],
                "--in-place",
                &src,
                &journal,
            ),
            (
                "--in-place with --to",
                vec![
                    "--apply",
                    &plan_file,
                    "--in-place",
                    "--journal",
                    &journal,
                    "--to",
                    &elsewhere,
                    &src,
                ],
                "--to",
                &src,
                &journal,
            ),
            (
                "--in-place with --dry-run",
                vec![
                    "--apply",
                    &plan_file,
                    "--in-place",
                    "--journal",
                    &journal,
                    "--dry-run",
                    &src,
                ],
                "--dry-run",
                &src,
                &journal,
            ),
            (
                "--in-place with -y",
                vec![
                    "--apply",
                    &plan_file,
                    "--in-place",
                    "--journal",
                    &journal,
                    "-y",
                    &src,
                ],
                "--yes",
                &src,
                &journal,
            ),
            (
                "a journal whose directory is missing",
                vec![
                    "--apply",
                    &plan_file,
                    "--in-place",
                    "--journal",
                    &missing,
                    &src,
                ],
                "does not exist",
                &src,
                &missing,
            ),
            (
                "a journal that already exists",
                vec![
                    "--apply",
                    &plan_file,
                    "--in-place",
                    "--journal",
                    &plan_file,
                    &src,
                ],
                "already exists",
                &src,
                &journal,
            ),
            (
                "a journal that is the device",
                vec!["--apply", &plan_file, "--in-place", "--journal", &src, &src],
                "is the device being repaired",
                &src,
                &journal,
            ),
            (
                "a device that does not hold the plan's before-images",
                vec![
                    "--apply",
                    &plan_file,
                    "--in-place",
                    "--journal",
                    &journal,
                    &other,
                ],
                "before-image",
                &other,
                &journal,
            ),
        ];
        let plan_sha = hex_sha256(&saved);
        for (what, args, reason, target, not_created) in cases {
            let (code, said) = fsck_cli(&args);
            assert_ne!(code, 0, "{k}: {what} went ahead: {said}");
            assert!(
                said.contains(reason),
                "{k}: {what} refused without naming '{reason}': {said}"
            );
            let want = if target == other {
                &other_sha
            } else {
                &pristine_sha
            };
            assert_eq!(
                &hex_sha256(&std::fs::read(target).unwrap()),
                want,
                "{k}: {what} changed {target}"
            );
            assert!(
                !Path::new(not_created).exists(),
                "{k}: {what} created {not_created}"
            );
            assert!(
                !Path::new(&elsewhere).exists(),
                "{k}: {what} created {elsewhere}"
            );
            assert_eq!(
                hex_sha256(&std::fs::read(&plan_file).unwrap()),
                plan_sha,
                "{k}: {what} changed the plan file"
            );
        }

        // A copy another process holds under an exclusive lock is refused.
        {
            let holder = std::fs::File::open(&src).unwrap();
            holder.lock().unwrap();
            let (code, said) = fsck_cli(&[
                "--apply",
                &plan_file,
                "--in-place",
                "--journal",
                &journal,
                &src,
            ]);
            assert_ne!(code, 0, "{k}: a locked copy went ahead: {said}");
            assert!(
                said.contains("held by another process"),
                "{k}: a locked copy refused without naming the holder: {said}"
            );
            assert_eq!(
                hex_sha256(&std::fs::read(&src).unwrap()),
                pristine_sha,
                "{k}: a locked copy was changed"
            );
            assert!(
                !Path::new(&journal).exists(),
                "{k}: a locked copy created the journal"
            );
        }

        // The in-place apply itself.
        let (code, said) = fsck_cli(&[
            "--apply",
            &plan_file,
            "--in-place",
            "--journal",
            &journal,
            &src,
        ]);
        assert_eq!(code, 0, "{k}: --apply --in-place exited {code}: {said}");
        let want = manifest(k);
        let got = walk(&src);
        assert!(
            got == want,
            "{k}: {} files after --apply --in-place, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let binding = JournalBinding::of(&saved).unwrap();
        let read = read_journal(&std::fs::read(&journal).unwrap(), &binding)
            .unwrap_or_else(|e| panic!("{k}: the journal read back refused: {e:?}"));
        assert_eq!(read.torn, None, "{k}: the journal ends in a torn record");
        let done = progress(&read, &loaded).unwrap_or_else(|e| panic!("{k}: progress: {e:?}"));
        assert_eq!(
            (done.applied as usize, done.open),
            (loaded.units.len(), None),
            "{k}: the journal does not mark every unit applied"
        );

        std::fs::remove_dir_all(&journal_dir).unwrap();
        std::fs::remove_file(&other).unwrap();
        std::fs::remove_file(&plan_file).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(feature = "cli")]
#[test]
fn fsck_apply_in_place_resume_finishes_a_stopped_repair_from_its_journal() {
    // `fsck.ntfs --apply PLAN --in-place --resume --journal FILE TARGET`
    // (#963): an in-place apply stopped halfway through a Windows
    // interrupted capture's plan leaves a journal; resumed from the CLI on
    // the same journal, the copy must reach the files and metadata Windows'
    // own restart reached, and the journal must mark every unit applied.
    // `--resume` naming no journal is a usage error that writes nothing.
    use fs_ntfs::block_io::PathIo;
    use fs_ntfs::repair_journal::{
        apply_in_place_observed, progress, read_journal, ApplyObserver, InPlaceError,
        JournalBinding,
    };
    use fs_ntfs::repair_plan::{load_plan, plan_in_place_io, save_plan};
    use std::path::Path;

    /// Asks for a stop once `stop` units have begun.
    struct StopAt {
        asked: usize,
        stop: usize,
    }
    impl ApplyObserver for StopAt {
        fn cancel_requested(&mut self) -> bool {
            self.asked += 1;
            self.asked > self.stop
        }
    }

    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_in_place_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        let units = loaded.units.len();
        assert!(units > 0, "{k}: a log holding work planned nothing");
        let plan_file = format!("{src}.resume.plan");
        std::fs::write(&plan_file, &saved).unwrap();
        let journal_dir = format!("{src}.resume.d");
        let _ = std::fs::remove_dir_all(&journal_dir);
        std::fs::create_dir(&journal_dir).unwrap();
        let journal = format!("{journal_dir}/repair.journal");
        let pristine_sha = hex_sha256(&pristine);
        let device = format!("{src}.resume.img");
        std::fs::write(&device, &pristine).unwrap();
        drop(pristine);

        // `--resume` with no journal to resume is refused before anything
        // is written.
        let (code, said) = fsck_cli(&[
            "--apply",
            &plan_file,
            "--in-place",
            "--resume",
            "--journal",
            &journal,
            &device,
        ]);
        assert_eq!(
            code, 16,
            "{k}: --resume with no journal exited {code}: {said}"
        );
        assert_eq!(
            hex_sha256(&std::fs::read(&device).unwrap()),
            pristine_sha,
            "{k}: --resume with no journal changed the copy"
        );
        assert!(
            !Path::new(&journal).exists(),
            "{k}: --resume with no journal created one"
        );

        // Stop the apply after half the units, as a cancel would.
        let stop = units / 2;
        std::fs::write(&journal, b"").unwrap();
        {
            let mut device = PathIo::open_rw(Path::new(&device)).unwrap();
            let mut jrn = PathIo::open_rw(Path::new(&journal)).unwrap();
            let sha: [u8; 32] = Sha256::digest(&saved).into();
            match apply_in_place_observed(
                &loaded,
                sha,
                &mut device,
                &mut jrn,
                &mut StopAt { asked: 0, stop },
            ) {
                Err(InPlaceError::Cancelled { applied }) => {
                    assert_eq!(applied as usize, stop, "{k}: units applied before the stop")
                }
                other => panic!("{k}: the apply stopped at {stop} of {units} gave {other:?}"),
            }
        }

        let (code, said) = fsck_cli(&[
            "--apply",
            &plan_file,
            "--in-place",
            "--resume",
            "--journal",
            &journal,
            &device,
        ]);
        assert_eq!(
            code, 0,
            "{k}: --apply --in-place --resume exited {code}: {said}"
        );
        let binding = JournalBinding::of(&saved).unwrap();
        let read = read_journal(&std::fs::read(&journal).unwrap(), &binding)
            .unwrap_or_else(|e| panic!("{k}: the journal read back refused: {e:?}"));
        assert_eq!(read.torn, None, "{k}: the journal ends in a torn record");
        let done = progress(&read, &loaded).unwrap_or_else(|e| panic!("{k}: progress: {e:?}"));
        assert_eq!(
            (done.applied as usize, done.open),
            (units, None),
            "{k}: the resumed journal does not mark every unit applied"
        );
        let want = manifest(k);
        let got = walk(&device);
        assert!(
            got == want,
            "{k}: {} files after --resume, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &in_place_image(k, &device),
            &Image::read(&recovered),
            log_end,
        );

        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_dir_all(&journal_dir).unwrap();
        std::fs::remove_file(&plan_file).unwrap();
        std::fs::remove_file(&device).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

/// The `(units, units_total)` of each progress line `fsck.ntfs --apply
/// --in-place` printed to stderr, in order: `... progress: U of N units, B
/// of T bytes` (#964).
#[cfg(all(unix, feature = "cli", feature = "harness"))]
fn apply_progress_lines(said: &str) -> Vec<(u64, u64)> {
    said.lines()
        .filter_map(|line| line.split_once(" progress: "))
        .map(|(_, rest)| {
            let words: Vec<&str> = rest.split_whitespace().collect();
            match words[..] {
                [u, "of", n, "units,", _, "of", _, "bytes"] => {
                    (u.parse().unwrap(), n.parse().unwrap())
                }
                _ => panic!("an unreadable progress line: {rest}"),
            }
        })
        .collect()
}

#[cfg(all(unix, feature = "cli", feature = "harness"))]
#[test]
fn fsck_apply_in_place_stops_at_a_unit_boundary_on_sigint() {
    // `fsck.ntfs --apply PLAN --in-place --journal FILE` (#964) reports
    // its progress on stderr and stops on SIGINT at the next unit
    // boundary. The harness build's FSCK_NTFS_HOLD_AFTER_UNITS holds the
    // apply after that many units until a signal arrives, so this test
    // sends a real SIGINT at a known point. The apply must exit with
    // fsck(8)'s operational status (8), name the units applied, and leave
    // a journal that `--resume` finishes into what Windows' own restart
    // produced. Progress lines must rise, and a finished apply's last one
    // must name the plan's total.
    use fs_ntfs::repair_journal::{progress, read_journal, JournalBinding};
    use fs_ntfs::repair_plan::{load_plan, plan_in_place_io, save_plan};
    use std::io::BufRead;

    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_in_place_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        let units = loaded.units.len() as u64;
        assert!(units > 0, "{k}: a log holding work planned nothing");
        let plan_file = format!("{src}.sigint.plan");
        std::fs::write(&plan_file, &saved).unwrap();
        let journal_dir = format!("{src}.sigint.d");
        let _ = std::fs::remove_dir_all(&journal_dir);
        std::fs::create_dir(&journal_dir).unwrap();
        let journal = format!("{journal_dir}/repair.journal");
        let device = format!("{src}.sigint.img");
        std::fs::write(&device, &pristine).unwrap();
        drop(pristine);

        let hold = units / 2;
        let mut child = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
            .args([
                "fsck",
                "--apply",
                &plan_file,
                "--in-place",
                "--journal",
                &journal,
                &device,
            ])
            .env("FSCK_NTFS_HOLD_AFTER_UNITS", hold.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fsck.ntfs");
        let mut stderr = std::io::BufReader::new(child.stderr.take().unwrap());
        let mut said = String::new();
        loop {
            let mut line = String::new();
            let read = stderr.read_line(&mut line).unwrap();
            assert!(
                read > 0,
                "{k}: fsck.ntfs ended before reporting {hold} units applied: {said}"
            );
            said.push_str(&line);
            if apply_progress_lines(&line).first().map(|p| p.0) == Some(hold) {
                break;
            }
        }
        // SAFETY: kill(2) on the child this test spawned and has not reaped.
        let sent = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(sent, 0, "{k}: SIGINT: {}", std::io::Error::last_os_error());
        stderr.read_to_string(&mut said).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(
            status.code(),
            Some(8),
            "{k}: the interrupted apply exited {status}: {said}"
        );
        assert!(
            said.contains(&format!("stopped after {hold} of {units} units")),
            "{k}: the interrupted apply did not name the {hold} units applied: {said}"
        );
        let stopped = apply_progress_lines(&said);
        assert!(
            stopped.windows(2).all(|p| p[0].0 < p[1].0) && stopped.last() == Some(&(hold, units)),
            "{k}: progress before the stop does not rise to {hold} of {units}: {stopped:?}"
        );
        let binding = JournalBinding::of(&saved).unwrap();
        let read = read_journal(&std::fs::read(&journal).unwrap(), &binding)
            .unwrap_or_else(|e| panic!("{k}: the journal read back refused: {e:?}"));
        let done = progress(&read, &loaded).unwrap_or_else(|e| panic!("{k}: progress: {e:?}"));
        assert_eq!(
            (done.applied as u64, done.open),
            (hold, None),
            "{k}: the interrupted journal does not stop at a unit boundary"
        );

        let (code, said) = fsck_cli(&[
            "--apply",
            &plan_file,
            "--in-place",
            "--resume",
            "--journal",
            &journal,
            &device,
        ]);
        assert_eq!(
            code, 0,
            "{k}: --apply --in-place --resume exited {code}: {said}"
        );
        let resumed = apply_progress_lines(&said);
        assert!(
            resumed.windows(2).all(|p| p[0].0 < p[1].0)
                && resumed.first().is_some_and(|p| p.0 == hold)
                && resumed.last() == Some(&(units, units)),
            "{k}: progress of the resume does not rise from {hold} to {units}: {resumed:?}"
        );
        let read = read_journal(&std::fs::read(&journal).unwrap(), &binding)
            .unwrap_or_else(|e| panic!("{k}: the journal read back refused: {e:?}"));
        let done = progress(&read, &loaded).unwrap_or_else(|e| panic!("{k}: progress: {e:?}"));
        assert_eq!(
            (done.applied as u64, done.open),
            (units, None),
            "{k}: the resumed journal does not mark every unit applied"
        );
        let want = manifest(k);
        let got = walk(&device);
        assert!(
            got == want,
            "{k}: {} files after the interrupted apply resumed, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &in_place_image(k, &device),
            &Image::read(&recovered),
            log_end,
        );

        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_dir_all(&journal_dir).unwrap();
        std::fs::remove_file(&plan_file).unwrap();
        std::fs::remove_file(&device).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}

#[cfg(all(unix, feature = "cli", feature = "harness"))]
#[test]
fn fsck_a_second_resume_of_a_journal_another_holds_is_refused_and_the_first_completes() {
    // `fsck.ntfs --apply PLAN --in-place --resume --journal FILE` (#936)
    // holds the journal for the whole resume. A resume stopped halfway
    // through a Windows interrupted capture's plan is resumed and held by
    // the harness build's FSCK_NTFS_HOLD_AFTER_UNITS; a second resume of the
    // same journal, on a copy of the stopped volume and on the volume
    // itself, must be refused naming the journal, with the journal, the
    // copy and the volume unchanged. Released by SIGUSR1, the first resume
    // must reach the files and metadata Windows' own restart reached.
    use fs_ntfs::block_io::PathIo;
    use fs_ntfs::repair_journal::{
        apply_in_place_observed, progress, read_journal, ApplyObserver, InPlaceError,
        JournalBinding,
    };
    use fs_ntfs::repair_plan::{load_plan, plan_in_place_io, save_plan};
    use std::io::BufRead;
    use std::path::Path;

    /// Asks for a stop once `stop` units have begun.
    struct StopAt {
        asked: usize,
        stop: usize,
    }
    impl ApplyObserver for StopAt {
        fn cancel_requested(&mut self) -> bool {
            self.asked += 1;
            self.asked > self.stop
        }
    }

    for (k, log_end) in captures_holding_work() {
        let src = unpack(&format!("windows-interrupted-{k}"));
        let pristine = std::fs::read(&src).unwrap();
        let mut v = RecordedRepair {
            bytes: pristine.clone(),
            units: Vec::new(),
            open: Vec::new(),
        };
        let plan = plan_in_place_io(&mut v, fsck::FsckOptions::default())
            .unwrap_or_else(|e| panic!("{k}: plan: {e}"));
        let saved = save_plan(&plan, &mut v).unwrap_or_else(|e| panic!("{k}: save: {e}"));
        let loaded = load_plan(&saved).unwrap_or_else(|e| panic!("{k}: load: {e}"));
        let units = loaded.units.len();
        assert!(units > 0, "{k}: a log holding work planned nothing");
        let plan_file = format!("{src}.held.plan");
        std::fs::write(&plan_file, &saved).unwrap();
        let journal_dir = format!("{src}.held.d");
        let _ = std::fs::remove_dir_all(&journal_dir);
        std::fs::create_dir(&journal_dir).unwrap();
        let journal = format!("{journal_dir}/repair.journal");
        let device = format!("{src}.held.img");
        std::fs::write(&device, &pristine).unwrap();
        drop(pristine);

        let stop = units / 2;
        std::fs::write(&journal, b"").unwrap();
        {
            let mut io = PathIo::open_rw(Path::new(&device)).unwrap();
            let mut jrn = PathIo::open_rw(Path::new(&journal)).unwrap();
            let sha: [u8; 32] = Sha256::digest(&saved).into();
            match apply_in_place_observed(
                &loaded,
                sha,
                &mut io,
                &mut jrn,
                &mut StopAt { asked: 0, stop },
            ) {
                Err(InPlaceError::Cancelled { applied }) => {
                    assert_eq!(applied as usize, stop, "{k}: units applied before the stop")
                }
                other => panic!("{k}: the apply stopped at {stop} of {units} gave {other:?}"),
            }
        }
        let other = format!("{src}.held.other.img");
        std::fs::copy(&device, &other).unwrap();

        let mut child = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
            .args([
                "fsck",
                "--apply",
                &plan_file,
                "--in-place",
                "--resume",
                "--journal",
                &journal,
                &device,
            ])
            .env("FSCK_NTFS_HOLD_AFTER_UNITS", stop.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fsck.ntfs");
        let mut stderr = std::io::BufReader::new(child.stderr.take().unwrap());
        let mut said = String::new();
        loop {
            let mut line = String::new();
            let read = stderr.read_line(&mut line).unwrap();
            assert!(
                read > 0,
                "{k}: the first resume ended before reporting {stop} units applied: {said}"
            );
            said.push_str(&line);
            if apply_progress_lines(&line).first().map(|p| p.0) == Some(stop as u64) {
                break;
            }
        }

        let journal_sha = hex_sha256(&std::fs::read(&journal).unwrap());
        for (what, target) in [("a copy", &other), ("the held volume", &device)] {
            let target_sha = hex_sha256(&std::fs::read(target).unwrap());
            let (code, second) = fsck_cli(&[
                "--apply",
                &plan_file,
                "--in-place",
                "--resume",
                "--journal",
                &journal,
                target,
            ]);
            assert_ne!(
                code, 0,
                "{k}: a second resume of the held journal on {what} went ahead: {second}"
            );
            assert!(
                second.contains(&journal) && second.contains("held by another process"),
                "{k}: a second resume on {what} was refused without naming the held journal: \
                 {second}"
            );
            assert_eq!(
                hex_sha256(&std::fs::read(target).unwrap()),
                target_sha,
                "{k}: a second resume of the held journal changed {what}"
            );
            assert_eq!(
                hex_sha256(&std::fs::read(&journal).unwrap()),
                journal_sha,
                "{k}: a second resume on {what} changed the held journal"
            );
        }

        // SAFETY: kill(2) on the child this test spawned and has not reaped.
        let sent = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGUSR1) };
        assert_eq!(sent, 0, "{k}: SIGUSR1: {}", std::io::Error::last_os_error());
        stderr.read_to_string(&mut said).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(
            status.code(),
            Some(0),
            "{k}: the first resume, released, exited {status}: {said}"
        );
        let binding = JournalBinding::of(&saved).unwrap();
        let read = read_journal(&std::fs::read(&journal).unwrap(), &binding)
            .unwrap_or_else(|e| panic!("{k}: the journal read back refused: {e:?}"));
        let done = progress(&read, &loaded).unwrap_or_else(|e| panic!("{k}: progress: {e:?}"));
        assert_eq!(
            (done.applied as usize, done.open),
            (units, None),
            "{k}: the first resume's journal does not mark every unit applied"
        );
        let want = manifest(k);
        let got = walk(&device);
        assert!(
            got == want,
            "{k}: {} files after the first resume, {} in Windows' manifest",
            got.len(),
            want.len()
        );
        let recovered = unpack(&format!("windows-interrupted-{k}.recovered"));
        oracle::same_metadata_as_windows(
            k,
            &Image::read(&src),
            &in_place_image(k, &device),
            &Image::read(&recovered),
            log_end,
        );

        std::fs::remove_file(&recovered).unwrap();
        std::fs::remove_dir_all(&journal_dir).unwrap();
        std::fs::remove_file(&plan_file).unwrap();
        std::fs::remove_file(&other).unwrap();
        std::fs::remove_file(&device).unwrap();
        std::fs::remove_file(&src).unwrap();
    }
}
