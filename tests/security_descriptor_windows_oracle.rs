//! Executable Windows capture and independent owner/group/control comparison.
//! Run on Windows: cargo test --test security_descriptor_windows_oracle.
//! NTFS_SD_CAPTURE names the retained TSV capture (descriptor/control/SID bytes).
#![cfg(windows)]

use fs_ntfs::security_descriptor::{AclState, SecurityDescriptor, Sid};
// Windows reads of the DACL: the presence flag, the ACL pointer, and per-ACE pointers.
use std::ffi::c_void;
use std::ptr;

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        out: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn GetSecurityDescriptorOwner(
        sd: *const c_void,
        sid: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn GetSecurityDescriptorGroup(
        sd: *const c_void,
        sid: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn GetSecurityDescriptorControl(
        sd: *const c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
    fn GetSecurityDescriptorDacl(
        sd: *const c_void,
        present: *mut i32,
        acl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn GetAclInformation(acl: *const c_void, info: *mut c_void, length: u32, class: u32) -> i32;
    fn GetAce(acl: *const c_void, index: u32, ace: *mut *mut c_void) -> i32;
    fn GetLengthSid(sid: *const c_void) -> u32;
    fn GetSidIdentifierAuthority(sid: *const c_void) -> *const u8;
    fn IsValidSecurityDescriptor(sd: *const c_void) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

struct WindowsDescriptor(*mut c_void);
impl Drop for WindowsDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

unsafe fn compare_sid(sid: *const c_void, decoded: Option<Sid<'_>>) -> String {
    if sid.is_null() {
        assert!(decoded.is_none());
        return "-".into();
    }
    let bytes = std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize);
    let decoded = decoded.expect("Windows reports a SID");
    assert_eq!(decoded.as_bytes(), bytes);
    let authority = std::slice::from_raw_parts(GetSidIdentifierAuthority(sid), 6)
        .iter()
        .fold(0u64, |n, b| (n << 8) | u64::from(*b));
    assert_eq!(decoded.identifier_authority(), authority);
    hex(bytes)
}

/// Compares the decoded DACL with Windows' ACL bytes, ACE count and each ACE.
/// Returns the Windows ACL as hex, or "-" for an absent or null DACL.
unsafe fn compare_dacl(sd: *const c_void, decoded: &SecurityDescriptor<'_>) -> String {
    let mut present = 0;
    let mut acl = ptr::null_mut();
    let mut defaulted = 0;
    assert_ne!(
        GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted),
        0
    );
    let Some(ours) = decoded.dacl() else {
        assert!(
            present == 0 || acl.is_null(),
            "decoder omitted a Windows DACL"
        );
        return "-".into();
    };
    assert_ne!(present, 0);
    let header = std::slice::from_raw_parts(acl.cast::<u8>(), 8);
    let acl_size = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let acl_bytes = std::slice::from_raw_parts(acl.cast::<u8>(), acl_size);
    assert_eq!(ours.as_bytes(), acl_bytes);
    assert_eq!(ours.revision(), header[0]);
    let mut info = [0u32; 3];
    assert_ne!(GetAclInformation(acl, info.as_mut_ptr().cast(), 12, 2), 0);
    assert_eq!(usize::from(ours.ace_count()), info[0] as usize);
    assert_eq!(ours.aces().count(), info[0] as usize);
    for (i, ace_ours) in ours.aces().enumerate() {
        let mut ace = ptr::null_mut();
        assert_ne!(GetAce(acl, i as u32, &mut ace), 0);
        let head = std::slice::from_raw_parts(ace.cast::<u8>(), 4);
        let size = usize::from(u16::from_le_bytes([head[2], head[3]]));
        assert_eq!(
            ace_ours.as_bytes(),
            std::slice::from_raw_parts(ace.cast::<u8>(), size)
        );
        assert_eq!(ace_ours.ace_type, head[0]);
        assert_eq!(ace_ours.flags, head[1]);
    }
    hex(acl_bytes)
}

#[test]
fn windows_authored_descriptors_match_security_apis_and_are_captured() {
    let mut capture = String::from(
        "# Windows Advapi32 capture: sddl\tdescriptor_hex\tcontrol\towner_hex\tgroup_hex\tdacl_hex\n",
    );
    // Windows authors these bytes; ACL contents are only preserved by this slice.
    let cases = [
        "O:SYG:BA",
        "O:SY",
        "G:BA",
        "O:SYG:SYD:",
        "O:SYG:BAD:P(A;;FA;;;SY)",
        "O:SYG:BAD:AI(A;ID;FR;;;WD)S:(AU;SAFA;FR;;;WD)",
        "O:SYG:BAD:(A;;FA;;;SY)(D;OICI;FR;;;WD)",
        "O:S-1-0x010000000005-21G:S-1-0xffffffffffff-4294967295",
    ];
    for sddl in cases {
        let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut address = ptr::null_mut();
        let mut size = 0;
        unsafe {
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut address,
                    &mut size
                ),
                0,
                "Windows SDDL capture {sddl}: {}",
                std::io::Error::last_os_error()
            );
            let sd = WindowsDescriptor(address);
            assert_ne!(
                IsValidSecurityDescriptor(sd.0),
                0,
                "Windows validity: {sddl}"
            );
            let bytes = std::slice::from_raw_parts(sd.0.cast::<u8>(), size as usize);
            let decoded = SecurityDescriptor::parse(bytes).expect("decode Windows descriptor");
            let mut control = 0;
            let mut revision = 0;
            assert_ne!(
                GetSecurityDescriptorControl(sd.0, &mut control, &mut revision),
                0
            );
            assert_eq!(decoded.header.control, control);
            assert_eq!(u32::from(decoded.header.revision), revision);
            let mut owner = ptr::null_mut();
            let mut group = ptr::null_mut();
            let mut defaulted = 0;
            assert_ne!(
                GetSecurityDescriptorOwner(sd.0, &mut owner, &mut defaulted),
                0
            );
            assert_ne!(
                GetSecurityDescriptorGroup(sd.0, &mut group, &mut defaulted),
                0
            );
            let owner_hex = compare_sid(owner, decoded.owner);
            let group_hex = compare_sid(group, decoded.group);
            let dacl_hex = compare_dacl(sd.0, &decoded);
            assert_eq!(decoded.as_bytes(), bytes);
            capture.push_str(&format!(
                "{sddl}\t{}\t{control}\t{owner_hex}\t{group_hex}\t{dacl_hex}\n",
                hex(bytes)
            ));
        }
    }
    let path = std::env::var_os("NTFS_SD_CAPTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("security-descriptor-windows.tsv"));
    std::fs::write(path, capture).expect("retain Windows oracle capture");
}

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSidToSidW(text: *const u16, sid: *mut *mut c_void) -> i32;
    fn EqualSid(a: *const c_void, b: *const c_void) -> i32;
    fn AddAccessAllowedObjectAce(
        acl: *mut c_void,
        revision: u32,
        ace_flags: u32,
        mask: u32,
        object: *const c_void,
        inherited: *const c_void,
        sid: *const c_void,
    ) -> i32;
}
#[link(name = "rpcrt4")]
extern "system" {
    fn UuidFromStringW(text: *const u16, uuid: *mut [u8; 16]) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Windows parses the GUID text; its in-memory GUID is the stored byte order.
fn windows_guid(text: Option<&str>) -> Option<[u8; 16]> {
    let text = wide(text?);
    let mut guid = [0u8; 16];
    assert_eq!(unsafe { UuidFromStringW(text.as_ptr(), &mut guid) }, 0);
    Some(guid)
}

#[test]
fn windows_authored_object_aces_match_windows_guids_and_sids() {
    const OBJECT: &str = "00299570-246d-11d0-a768-00aa006e0529";
    const INHERITED: &str = "bf967aba-0de6-11d0-a285-00aa003049e2";
    let cases = [
        ("O:SYG:BAD:(OA;;CR;;;WD)".to_string(), None, None),
        (
            format!("O:SYG:BAD:(OA;;CR;{OBJECT};;WD)"),
            Some(OBJECT),
            None,
        ),
        (
            format!("O:SYG:BAD:(OD;;RP;;{INHERITED};WD)"),
            None,
            Some(INHERITED),
        ),
        (
            format!("O:SYG:BAD:(OA;CI;RPWP;{OBJECT};{INHERITED};WD)"),
            Some(OBJECT),
            Some(INHERITED),
        ),
    ];
    let world = wide("WD");
    let mut world_sid = ptr::null_mut();
    assert_ne!(
        unsafe { ConvertStringSidToSidW(world.as_ptr(), &mut world_sid) },
        0
    );
    let world_sid = WindowsDescriptor(world_sid);
    for (sddl, object, inherited) in cases {
        let text = wide(&sddl);
        let mut address = ptr::null_mut();
        let mut size = 0;
        unsafe {
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut address,
                    &mut size
                ),
                0,
                "Windows SDDL capture {sddl}: {}",
                std::io::Error::last_os_error()
            );
            let sd = WindowsDescriptor(address);
            let bytes = std::slice::from_raw_parts(sd.0.cast::<u8>(), size as usize);
            let decoded = SecurityDescriptor::parse(bytes).expect("decode Windows descriptor");
            compare_dacl(sd.0, &decoded);
            let aces: Vec<_> = decoded.dacl().expect("DACL").aces().collect();
            assert_eq!(aces.len(), 1, "{sddl}");
            if object.is_none() && inherited.is_none() {
                // The SDDL converter explicitly canonicalizes GUID-free OA
                // strings to ACCESS_ALLOWED_ACE. Keep this case as a strict
                // check of Windows' result, not an object-layout expectation:
                // https://learn.microsoft.com/en-us/windows/win32/api/sddl/nf-sddl-convertstringsecuritydescriptortosecuritydescriptorw
                assert_eq!(aces[0].ace_type, 0, "{sddl}");
                assert!(aces[0].object().expect("bounded ordinary ACE").is_none());
                let ordinary = aces[0].as_bytes();
                assert_eq!(
                    u32::from_le_bytes(ordinary[4..8].try_into().unwrap()),
                    0x100
                );
                assert_eq!(ordinary.len(), 8 + GetLengthSid(world_sid.0) as usize);
                assert_ne!(EqualSid(world_sid.0, ordinary[8..].as_ptr().cast()), 0);
                continue;
            }
            let view = aces[0].object().expect("bounded").expect("object ACE");
            let flags = u32::from(object.is_some()) | (u32::from(inherited.is_some()) << 1);
            assert_eq!(view.object_flags, flags, "{sddl}");
            assert_eq!(view.object_type, windows_guid(object), "{sddl}");
            assert_eq!(
                view.inherited_object_type,
                windows_guid(inherited),
                "{sddl}"
            );
            assert_ne!(
                EqualSid(world_sid.0, view.sid.as_bytes().as_ptr().cast()),
                0,
                "{sddl}"
            );
            assert_eq!(
                GetLengthSid(world_sid.0) as usize,
                view.sid.as_bytes().len()
            );
        }
    }

    // Ask the native ACL API, independently of the SDDL converter, for an
    // object ACE with neither GUID. Windows authors an ordinary
    // ACCESS_ALLOWED ACE (type 0, no object flags or GUIDs) rather than a
    // type-5 ACE with zero flags: observed on the Windows runner in run
    // 38834450586 (left 0, right 5). The parser must report that ACE as
    // ordinary; the unit matrix still covers the zero-flag object layout.
    // https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-addaccessallowedobjectace
    unsafe {
        let mut acl = [0u64; 32];
        let acl_ptr = acl.as_mut_ptr().cast::<c_void>();
        assert_ne!(InitializeAcl(acl_ptr, 256, 4), 0);
        assert_ne!(
            AddAccessAllowedObjectAce(acl_ptr, 4, 0, 0x100, ptr::null(), ptr::null(), world_sid.0),
            0,
            "Windows zero-GUID object ACE: {}",
            std::io::Error::last_os_error()
        );
        let mut absolute = [0u64; 8];
        let abs = absolute.as_mut_ptr().cast::<c_void>();
        assert_ne!(InitializeSecurityDescriptor(abs, 1), 0);
        assert_ne!(SetSecurityDescriptorDacl(abs, 1, acl_ptr, 0), 0);
        let mut relative = [0u64; 64];
        let rel = relative.as_mut_ptr().cast::<c_void>();
        let mut size = (relative.len() * 8) as u32;
        assert_ne!(MakeSelfRelativeSD(abs, rel, &mut size), 0);
        assert_ne!(IsValidSecurityDescriptor(rel), 0);
        let bytes = std::slice::from_raw_parts(rel.cast::<u8>(), size as usize);
        let decoded = SecurityDescriptor::parse(bytes).expect("Windows native object descriptor");
        compare_dacl(rel, &decoded);
        let aces: Vec<_> = decoded.dacl().expect("native DACL").aces().collect();
        assert_eq!(aces.len(), 1);
        assert_eq!(
            aces[0].ace_type, 0,
            "native API authors an ordinary ACE without GUIDs"
        );
        assert!(aces[0].object().expect("bounded ordinary ACE").is_none());
        let ordinary = aces[0].as_bytes();
        assert_eq!(
            u32::from_le_bytes(ordinary[4..8].try_into().unwrap()),
            0x100
        );
        assert_eq!(ordinary.len(), 8 + GetLengthSid(world_sid.0) as usize);
        assert_ne!(EqualSid(world_sid.0, ordinary[8..].as_ptr().cast()), 0);
    }
}

