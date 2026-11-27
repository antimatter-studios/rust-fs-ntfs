//! The C descriptor views index the caller's own bytes (#1399, ACCESS-01).

use fs_ntfs::capi_sd::{
    fs_ntfs_sd_ace, fs_ntfs_sd_parse, FsNtfsSdAce, FsNtfsSdError, FsNtfsSdInfo,
};

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
    // The second ACE claims more than AclSize holds.
    let mut b = descriptor();
    b[66] = 12;
    let (rc, _, e) = parse(&b);
    assert_eq!((rc, e.offset, e.kind), (-1, 66, 9));
    // The first ACE's SID is malformed: framing parses, its fields do not.
    let mut b = descriptor();
    b[49] = 16;
    assert_eq!(parse(&b).0, 0);
    let (rc, _, e) = ace(&b, 0, 0);
    assert_eq!((rc, e.offset, e.kind), (-1, 49, 8));
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
