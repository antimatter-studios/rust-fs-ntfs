//! `$Extend\$Quota:$Q`, read: each owner's quota entry, with its usage,
//! warning limit, hard limit and exceeded time, and the quota control
//! entry, whose flags say whether the volume tracks and enforces quotas
//! (#868, part of #679).
//!
//! An entry's data, keyed by owner id (a little-endian u32):
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0x00 | 4 | version |
//! | 0x04 | 4 | flags |
//! | 0x08 | 8 | bytes used |
//! | 0x10 | 8 | change time (NT time) |
//! | 0x18 | 8 | warning limit (threshold) |
//! | 0x20 | 8 | hard limit |
//! | 0x28 | 8 | exceeded time (NT time) |
//! | 0x30 | ... | the owner's SID; absent from the control entry |
//!
//! Owner id 1 is the quota control entry: no file is charged to it, its
//! limits are the defaults a new owner gets, and its flags carry the
//! volume's quota state. A limit of `u64::MAX` is no limit. Checked against
//! Windows' own `fsutil quota query` for a volume it enforces quotas on
//! (`tests/quota_limits_windows_capture.rs`).

//! `$Extend\$Quota`'s two indexes, read whole (#1085, part of #849).
//!
//! `$O` maps each owner SID to the owner id files are charged to, and `$Q`
//! holds each owner id's quota record: its flags, the bytes its files are
//! charged, its limits and its SID. Owner id 1 is the quota control entry,
//! whose flags say whether the volume tracks quotas, and which has no SID.
//! The layouts are MS-DTYP's SID (2.4.2.2) and the Linux-NTFS
//! documentation's quota control entry; a matrix scenario reads what
//! Windows writes for an owner new to the volume through these.

use crate::block_io::BlockIo;
use crate::error::Error;
use crate::view_index;

/// The owner id of `$Q`'s quota control entry.
pub const CONTROL_OWNER: u32 = crate::write::QUOTA_CONTROL_OWNER;
/// The control entry's flag for a volume that tracks quotas.
pub const FLAG_TRACKING_ENABLED: u32 = crate::write::QUOTA_FLAG_TRACKING_ENABLED;
/// The index that maps owner SIDs to owner ids.
pub const OWNER_INDEX: &str = "$O";
/// A `$Q` record's fields before its SID.
const RECORD_FIXED: usize = 0x30;

/// One `$Q` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaRecord {
    pub version: u32,
    pub flags: u32,
    pub bytes_used: u64,
    pub change_time: u64,
    pub warning_limit: u64,
    pub hard_limit: u64,
    pub exceeded_time: u64,
    /// The owner's SID; `None` on the control entry, which has none.
    pub sid: Option<Vec<u8>>,
}

impl QuotaRecord {
    /// A record from a `$Q` entry's data.
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        if data.len() < RECORD_FIXED {
            return Err(Error::io(format!(
                "a $Q record of {} bytes, fewer than its {RECORD_FIXED} fixed ones",
                data.len()
            )));
        }
        let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        Ok(Self {
            version: u32_at(0x00),
            flags: u32_at(0x04),
            bytes_used: u64_at(0x08),
            change_time: u64_at(0x10),
            warning_limit: u64_at(0x18),
            hard_limit: u64_at(0x20),
            exceeded_time: u64_at(0x28),
            sid: (data.len() > RECORD_FIXED).then(|| data[RECORD_FIXED..].to_vec()),
        })
    }
}

/// One `$O` entry: an owner's SID and the id it is charged under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerEntry {
    pub sid: Vec<u8>,
    pub owner_id: u32,
}

/// Both indexes, each in its index order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaTables {
    pub owners: Vec<OwnerEntry>,
    pub records: Vec<(u32, QuotaRecord)>,
}

