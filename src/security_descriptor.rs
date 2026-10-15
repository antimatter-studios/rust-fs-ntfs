//! Bounded, read-only SELF_RELATIVE descriptor, SID and ACL framing views.
//!
//! Layout sources: Microsoft [MS-DTYP] 2.4.6 and 2.4.2.2:
//! <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/7d4dac05-9cef-4563-a058-f108abecce1d>.
//! SID layout: <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/f992ad60-0fe4-4b87-9fed-beb478836861>.
//! ACL headers and ACE framing are bounded by AclSize. ACE meaning (masks,
//! SIDs, callback data) and absent/null/empty ACL semantics are deliberately NOT
//! decoded. `as_bytes` preserves all bytes for later decoders.

use std::fmt;

/// Default input resource bound; callers may choose a different explicit bound.
pub const DEFAULT_MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;
const HEADER_BYTES: usize = 20;

/// An error location is a byte offset relative to the descriptor start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    pub offset: usize,
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Truncated,
    ResourceLimit,
    DescriptorRevision,
    NotSelfRelative,
    RelativeOffset,
    AclOffsetWithoutPresentBit,
    SidRevision,
    SidSubauthorityCount,
    AclSize,
    LengthOverflow,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "security descriptor byte {}: {:?}",
            self.offset, self.kind
        )
    }
}
impl std::error::Error for DecodeError {}

fn error(offset: usize, kind: ErrorKind) -> DecodeError {
    DecodeError { offset, kind }
}

fn bounded(bytes: &[u8], offset: usize, len: usize) -> Result<&[u8], DecodeError> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| error(offset, ErrorKind::LengthOverflow))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| error(offset.min(bytes.len()), ErrorKind::Truncated))
}

/// A validated SID, borrowing exactly its encoded length (at most 68 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sid<'a> {
    bytes: &'a [u8],
}

impl<'a> Sid<'a> {
    fn at(bytes: &'a [u8], offset: usize) -> Result<Self, DecodeError> {
        let header = bounded(bytes, offset, 8)?;
        if header[0] != 1 {
            return Err(error(offset, ErrorKind::SidRevision));
        }
        let count = usize::from(header[1]);
        if count > 15 {
            return Err(error(offset + 1, ErrorKind::SidSubauthorityCount));
        }
        let len = count
            .checked_mul(4)
            .and_then(|n| n.checked_add(8))
            .ok_or_else(|| error(offset + 1, ErrorKind::LengthOverflow))?;
        Ok(Self {
            bytes: bounded(bytes, offset, len)?,
        })
    }

    pub fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }

    pub fn revision(self) -> u8 {
        self.bytes[0]
    }

    /// All six authority bytes in network order, without narrowing to u32.
    pub fn identifier_authority(self) -> u64 {
        self.bytes[2..8]
            .iter()
            .fold(0, |authority, byte| (authority << 8) | u64::from(*byte))
    }

    pub fn subauthorities(self) -> impl ExactSizeIterator<Item = u32> + 'a {
        self.bytes[8..]
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// A validated ACL, borrowing exactly its declared AclSize bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acl<'a> {
    bytes: &'a [u8],
}

impl<'a> Acl<'a> {
    fn at(bytes: &'a [u8], offset: usize) -> Result<Self, DecodeError> {
        let h = bounded(bytes, offset, 8)?;
        let size = usize::from(u16::from_le_bytes([h[2], h[3]]));
        let count = usize::from(u16::from_le_bytes([h[4], h[5]]));
        if size < 8 {
            return Err(error(offset + 2, ErrorKind::AclSize));
        }
        let acl = Self {
            bytes: bounded(bytes, offset, size)?,
        };
        // Every ACE header and its declared size must lie inside AclSize.
        let mut at = 8;
        for _ in 0..count {
            if at + 4 > size {
                return Err(error(offset + at, ErrorKind::AclSize));
            }
            let ace_size = usize::from(u16::from_le_bytes([acl.bytes[at + 2], acl.bytes[at + 3]]));
            if ace_size < 4 || at + ace_size > size {
                return Err(error(offset + at + 2, ErrorKind::AclSize));
            }
            at += ace_size;
        }
        Ok(acl)
    }

