//! `check_io` reports an owner whose quota usage in `$Extend\$Quota:$Q`
//! is not what its files are charged (#678, part of #510), and a file whose
//! own charge is not what its allocation costs (#861).
//!
//! chkdsk recounts `$Q` from the files it finds and reports a mismatch. The
//! checker looked at no quota accounting at all, so drift left by a write
//! was invisible until Windows saw the volume.
//!
//! Graded by Windows: `windows-interrupted-quota.recovered` and
//! `windows-interrupted-quota-spilled.recovered` are volumes Windows wrote
//! with quota tracking on, whose accounting Windows' own recount
//! (`win-quota-verify`) agrees with, so the checker has to call them clean.
//! A copy with one owner's usage moved in `$Q`, edited here byte by byte in
//! `$Quota`'s index root, has to be reported, naming that owner, its usage
//! and what its files are charged. A copy with one file's charge moved,
//! and its owner's usage moved with it, has to be reported naming that
//! file's record, its charge and what its allocation costs, and no owner.

mod common;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::fsck::{self, CheckFinding};
use fs_ntfs::{mft_io, write};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const TRACKED: &str = "windows-interrupted-quota.recovered";
const SPILLED: &str = "windows-interrupted-quota-spilled.recovered";

const INDEX_ROOT: u32 = 0x90;
/// `$Q`'s quota control entry, which no file is charged to.
const CONTROL_ID: u32 = 1;
/// How far the edited owner's usage is moved.
const DRIFT: u64 = 4096;

fn unpack(name: &str, tag: &str) -> String {
    let path = common::temp_image_path(format!("check_quota_{tag}"));
    let out = File::create(&path).expect("create image copy");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    path
}

fn le(b: &[u8], at: usize, n: usize) -> u64 {
    let mut v = [0u8; 8];
    v[..n].copy_from_slice(&b[at..at + n]);
    u64::from_le_bytes(v)
}

fn quota_findings(img: &str) -> Vec<CheckFinding> {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    fsck::check_io(&mut io)
        .expect("check_io")
        .findings
        .into_iter()
        .filter(|f| {
            matches!(
                f,
                CheckFinding::QuotaUsageDrift { .. }
                    | CheckFinding::QuotaUnreadable { .. }
                    | CheckFinding::QuotaChargeDrift { .. }
            )
        })
        .collect()
}

/// Move the first owner's usage in the resident root of `$Quota:$Q` by
/// `DRIFT` bytes, read straight from the record's attribute and index
/// headers; the owner id and its usage before the edit.
fn drift_first_owner(img: &str) -> (u32, u64) {
    drift_owner(img, None)
}

/// [`drift_first_owner`], for the owner `want` when one is named.
fn drift_owner(img: &str, want: Option<u32>) -> (u32, u64) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let rec = write::resolve_path_to_record_number_io(&mut io, "/$Extend/$Quota")
        .expect("/$Extend/$Quota");
    let mut edited = None;
    mft_io::update_mft_record_io(&mut io, rec, |r| {
        let mut a = le(r, 0x14, 2) as usize;
        while le(r, a, 4) as u32 != 0xFFFF_FFFF {
            let name: Vec<u16> = (0..r[a + 9] as usize)
                .map(|i| le(r, a + le(r, a + 10, 2) as usize + 2 * i, 2) as u16)
                .collect();
            if le(r, a, 4) as u32 == INDEX_ROOT && String::from_utf16_lossy(&name) == "$Q" {
                let h = a + le(r, a + 0x14, 2) as usize + 0x10;
                let (mut at, end) = (h + le(r, h, 4) as usize, h + le(r, h + 4, 4) as usize);
                while at < end && le(r, at + 12, 2) & 2 == 0 {
                    let id = le(r, at + 16, 4) as u32;
                    if id != CONTROL_ID && want.is_none_or(|w| w == id) {
                        let used = at + le(r, at, 2) as usize + 8;
                        let was = le(r, used, 8);
                        r[used..used + 8].copy_from_slice(&(was + DRIFT).to_le_bytes());
                        edited = Some((id, was));
                        return Ok(());
                    }
                    at += le(r, at + 8, 2) as usize;
                }
            }
            a += le(r, a + 4, 4) as usize;
        }
        Ok(())
    })
    .expect("rewrite $Quota's record");
    edited.expect("an owner entry in $Q's index root")
}

