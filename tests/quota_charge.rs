//! On a volume that tracks quotas, a file that goes takes its charge off
//! its owner (#674, part of #510).
//!
//! The volume is `windows-interrupted-quota.recovered`, which Windows wrote
//! with quota tracking on (`fsutil quota track`). A file's
//! `$STANDARD_INFORMATION` names the owner it is charged to (`owner_id`, at
//! 0x30) and what it is charged (`quota_charged`, at 0x38), and
//! `$Extend\$Quota:$Q` holds each owner's usage, keyed by owner id.
//! Measured on that volume, every one of its 709 files with an owner is
//! charged its MFT record plus the allocated size of its non-resident
//! `$DATA`: resident attributes and a directory's index blocks are charged
//! nothing beyond the record. Each owner's usage is the sum of what its
//! files are charged.
//!
//! Unlinking a file's last name, removing a directory and renaming over a
//! file each free a charged record, and have to take its charge off the
//! owner's usage, as Windows does, or the owner stays charged for a file
//! that is gone. The volume is read back by a raw reader written for this
//! test, not by this crate. The Windows matrix scenario
//! `win-format-win-quota-mac-delete-win-chkdsk` has chkdsk and Windows' own
//! recount of its quota usage grade the same deletes on a volume Windows
//! made.
//!
//! Rewriting, truncating or extending a charged file changes its
//! allocation, and has to move its charge and its owner's usage by the
//! difference, as Windows does (#675). The Windows matrix scenario
//! `win-format-win-quota-mac-write-win-chkdsk` grades the same kinds of
//! write on a volume Windows made.
//!
//! A file or directory created in a charged directory is charged its record
//! to that directory's owner, and the owner's usage grows by the charge
//! (#848, part of #676). The Windows matrix scenario
//! `win-format-win-quota-mac-create-win-chkdsk` grades the same creates on a
//! volume Windows made.

mod common;

use fs_ntfs::block_io::PathIo;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

/// Written by Windows with quota tracking on.
const TRACKED: &str = "windows-interrupted-quota.recovered";

const STANDARD_INFORMATION: u32 = 0x10;
const ATTRIBUTE_LIST: u32 = 0x20;
const FILE_NAME: u32 = 0x30;
const DATA: u32 = 0x80;
const INDEX_ROOT: u32 = 0x90;

/// `$Q`'s quota control entry, which no file is charged to.
const CONTROL_ID: u32 = 1;
/// `$Extend`'s record, the parent of `$Quota`.
const EXTEND_RECORD: u64 = 11;

fn unpack(name: &str, tag: &str) -> String {
    let path = common::temp_image_path(format!("quota_charge_{tag}"));
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
    let mut v = 0u64;
    for i in (0..n).rev() {
        v = (v << 8) | u64::from(b[at + i]);
    }
    v
}

/// Fixups undone, or `None` for a record that is not a FILE record or is
/// torn.
fn fixed(raw: &[u8]) -> Option<Vec<u8>> {
    if &raw[..4] != b"FILE" {
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

struct Attr {
    at: usize,
    ty: u32,
    non_resident: bool,
    name: String,
}

/// The attributes of a record, fixups undone.
fn attrs(rec: &[u8]) -> Vec<Attr> {
    let mut at = le(rec, 0x14, 2) as usize;
    let mut out = Vec::new();
    while at + 8 <= rec.len() {
        let ty = le(rec, at, 4) as u32;
        let len = le(rec, at + 4, 4) as usize;
        if ty == 0xFFFF_FFFF || len == 0 || at + len > rec.len() {
            break;
        }
        let (name_len, name_at) = (rec[at + 9] as usize, at + le(rec, at + 10, 2) as usize);
        let units: Vec<u16> = (0..name_len)
            .map(|i| le(rec, name_at + 2 * i, 2) as u16)
            .collect();
        out.push(Attr {
            at,
            ty,
            non_resident: rec[at + 8] != 0,
            name: String::from_utf16_lossy(&units),
        });
        at += len;
    }
    out
}

/// A resident attribute's value.
fn value<'r>(rec: &'r [u8], a: &Attr) -> &'r [u8] {
    let start = a.at + le(rec, a.at + 0x14, 2) as usize;
    &rec[start..start + le(rec, a.at + 0x10, 4) as usize]
}