    pub fn revision(self) -> u8 {
        self.bytes[0]
    }

    pub fn ace_count(self) -> u16 {
        u16::from_le_bytes([self.bytes[4], self.bytes[5]])
    }

    pub fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// ACEs in stored order. Framing was validated by `SecurityDescriptor::parse`.
    pub fn aces(self) -> impl Iterator<Item = Ace<'a>> + 'a {
        let bytes = self.bytes;
        let mut at = 8;
        (0..self.ace_count()).map(move |_| {
            let size = usize::from(u16::from_le_bytes([bytes[at + 2], bytes[at + 3]]));
            let ace = Ace {
                ace_type: bytes[at],
                flags: bytes[at + 1],
                bytes: &bytes[at..at + size],
            };
            at += size;
            ace
        })
    }
}

/// One ACE: type and flags are raw; the payload bytes are preserved unmodified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ace<'a> {
    pub ace_type: u8,
    pub flags: u8,
    bytes: &'a [u8],
}

impl<'a> Ace<'a> {
    pub fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }
}

/// Validated header fields. Control is raw, including all reserved/unknown bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub revision: u8,
    /// Preserved even when SE_RM_CONTROL_VALID is clear.
    pub resource_manager_control: u8,
    pub control: u16,
    pub owner_offset: u32,
    pub group_offset: u32,
    pub sacl_offset: u32,
    pub dacl_offset: u32,
}

/// Header, optional owner/group SIDs and validated ACL/ACE framing.
/// Success does not evaluate ACE payload semantics or authorization policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityDescriptor<'a> {
    bytes: &'a [u8],
    pub header: Header,
    pub owner: Option<Sid<'a>>,
    pub group: Option<Sid<'a>>,
    sacl: Option<Acl<'a>>,
    dacl: Option<Acl<'a>>,
}

