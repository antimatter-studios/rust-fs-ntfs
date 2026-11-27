//! The C descriptor views index the caller's own bytes (#1399, ACCESS-01).

use fs_ntfs::capi_sd::{
    fs_ntfs_sd_ace, fs_ntfs_sd_parse, FsNtfsSdAce, FsNtfsSdError, FsNtfsSdInfo,
};
use fs_ntfs::security_descriptor::{Component, ErrorKind, SecurityDescriptor};

const SID: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];

/// Owner at 20, an empty-handed group, a null SACL and a DACL at 32 holding
/// a callback ACE (type 9) then an unknown ACE (type 0x42).
fn descriptor() -> Vec<u8> {
    let mut b = vec![1, 0x5a, 0x14, 0x80];
    for offset in [20u32, 0, 0, 32] {
        b.extend_from_slice(&offset.to_le_bytes());
    }
    b.extend_from_slice(&SID);
    b.extend_from_slice(&[2, 0, 40, 0, 2, 0, 0, 0]);
    b.extend_from_slice(&[9, 0x13, 24, 0, 0xff, 0x01, 0x1f, 0]);
    b.extend_from_slice(&SID);
    b.extend_from_slice(b"artx");
    b.extend_from_slice(&[0x42, 0xc0, 8, 0, 0xde, 0xad, 0xbe, 0xef]);
    b
}

fn parse(b: &[u8]) -> (i32, FsNtfsSdInfo, FsNtfsSdError) {
    let (mut info, mut err) = (FsNtfsSdInfo::default(), FsNtfsSdError::default());
    let rc = unsafe { fs_ntfs_sd_parse(b.as_ptr().cast(), b.len(), &mut info, &mut err) };
    (rc, info, err)
}

fn ace(b: &[u8], sacl: i32, index: u32) -> (i32, FsNtfsSdAce, FsNtfsSdError) {
    let (mut out, mut err) = (FsNtfsSdAce::default(), FsNtfsSdError::default());
    let rc = unsafe { fs_ntfs_sd_ace(b.as_ptr().cast(), b.len(), sacl, index, &mut out, &mut err) };
    (rc, out, err)
}

#[test]
fn header_sids_and_acl_states_are_reported() {
    let b = descriptor();
    let (rc, i, _) = parse(&b);
    assert_eq!(rc, 0);
    assert_eq!(
        (i.revision, i.resource_manager_control, i.control),
        (1, 0x5a, 0x8014)
    );
    assert_eq!((i.owner_offset, i.owner_len), (20, 12));
    assert_eq!((i.group_offset, i.group_len), (0, 0));
    // SACL present with offset zero is null (1); the DACL is non-empty (3).
    assert_eq!((i.sacl.state, i.sacl.offset, i.sacl.size), (1, 0, 0));
    assert_eq!(
        (i.dacl.state, i.dacl.offset, i.dacl.size, i.dacl.ace_count),
        (3, 32, 40, 2)
    );
    assert_eq!(i.dacl.revision, 2);

    let mut absent = b.clone();
    absent[2] = 0;
    absent[16..20].fill(0);
    let (_, i, _) = parse(&absent);
    assert_eq!((i.sacl.state, i.dacl.state), (0, 0));
}

#[test]
fn aces_keep_callback_and_unknown_bytes_and_end_normally() {
    let b = descriptor();
    let (rc, a, _) = ace(&b, 0, 0);
    assert_eq!(rc, 1);
    assert_eq!((a.offset, a.size, a.ace_type, a.flags), (40, 24, 9, 0x13));
    assert_eq!((a.decoded, a.is_object, a.mask), (1, 0, 0x001f_01ff));
    assert_eq!((a.sid_offset, a.sid_len), (48, 12));
    assert_eq!((a.data_offset, a.data_len), (60, 4));
    assert_eq!(&b[60..64], b"artx");

    let (rc, a, _) = ace(&b, 0, 1);
    assert_eq!(rc, 1);
    assert_eq!((a.offset, a.size, a.ace_type, a.flags), (64, 8, 0x42, 0xc0));
    assert_eq!((a.decoded, a.sid_len, a.data_len), (0, 0, 0));

    assert_eq!(ace(&b, 0, 2).0, 0);
    assert_eq!(ace(&b, 0, u32::MAX).0, 0);
    assert_eq!(ace(&b, 1, 0).0, 0, "a null SACL has no ACE");
}

#[test]
fn malformed_input_is_located_from_the_descriptor_start() {
    // Owner SID revision.
    let mut b = descriptor();
    b[20] = 2;
    let (rc, _, e) = parse(&b);
    assert_eq!((rc, e.offset, e.kind), (-1, 20, 7));
    assert_eq!((e.component, e.ace_index), (1, -1), "owner");
    // The second ACE claims more than AclSize holds.
    let mut b = descriptor();
    b[66] = 12;
    let (rc, _, e) = parse(&b);
    assert_eq!((rc, e.offset, e.kind), (-1, 66, 9));
    assert_eq!((e.component, e.ace_index), (4, 1), "second DACL ACE");
    // The first ACE's SID is malformed: framing parses, its fields do not.
    let mut b = descriptor();
    b[49] = 16;
    assert_eq!(parse(&b).0, 0);
    let (rc, _, e) = ace(&b, 0, 0);
    assert_eq!((rc, e.offset, e.kind), (-1, 49, 8));
    assert_eq!((e.component, e.ace_index), (4, 0), "first DACL ACE");
}

