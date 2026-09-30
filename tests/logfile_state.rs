//! Whether `$LogFile` holds anything to replay, decided from its restart
//! area, and what `fsck` does with that answer (#375).
//!
//! THE ORACLES ARE NOT THIS CRATE.
//!
//! * **Windows' own log.** `test-disks/windows-clean-logfile.bin.gz` is the
//!   `$LogFile` of `ntfs-cli-read.img`, a volume Windows formatted, wrote to
//!   and cleanly detached (`test-disks/build-windows-native-read-fixtures.ps1`
//!   on `windows-latest`, CI run 36631252288 on `main` at 4a7622f, artifact
//!   `ntfs-windows-native-read-fixtures`). Cut out with ntfs-3g's `ntfscat
//!   -i 2`; 3,194,880 bytes, SHA-256 b5b8d7ed...a8a54 before gzip. It shows
//!   what a clean log looks like to the one writer that defines the format:
//!   restart-area flags `0x0002` (clean) with a client still in use, and a
//!   client restart LSN that resolves to a checkpoint record with no
//!   transactions and no dirty pages.
//! * **ntfs-3g**, run at arm's length (GPL, never linked). `ntfs-3g.probe
//!   --readwrite` refuses a volume whose log records an unclean shutdown
//!   ("The disk contains an unclean file system") and one whose dirty flag
//!   is set, so it grades both what mkfs writes and what fsck leaves.
//!   Missing, the test FAILS naming the package.

mod common;

use std::path::Path;
use std::process::Command;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::fsck;
use fs_ntfs::mkfs::format_filesystem;

const SIZE: u64 = 64 << 20;

fn format(stem: &str) -> String {
    let img = common::temp_image_path(stem);
    std::fs::File::create(&img)
        .and_then(|f| f.set_len(SIZE))
        .expect("create image");
    let mut dev = PathIo::open_rw(Path::new(&img)).expect("open image");
    format_filesystem(&mut dev, SIZE, 4096, 4096, None, Some(1)).expect("format");
    img
}

