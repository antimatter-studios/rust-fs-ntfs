//! Executable Windows capture and independent owner/group/control comparison.
//! Run on Windows: cargo test --test security_descriptor_windows_oracle.
//! NTFS_SD_CAPTURE names the retained TSV capture (descriptor/control/SID bytes).
#![cfg(windows)]

use fs_ntfs::security_descriptor::{SecurityDescriptor, Sid};
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
        .unwrap_or_else(|| std::env::temp_dir().join("ntfs-security-descriptors.tsv"));
    std::fs::write(path, capture).expect("retain Windows oracle capture");
}