#[test]
fn null_arguments_are_refused() {
    let b = descriptor();
    let mut info = FsNtfsSdInfo::default();
    let null = std::ptr::null_mut();
    unsafe {
        assert_eq!(fs_ntfs_sd_parse(std::ptr::null(), 0, &mut info, null), -1);
        assert_eq!(
            fs_ntfs_sd_parse(b.as_ptr().cast(), b.len(), std::ptr::null_mut(), null),
            -1
        );
        assert_eq!(
            fs_ntfs_sd_ace(b.as_ptr().cast(), b.len(), 0, 0, std::ptr::null_mut(), null),
            -1
        );
        assert_eq!(fs_ntfs_sd_parse(b.as_ptr().cast(), 19, &mut info, null), -1);
    }
}

/// Owner, group, a SACL with an object audit ACE, and a DACL with a callback
/// ACE then an unknown ACE. The DACL ends the input.
fn every_part() -> Vec<u8> {
    let mut b = vec![1, 0, 0x14, 0x80];
    for offset in [20u32, 32, 44, 88] {
        b.extend_from_slice(&offset.to_le_bytes());
    }
    b.extend_from_slice(&SID);
    b.extend_from_slice(&SID);
    b.extend_from_slice(&[4, 0, 44, 0, 1, 0, 0, 0]);
    b.extend_from_slice(&[7, 0x40, 36, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
    b.extend_from_slice(&[0x11; 16]);
    // A SID without subauthorities.
    b.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 5]);
    b.extend_from_slice(&descriptor()[32..]);
    b
}

/// The documented `FS_NTFS_SD_ERR_*` values.
fn c_kind(kind: ErrorKind) -> u32 {
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

/// The documented `FS_NTFS_SD_IN_*` values of a whole-descriptor parse.
fn c_component(component: Component) -> u32 {
    match component {
        Component::Header => 0,
        Component::Owner => 1,
        Component::Group => 2,
        Component::Sacl => 3,
        Component::Dacl => 4,
        Component::Ace => panic!("a descriptor parse never names a lone ACE"),
    }
}

/// Rust and C agree on one input. A refusal is located inside the input. An
/// accepted ACL yields each stored ACE or locates its defect, then ends.
fn check_both(b: &[u8]) {
    let (rc, info, e) = parse(b);
    let parsed = match SecurityDescriptor::parse(b) {
        Ok(parsed) => parsed,
        Err(rust) => {
            assert_eq!(rc, -1, "C accepted {b:?}");
            assert!(rust.offset <= b.len(), "{rust} outside {b:?}");
            assert_eq!(
                (e.offset as usize, e.kind, e.component, e.ace_index),
                (
                    rust.offset,
                    c_kind(rust.kind),
                    c_component(rust.component),
                    rust.ace_index.map_or(-1, i32::from)
                ),
                "{b:?}"
            );
            for sacl in [0, 1] {
                let (rc, _, from_ace) = ace(b, sacl, 0);
                assert_eq!(rc, -1);
                assert_eq!((from_ace.offset, from_ace.kind), (e.offset, e.kind));
            }
            return;
        }
    };
    assert_eq!(rc, 0, "C refused {b:?}");
    for (sacl, view, component, acl) in [
        (0, info.dacl, 4, parsed.dacl()),
        (1, info.sacl, 3, parsed.sacl()),
    ] {
        let mut at = view.offset as usize + 8;
        let stored: Vec<_> = acl.into_iter().flat_map(|acl| acl.aces()).collect();
        assert_eq!(stored.len(), usize::from(view.ace_count));
        for (index, rust) in (0u32..).zip(stored) {
            let (rc, out, e) = ace(b, sacl, index);
            let size = rust.as_bytes().len();
            assert_eq!(&b[at..at + size], rust.as_bytes());
            match rust.fields() {
                Ok(fields) => {
                    assert_eq!(rc, 1, "{b:?}");
                    assert_eq!((out.offset as usize, out.size as usize), (at, size));
                    assert_eq!((out.ace_type, out.flags), (rust.ace_type, rust.flags));
                    assert_eq!(out.decoded, u8::from(fields.is_some()));
                    if let Some(f) = fields {
                        assert_eq!(out.mask, f.mask);
                        assert_eq!(out.sid_offset as usize, at + f.sid_offset);
                        assert_eq!(
                            (out.data_offset + out.data_len) as usize,
                            at + size,
                            "{b:?}"
                        );
                    }
                }
                Err(rust) => {
                    assert_eq!(rc, -1, "{b:?}");
                    assert!(rust.offset <= size);
                    assert_eq!(
                        (e.offset as usize, e.kind, e.component, e.ace_index),
                        (at + rust.offset, c_kind(rust.kind), component, index as i32),
                        "{b:?}"
                    );
                }
            }
            at += size;
        }
        let end = u32::from(view.ace_count);
        assert_eq!(ace(b, sacl, end).0, 0, "one normal end: {b:?}");
        assert_eq!(ace(b, sacl, end + 1).0, 0, "the end repeats: {b:?}");
    }
}

#[test]
fn every_truncation_is_refused_by_both_parsers() {
    let b = every_part();
    check_both(&b);
    assert_eq!(parse(&b).0, 0, "positive control");
    assert_eq!(ace(&b, 1, 0).0, 1, "the SACL ACE decodes");
    for end in 0..b.len() {
        assert_eq!(parse(&b[..end]).0, -1, "prefix of {end} bytes");
        check_both(&b[..end]);
    }
}

#[test]
fn every_single_byte_change_is_bounded_and_agrees_between_rust_and_c() {
    let b = every_part();
    for at in 0..b.len() {
        for value in 0..=u8::MAX {
            let mut changed = b.clone();
            changed[at] = value;
            check_both(&changed);
        }
    }
}