impl<'a> SecurityDescriptor<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        Self::parse_with_limit(bytes, DEFAULT_MAX_DESCRIPTOR_BYTES)
    }

    /// No allocation or scan proportional to an untrusted on-disk length.
    /// Targets are decoded independently: field order, padding and shared SID
    /// storage are not constrained by a presumed serialized component order.
    pub fn parse_with_limit(bytes: &'a [u8], max_bytes: usize) -> Result<Self, DecodeError> {
        if bytes.len() > max_bytes {
            return Err(error(0, ErrorKind::ResourceLimit));
        }
        let h = bounded(bytes, 0, HEADER_BYTES)?;
        if h[0] != 1 {
            return Err(error(0, ErrorKind::DescriptorRevision));
        }
        let control = u16::from_le_bytes([h[2], h[3]]);
        if control & 0x8000 == 0 {
            return Err(error(2, ErrorKind::NotSelfRelative));
        }
        let mut offsets = [0u32; 4];
        for (i, slot) in offsets.iter_mut().enumerate() {
            let at = 4 + i * 4;
            *slot = u32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]);
            if *slot == 0 {
                continue;
            }
            let offset =
                usize::try_from(*slot).map_err(|_| error(at, ErrorKind::RelativeOffset))?;
            if offset < HEADER_BYTES || offset >= bytes.len() {
                return Err(error(at, ErrorKind::RelativeOffset));
            }
            if i >= 2 {
                let present = if i == 2 { 0x10 } else { 0x4 };
                if control & present == 0 {
                    return Err(error(at, ErrorKind::AclOffsetWithoutPresentBit));
                }
            }
        }
        let sid = |offset: u32| {
            if offset == 0 {
                Ok(None)
            } else {
                Sid::at(bytes, offset as usize).map(Some)
            }
        };
        let acl = |offset: u32| {
            if offset == 0 {
                Ok(None)
            } else {
                Acl::at(bytes, offset as usize).map(Some)
            }
        };
        let sacl = acl(offsets[2])?;
        let dacl = acl(offsets[3])?;
        Ok(Self {
            sacl,
            dacl,
            bytes,
            header: Header {
                revision: h[0],
                resource_manager_control: h[1],
                control,
                owner_offset: offsets[0],
                group_offset: offsets[1],
                sacl_offset: offsets[2],
                dacl_offset: offsets[3],
            },
            owner: sid(offsets[0])?,
            group: sid(offsets[1])?,
        })
    }

    /// None when the SACL is absent or null (offset zero).
    pub fn sacl(&self) -> Option<Acl<'a>> {
        self.sacl
    }

    /// None when the DACL is absent or null (offset zero).
    pub fn dacl(&self) -> Option<Acl<'a>> {
        self.dacl
    }

    pub fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> Vec<u8> {
        let mut b = vec![0; 32];
        b[0] = 1;
        b[3] = 0x80;
        b[4..8].copy_from_slice(&20u32.to_le_bytes());
        b[20..28].copy_from_slice(&[1, 1, 1, 0, 0, 0, 0, 5]);
        b[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        b
    }

    #[test]
    fn wide_authority_raw_bits_and_bytes_are_lossless() {
        let mut b = descriptor();
        b[1] = 0xab;
        b[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        b.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let d = SecurityDescriptor::parse(&b).unwrap();
        assert_eq!(d.header.control, 0xffff);
        assert_eq!(d.header.resource_manager_control, 0xab);
        assert_eq!(d.owner.unwrap().identifier_authority(), 0x0100_0000_0005);
        assert_eq!(
            d.owner.unwrap().subauthorities().collect::<Vec<_>>(),
            [u32::MAX]
        );
        assert_eq!(d.as_bytes(), b);
        b[22..28].fill(0xff);
        assert_eq!(
            SecurityDescriptor::parse(&b)
                .unwrap()
                .owner
                .unwrap()
                .identifier_authority(),
            0xffff_ffff_ffff
        );
    }

    #[test]
    fn every_truncated_prefix_is_an_error() {
        let b = descriptor();
        for end in 0..b.len() {
            let e = SecurityDescriptor::parse(&b[..end]).unwrap_err();
            assert!(e.offset <= end);
        }
    }

    #[test]
    fn zero_offsets_and_shared_unordered_sid_targets_are_valid() {
        let mut b = descriptor();
        b[8..12].copy_from_slice(&20u32.to_le_bytes());
        let d = SecurityDescriptor::parse(&b).unwrap();
        assert_eq!(d.owner, d.group);
        b[4..12].fill(0);
        let d = SecurityDescriptor::parse(&b).unwrap();
        assert_eq!(d.owner, None);
        assert_eq!(d.group, None);
        // No padding is required by MS-DTYP's wire format.
        b.insert(20, 0);
        b[8..12].copy_from_slice(&21u32.to_le_bytes());
        assert!(SecurityDescriptor::parse(&b).unwrap().group.is_some());
    }

    #[test]
    fn invalid_revisions_counts_offsets_and_limits_are_located() {
        let b = descriptor();
        for (at, value, kind) in [
            (0, 2, ErrorKind::DescriptorRevision),
            (3, 0, ErrorKind::NotSelfRelative),
            (20, 2, ErrorKind::SidRevision),
            (21, 16, ErrorKind::SidSubauthorityCount),
            (21, 255, ErrorKind::SidSubauthorityCount),
        ] {
            let mut bad = b.clone();
            bad[at] = value;
            let e = SecurityDescriptor::parse(&bad).unwrap_err();
            assert_eq!(e, error(if at == 3 { 2 } else { at }, kind));
        }
        for offset in [1, 19, 32, u32::MAX] {
            let mut bad = b.clone();
            bad[4..8].copy_from_slice(&offset.to_le_bytes());
            assert_eq!(
                SecurityDescriptor::parse(&bad).unwrap_err(),
                error(4, ErrorKind::RelativeOffset)
            );
        }
        assert_eq!(
            SecurityDescriptor::parse_with_limit(&b, 31)
                .unwrap_err()
                .kind,
            ErrorKind::ResourceLimit
        );
        assert!(SecurityDescriptor::parse_with_limit(&b, 32).is_ok());
        assert_eq!(
            bounded(&b, usize::MAX, 8).unwrap_err().kind,
            ErrorKind::LengthOverflow
        );
    }

    #[test]
    fn sid_count_endpoints_and_acl_header_bounds() {
        let mut b = descriptor();
        b[21] = 0;
        b.truncate(28);
        assert_eq!(
            SecurityDescriptor::parse(&b)
                .unwrap()
                .owner
                .unwrap()
                .subauthorities()
                .len(),
            0
        );
        b[21] = 15;
        b.resize(88, 0);
        assert_eq!(
            SecurityDescriptor::parse(&b)
                .unwrap()
                .owner
                .unwrap()
                .subauthorities()
                .len(),
            15
        );
        b[16..20].copy_from_slice(&84u32.to_le_bytes());
        assert_eq!(
            SecurityDescriptor::parse(&b).unwrap_err().kind,
            ErrorKind::AclOffsetWithoutPresentBit
        );
        b[2] = 4;
        assert_eq!(
            SecurityDescriptor::parse(&b).unwrap_err(),
            error(84, ErrorKind::Truncated)
        );
        b[16..20].copy_from_slice(&80u32.to_le_bytes());
        b[82..84].copy_from_slice(&8u16.to_le_bytes());
        assert!(SecurityDescriptor::parse(&b).is_ok());
    }

    #[test]
    fn sid_targets_can_overlap_and_appear_in_reverse_order() {
        let mut b = descriptor();
        b.resize(36, 0);
        // The second SID's header overlaps the first SID's authority and RID.
        // Each target is independently a structurally valid SID.
        b[24..32].copy_from_slice(&[1, 1, 0, 0, 0, 0, 0, 5]);
        b[4..8].copy_from_slice(&24u32.to_le_bytes());
        b[8..12].copy_from_slice(&20u32.to_le_bytes());
        let d = SecurityDescriptor::parse(&b).unwrap();
        assert_eq!(d.owner.unwrap().as_bytes(), &b[24..36]);
        assert_eq!(d.group.unwrap().as_bytes(), &b[20..32]);
    }

    #[test]
    fn ace_extending_past_declared_acl_size_is_an_error() {
        // DACL at 32: AclSize 8, AceCount 1, but the ACE claims 16 bytes at 40.
        let mut b = descriptor();
        b[2..4].copy_from_slice(&0x8004u16.to_le_bytes());
        b[16..20].copy_from_slice(&32u32.to_le_bytes());
        b.resize(56, 0);
        b[32] = 2;
        b[34..36].copy_from_slice(&8u16.to_le_bytes());
        b[36..38].copy_from_slice(&1u16.to_le_bytes());
        b[42..44].copy_from_slice(&16u16.to_le_bytes());
        assert_eq!(
            SecurityDescriptor::parse(&b).unwrap_err(),
            error(40, ErrorKind::AclSize)
        );
        // A correctly framed ACE is retained byte-for-byte.
        b[34..36].copy_from_slice(&24u16.to_le_bytes());
        let d = SecurityDescriptor::parse(&b).unwrap();
        let aces: Vec<_> = d.dacl().unwrap().aces().collect();
        assert_eq!(aces.len(), 1);
        assert_eq!(aces[0].as_bytes(), &b[40..56]);
    }
}