/// A raw reading of an NTFS image, written for this test from the boot
/// sector, `$MFT`'s runs and update-sequence fixups, and nothing in this
/// crate.
struct Vol {
    bytes: Vec<u8>,
    cluster: u64,
    record: u64,
    mft_runs: Vec<(u64, u64)>,
}

impl Vol {
    fn read(path: &str) -> Vol {
        let bytes = std::fs::read(path).unwrap();
        let cluster = le(&bytes, 0x0B, 2) * le(&bytes, 0x0D, 1);
        let c = bytes[0x40] as i8;
        let record = if c > 0 { c as u64 * cluster } else { 1 << -c };
        let at = (le(&bytes, 0x30, 8) * cluster) as usize;
        let rec0 = fixed(&bytes[at..at + record as usize]).expect("$MFT's own record");
        let data = attrs(&rec0)
            .into_iter()
            .find(|a| a.ty == DATA && a.non_resident)
            .expect("$MFT's $DATA");
        let mut run = data.at + le(&rec0, data.at + 0x20, 2) as usize;
        let mut lcn = 0i64;
        let mut mft_runs = Vec::new();
        while rec0[run] != 0 {
            let (ln, on) = ((rec0[run] & 15) as usize, (rec0[run] >> 4) as usize);
            let len = le(&rec0, run + 1, ln);
            let raw = le(&rec0, run + 1 + ln, on);
            let shift = 64 - 8 * on as u32;
            lcn += ((raw << shift) as i64) >> shift;
            mft_runs.push((lcn as u64, len));
            run += 1 + ln + on;
        }
        Vol {
            bytes,
            cluster,
            record,
            mft_runs,
        }
    }

    fn records(&self) -> u64 {
        self.mft_runs.iter().map(|r| r.1).sum::<u64>() * self.cluster / self.record
    }

    /// Record `n`, fixups undone, when it is in use.
    fn record(&self, n: u64) -> Option<Vec<u8>> {
        let mut off = n * self.record;
        for &(lcn, len) in &self.mft_runs {
            if off < len * self.cluster {
                let at = (lcn * self.cluster + off) as usize;
                let rec = fixed(&self.bytes[at..at + self.record as usize])?;
                return (le(&rec, 0x16, 2) & 1 != 0).then_some(rec);
            }
            off -= len * self.cluster;
        }
        None
    }

    /// Every in-use base record, by number.
    fn base_records(&self) -> impl Iterator<Item = (u64, Vec<u8>)> + '_ {
        (0..self.records()).filter_map(|n| {
            let rec = self.record(n)?;
            (le(&rec, 0x20, 6) == 0).then_some((n, rec))
        })
    }

    /// `$Extend\$Quota`'s record: the one named `$Quota` under `$Extend`.
    fn quota_record(&self) -> Vec<u8> {
        self.base_records()
            .map(|(_, rec)| rec)
            .find(|rec| {
                attrs(rec).iter().filter(|a| a.ty == FILE_NAME).any(|a| {
                    let v = value(rec, a);
                    let units: Vec<u16> = (0..v[0x40] as usize)
                        .map(|i| le(v, 0x42 + 2 * i, 2) as u16)
                        .collect();
                    le(v, 0, 6) == EXTEND_RECORD && String::from_utf16_lossy(&units) == "$Quota"
                })
            })
            .expect("$Extend\\$Quota")
    }
}

/// The owner a record is charged to and what it is charged, from its
/// `$STANDARD_INFORMATION`; `None` for the short form, which has no quota
/// fields.
fn charge_of(rec: &[u8]) -> Option<(u32, u64)> {
    let a = attrs(rec)
        .into_iter()
        .find(|a| a.ty == STANDARD_INFORMATION && !a.non_resident)?;
    let v = value(rec, &a);
    (v.len() >= 0x40).then(|| (le(v, 0x30, 4) as u32, le(v, 0x38, 8)))
}

