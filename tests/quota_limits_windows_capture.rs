//! Each `$Extend\$Quota:$Q` entry's warning limit, hard limit and usage,
//! and the control entry's enforcement flag, read as Windows set them
//! (#868, part of #679).
//!
//! The volume is made by test-disks/capture-quota-limits.ps1 in the Windows
//! CI job: Windows formats it, turns quota enforcement on with
//! `fsutil quota enforce`, gives LOCAL SERVICE a file and a warning level
//! and hard limit of its own with `fsutil quota modify`, and detaches it.
//! The expected values are what Windows' own `fsutil quota query` printed
//! for that volume before the detach, never this crate's reading. A missing
//! capture fails, naming that script.
#![cfg(windows)]

use fs_ntfs::block_io::PathIo;
use fs_ntfs::quota::{entries_io, QuotaEntry};
use std::path::Path;

const IMAGE: &str = "test-disks/quota-limits.img";
const CAPTURE: &str = "test-disks/quota-limits.tsv";

/// FileSystemControlFlags, as `fsutil quota query` prints them.
const FILE_VC_QUOTA_TRACK: u64 = 0x1;
const FILE_VC_QUOTA_ENFORCE: u64 = 0x2;

/// What the capture script asked `fsutil quota modify` to set.
const SET_THRESHOLD: u64 = 1024 * 1024;
const SET_LIMIT: u64 = 2 * 1024 * 1024;
const LOCAL_SERVICE: &str = "S-1-5-19";

struct Owner {
    sid: String,
    used: u64,
    threshold: u64,
    limit: u64,
}

struct Captured {
    control_flags: u64,
    default_threshold: u64,
    default_limit: u64,
    owners: Vec<Owner>,
}

fn number(text: &str) -> u64 {
    let text = text.trim();
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    }
    .unwrap_or_else(|e| panic!("{CAPTURE}: '{text}' is not a number: {e}"))
}

fn captured() -> Captured {
    let tsv = std::fs::read_to_string(CAPTURE)
        .unwrap_or_else(|e| panic!("{CAPTURE}: {e}: run test-disks/capture-quota-limits.ps1"));
    let rows: Vec<Vec<&str>> = tsv
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| l.split('\t').collect())
        .collect();
    let field = |name: &str| -> u64 {
        let row = rows
            .iter()
            .find(|r| r[0] == name)
            .unwrap_or_else(|| panic!("{CAPTURE} has no {name} row"));
        number(row[1])
    };
    let owners = rows
        .iter()
        .filter(|r| r[0] == "owner")
        .map(|r| {
            assert_eq!(r.len(), 5, "{CAPTURE}: an owner row of {} fields", r.len());
            Owner {
                sid: r[1].to_string(),
                used: number(r[2]),
                threshold: number(r[3]),
                limit: number(r[4]),
            }
        })
        .collect();
    Captured {
        control_flags: field("control_flags"),
        default_threshold: field("default_threshold"),
        default_limit: field("default_limit"),
        owners,
    }
}

/// A SID in the `S-R-A-S1-...` form Windows prints.
fn sid_string(sid: &[u8]) -> String {
    assert!(sid.len() >= 8, "a SID of {} bytes", sid.len());
    let authority = sid[2..8].iter().fold(0u64, |a, b| (a << 8) | u64::from(*b));
    let mut text = format!("S-{}-{authority}", sid[0]);
    for sub in sid[8..].chunks_exact(4) {
        text.push_str(&format!("-{}", u32::from_le_bytes(sub.try_into().unwrap())));
    }
    text
}

fn read() -> Vec<QuotaEntry> {
    assert!(
        Path::new(IMAGE).exists(),
        "{IMAGE}: run test-disks/capture-quota-limits.ps1"
    );
    let mut io = PathIo::open_ro(Path::new(IMAGE)).expect("open the captured volume");
    entries_io(&mut io).expect("read $Extend\\$Quota:$Q")
}

#[test]
fn the_capture_is_of_a_volume_windows_enforces_quotas_on() {
    let c = captured();
    assert_ne!(
        c.control_flags & FILE_VC_QUOTA_ENFORCE,
        0,
        "fsutil: enforcement is off"
    );
    let ls = c
        .owners
        .iter()
        .find(|o| o.sid == LOCAL_SERVICE)
        .expect("fsutil lists no entry for LOCAL SERVICE");
    assert_eq!((ls.threshold, ls.limit), (SET_THRESHOLD, SET_LIMIT));
    assert_ne!(ls.used, 0, "LOCAL SERVICE's file is charged nothing");
}

#[test]
fn the_control_entry_carries_the_enforcement_flag_and_defaults_windows_set() {
    let c = captured();
    let entries = read();
    let control = entries
        .iter()
        .find(|e| e.is_control())
        .expect("$Q has no quota control entry");
    assert_eq!(
        control.enforcement_enabled(),
        c.control_flags & FILE_VC_QUOTA_ENFORCE != 0,
        "enforcement, flags 0x{:x} against fsutil's 0x{:x}",
        control.flags,
        c.control_flags
    );
    assert_eq!(
        control.tracking_enabled(),
        c.control_flags & FILE_VC_QUOTA_TRACK != 0,
        "tracking, flags 0x{:x} against fsutil's 0x{:x}",
        control.flags,
        c.control_flags
    );
    assert_eq!(
        control.warning_limit, c.default_threshold,
        "default threshold"
    );
    assert_eq!(control.hard_limit, c.default_limit, "default limit");
}

#[test]
fn each_owner_entry_carries_the_usage_and_limits_windows_set() {
    let c = captured();
    let entries = read();
    let owners: Vec<&QuotaEntry> = entries.iter().filter(|e| !e.is_control()).collect();
    for want in &c.owners {
        let got = owners
            .iter()
            .find(|e| sid_string(&e.sid) == want.sid)
            .unwrap_or_else(|| panic!("$Q has no entry for {}", want.sid));
        assert_eq!(got.bytes_used, want.used, "{}: quota used", want.sid);
        assert_eq!(
            got.warning_limit, want.threshold,
            "{}: quota threshold",
            want.sid
        );
        assert_eq!(got.hard_limit, want.limit, "{}: quota limit", want.sid);
    }
    let read_sids: Vec<String> = owners.iter().map(|e| sid_string(&e.sid)).collect();
    let fsutil_sids: Vec<&str> = c.owners.iter().map(|o| o.sid.as_str()).collect();
    assert_eq!(
        read_sids.len(),
        fsutil_sids.len(),
        "$Q holds {read_sids:?}, fsutil lists {fsutil_sids:?}"
    );
}
