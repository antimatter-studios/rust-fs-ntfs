//! `$Extend\$Quota`'s two indexes read whole (#1085): `$O`, each owner SID
//! to its owner id, and `$Q`, each owner id's quota record. The decoding is
//! pinned on bytes built from the published layouts (MS-DTYP's SID; the
//! Linux-NTFS documentation's quota control entry), and the reader is
//! cross-checked on a volume Windows wrote with tracking on, whose two
//! indexes must name the same owners as each other.

mod common;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::quota::{self, QuotaRecord};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

/// A SID's bytes: revision 1, `authority`, then each sub-authority.
fn sid(authority: u64, subs: &[u32]) -> Vec<u8> {
    let mut out = vec![1, subs.len() as u8];
    out.extend_from_slice(&authority.to_be_bytes()[2..]);
    for s in subs {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// A `$Q` record's bytes: version, flags, bytes used, change time, warning
/// and hard limits, exceeded time, then the SID if any.
fn record(flags: u32, used: u64, owner: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&used.to_le_bytes());
    out.extend_from_slice(&0x01d9_0000_0000_0000u64.to_le_bytes());
    out.extend_from_slice(&u64::MAX.to_le_bytes());
    out.extend_from_slice(&u64::MAX.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    if let Some(owner) = owner {
        out.extend_from_slice(owner);
    }
    out
}

#[test]
fn a_sid_reads_as_its_string_form() {
    assert_eq!(quota::sid_string(&sid(5, &[20])).unwrap(), "S-1-5-20");
    assert_eq!(
        quota::sid_string(&sid(5, &[21, 1000, 2000, 3000, 4242])).unwrap(),
        "S-1-5-21-1000-2000-3000-4242"
    );
    assert_eq!(
        quota::sid_string(&sid(5, &[32, 544])).unwrap(),
        "S-1-5-32-544"
    );
    let mut short = sid(5, &[20]);
    short.pop();
    assert!(
        quota::sid_string(&short).is_err(),
        "a SID shorter than its count"
    );
    assert!(quota::sid_string(&[1]).is_err(), "a SID with no header");
}

#[test]
fn a_quota_record_reads_its_fields_and_its_sid() {
    let owner = sid(5, &[21, 1000, 2000, 3000, 4242]);
    let parsed = QuotaRecord::parse(&record(0x1, 81920, Some(&owner))).unwrap();
    assert_eq!(parsed.version, 2);
    assert_eq!(parsed.flags, 0x1);
    assert_eq!(parsed.bytes_used, 81920);
    assert_eq!(parsed.change_time, 0x01d9_0000_0000_0000);
    assert_eq!(parsed.warning_limit, u64::MAX);
    assert_eq!(parsed.hard_limit, u64::MAX);
    assert_eq!(parsed.exceeded_time, 0);
    assert_eq!(parsed.sid.as_deref(), Some(&owner[..]));
    // The control entry carries no SID.
    let control = QuotaRecord::parse(&record(0x10, 0, None)).unwrap();
    assert_eq!(control.flags, 0x10);
    assert_eq!(control.sid, None);
    assert!(
        QuotaRecord::parse(&record(0, 0, None)[..47]).is_err(),
        "a record shorter than its fixed fields"
    );
}

#[test]
fn windows_quota_indexes_name_the_same_owners() {
    let img = common::temp_image_path("quota_tables_tracked");
    let out = File::create(&img).expect("create image copy");
    let status = Command::new("gzip")
        .args([
            "-dc",
            "test-disks/windows-interrupted-quota.recovered.img.gz",
        ])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress the quota fixture");
    let mut io = PathIo::open_ro(Path::new(&img)).expect("open the fixture");
    let tables = quota::tables_io(&mut io).expect("read $Quota's indexes");

    let control = tables
        .records
        .iter()
        .find(|(id, _)| *id == quota::CONTROL_OWNER)
        .map(|(_, r)| r)
        .expect("$Q holds the quota control entry");
    assert_ne!(
        control.flags & quota::FLAG_TRACKING_ENABLED,
        0,
        "Windows tracks quotas on this volume"
    );
    let owners: Vec<_> = tables
        .records
        .iter()
        .filter(|(id, _)| *id != quota::CONTROL_OWNER)
        .collect();
    assert!(!owners.is_empty(), "Windows charged some owner");
    for (id, record) in &owners {
        let sid = record
            .sid
            .as_ref()
            .unwrap_or_else(|| panic!("owner {id}'s $Q record has no SID"));
        let mapped: Vec<_> = tables.owners.iter().filter(|o| &o.sid == sid).collect();
        assert_eq!(
            mapped.len(),
            1,
            "$O maps {} once",
            quota::sid_string(sid).unwrap()
        );
        assert_eq!(
            mapped[0].owner_id, *id,
            "$O and $Q disagree on the owner id"
        );
    }
    assert_eq!(
        tables.owners.len(),
        owners.len(),
        "$O holds an owner with no $Q record"
    );

    // A file Windows wrote is charged to an owner that $Q holds.
    let manifest =
        std::fs::read_to_string("test-disks/windows-interrupted-quota.recovered.manifest").unwrap();
    let first = manifest
        .lines()
        .next()
        .and_then(|l| l.split('\t').next())
        .unwrap();
    let path = format!("/{first}");
    let (owner, charged) = quota::charge_io(&mut io, &path)
        .expect("read the file's charge")
        .expect("the file has quota fields");
    assert!(charged > 0, "{path} is charged nothing");
    assert!(
        owners.iter().any(|(id, _)| *id == owner),
        "{path} is charged to owner {owner}, whom $Q does not hold"
    );
    drop(io);
    let _ = std::fs::remove_file(&img);
}