/// `ntfs-3g.probe --readwrite`: status and stderr. 0 is "mountable
/// read-write"; 14 is ntfs-3g's "unclean". It does not look at the dirty
/// flag: [`ntfs3g_opens`] does.
fn ntfs3g_probe_rw(img: &str) -> (i32, String) {
    let out = Command::new("ntfs-3g.probe")
        .args(["--readwrite", img])
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "ntfs-3g.probe could not be run ({e}): install ntfs-3g (`apt-get install ntfs-3g`)"
            )
        });
    (
        out.status.code().expect("exit status"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Whether ntfs-3g's library opens the volume at all, and its stderr.
/// Unlike `ntfs-3g.probe`, an open refuses a volume whose dirty flag is set
/// ("scheduled for check"), so this is the oracle for the flag.
fn ntfs3g_opens(img: &str) -> (bool, String) {
    let out = Command::new("ntfscat")
        .args(["-i", "3", img])
        .output()
        .unwrap_or_else(|e| {
            panic!("ntfscat could not be run ({e}): install ntfs-3g (`apt-get install ntfs-3g`)")
        });
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `$LogFile`'s bytes on `img`, read through ntfs-3g rather than through
/// this crate, so a "the log was left alone" check does not ask the code
/// under test where the log is.
fn logfile_bytes(img: &str) -> Vec<u8> {
    let out = Command::new("ntfscat")
        .args(["-f", "-i", "2", img])
        .output()
        .unwrap_or_else(|e| {
            panic!("ntfscat could not be run ({e}): install ntfs-3g (`apt-get install ntfs-3g`)")
        });
    assert!(
        out.status.success(),
        "ntfscat -i 2 {img}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn a_volume_this_crate_formats_is_one_ntfs3g_will_mount_read_write() {
    let img = format("logfile_fresh_ntfs3g");
    let (code, err) = ntfs3g_probe_rw(&img);
    assert_eq!(
        code, 0,
        "ntfs-3g refuses a read-write mount of a volume mkfs just made: {err}"
    );
}

#[test]
fn a_dirty_volume_this_crate_formatted_is_repaired_and_its_log_kept() {
    let img = format("logfile_dirty_repair");
    assert!(fsck::set_dirty(&img).expect("set_dirty"));
    let (opened, err) = ntfs3g_opens(&img);
    assert!(
        !opened && err.contains("scheduled for check"),
        "setup: ntfs-3g must refuse the volume once it is dirty: {err}"
    );
    let log_before = logfile_bytes(&img);

    let report =
        fsck::fsck(&img).expect("fsck must clear the dirty flag over a log with nothing to replay");
    assert!(report.dirty_cleared, "{report:?}");
    assert_eq!(
        report.logfile_bytes, 0,
        "a log that records nothing to replay is kept, not overwritten"
    );
    assert!(!fsck::is_dirty(&img).unwrap());
    assert!(
        logfile_bytes(&img) == log_before,
        "$LogFile changed; fsck must leave a clean log intact"
    );

    let (opened, err) = ntfs3g_opens(&img);
    assert!(
        opened,
        "after fsck, ntfs-3g still will not open the volume: {err}"
    );
    let (code, err) = ntfs3g_probe_rw(&img);
    assert_eq!(
        code, 0,
        "after fsck, ntfs-3g refuses a read-write mount: {err}"
    );
}

// ---------------------------------------------------------------------------
// The decision itself, on the log Windows wrote.
// ---------------------------------------------------------------------------

use fs_ntfs::logfile::{self, CleanBecause, LogfileState};

fn windows_log() -> Vec<u8> {
    let out = Command::new("gzip")
        .args(["-dc", "test-disks/windows-clean-logfile.bin.gz"])
        .output()
        .expect("spawn gzip");
    assert!(
        out.status.success(),
        "gzip -dc windows-clean-logfile.bin.gz"
    );
    assert_eq!(
        out.stdout.len(),
        3_194_880,
        "the Windows $LogFile is 3,194,880 bytes"
    );
    out.stdout
}

/// Restart-area offset of the `flags` field in restart page `page`.
fn flags_at(log: &[u8], page: usize) -> usize {
    page + u16::from_le_bytes([log[page + 0x18], log[page + 0x19]]) as usize
        + logfile::RESTART_AREA_FLAGS_OFFSET
}

/// Clear `RESTART_VOLUME_IS_CLEAN` in both restart pages. The flags sit in
/// each page's first sector, clear of the update sequence array.
fn without_clean_flag(mut log: Vec<u8>) -> Vec<u8> {
    for page in [0, 4096] {
        let at = flags_at(&log, page);
        log[at] &= !(logfile::RESTART_VOLUME_IS_CLEAN as u8);
    }
    log
}

#[test]
fn windows_marks_a_cleanly_detached_volume_clean() {
    assert_eq!(
        logfile::state(&windows_log()),
        LogfileState::Clean(CleanBecause::MarkedClean)
    );
}

/// Without the flag, the log is judged on its checkpoint alone -- which is
/// the path that has to find the record where the restart LSN says it is.
/// On Windows' bytes that is 0x220a8, and the record there is a checkpoint
/// with no open transaction and no dirty page.
#[test]
fn windows_last_checkpoint_alone_shows_nothing_to_replay() {
    assert_eq!(
        logfile::state(&without_clean_flag(windows_log())),
        LogfileState::Clean(CleanBecause::CheckpointOnly)
    );
}

#[test]
fn records_after_the_last_checkpoint_may_be_transactions() {
    let mut log = without_clean_flag(windows_log());
    for page in [0usize, 4096] {
        let ra = page + u16::from_le_bytes([log[page + 0x18], log[page + 0x19]]) as usize;
        let lsn = u64::from_le_bytes(log[ra..ra + 8].try_into().unwrap()) + 0x40;
        log[ra..ra + 8].copy_from_slice(&lsn.to_le_bytes());
    }
    let state = logfile::state(&log);
    assert!(
        matches!(&state, LogfileState::Pending(why) if why.contains("after its last checkpoint")),
        "{state:?}"
    );
}

#[test]
fn a_checkpoint_listing_open_transactions_may_be_transactions() {
    let mut log = without_clean_flag(windows_log());
    // The checkpoint record is at 0x220a8; its body follows the 0x30-byte
    // record header, and the transaction table's length is at body + 0x3C.
    let at = 0x220a8 + 0x30 + 0x3C;
    log[at..at + 4].copy_from_slice(&0x28u32.to_le_bytes());
    let state = logfile::state(&log);
    assert!(
        matches!(&state, LogfileState::Pending(why) if why.contains("open transactions")),
        "{state:?}"
    );
}

#[test]
fn a_torn_restart_page_is_not_trusted() {
    let mut log = without_clean_flag(windows_log());
    // The last two bytes of each restart page's first sector carry its
    // update sequence number; both pages torn leaves nothing to read.
    log[510] ^= 0xff;
    log[4096 + 510] ^= 0xff;
    assert!(logfile::state(&log).needs_replay());
}

// ---------------------------------------------------------------------------
// Logs Windows was still writing when they were captured (#366).
//
// `test-disks/windows-interrupted-logfile-{1,5}.bin.gz` are the `$LogFile`s
// of two VSS snapshots of a VHD on `windows-latest` taken while a workload
// created, renamed and deleted files on it (logfile-oracle.yml, run
// 36664699296, test-disks/capture-interrupted-logfile.ps1; candidates 1 and
// 5). Cut out with ntfs-3g's `ntfscat -f -i 2`; 2,097,152 bytes each,
// SHA-256 e6a36b92...d638eb and fa15e834...67ad1ec before gzip. ntfs-3g
// calls both volumes unclean. They are LFS 2.0 logs with a client open.
//
// * Candidate 1's restart area names checkpoint LSN 0x33e791, and three
//   RCRD pages end past it (the highest at 0x33ea1e): records Windows
//   logged after its last checkpoint, which a replay would redo.
// * Candidate 5's newest checkpoint record is only in the LFS tail copy at
//   log offset 0x2000 -- its home page was not yet written -- and no page
//   ends past it.
// ---------------------------------------------------------------------------

fn windows_interrupted_log(k: u32) -> Vec<u8> {
    let path = format!("test-disks/windows-interrupted-logfile-{k}.bin.gz");
    let out = Command::new("gzip")
        .args(["-dc", &path])
        .output()
        .expect("spawn gzip");
    assert!(out.status.success(), "gzip -dc {path}");
    assert_eq!(out.stdout.len(), 2_097_152, "{path} is a 2 MiB $LogFile");
    out.stdout
}

#[test]
fn records_windows_logged_after_its_last_checkpoint_may_be_transactions() {
    let state = logfile::state(&windows_interrupted_log(1));
    assert!(
        matches!(&state, LogfileState::Pending(why) if why.contains("after its last checkpoint")),
        "{state:?}"
    );
}

#[test]
fn a_checkpoint_windows_left_in_the_tail_copy_is_found() {
    let state = logfile::state(&windows_interrupted_log(5));
    assert!(
        !matches!(&state, LogfileState::Pending(why) if why.contains("cannot be read")),
        "{state:?}"
    );
}
