//! C views over a caller-held self-relative security descriptor (#1399).
//!
//! Nothing is allocated or copied out of the input: every offset these
//! structures report indexes the caller's own buffer, so owner, group, ACL,
//! ACE and callback bytes stay exactly as stored. Parsing and bounds are
//! [`crate::security_descriptor`]'s.

use crate::security_descriptor::{AclState, DecodeError, ErrorKind, SecurityDescriptor};
use crate::{ffi_guard, set_einval};
use std::ffi::{c_int, c_void};

/// Where and why a descriptor is malformed. `kind` is `FS_NTFS_SD_ERR_*`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsNtfsSdError {
    pub offset: u32,
    pub kind: u32,
}

/// One ACL role. `state` is `FS_NTFS_SD_ACL_*`; the rest is zero unless an
/// ACL is stored.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsNtfsSdAcl {
    pub offset: u32,
    pub size: u32,
    pub ace_count: u16,
    pub revision: u8,
    pub state: u8,
}

/// Decoded header, SIDs and ACL framing. An absent SID has length zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsNtfsSdInfo {
    pub revision: u8,
    pub resource_manager_control: u8,
    pub control: u16,
    pub owner_offset: u32,
    pub owner_len: u32,
    pub group_offset: u32,
    pub group_len: u32,
    pub sacl: FsNtfsSdAcl,
    pub dacl: FsNtfsSdAcl,
}

/// One ACE. `offset`, `size`, `ace_type` and `flags` are always filled.
/// The remaining fields are zero when `decoded` is 0 (an opaque ACE type).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsNtfsSdAce {
    pub offset: u32,
    pub size: u32,
    pub ace_type: u8,
    pub flags: u8,
    pub decoded: u8,
    /// 1 iff the layout has the object Flags field.
    pub is_object: u8,
    pub mask: u32,
    pub object_flags: u32,
    pub sid_offset: u32,
    pub sid_len: u32,
    /// Bytes after the SID inside the ACE: callback application data.
    pub data_offset: u32,
    pub data_len: u32,
    pub object_type: [u8; 16],
    pub inherited_object_type: [u8; 16],
}

fn kind_code(kind: ErrorKind) -> u32 {
    match kind {
        ErrorKind::Truncated => 1,
        ErrorKind::ResourceLimit => 2,
        ErrorKind::DescriptorRevision => 3,
        ErrorKind::NotSelfRelative => 4,
        ErrorKind::RelativeOffset => 5,
        ErrorKind::AclOffsetWithoutPresentBit => 6,
        ErrorKind::SidRevision => 7,
        ErrorKind::SidSubauthorityCount => 8,
        ErrorKind::AclSize => 9,
        ErrorKind::LengthOverflow => 10,
    }
}

/// Offsets and lengths fit u32: the parser refuses input over 1 MiB.
fn narrow(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn report(what: &str, e: DecodeError, base: usize, out: *mut FsNtfsSdError) -> c_int {
    let offset = base.saturating_add(e.offset);
    if !out.is_null() {
        // SAFETY: non-null; the caller provides a writable FsNtfsSdError.
        unsafe {
            out.write(FsNtfsSdError {
                offset: narrow(offset),
                kind: kind_code(e.kind),
            })
        };
    }
    set_einval(&format!(
        "{what}: security descriptor byte {offset}: {:?}",
        e.kind
    ));
    -1
}

/// # Safety
/// `sd` is null or readable for `len` bytes for the lifetime `'a`.
unsafe fn input<'a>(sd: *const c_void, len: usize) -> Option<&'a [u8]> {
    if sd.is_null() || len > isize::MAX as usize {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(sd.cast::<u8>(), len) })
}

fn acl_view(state: AclState<'_>, offset: u32) -> FsNtfsSdAcl {
    let code = match state {
        AclState::Absent => 0,
        AclState::Null => 1,
        AclState::Empty(_) => 2,
        AclState::NonEmpty(_) => 3,
    };
    match state.acl() {
        Some(acl) => FsNtfsSdAcl {
            offset,
            size: narrow(acl.as_bytes().len()),
            ace_count: acl.ace_count(),
            revision: acl.revision(),
            state: code,
        },
        None => FsNtfsSdAcl {
            state: code,
            ..FsNtfsSdAcl::default()
        },
    }
}

