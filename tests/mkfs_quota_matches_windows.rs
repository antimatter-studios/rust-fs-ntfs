//! A fresh volume's `$Extend\$Quota` holds what a Windows-formatted one
//! holds (#862).
//!
//! Measured on two volumes Windows formatted with quota tracking off,
//! `windows-interrupted-1.recovered` (4K clusters) and
//! `windows-interrupted-small-cluster.recovered` (1K clusters), `$Quota`
//! carries the same three entries at both cluster sizes:
//!
//! * `$Q` key 1, the quota control entry: version 2, flags 1, nothing
//!   used, no warning or hard limit (-1), never exceeded.
//! * `$Q` key 0x100, the default owner's entry: the same quota record,
//!   followed by the owner's SID, S-1-5-32-544 (BUILTIN\Administrators).
//! * `$O` that SID, mapped to owner id 0x100.
//!
//! One field differs per volume: each `$Q` entry's change time (bytes
//! 0x10..0x18 of its data), which Windows sets to when the volume was
//! formatted. It is compared as "set", not by value; every other byte of
//! every entry, keys and entry headers included, must match Windows'.
//!
//! Both sides are read by a raw reader written for this test, not by this
//! crate, and the expected bytes are Windows', not ours.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const VOL_SIZE: u64 = 64 * 1024 * 1024;
const INDEX_ROOT: u32 = 0x90;
/// Where a `$Q` entry's change time sits in its data.
const CHANGE_TIME: std::ops::Range<usize> = 0x10..0x18;

fn le(b: &[u8], at: usize, n: usize) -> u64 {
    (0..n).rev().fold(0, |v, i| (v << 8) | b[at + i] as u64)
}

fn unpack(name: &str) -> Vec<u8> {
    let path = common::temp_image_path(format!("mkfs_quota_{name}"));
    let out = File::create(&path).expect("create image copy");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    let bytes = std::fs::read(&path).expect("read image copy");
    let _ = std::fs::remove_file(&path);
    bytes
}

fn formatted(cluster: u32) -> Vec<u8> {
    let path = common::temp_image_path(format!("mkfs_quota_ours_{cluster}"));
    File::create(&path)
        .and_then(|f| f.set_len(VOL_SIZE))
        .expect("create image");
    let mut io = PathIo::open_rw(Path::new(&path)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, cluster, 1024, Some("QUOTA"), Some(7))
        .expect("format_filesystem");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    drop(io);
    let bytes = std::fs::read(&path).expect("read image");
    let _ = std::fs::remove_file(&path);
    bytes
}

/// Each entry of `$Quota`'s index root named `index` (the first in-use MFT
/// record with a `$Q` root), LAST sentinel included, as raw entry bytes.
fn root_entries(vol: &[u8], index: &str) -> Vec<Vec<u8>> {
    let cluster = le(vol, 0x0B, 2) * le(vol, 0x0D, 1);
    let c = vol[0x40] as i8;
    let size = if c > 0 { c as u64 * cluster } else { 1 << -c } as usize;
    let mft = (le(vol, 0x30, 8) * cluster) as usize;
    for n in 0..64 {
        let mut rec = vol[mft + n * size..mft + (n + 1) * size].to_vec();
        if &rec[..4] != b"FILE" || le(&rec, 0x16, 2) & 1 == 0 {
            continue;
        }
        let (usa, count) = (le(&rec, 4, 2) as usize, le(&rec, 6, 2) as usize);
        for i in 1..count {
            let (end, fix) = (i * 512 - 2, usa + 2 * i);
            let word = [rec[fix], rec[fix + 1]];
            rec[end..end + 2].copy_from_slice(&word);
        }
        let mut roots = Vec::new();
        let mut a = le(&rec, 0x14, 2) as usize;
        while le(&rec, a, 4) != 0xffff_ffff {
            let len = le(&rec, a + 4, 4) as usize;
            let (name_len, name_at) = (rec[a + 9] as usize, a + le(&rec, a + 10, 2) as usize);
            let units: Vec<u16> = (0..name_len)
                .map(|i| le(&rec, name_at + 2 * i, 2) as u16)
                .collect();
            if le(&rec, a, 4) == INDEX_ROOT as u64 {
                let v = &rec[a + le(&rec, a + 0x14, 2) as usize..a + len];
                let mut e = 0x10 + le(v, 0x10, 4) as usize;
                let mut out = Vec::new();
                loop {
                    let (entry_len, flags) = (le(v, e + 8, 2) as usize, le(v, e + 12, 4));
                    out.push(v[e..e + entry_len].to_vec());
                    if flags & 2 != 0 {
                        break;
                    }
                    e += entry_len;
                }
                roots.push((String::from_utf16_lossy(&units), out));
            }
            a += len;
        }
        if roots.iter().any(|(name, _)| name == "$Q") {
            return roots
                .into_iter()
                .find(|(name, _)| name == index)
                .map(|(_, entries)| entries)
                .unwrap_or_else(|| panic!("$Quota has no index root named {index}"));
        }
    }
    panic!("no in-use record has a $Q index root");
}

/// A `$Q` entry with its change time cleared, and whether it was set.
fn without_change_time(entry: &[u8]) -> (Vec<u8>, bool) {
    let mut e = entry.to_vec();
    let data_len = le(&e, 2, 2) as usize;
    if data_len == 0 {
        return (e, true);
    }
    let at = le(&e, 0, 2) as usize;
    let range = at + CHANGE_TIME.start..at + CHANGE_TIME.end;
    let set = e[range.clone()].iter().any(|&b| b != 0);
    e[range].fill(0);
    (e, set)
}

fn quota_matches_windows(fixture: &str, cluster: u32) {
    let windows = unpack(fixture);
    let ours = formatted(cluster);

    let (win_o, our_o) = (root_entries(&windows, "$O"), root_entries(&ours, "$O"));
    assert_eq!(
        our_o.iter().map(|e| hex(e)).collect::<Vec<_>>(),
        win_o.iter().map(|e| hex(e)).collect::<Vec<_>>(),
        "$Quota:$O at {cluster}-byte clusters differs from {fixture}'s"
    );

    let win_q = root_entries(&windows, "$Q");
    let our_q = root_entries(&ours, "$Q");
    let strip = |q: &[Vec<u8>]| -> Vec<(String, bool)> {
        q.iter()
            .map(|e| {
                let (bytes, set) = without_change_time(e);
                (hex(&bytes), set)
            })
            .collect()
    };
    assert_eq!(
        strip(&our_q),
        strip(&win_q),
        "$Quota:$Q at {cluster}-byte clusters differs from {fixture}'s \
         (change times compared as set, not by value)"
    );
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn quota_at_4k_clusters_holds_what_windows_formats() {
    quota_matches_windows("windows-interrupted-1.recovered", 4096);
}

#[test]
fn quota_at_1k_clusters_holds_what_windows_formats() {
    quota_matches_windows("windows-interrupted-small-cluster.recovered", 1024);
}
