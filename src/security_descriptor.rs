//! Bounded, read-only SELF_RELATIVE descriptor, SID and ACL framing views.
//!
//! Layout sources: Microsoft [MS-DTYP] 2.4.6 and 2.4.2.2:
//! <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/7d4dac05-9cef-4563-a058-f108abecce1d>.
//! SID layout: <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/f992ad60-0fe4-4b87-9fed-beb478836861>.
//! ACL headers and ACE framing are bounded by AclSize. The object allow/deny
//! layout has a view (`Ace::object`); `Ace::fields` also covers the basic,
//! audit and callback layouts Windows authors from SDDL, keeping callback data
//! as raw bytes. Every other ACE kind stays opaque. `dacl_state` and
//! `sacl_state` distinguish absent, null, empty and non-empty ACLs from the raw
//! present bit and offset, as GetSecurityDescriptorDacl/Sacl report them; no
//! access decision is evaluated. `as_bytes` preserves all bytes for later decoders.

use std::fmt;

/// Default input resource bound; callers may choose a different explicit bound.
pub const DEFAULT_MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;
const HEADER_BYTES: usize = 20;

/// An error location is a byte offset relative to the descriptor start, or to
/// the ACE start when `component` is [`Component::Ace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    pub offset: usize,
    pub kind: ErrorKind,
    /// The part of the descriptor the defect belongs to. A bad header offset
    /// belongs to the part that offset names.
    pub component: Component,
    /// The stored-order index of the ACE whose framing is bad. `None` for a
    /// defect outside an ACL's ACE walk, and for errors from [`Ace::object`]
    /// and [`Ace::fields`], whose caller already holds the ACE.
    pub ace_index: Option<u16>,
}

/// The named parts of a self-relative descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    /// The input as a whole, or the header outside its four offsets.
    Header,
    Owner,
    Group,
    Sacl,
    Dacl,
    /// An ACE decoded on its own, outside any descriptor.
    Ace,
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
            "security descriptor byte {}: {:?} in {:?}",
            self.offset, self.kind, self.component
        )?;
        match self.ace_index {
            Some(index) => write!(f, " ACE {index}"),
            None => Ok(()),
        }
    }
}
impl std::error::Error for DecodeError {}

fn error(offset: usize, kind: ErrorKind) -> DecodeError {
    DecodeError {
        offset,
        kind,
        component: Component::Header,
        ace_index: None,
    }
}

impl DecodeError {
    fn within(self, component: Component) -> Self {
        Self { component, ..self }
    }

    fn at_ace(self, index: u16) -> Self {
        Self {
            ace_index: Some(index),
            ..self
        }
    }
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
        let count = u16::from_le_bytes([h[4], h[5]]);
        if size < 8 {
            return Err(error(offset + 2, ErrorKind::AclSize));
        }
        let acl = Self {
            bytes: bounded(bytes, offset, size)?,
        };
        // Every ACE header and its declared size must lie inside AclSize.
        let mut at = 8;
        for index in 0..count {
            if at + 4 > size {
                return Err(error(offset + at, ErrorKind::AclSize).at_ace(index));
            }
            let ace_size = usize::from(u16::from_le_bytes([acl.bytes[at + 2], acl.bytes[at + 3]]));
            if ace_size < 4 || at + ace_size > size {
                return Err(error(offset + at + 2, ErrorKind::AclSize).at_ace(index));
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

/// The fields of an ACE whose layout this module decodes. `trailing` is every
/// byte after the SID inside AceSize: a callback ACE's application data, kept
/// raw and never evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AceFields<'a> {
    pub mask: u32,
    /// `None` for a layout without the object Flags field.
    pub object_flags: Option<u32>,
    pub object_type: Option<[u8; 16]>,
    pub inherited_object_type: Option<[u8; 16]>,
    pub sid: Sid<'a>,
    /// Offset of the SID from the ACE start.
    pub sid_offset: usize,
    pub trailing: &'a [u8],
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

    /// The object allow/deny layout ([MS-DTYP] 2.4.4.3 and 2.4.4.4):
    /// <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/c79a383c-2b3f-4655-abe7-dcbb7ce0cfbe>.
    /// `Ok(None)` for every other ACE type, whose bytes stay opaque. Error
    /// offsets are relative to the ACE start. No access policy is evaluated.
    pub fn object(self) -> Result<Option<ObjectAce<'a>>, DecodeError> {
        if self.ace_type != ACCESS_ALLOWED_OBJECT_ACE_TYPE
            && self.ace_type != ACCESS_DENIED_OBJECT_ACE_TYPE
        {
            return Ok(None);
        }
        let fields = self.layout(true)?;
        Ok(Some(ObjectAce {
            ace: self,
            mask: fields.mask,
            object_flags: fields.object_flags.unwrap_or_default(),
            object_type: fields.object_type,
            inherited_object_type: fields.inherited_object_type,
            sid: fields.sid,
        }))
    }

    /// Mask, optional object GUIDs, SID and trailing bytes of these ACE types:
    ///
    /// - 0, 1, 2: allow, deny, audit ([MS-DTYP] 2.4.4.2, 2.4.4.4, 2.4.4.10).
    /// - 5, 6, 7: their object forms.
    /// - 9, 10: allow and deny callback ([MS-DTYP] 2.4.4.6, 2.4.4.7):
    ///   <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/c9579cf4-0f4a-44f1-9444-422dfb10557a>,
    ///   <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/35adad6b-fda5-4cc1-b1b5-9beda5b07d2e>.
    /// - 11: allow callback object ([MS-DTYP] 2.4.4.8):
    ///   <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/fe1838ea-ea34-4a5e-b40e-eb870f8322ae>.
    /// - 13: audit callback ([MS-DTYP] 2.4.4.12):
    ///   <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/bd6b6fd8-4bef-427e-9a43-b9b46457e934>.
    ///
    /// `Ok(None)` for every other type. Error offsets are relative to the ACE
    /// start. No condition or access policy is evaluated.
    pub fn fields(self) -> Result<Option<AceFields<'a>>, DecodeError> {
        match self.ace_type {
            0..=2 | 9 | 10 | 13 => self.layout(false).map(Some),
            5..=7 | 11 => self.layout(true).map(Some),
            _ => Ok(None),
        }
    }