/// Parse the self-relative descriptor in `sd[0..len]` into `*out`.
/// Returns 0, or -1 with `*err` (when non-null) locating the defect. A null
/// argument returns -1 and leaves `*err` untouched.
///
/// # Safety
/// `sd` is readable for `len` bytes; `out` and a non-null `err` are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fs_ntfs_sd_parse(
    sd: *const c_void,
    len: usize,
    out: *mut FsNtfsSdInfo,
    err: *mut FsNtfsSdError,
) -> c_int {
    ffi_guard("fs_ntfs_sd_parse", -1, move || {
        let Some(bytes) = (unsafe { input(sd, len) }) else {
            set_einval("fs_ntfs_sd_parse: null or oversized sd");
            return -1;
        };
        if out.is_null() {
            set_einval("fs_ntfs_sd_parse: null out");
            return -1;
        }
        let d = match SecurityDescriptor::parse(bytes) {
            Ok(d) => d,
            Err(e) => return report("fs_ntfs_sd_parse", e, 0, err),
        };
        let h = d.header;
        let info = FsNtfsSdInfo {
            revision: h.revision,
            resource_manager_control: h.resource_manager_control,
            control: h.control,
            owner_offset: h.owner_offset,
            owner_len: d.owner.map_or(0, |s| narrow(s.as_bytes().len())),
            group_offset: h.group_offset,
            group_len: d.group.map_or(0, |s| narrow(s.as_bytes().len())),
            sacl: acl_view(d.sacl_state(), h.sacl_offset),
            dacl: acl_view(d.dacl_state(), h.dacl_offset),
        };
        // SAFETY: non-null; the caller provides a writable FsNtfsSdInfo.
        unsafe { out.write(info) };
        0
    })
}

/// ACE `index`, in stored order, of the DACL (`sacl == 0`) or the SACL.
/// Returns 1 with `*out` filled, 0 when `index` is past the last ACE (the
/// normal end, including an absent, null or empty ACL), or -1 with `*err`
/// locating a defect in the descriptor or in this ACE's fields.
///
/// # Safety
/// As [`fs_ntfs_sd_parse`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fs_ntfs_sd_ace(
    sd: *const c_void,
    len: usize,
    sacl: c_int,
    index: u32,
    out: *mut FsNtfsSdAce,
    err: *mut FsNtfsSdError,
) -> c_int {
    ffi_guard("fs_ntfs_sd_ace", -1, move || {
        let Some(bytes) = (unsafe { input(sd, len) }) else {
            set_einval("fs_ntfs_sd_ace: null or oversized sd");
            return -1;
        };
        if out.is_null() {
            set_einval("fs_ntfs_sd_ace: null out");
            return -1;
        }
        let d = match SecurityDescriptor::parse(bytes) {
            Ok(d) => d,
            Err(e) => return report("fs_ntfs_sd_ace", e, 0, err),
        };
        let (acl, acl_offset) = if sacl != 0 {
            (d.sacl(), d.header.sacl_offset)
        } else {
            (d.dacl(), d.header.dacl_offset)
        };
        let Some(acl) = acl else {
            return 0;
        };
        // ACEs are contiguous from the 8-byte ACL header.
        let mut offset = acl_offset as usize + 8;
        for (i, ace) in (0u32..).zip(acl.aces()) {
            let size = ace.as_bytes().len();
            if i != index {
                offset += size;
                continue;
            }
            let mut view = FsNtfsSdAce {
                offset: narrow(offset),
                size: narrow(size),
                ace_type: ace.ace_type,
                flags: ace.flags,
                ..FsNtfsSdAce::default()
            };
            match ace.fields() {
                Ok(Some(f)) => {
                    let sid_len = f.sid.as_bytes().len();
                    view.decoded = 1;
                    view.is_object = u8::from(f.object_flags.is_some());
                    view.mask = f.mask;
                    view.object_flags = f.object_flags.unwrap_or_default();
                    view.sid_offset = narrow(offset + f.sid_offset);
                    view.sid_len = narrow(sid_len);
                    view.data_offset = narrow(offset + f.sid_offset + sid_len);
                    view.data_len = narrow(f.trailing.len());
                    view.object_type = f.object_type.unwrap_or_default();
                    view.inherited_object_type = f.inherited_object_type.unwrap_or_default();
                }
                Ok(None) => {}
                Err(e) => return report("fs_ntfs_sd_ace", e, offset, err),
            }
            // SAFETY: non-null; the caller provides a writable FsNtfsSdAce.
            unsafe { out.write(view) };
            return 1;
        }
        0
    })
}