/// [`charge_of`] for a record that has the quota fields.
fn charge(rec: &[u8]) -> (u32, u64) {
    charge_of(rec).expect("a $STANDARD_INFORMATION with quota fields")
}

/// The allocated size of the record's non-resident `$DATA` streams, each
/// read from its first extent.
fn data_allocated(rec: &[u8]) -> u64 {
    attrs(rec)
        .iter()
        .filter(|a| a.ty == DATA && a.non_resident && le(rec, a.at + 0x10, 8) == 0)
        .map(|a| le(rec, a.at + 0x28, 8))
        .sum()
}

/// What the files charged to each owner add up to.
fn charged(vol: &Vol) -> BTreeMap<u32, u64> {
    let mut out = BTreeMap::new();
    for (_, rec) in vol.base_records() {
        if let Some((owner, qc)) = charge_of(&rec).filter(|c| c.0 != 0) {
            *out.entry(owner).or_default() += qc;
        }
    }
    out
}

/// Each owner's usage in `$Q`, the control entry left out, and the
/// control entry's flags.
fn usage(vol: &Vol) -> (BTreeMap<u32, u64>, u32) {
    let rec = vol.quota_record();
    let a = attrs(&rec)
        .into_iter()
        .find(|a| a.ty == INDEX_ROOT && a.name == "$Q")
        .expect("$Quota's $Q");
    let v = value(&rec, &a);
    let h = 0x10;
    assert_eq!(
        v[h + 0x0C] & 1,
        0,
        "$Q has spilled into index blocks; this reader reads only its root"
    );
    let (mut at, end) = (h + le(v, h, 4) as usize, h + le(v, h + 4, 4) as usize);
    let mut out = BTreeMap::new();
    let mut control = None;
    while at < end && le(v, at + 12, 2) & 2 == 0 {
        let id = le(v, at + 16, 4) as u32;
        let data = at + le(v, at, 2) as usize;
        if id == CONTROL_ID {
            control = Some(le(v, data + 4, 4) as u32);
        } else {
            out.insert(id, le(v, data + 8, 8));
        }
        at += le(v, at + 8, 2) as usize;
    }
    (out, control.expect("$Q's control entry"))
}

fn record_of(img: &str, path: &str) -> u64 {
    let mut io = PathIo::open_ro(Path::new(img)).unwrap();
    fs_ntfs::read::resolve_path(&mut io, path).unwrap_or_else(|e| panic!("resolve {path}: {e}"))
}

/// The files of `name`'s manifest that Windows charged for non-resident
/// data, in the manifest's order.
fn charged_files(img: &str, name: &str) -> Vec<String> {
    let vol = Vol::read(img);
    std::fs::read_to_string(format!("test-disks/{name}.manifest"))
        .expect("read manifest")
        .lines()
        .map(|line| format!("/{}", line.split('\t').next().unwrap()))
        .filter(|path| {
            let rec = vol.record(record_of(img, path)).unwrap();
            charge(&rec).0 != 0 && data_allocated(&rec) > 0
        })
        .collect()
}

/// `path` is charged its record and the allocation of its data, and every
/// owner's usage is what its files are charged.
fn assert_in_step(img: &str, path: &str, what: &str) {
    let vol = Vol::read(img);
    let rec = vol.record(record_of(img, path)).unwrap();
    let (owner, qc) = charge(&rec);
    assert_ne!(owner, 0, "{what}: {path} lost its owner");
    assert_eq!(
        qc,
        vol.record + data_allocated(&rec),
        "{what}: {path} is charged {qc}, not its {}-byte record and the {} bytes its data \
         has allocated",
        vol.record,
        data_allocated(&rec)
    );
    assert_eq!(
        usage(&vol).0,
        charged(&vol),
        "{what}: an owner's usage in $Q is not what its files are charged"
    );
}