    fn layout(self, object: bool) -> Result<AceFields<'a>, DecodeError> {
        self.decode_layout(object)
            .map_err(|e| e.within(Component::Ace))
    }

    fn decode_layout(self, object: bool) -> Result<AceFields<'a>, DecodeError> {
        let fixed = bounded(self.bytes, 4, if object { 8 } else { 4 })?;
        let mask = u32::from_le_bytes([fixed[0], fixed[1], fixed[2], fixed[3]]);
        let object_flags =
            object.then(|| u32::from_le_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]));
        let mut at = 4 + fixed.len();
        let flags = object_flags.unwrap_or_default();
        let object_type = optional_guid(self.bytes, &mut at, flags & ACE_OBJECT_TYPE_PRESENT != 0)?;
        let inherited_object_type = optional_guid(
            self.bytes,
            &mut at,
            flags & ACE_INHERITED_OBJECT_TYPE_PRESENT != 0,
        )?;
        let sid = Sid::at(self.bytes, at)?;
        Ok(AceFields {
            mask,
            object_flags,
            object_type,
            inherited_object_type,
            sid,
            sid_offset: at,
            trailing: &self.bytes[at + sid.as_bytes().len()..],
        })
    }
}

const ACCESS_ALLOWED_OBJECT_ACE_TYPE: u8 = 0x05;
const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 0x06;
const ACE_OBJECT_TYPE_PRESENT: u32 = 0x1;
const ACE_INHERITED_OBJECT_TYPE_PRESENT: u32 = 0x2;

fn optional_guid(
    bytes: &[u8],
    at: &mut usize,
    present: bool,
) -> Result<Option<[u8; 16]>, DecodeError> {
    if !present {
        return Ok(None);
    }
    let guid = bounded(bytes, *at, 16)?;
    *at += 16;
    let mut out = [0; 16];
    out.copy_from_slice(guid);
    Ok(Some(out))
}