/// A SID in its string form, `S-1-5-21-...`: revision, the 48-bit
/// big-endian identifier authority, then each little-endian
/// sub-authority.
pub fn sid_string(sid: &[u8]) -> Result<String, Error> {
    let count = usize::from(
        *sid.get(1)
            .ok_or_else(|| Error::io("a SID with no header"))?,
    );
    if sid.len() != 8 + 4 * count {
        return Err(Error::io(format!(
            "a SID of {} bytes naming {count} sub-authorities",
            sid.len()
        )));
    }
    let authority = sid[2..8].iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
    let mut out = format!("S-{}-{authority}", sid[0]);
    for sub in sid[8..].chunks_exact(4) {
        out.push_str(&format!(
            "-{}",
            u32::from_le_bytes(sub.try_into().expect("four bytes"))
        ));
    }
    Ok(out)
}

/// Every `$O` entry and every `$Q` record of the volume's `$Extend\$Quota`.
pub fn tables_io<T: BlockIo + ?Sized>(io: &mut T) -> Result<QuotaTables, Error> {
    let path = view_index::QUOTA_PATH;
    let rec = crate::write::resolve_path_to_record_number_io(io, path)?;
    let mut owners = Vec::new();
    for (sid, _) in view_index::keys_io(io, rec, OWNER_INDEX)? {
        let data = view_index::data_io(io, rec, OWNER_INDEX, &sid)?
            .ok_or_else(|| Error::io(format!("{path}: an $O key with no entry")))?;
        let id = data
            .get(..4)
            .ok_or_else(|| Error::io(format!("{path}: an $O entry of {} bytes", data.len())))?;
        owners.push(OwnerEntry {
            sid,
            owner_id: u32::from_le_bytes(id.try_into().expect("four bytes")),
        });
    }
    let mut records = Vec::new();
    for (key, _) in view_index::keys_io(io, rec, view_index::QUOTA_INDEX)? {
        let id: [u8; 4] = key
            .as_slice()
            .try_into()
            .map_err(|_| Error::io(format!("{path}: a $Q key of {} bytes", key.len())))?;
        let data = view_index::data_io(io, rec, view_index::QUOTA_INDEX, &key)?
            .ok_or_else(|| Error::io(format!("{path}: a $Q key with no entry")))?;
        records.push((u32::from_le_bytes(id), QuotaRecord::parse(&data)?));
    }
    Ok(QuotaTables { owners, records })
}

/// The owner id the file at `path` is charged to and what it is charged,
/// from its `$STANDARD_INFORMATION`; `None` for the 48-byte form, which has
/// no quota fields.
pub fn charge_io<T: BlockIo + ?Sized>(io: &mut T, path: &str) -> Result<Option<(u32, u64)>, Error> {
    let rec = crate::write::resolve_path_to_record_number_io(io, path)?;
    let (_, record) = crate::mft_io::read_mft_record_io(io, rec)?;
    Ok(crate::write::quota_charge_of(&record))
}

use crate::view_index::{data_io, keys_io, QUOTA_INDEX, QUOTA_PATH};
use crate::write::resolve_path_to_record_number_io;

/// The owner id of `$Q`'s quota control entry.
pub const QUOTA_CONTROL_OWNER: u32 = 1;
/// In the control entry's flags: Windows charges each file to its owner.
pub const QUOTA_FLAG_TRACKING_ENABLED: u32 = 0x10;
/// In the control entry's flags: Windows refuses a write that would take
/// an owner past its hard limit.
pub const QUOTA_FLAG_ENFORCEMENT_ENABLED: u32 = 0x20;

const ENTRY_HEADER: usize = 0x30;

/// One `$Q` entry, as the volume holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaEntry {
    /// The key: the owner id files' `$STANDARD_INFORMATION` names.
    pub owner_id: u32,
    pub version: u32,
    pub flags: u32,
    /// What the owner's files are charged, in bytes.
    pub bytes_used: u64,
    pub change_time: u64,
    /// The warning limit (Windows' "threshold"), in bytes.
    pub warning_limit: u64,
    /// The hard limit, in bytes.
    pub hard_limit: u64,
    /// When the owner went past its warning limit; 0 when it has not.
    pub exceeded_time: u64,
    /// The owner's SID, as stored; empty for the control entry.
    pub sid: Vec<u8>,
}