#[test]
fn windows_charges_each_owner_the_record_and_data_allocation_of_its_files() {
    let img = unpack(TRACKED, "measure");
    let vol = Vol::read(&img);
    let (used, flags) = usage(&vol);
    assert_eq!(
        flags & 0x10,
        0x10,
        "{TRACKED}'s quota control entry does not say tracking is on: {flags:#x}"
    );
    let mut owned = 0;
    for (n, rec) in vol.base_records() {
        let Some((_owner, qc)) = charge_of(&rec).filter(|c| c.0 != 0) else {
            continue;
        };
        assert!(
            !attrs(&rec).iter().any(|a| a.ty == ATTRIBUTE_LIST),
            "record {n} has an $ATTRIBUTE_LIST, a shape this volume was not measured with"
        );
        owned += 1;
        assert_eq!(
            qc,
            vol.record + data_allocated(&rec),
            "record {n}: Windows charged {qc}"
        );
    }
    assert!(
        owned >= 700,
        "only {owned} files have an owner: not the volume this was measured on"
    );
    assert_eq!(
        used,
        charged(&vol),
        "Windows' $Q does not hold what its files are charged"
    );
}

#[test]
fn unlinking_a_charged_file_refunds_its_owner() {
    let img = unpack(TRACKED, "unlink");
    let path = charged_files(&img, TRACKED).swap_remove(0);
    let vol = Vol::read(&img);
    let (owner, qc) = charge(&vol.record(record_of(&img, &path)).unwrap());
    let before = usage(&vol).0[&owner];

    fs_ntfs::write::unlink(Path::new(&img), &path).unwrap_or_else(|e| panic!("unlink {path}: {e}"));

    let vol = Vol::read(&img);
    assert_eq!(
        usage(&vol).0[&owner],
        before - qc,
        "unlinking {path} left owner {owner} charged for its {qc} bytes"
    );
    assert_eq!(
        usage(&vol).0,
        charged(&vol),
        "an owner's usage in $Q is not what its files are charged"
    );
}

#[test]
fn removing_a_charged_directory_refunds_its_owner() {
    // An empty directory Windows made, charged its record.
    let img = unpack(TRACKED, "rmdir");
    let path = "/junction-target";
    let vol = Vol::read(&img);
    let (owner, qc) = charge(&vol.record(record_of(&img, path)).unwrap());
    assert_ne!(owner, 0, "{path} is charged to no owner");
    let before = usage(&vol).0[&owner];

    fs_ntfs::write::rmdir(Path::new(&img), path).unwrap_or_else(|e| panic!("rmdir {path}: {e}"));

    let vol = Vol::read(&img);
    assert_eq!(
        usage(&vol).0[&owner],
        before - qc,
        "removing {path} left owner {owner} charged for its {qc} bytes"
    );
    assert_eq!(usage(&vol).0, charged(&vol));
}

#[test]
fn renaming_over_a_charged_file_refunds_the_owner_of_the_file_replaced() {
    // Two charged files in one directory whose names are the same length,
    // so the rename stays in place in a directory whose index has spilled.
    let img = unpack(TRACKED, "rename");
    let files = charged_files(&img, TRACKED);
    let (from, onto) = files
        .iter()
        .enumerate()
        .find_map(|(i, a)| {
            let (dir, name) = a.rsplit_once('/').unwrap();
            files[i + 1..].iter().find_map(|b| {
                let (d, n) = b.rsplit_once('/').unwrap();
                (d == dir && n.len() == name.len()).then(|| (a.clone(), n.to_string()))
            })
        })
        .expect("two charged files with names of one length in one directory");
    let target = format!("{}/{onto}", from.rsplit_once('/').unwrap().0);
    let vol = Vol::read(&img);
    let (owner, qc) = charge(&vol.record(record_of(&img, &target)).unwrap());
    let before = usage(&vol).0[&owner];

    let mut io = PathIo::open_rw(Path::new(&img)).unwrap();
    fs_ntfs::write::rename_replace_io(&mut io, &from, &onto, true)
        .unwrap_or_else(|e| panic!("rename {from} over {target}: {e}"));
    drop(io);

    let vol = Vol::read(&img);
    assert_eq!(
        usage(&vol).0[&owner],
        before - qc,
        "renaming {from} over {target} left owner {owner} charged for its {qc} bytes"
    );
    assert_eq!(usage(&vol).0, charged(&vol));
}