/// An object allow/deny ACE. Flags are raw, including unknown bits; GUIDs are
/// the stored bytes and exist only when their flag bit is set. `ace.as_bytes()`
/// keeps the whole ACE, including any bytes after the SID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectAce<'a> {
    pub ace: Ace<'a>,
    pub mask: u32,
    pub object_flags: u32,
    pub object_type: Option<[u8; 16]>,
    pub inherited_object_type: Option<[u8; 16]>,
    pub sid: Sid<'a>,
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
        const ROLES: [Component; 4] = [
            Component::Owner,
            Component::Group,
            Component::Sacl,
            Component::Dacl,
        ];
        for (i, slot) in offsets.iter_mut().enumerate() {
            let at = 4 + i * 4;
            *slot = u32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]);
            if *slot == 0 {
                continue;
            }
            let bad_offset = error(at, ErrorKind::RelativeOffset).within(ROLES[i]);
            let offset = usize::try_from(*slot).map_err(|_| bad_offset)?;
            if offset < HEADER_BYTES || offset >= bytes.len() {
                return Err(bad_offset);
            }
            if i >= 2 {
                let present = if i == 2 { 0x10 } else { 0x4 };
                if control & present == 0 {
                    return Err(error(at, ErrorKind::AclOffsetWithoutPresentBit).within(ROLES[i]));
                }
            }
        }
        let sid = |offset: u32, role: Component| {
            if offset == 0 {
                Ok(None)
            } else {
                Sid::at(bytes, offset as usize)
                    .map(Some)
                    .map_err(|e| e.within(role))
            }
        };
        let acl = |offset: u32, role: Component| {
            if offset == 0 {
                Ok(None)
            } else {
                Acl::at(bytes, offset as usize)
                    .map(Some)
                    .map_err(|e| e.within(role))
            }
        };
        let sacl = acl(offsets[2], Component::Sacl)?;
        let dacl = acl(offsets[3], Component::Dacl)?;
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
            owner: sid(offsets[0], Component::Owner)?,
            group: sid(offsets[1], Component::Group)?,
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

    /// Absent when SE_SACL_PRESENT (0x0010) is clear; null when it is set
    /// with offset zero. The raw control and offset stay in `header`.
    pub fn sacl_state(&self) -> AclState<'a> {
        AclState::from_parts(self.header.control & 0x10 != 0, self.sacl)
    }

    /// Absent when SE_DACL_PRESENT (0x0004) is clear; null when it is set
    /// with offset zero. The raw control and offset stay in `header`.
    pub fn dacl_state(&self) -> AclState<'a> {
        AclState::from_parts(self.header.control & 0x4 != 0, self.dacl)
    }

    pub fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }
}