#[link(name = "advapi32")]
extern "system" {
    fn AddAce(acl: *mut c_void, revision: u32, start: u32, list: *const c_void, len: u32) -> i32;
    fn IsValidAcl(acl: *const c_void) -> i32;
    fn SetSecurityDescriptorOwner(sd: *mut c_void, owner: *mut c_void, defaulted: i32) -> i32;
    fn SetSecurityDescriptorGroup(sd: *mut c_void, group: *mut c_void, defaulted: i32) -> i32;
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn DuplicateToken(token: *mut c_void, level: u32, duplicate: *mut *mut c_void) -> i32;
    fn CheckTokenMembership(token: *mut c_void, sid: *mut c_void, member: *mut i32) -> i32;
    fn AccessCheckByType(
        sd: *const c_void,
        principal_self: *mut c_void,
        token: *mut c_void,
        desired: u32,
        object_types: *mut c_void,
        object_type_count: u32,
        mapping: *const [u32; 4],
        privileges: *mut c_void,
        privileges_len: *mut u32,
        granted: *mut u32,
        status: *mut i32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[link(name = "bcrypt")]
extern "system" {
    fn BCryptHash(
        algorithm: *mut c_void,
        secret: *const u8,
        secret_len: u32,
        input: *const u8,
        input_len: u32,
        output: *mut u8,
        output_len: u32,
    ) -> i32;
}

/// SHA-256 computed by Windows CNG (BCRYPT_SHA256_ALG_HANDLE); the workflow
/// recomputes it with coreutils, so neither side vouches for itself.
fn windows_sha256(bytes: &[u8]) -> String {
    let mut digest = [0u8; 32];
    let status = unsafe {
        BCryptHash(
            0x41 as *mut c_void,
            ptr::null(),
            0,
            bytes.as_ptr(),
            bytes.len() as u32,
            digest.as_mut_ptr(),
            32,
        )
    };
    assert_eq!(status, 0, "Windows BCryptHash SHA-256 status {status:#x}");
    hex(&digest)
}

/// Retains the descriptor Windows produced and what Windows decided about it:
/// the MakeSelfRelativeSD output bytes unmodified, their SHA-256, the commit
/// and API chain that produced them, and the AccessCheckByType verdicts.
fn retain_zero_flag_evidence(
    ace: &[u8],
    stored: &[u8],
    descriptor: &[u8],
    verdicts: [(u32, (u32, bool)); 3],
) {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| "unset".into());
    let base = std::env::var_os("NTFS_SD_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("security-descriptor-zero-flag"));
    let mut lines = vec![
        ("head".to_string(), var("NTFS_SD_HEAD")),
        ("checkout".to_string(), var("GITHUB_SHA")),
        ("run".to_string(), format!("{}/{}", var("GITHUB_RUN_ID"), var("GITHUB_RUN_ATTEMPT"))),
        (
            "build".to_string(),
            format!(
                "{} {} debug_assertions={}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
                cfg!(debug_assertions)
            ),
        ),
        (
            "api".to_string(),
            "InitializeAcl(rev4)>AddAce(rev4)>IsValidAcl>GetAce>InitializeSecurityDescriptor>SetSecurityDescriptorDacl/Owner/Group(Everyone)>MakeSelfRelativeSD>IsValidSecurityDescriptor".to_string(),
        ),
        (
            "provenance".to_string(),
            "descriptor bytes are the MakeSelfRelativeSD output buffer truncated to its returned size, unmodified; input_ace is ours, stored_ace is what GetAce returned".to_string(),
        ),
        ("input_ace_hex".to_string(), hex(ace)),
        ("stored_ace_hex".to_string(), hex(stored)),
        ("descriptor_len".to_string(), descriptor.len().to_string()),
        ("descriptor_hex".to_string(), hex(descriptor)),
        ("descriptor_sha256".to_string(), windows_sha256(descriptor)),
    ];
    for (desired, (granted, status)) in verdicts {
        lines.push((
            format!("access_{desired:#x}"),
            format!("granted={granted:#x} status={status}"),
        ));
    }
    let text: String = lines
        .iter()
        .map(|(key, value)| format!("{key}\t{value}\n"))
        .collect();
    std::fs::write(base.with_extension("bin"), descriptor).expect("retain descriptor bytes");
    std::fs::write(base.with_extension("tsv"), text).expect("retain descriptor evidence");
}

/// Windows authorization, not our parser, decides what the descriptor grants:
/// AccessCheckByType against an impersonation copy of this process token.
unsafe fn windows_access(sd: *const c_void, token: *mut c_void, desired: u32) -> (u32, bool) {
    let mapping = [0x8000_0000u32, 0x4000_0000, 0x2000_0000, 0x1000_0000];
    let mut privileges = [0u32; 16];
    let mut privileges_len = (privileges.len() * 4) as u32;
    let (mut granted, mut status) = (0u32, 0i32);
    assert_ne!(
        AccessCheckByType(
            sd,
            ptr::null_mut(),
            token,
            desired,
            ptr::null_mut(),
            0,
            &mapping,
            privileges.as_mut_ptr().cast(),
            &mut privileges_len,
            &mut granted,
            &mut status,
        ),
        0,
        "Windows AccessCheckByType desired {desired:#x}: {}",
        std::io::Error::last_os_error()
    );
    (granted, status != 0)
}

/// MS-DTYP 2.4.4.3: ACCESS_ALLOWED_OBJECT_ACE with Flags 0 carries neither
/// GUID. AddAccessAllowedObjectAce never authors it (see above), so the raw
/// ACE goes to Windows through AddAce and Windows alone judges it: IsValidAcl,
/// IsValidSecurityDescriptor and GetAce read back the bytes Windows stored.
/// A truncated ACE is the malformed control Windows must refuse.
/// https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-addace
/// https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/c79a383c-2b3f-4655-abe7-dcbb7ce0cfbe
#[test]
fn windows_accepts_zero_flag_object_ace_through_add_ace() {
    let world = wide("WD");
    let mut world_sid = ptr::null_mut();
    assert_ne!(
        unsafe { ConvertStringSidToSidW(world.as_ptr(), &mut world_sid) },
        0
    );
    let world_sid = WindowsDescriptor(world_sid);
    unsafe {
        let sid_len = GetLengthSid(world_sid.0) as usize;
        let sid = std::slice::from_raw_parts(world_sid.0.cast::<u8>(), sid_len);
        let size = 12 + sid_len;
        let mut ace = vec![5u8, 0];
        ace.extend_from_slice(&(size as u16).to_le_bytes());
        ace.extend_from_slice(&0x100u32.to_le_bytes());
        ace.extend_from_slice(&0u32.to_le_bytes());
        ace.extend_from_slice(sid);

        // Malformed control: the header claims fewer bytes than the SID needs.
        // Only the claimed 12 bytes go to AddAce: it walks the list by
        // AceSize, and the SID bytes beyond them read as an ACE of size 0,
        // which it never steps past.
        let mut short = ace.clone();
        short[2..4].copy_from_slice(&12u16.to_le_bytes());
        short.truncate(12);
        let mut bad = [0u64; 32];
        let bad_ptr = bad.as_mut_ptr().cast::<c_void>();
        assert_ne!(InitializeAcl(bad_ptr, 256, 4), 0);
        let added = AddAce(
            bad_ptr,
            4,
            u32::MAX,
            short.as_ptr().cast(),
            short.len() as u32,
        );
        assert!(
            added == 0 || IsValidAcl(bad_ptr) == 0,
            "Windows must refuse a truncated object ACE"
        );

        let mut acl = [0u64; 32];
        let acl_ptr = acl.as_mut_ptr().cast::<c_void>();
        assert_ne!(InitializeAcl(acl_ptr, 256, 4), 0);
        assert_ne!(
            AddAce(acl_ptr, 4, u32::MAX, ace.as_ptr().cast(), size as u32),
            0,
            "Windows AddAce zero-flag object ACE: {}",
            std::io::Error::last_os_error()
        );
        assert_ne!(
            IsValidAcl(acl_ptr),
            0,
            "Windows IsValidAcl zero-flag object ACE"
        );
        let mut stored = ptr::null_mut();
        assert_ne!(GetAce(acl_ptr, 0, &mut stored), 0);
        let native = std::slice::from_raw_parts(stored.cast::<u8>(), size);
        assert_eq!(native, &ace[..], "Windows stored ACE bytes");

        let mut absolute = [0u64; 8];
        let abs = absolute.as_mut_ptr().cast::<c_void>();
        assert_ne!(InitializeSecurityDescriptor(abs, 1), 0);
        assert_ne!(SetSecurityDescriptorDacl(abs, 1, acl_ptr, 0), 0);
        // AccessCheckByType refuses a descriptor without owner and group.
        assert_ne!(SetSecurityDescriptorOwner(abs, world_sid.0, 0), 0);
        assert_ne!(SetSecurityDescriptorGroup(abs, world_sid.0, 0), 0);
        let mut relative = [0u64; 64];
        let rel = relative.as_mut_ptr().cast::<c_void>();
        let mut rel_size = (relative.len() * 8) as u32;
        assert_ne!(MakeSelfRelativeSD(abs, rel, &mut rel_size), 0);
        assert_ne!(IsValidSecurityDescriptor(rel), 0);

        // Rights semantics come from Windows authorization on the token that
        // holds Everyone: the ACE's 0x100 is granted, a right it omits is not.
        let mut process = ptr::null_mut();
        assert_ne!(
            OpenProcessToken(GetCurrentProcess(), 0x0002 | 0x0008, &mut process),
            0
        );
        let mut token = ptr::null_mut();
        assert_ne!(DuplicateToken(process, 2, &mut token), 0);
        let mut member = 0i32;
        assert_ne!(CheckTokenMembership(token, world_sid.0, &mut member), 0);
        assert_ne!(member, 0, "the impersonation token holds Everyone");
        let allowed = windows_access(rel, token, 0x100);
        let denied = windows_access(rel, token, 0x1);
        let denied_with = windows_access(rel, token, 0x101);
        CloseHandle(token);
        CloseHandle(process);
        // Retained before the verdict assertions so a Windows refusal is kept too.
        let produced = std::slice::from_raw_parts(rel.cast::<u8>(), rel_size as usize);
        retain_zero_flag_evidence(
            &ace,
            native,
            produced,
            [(0x100, allowed), (0x1, denied), (0x101, denied_with)],
        );
        assert_eq!(allowed, (0x100, true), "Windows grants the ACE's right");
        assert!(
            !denied.1,
            "Windows denies a right the ACE omits: {denied:?}"
        );
        assert!(
            !denied_with.1,
            "Windows denies a request beyond the ACE: {denied_with:?}"
        );

        let bytes = std::slice::from_raw_parts(rel.cast::<u8>(), rel_size as usize);
        let decoded = SecurityDescriptor::parse(bytes).expect("Windows zero-flag descriptor");
        assert_eq!(decoded.owner.map(|sid| sid.as_bytes()), Some(&native[12..]));
        compare_dacl(rel, &decoded);
        let aces: Vec<_> = decoded.dacl().expect("DACL").aces().collect();
        assert_eq!(aces.len(), 1);
        assert_eq!(aces[0].ace_type, 5);
        let view = aces[0].object().expect("bounded").expect("object ACE");
        assert_eq!(view.object_flags, 0);
        assert_eq!(view.object_type, None);
        assert_eq!(view.inherited_object_type, None);
        assert_eq!(u32::from_le_bytes(native[4..8].try_into().unwrap()), 0x100);
        assert_ne!(
            EqualSid(world_sid.0, view.sid.as_bytes().as_ptr().cast()),
            0
        );
        assert_eq!(view.sid.as_bytes(), &native[12..]);
    }
}

#[link(name = "advapi32")]
extern "system" {
    fn InitializeSecurityDescriptor(sd: *mut c_void, revision: u32) -> i32;
    fn SetSecurityDescriptorDacl(
        sd: *mut c_void,
        present: i32,
        acl: *mut c_void,
        defaulted: i32,
    ) -> i32;
    fn SetSecurityDescriptorSacl(
        sd: *mut c_void,
        present: i32,
        acl: *mut c_void,
        defaulted: i32,
    ) -> i32;
    fn SetSecurityDescriptorControl(sd: *mut c_void, mask: u16, bits: u16) -> i32;
    fn GetSecurityDescriptorSacl(
        sd: *const c_void,
        present: *mut i32,
        acl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn MakeSelfRelativeSD(absolute: *mut c_void, relative: *mut c_void, size: *mut u32) -> i32;
    fn InitializeAcl(acl: *mut c_void, size: u32, revision: u32) -> i32;
    fn AddAccessAllowedAce(acl: *mut c_void, revision: u32, mask: u32, sid: *mut c_void) -> i32;
    fn AddAuditAccessAce(
        acl: *mut c_void,
        revision: u32,
        mask: u32,
        sid: *mut c_void,
        success: i32,
        failure: i32,
    ) -> i32;
}

/// One ACL role as Windows reports it: present flag, pointer offset, defaulted
/// flag, AceCount and AclBytesInUse + AclBytesFree (the AclSize).
#[derive(Debug, PartialEq, Eq)]
struct WindowsAclRole {
    present: bool,
    offset: Option<usize>,
    defaulted: bool,
    ace_count: u32,
    acl_size: u32,
}

unsafe fn windows_role(sd: *const c_void, sacl: bool) -> WindowsAclRole {
    let (mut present, mut acl, mut defaulted) = (0, ptr::null_mut(), 0);
    let ok = if sacl {
        GetSecurityDescriptorSacl(sd, &mut present, &mut acl, &mut defaulted)
    } else {
        GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted)
    };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
    let mut info = [0u32; 3];
    if !acl.is_null() {
        assert_ne!(GetAclInformation(acl, info.as_mut_ptr().cast(), 12, 2), 0);
    }
    WindowsAclRole {
        present: present != 0,
        offset: (!acl.is_null()).then(|| acl as usize - sd as usize),
        defaulted: defaulted != 0,
        ace_count: info[0],
        acl_size: info[1] + info[2],
    }
}

fn our_role(state: AclState<'_>, offset: u32, defaulted: bool) -> WindowsAclRole {
    let acl = state.acl();
    WindowsAclRole {
        present: !matches!(state, AclState::Absent),
        offset: acl.map(|_| offset as usize),
        defaulted,
        ace_count: acl.map_or(0, |a| u32::from(a.ace_count())),
        acl_size: acl.map_or(0, |a| a.as_bytes().len() as u32),
    }
}

#[derive(Clone, Copy, Debug)]
enum Role {
    Absent,
    Null,
    Empty,
    NonEmpty,
}

#[test]
fn windows_built_acl_presence_states_match_both_roles() {
    let world = wide("WD");
    let mut world_sid = ptr::null_mut();
    assert_ne!(
        unsafe { ConvertStringSidToSidW(world.as_ptr(), &mut world_sid) },
        0
    );
    let world_sid = WindowsDescriptor(world_sid);
    let roles = [Role::Absent, Role::Null, Role::Empty, Role::NonEmpty];
    let mut executed = 0;
    for dacl in roles {
        for sacl in roles {
            for flags in [0u16, 0x1000 | 0x2000] {
                let defaulted = i32::from(flags != 0);
                let mut absolute = [0u64; 8];
                let mut dacl_buf = [0u64; 16];
                let mut sacl_buf = [0u64; 16];
                let mut relative = [0u64; 64];
                let mut size = (relative.len() * 8) as u32;
                unsafe {
                    let abs = absolute.as_mut_ptr().cast::<c_void>();
                    assert_ne!(InitializeSecurityDescriptor(abs, 1), 0);
                    let build = |role: Role, buf: *mut c_void, audit: bool| -> (i32, *mut c_void) {
                        match role {
                            Role::Absent => (0, ptr::null_mut()),
                            Role::Null => (1, ptr::null_mut()),
                            Role::Empty | Role::NonEmpty => {
                                assert_ne!(InitializeAcl(buf, 128, 2), 0);
                                if matches!(role, Role::NonEmpty) {
                                    let added = if audit {
                                        AddAuditAccessAce(buf, 2, 0x1, world_sid.0, 1, 0)
                                    } else {
                                        AddAccessAllowedAce(buf, 2, 0x1, world_sid.0)
                                    };
                                    assert_ne!(added, 0);
                                }
                                (1, buf)
                            }
                        }
                    };
                    let (dp, da) = build(dacl, dacl_buf.as_mut_ptr().cast(), false);
                    let (sp, sa) = build(sacl, sacl_buf.as_mut_ptr().cast(), true);
                    assert_ne!(SetSecurityDescriptorDacl(abs, dp, da, defaulted), 0);
                    assert_ne!(SetSecurityDescriptorSacl(abs, sp, sa, defaulted), 0);
                    if flags != 0 {
                        assert_ne!(SetSecurityDescriptorControl(abs, flags, flags), 0);
                    }
                    let rel = relative.as_mut_ptr().cast::<c_void>();
                    assert_ne!(
                        MakeSelfRelativeSD(abs, rel, &mut size),
                        0,
                        "{dacl:?}/{sacl:?}: {}",
                        std::io::Error::last_os_error()
                    );
                    assert_ne!(IsValidSecurityDescriptor(rel), 0);
                    let bytes = std::slice::from_raw_parts(rel.cast::<u8>(), size as usize);
                    let decoded = SecurityDescriptor::parse(bytes).expect("decode Windows SD");
                    let mut control = 0;
                    let mut revision = 0;
                    assert_ne!(
                        GetSecurityDescriptorControl(rel, &mut control, &mut revision),
                        0
                    );
                    assert_eq!(decoded.header.control, control, "{dacl:?}/{sacl:?}");
                    assert_eq!(control & flags, flags, "protected bits retained");
                    let h = decoded.header;
                    let wd = windows_role(rel, false);
                    let ws = windows_role(rel, true);
                    assert_eq!(
                        our_role(decoded.dacl_state(), h.dacl_offset, h.control & 0x8 != 0),
                        wd,
                        "DACL {dacl:?}/{sacl:?} {flags:#x}"
                    );
                    assert_eq!(
                        our_role(decoded.sacl_state(), h.sacl_offset, h.control & 0x20 != 0),
                        ws,
                        "SACL {dacl:?}/{sacl:?} {flags:#x}"
                    );
                    // Windows' own reading selects the expected variant.
                    for (state, w, role) in [
                        (decoded.dacl_state(), &wd, dacl),
                        (decoded.sacl_state(), &ws, sacl),
                    ] {
                        let expected = match (w.present, w.offset, w.ace_count) {
                            (false, _, _) => "Absent",
                            (true, None, _) => "Null",
                            (true, Some(_), 0) => "Empty",
                            (true, Some(_), _) => "NonEmpty",
                        };
                        let ours = match state {
                            AclState::Absent => "Absent",
                            AclState::Null => "Null",
                            AclState::Empty(_) => "Empty",
                            AclState::NonEmpty(_) => "NonEmpty",
                        };
                        assert_eq!(ours, expected, "{role:?}");
                        assert_eq!(format!("{role:?}"), expected, "Windows kept the built role");
                        if let Some(acl) = state.acl() {
                            let at = w.offset.unwrap();
                            assert_eq!(acl.as_bytes(), &bytes[at..at + w.acl_size as usize]);
                        }
                    }
                    // Negative controls on Windows-authored bytes: an ACL
                    // offset with its present bit cleared, and an offset past
                    // the end, are refused rather than repaired.
                    for (offset_at, present) in [(16usize, 0x4u16), (12, 0x10)] {
                        let at =
                            u32::from_le_bytes(bytes[offset_at..offset_at + 4].try_into().unwrap());
                        if at == 0 {
                            continue;
                        }
                        let mut cleared = bytes.to_vec();
                        let c = control & !present;
                        cleared[2..4].copy_from_slice(&c.to_le_bytes());
                        assert!(SecurityDescriptor::parse(&cleared).is_err());
                        let mut past = bytes.to_vec();
                        past[offset_at..offset_at + 4].copy_from_slice(&(size).to_le_bytes());
                        assert!(SecurityDescriptor::parse(&past).is_err());
                    }
                }
                executed += 1;
            }
        }
    }
    assert_eq!(executed, 32, "every DACL x SACL x control combination ran");
}