#[test]
fn unlinking_one_of_two_names_leaves_the_charge_where_it_is() {
    // The file stays, so its owner stays charged for it.
    let img = unpack(TRACKED, "link");
    let path = charged_files(&img, TRACKED).swap_remove(3);
    let (dir, name) = path.rsplit_once('/').unwrap();
    let alias = format!("{name}.lnk");
    fs_ntfs::write::link(Path::new(&img), &path, dir, &alias)
        .unwrap_or_else(|e| panic!("link {path} as {alias}: {e}"));
    let before = usage(&Vol::read(&img)).0;

    fs_ntfs::write::unlink(Path::new(&img), &format!("{dir}/{alias}"))
        .unwrap_or_else(|e| panic!("unlink {dir}/{alias}: {e}"));

    let vol = Vol::read(&img);
    assert_eq!(
        usage(&vol).0,
        before,
        "unlinking one of {path}'s two names changed what its owner is charged"
    );
    assert_eq!(usage(&vol).0, charged(&vol));
}

#[test]
fn rewriting_a_charged_file_charges_its_owner_what_its_allocation_gains_or_loses() {
    let img = unpack(TRACKED, "rewrite");
    let path = charged_files(&img, TRACKED).swap_remove(1);
    for size in [200_000usize, 5_000, 100] {
        fs_ntfs::write::write_file_contents(Path::new(&img), &path, &vec![0x5A; size])
            .unwrap_or_else(|e| panic!("write {size} bytes to {path}: {e}"));
        assert_in_step(&img, &path, &format!("rewritten to {size} bytes"));
    }
}

#[test]
fn truncating_and_extending_a_charged_file_moves_its_charge() {
    let img = unpack(TRACKED, "truncate");
    let path = charged_files(&img, TRACKED).swap_remove(2);

    fs_ntfs::write::truncate(Path::new(&img), &path, 1)
        .unwrap_or_else(|e| panic!("truncate {path}: {e}"));
    assert_in_step(&img, &path, "truncated to 1 byte");

    fs_ntfs::write::write_at(Path::new(&img), &path, 300_000, b"past the end")
        .unwrap_or_else(|e| panic!("write past the end of {path}: {e}"));
    assert_in_step(&img, &path, "extended past 300000 bytes");
}

#[test]
fn a_file_or_directory_created_in_a_charged_directory_is_charged_to_its_owner() {
    let img = unpack(TRACKED, "create");
    let parent = "/junction-target";
    let vol = Vol::read(&img);
    let (owner, _) = charge(&vol.record(record_of(&img, parent)).unwrap());
    assert_ne!(owner, 0, "{parent} is charged to no owner");
    let before = usage(&vol).0[&owner];

    fs_ntfs::write::create_file(Path::new(&img), parent, "created.txt")
        .unwrap_or_else(|e| panic!("create {parent}/created.txt: {e}"));
    fs_ntfs::write::mkdir(Path::new(&img), parent, "created-dir")
        .unwrap_or_else(|e| panic!("mkdir {parent}/created-dir: {e}"));

    let vol = Vol::read(&img);
    for name in ["created.txt", "created-dir"] {
        let path = format!("{parent}/{name}");
        let (got, qc) = charge(&vol.record(record_of(&img, &path)).unwrap());
        assert_eq!(
            got, owner,
            "{path} is charged to owner {got}, not {parent}'s {owner}"
        );
        assert_eq!(
            qc, vol.record,
            "{path} is charged {qc}, not its {}-byte record",
            vol.record
        );
    }
    assert_eq!(
        usage(&vol).0[&owner],
        before + 2 * vol.record,
        "owner {owner}'s usage did not grow by the two records created"
    );
    assert_eq!(usage(&vol).0, charged(&vol));
}