#[test]
fn windows_quota_accounting_checks_clean() {
    for (name, tag) in [(TRACKED, "tracked"), (SPILLED, "spilled")] {
        let img = unpack(name, tag);
        assert_eq!(
            quota_findings(&img),
            vec![],
            "{name}: Windows' own quota accounting reported as drift"
        );
    }
}

#[test]
fn an_owner_whose_usage_drifted_is_reported_with_both_numbers() {
    let img = unpack(TRACKED, "drift");
    let (owner, charged) = drift_first_owner(&img);
    assert_eq!(
        quota_findings(&img),
        vec![CheckFinding::QuotaUsageDrift {
            owner,
            usage: Some(charged + DRIFT),
            charged,
        }],
        "owner {owner}'s usage moved by {DRIFT} bytes in $Q"
    );
}

const ATTRIBUTE_LIST: u32 = 0x20;
const STANDARD_INFORMATION: u32 = 0x10;
const DATA: u32 = 0x80;
/// In `$STANDARD_INFORMATION`'s value: the owner id, and the bytes charged.
const SI_OWNER_ID: usize = 0x30;
const SI_QUOTA: usize = 0x38;
/// A `$DATA` attribute header's compression and sparse flags.
const STREAM_COMPRESSED_OR_SPARSE: u64 = 0x80FF;

/// Move the charge of the first in-use file charged to an owner, one with
/// no `$ATTRIBUTE_LIST` and no sparse or compressed stream, by `DRIFT`
/// bytes in its `$STANDARD_INFORMATION`, read straight from the record's
/// headers; the record, its owner and its charge before the edit.
fn drift_first_charged_file(img: &str) -> (u64, u32, u64) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    for rec in 24.. {
        let Ok((_, r)) = mft_io::read_mft_record_io(&mut io, rec) else {
            break;
        };
        // In use, not a directory, a base record.
        if &r[0..4] != b"FILE" || le(&r, 0x16, 2) != 1 || le(&r, 0x20, 8) != 0 {
            continue;
        }
        let (mut a, mut si, mut plain) = (le(&r, 0x14, 2) as usize, None, true);
        while le(&r, a, 4) as u32 != 0xFFFF_FFFF {
            match le(&r, a, 4) as u32 {
                ATTRIBUTE_LIST => plain = false,
                DATA if le(&r, a + 0x0C, 2) & STREAM_COMPRESSED_OR_SPARSE != 0 => plain = false,
                STANDARD_INFORMATION if le(&r, a + 0x10, 4) as usize >= SI_QUOTA + 8 => {
                    si = Some(a + le(&r, a + 0x14, 2) as usize)
                }
                _ => {}
            }
            a += le(&r, a + 4, 4) as usize;
        }
        let Some(si) = si.filter(|_| plain) else {
            continue;
        };
        let (owner, was) = (le(&r, si + SI_OWNER_ID, 4) as u32, le(&r, si + SI_QUOTA, 8));
        if owner == 0 || was == 0 {
            continue;
        }
        mft_io::update_mft_record_io(&mut io, rec, |r| {
            r[si + SI_QUOTA..si + SI_QUOTA + 8].copy_from_slice(&(was + DRIFT).to_le_bytes());
            Ok(())
        })
        .expect("rewrite the file's record");
        return (rec, owner, was);
    }
    panic!("{img}: no file charged to an owner");
}

#[test]
fn a_file_whose_charge_drifted_is_reported_with_both_numbers() {
    let img = unpack(TRACKED, "file_drift");
    let (record, owner, was) = drift_first_charged_file(&img);
    // The owner's usage moves with the file, so only the file is wrong.
    drift_owner(&img, Some(owner));
    assert_eq!(
        quota_findings(&img),
        vec![CheckFinding::QuotaChargeDrift {
            record,
            charged: was + DRIFT,
            due: was,
        }],
        "record {record}'s charge moved by {DRIFT} bytes, and owner {owner}'s usage with it"
    );
}