/// Presence of one ACL role. Empty and NonEmpty keep the ACL view, including
/// the header bytes of an empty ACL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclState<'a> {
    Absent,
    Null,
    Empty(Acl<'a>),
    NonEmpty(Acl<'a>),
}

impl<'a> AclState<'a> {
    fn from_parts(present: bool, acl: Option<Acl<'a>>) -> Self {
        match (present, acl) {
            (false, _) => Self::Absent,
            (true, None) => Self::Null,
            (true, Some(acl)) if acl.ace_count() == 0 => Self::Empty(acl),
            (true, Some(acl)) => Self::NonEmpty(acl),
        }
    }

    pub fn acl(self) -> Option<Acl<'a>> {
        match self {
            Self::Empty(acl) | Self::NonEmpty(acl) => Some(acl),
            Self::Absent | Self::Null => None,
        }
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
            let part = if at >= 20 {
                Component::Owner
            } else {
                Component::Header
            };
            assert_eq!(e, error(if at == 3 { 2 } else { at }, kind).within(part));
        }
        for offset in [1, 19, 32, u32::MAX] {
            let mut bad = b.clone();
            bad[4..8].copy_from_slice(&offset.to_le_bytes());
            assert_eq!(
                SecurityDescriptor::parse(&bad).unwrap_err(),
                error(4, ErrorKind::RelativeOffset).within(Component::Owner)
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
            error(84, ErrorKind::Truncated).within(Component::Dacl)
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
                .within(Component::Dacl)
                .at_ace(0)
        );
        // A correctly framed ACE is retained byte-for-byte.
        b[34..36].copy_from_slice(&24u16.to_le_bytes());
        let d = SecurityDescriptor::parse(&b).unwrap();
        let aces: Vec<_> = d.dacl().unwrap().aces().collect();
        assert_eq!(aces.len(), 1);
        assert_eq!(aces[0].as_bytes(), &b[40..56]);
    }

    const GUID_A: [u8; 16] = [0x11; 16];
    const GUID_B: [u8; 16] = [0x22; 16];
    const SID: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 0x20, 0, 0, 0];

    fn object_ace_bytes(ace_type: u8, object_flags: u32) -> Vec<u8> {
        let mut b = vec![ace_type, 0x13, 0, 0];
        b.extend_from_slice(&0x0012_019fu32.to_le_bytes());
        b.extend_from_slice(&object_flags.to_le_bytes());
        if object_flags & 1 != 0 {
            b.extend_from_slice(&GUID_A);
        }
        if object_flags & 2 != 0 {
            b.extend_from_slice(&GUID_B);
        }
        b.extend_from_slice(&SID);
        b.extend_from_slice(&[0xca, 0xfe]);
        let size = b.len() as u16;
        b[2..4].copy_from_slice(&size.to_le_bytes());
        b
    }

    fn ace(bytes: &[u8]) -> Ace<'_> {
        Ace {
            ace_type: bytes[0],
            flags: bytes[1],
            bytes,
        }
    }

    #[test]
    fn object_ace_guids_follow_each_flag_combination() {
        for ace_type in [5u8, 6] {
            for flags in 0u32..4 {
                let b = object_ace_bytes(ace_type, flags);
                let o = ace(&b).object().unwrap().unwrap();
                assert_eq!(o.mask, 0x0012_019f);
                assert_eq!(o.object_flags, flags);
                assert_eq!(o.object_type, (flags & 1 != 0).then_some(GUID_A));
                assert_eq!(o.inherited_object_type, (flags & 2 != 0).then_some(GUID_B));
                assert_eq!(o.sid.as_bytes(), SID);
                assert_eq!(o.ace.as_bytes(), b, "trailing bytes preserved");
            }
        }
    }

    #[test]
    fn object_ace_unknown_flag_bits_stay_raw_and_add_no_guid() {
        let b = object_ace_bytes(5, 0xffff_fffc);
        let o = ace(&b).object().unwrap().unwrap();
        assert_eq!(o.object_flags, 0xffff_fffc);
        assert_eq!((o.object_type, o.inherited_object_type), (None, None));
        assert_eq!(o.sid.as_bytes(), SID);
    }

    fn ace_error(offset: usize, kind: ErrorKind) -> DecodeError {
        error(offset, kind).within(Component::Ace)
    }

    #[test]
    fn object_ace_missing_or_truncated_fields_are_errors() {
        // Fixed mask and flags fields cut short.
        let b = object_ace_bytes(5, 0);
        assert_eq!(
            ace(&b[..10]).object(),
            Err(ace_error(4, ErrorKind::Truncated))
        );
        // A flagged GUID that is absent or truncated.
        let b = object_ace_bytes(5, 1);
        assert_eq!(
            ace(&b[..12]).object(),
            Err(ace_error(12, ErrorKind::Truncated))
        );
        assert_eq!(
            ace(&b[..20]).object(),
            Err(ace_error(12, ErrorKind::Truncated))
        );
        let b = object_ace_bytes(6, 3);
        assert_eq!(
            ace(&b[..28]).object(),
            Err(ace_error(28, ErrorKind::Truncated))
        );
        // The SID header or its subauthorities cut short.
        let b = object_ace_bytes(5, 2);
        assert_eq!(
            ace(&b[..32]).object(),
            Err(ace_error(28, ErrorKind::Truncated))
        );
        assert_eq!(
            ace(&b[..38]).object(),
            Err(ace_error(28, ErrorKind::Truncated))
        );
        // Flags 0 needs no GUID: the SID follows the flags directly.
        let b = object_ace_bytes(5, 0);
        assert_eq!(ace(&b[..24]).object().unwrap().unwrap().sid.as_bytes(), SID);
        // A SID that is not a structurally valid SID.
        let mut b = object_ace_bytes(5, 0);
        b[13] = 16;
        assert_eq!(
            ace(&b).object(),
            Err(ace_error(13, ErrorKind::SidSubauthorityCount))
        );
        b[13] = 1;
        b[12] = 2;
        assert_eq!(ace(&b).object(), Err(ace_error(12, ErrorKind::SidRevision)));
    }

    #[test]
    fn non_object_ace_kinds_stay_opaque() {
        for ace_type in [0u8, 1, 2, 3, 4, 7, 8, 9, 0x0b, 0x0c, 0x11, 0xff] {
            let b = object_ace_bytes(ace_type, 3);
            assert_eq!(ace(&b).object(), Ok(None));
            assert_eq!(ace(&b).as_bytes(), b);
        }
        // Even an ACE too short for any object fields is opaque, not an error.
        assert_eq!(ace(&[0, 0, 4, 0]).object(), Ok(None));
    }

    #[test]
    fn fields_decodes_basic_and_callback_layouts_and_keeps_trailing_bytes() {
        for ace_type in [0u8, 1, 2, 9, 10, 13] {
            let mut b = vec![ace_type, 0, 0, 0];
            b.extend_from_slice(&0x0012_019fu32.to_le_bytes());
            b.extend_from_slice(&SID);
            b.extend_from_slice(b"artx\x01\x02\x03\x04");
            let size = b.len() as u16;
            b[2..4].copy_from_slice(&size.to_le_bytes());
            let f = ace(&b).fields().unwrap().unwrap();
            assert_eq!(f.mask, 0x0012_019f);
            assert_eq!(f.object_flags, None);
            assert_eq!((f.sid_offset, f.sid.as_bytes()), (8, &SID[..]));
            assert_eq!(f.trailing, b"artx\x01\x02\x03\x04");
            assert_eq!(
                ace(&b[..19]).fields(),
                Err(ace_error(8, ErrorKind::Truncated))
            );
        }
        for ace_type in [5u8, 6, 7, 11] {
            let b = object_ace_bytes(ace_type, 3);
            let f = ace(&b).fields().unwrap().unwrap();
            assert_eq!(f.object_flags, Some(3));
            assert_eq!(f.object_type, Some(GUID_A));
            assert_eq!(f.inherited_object_type, Some(GUID_B));
            assert_eq!((f.sid_offset, f.sid.as_bytes()), (44, &SID[..]));
            assert_eq!(f.trailing, [0xca, 0xfe]);
        }
        for ace_type in [3u8, 4, 8, 12, 14, 15, 0x11, 0xff] {
            assert_eq!(ace(&object_ace_bytes(ace_type, 3)).fields(), Ok(None));
        }
    }

    #[test]
    fn errors_name_the_component_and_the_ace() {
        // Group SID at 20, empty SACL at 32, DACL at 40 holding two 4-byte ACEs.
        let mut b = descriptor();
        b[2..4].copy_from_slice(&0x8014u16.to_le_bytes());
        b[8..12].copy_from_slice(&20u32.to_le_bytes());
        b[4..8].fill(0);
        b[12..16].copy_from_slice(&32u32.to_le_bytes());
        b[16..20].copy_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&[2, 0, 8, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&[2, 0, 16, 0, 2, 0, 0, 0]);
        b.extend_from_slice(&[0x42, 0, 4, 0, 0x42, 0, 4, 0]);
        SecurityDescriptor::parse(&b).expect("positive control");
        let fail = |at: usize, value: u8| {
            let mut bad = b.clone();
            bad[at] = value;
            let e = SecurityDescriptor::parse(&bad).unwrap_err();
            (e.offset, e.kind, e.component, e.ace_index)
        };
        use Component::{Dacl, Group, Sacl};
        use ErrorKind::{AclSize, RelativeOffset, SidRevision};
        assert_eq!(fail(20, 2), (20, SidRevision, Group, None));
        assert_eq!(fail(8, 1), (8, RelativeOffset, Group, None));
        assert_eq!(fail(12, 1), (12, RelativeOffset, Sacl, None));
        assert_eq!(fail(34, 4), (34, AclSize, Sacl, None));
        assert_eq!(fail(42, 4), (42, AclSize, Dacl, None));
        assert_eq!(fail(50, 12), (50, AclSize, Dacl, Some(0)));
        assert_eq!(fail(54, 8), (54, AclSize, Dacl, Some(1)));
        // A third ACE has no header left inside AclSize.
        assert_eq!(fail(44, 3), (56, AclSize, Dacl, Some(2)));
        assert_eq!(
            SecurityDescriptor::parse(&{
                let mut bad = b.clone();
                bad[54] = 8;
                bad
            })
            .unwrap_err()
            .to_string(),
            "security descriptor byte 54: AclSize in Dacl ACE 1"
        );
    }

    #[test]
    fn object_ace_through_a_parsed_dacl() {
        let mut b = descriptor();
        b[2..4].copy_from_slice(&0x8004u16.to_le_bytes());
        b[16..20].copy_from_slice(&32u32.to_le_bytes());
        let entry = object_ace_bytes(6, 1);
        let acl_size = (8 + entry.len()) as u16;
        b.extend_from_slice(&[4, 0]);
        b.extend_from_slice(&acl_size.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&entry);
        let d = SecurityDescriptor::parse(&b).unwrap();
        let first = d.dacl().unwrap().aces().next().unwrap();
        let o = first.object().unwrap().unwrap();
        assert_eq!(o.object_type, Some(GUID_A));
        assert_eq!(o.inherited_object_type, None);
        assert_eq!(o.ace.as_bytes(), entry);
    }
}
