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

use crate::block_io::BlockIo;
use crate::error::Error;
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