impl QuotaEntry {
    /// The entry of owner `owner_id` from its `$Q` data. Fails on data
    /// shorter than the fixed fields.
    pub fn parse(owner_id: u32, data: &[u8]) -> Result<Self, Error> {
        if data.len() < ENTRY_HEADER {
            return Err(Error::io(format!(
                "{QUOTA_PATH}: owner {owner_id}'s $Q entry holds {} bytes, fewer than the \
                 {ENTRY_HEADER} of its fixed fields",
                data.len()
            )));
        }
        let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        Ok(Self {
            owner_id,
            version: u32_at(0x00),
            flags: u32_at(0x04),
            bytes_used: u64_at(0x08),
            change_time: u64_at(0x10),
            warning_limit: u64_at(0x18),
            hard_limit: u64_at(0x20),
            exceeded_time: u64_at(0x28),
            sid: data[ENTRY_HEADER..].to_vec(),
        })
    }

    /// Whether this is the quota control entry.
    pub fn is_control(&self) -> bool {
        self.owner_id == QUOTA_CONTROL_OWNER
    }

    /// For the control entry: whether the volume tracks quotas.
    pub fn tracking_enabled(&self) -> bool {
        self.flags & QUOTA_FLAG_TRACKING_ENABLED != 0
    }

    /// For the control entry: whether the volume enforces quota limits.
    pub fn enforcement_enabled(&self) -> bool {
        self.flags & QUOTA_FLAG_ENFORCEMENT_ENABLED != 0
    }
}

/// Every `$Q` entry of the volume, the control entry included, in owner id
/// order. Fails with [`crate::error::Kind::NotFound`] on a volume with no
/// `$Extend\$Quota`.
pub fn entries_io<T: BlockIo + ?Sized>(io: &mut T) -> Result<Vec<QuotaEntry>, Error> {
    let rec = resolve_path_to_record_number_io(io, QUOTA_PATH)?;
    let mut out = Vec::new();
    for (key, _) in keys_io(io, rec, QUOTA_INDEX).map_err(|e| e.context(QUOTA_PATH))? {
        let owner_id =
            u32::from_le_bytes(key.as_slice().try_into().map_err(|_| {
                Error::io(format!("{QUOTA_PATH}: a $Q key of {} bytes", key.len()))
            })?);
        let data = data_io(io, rec, QUOTA_INDEX, &key)
            .map_err(|e| e.context(QUOTA_PATH))?
            .ok_or_else(|| Error::io(format!("{QUOTA_PATH}: owner {owner_id}'s entry is gone")))?;
        out.push(QuotaEntry::parse(owner_id, &data)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_each_field_at_its_offset() {
        let mut data = vec![0u8; ENTRY_HEADER];
        for (at, n) in [
            (0x00, 4),
            (0x04, 4),
            (0x08, 8),
            (0x10, 8),
            (0x18, 8),
            (0x20, 8),
            (0x28, 8),
        ] {
            for i in 0..n {
                data[at + i] = (at + i) as u8 + 1;
            }
        }
        data.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 5, 19, 0, 0, 0]);
        let e = QuotaEntry::parse(0x101, &data).unwrap();
        let u64_at = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        assert_eq!(e.owner_id, 0x101);
        assert_eq!(e.version, 0x0403_0201);
        assert_eq!(e.flags, 0x0807_0605);
        assert_eq!(e.bytes_used, u64_at(0x08));
        assert_eq!(e.change_time, u64_at(0x10));
        assert_eq!(e.warning_limit, u64_at(0x18));
        assert_eq!(e.hard_limit, u64_at(0x20));
        assert_eq!(e.exceeded_time, u64_at(0x28));
        assert_eq!(e.sid, &data[ENTRY_HEADER..]);
        assert!(!e.is_control());
    }

    #[test]
    fn parse_refuses_data_shorter_than_the_fixed_fields() {
        assert!(QuotaEntry::parse(1, &[0u8; ENTRY_HEADER - 1]).is_err());
    }
}
