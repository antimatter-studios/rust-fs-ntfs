//! Executable Windows capture and independent owner/group/control comparison.
//! Run on Windows: cargo test --test security_descriptor_windows_oracle.
//! NTFS_SD_CAPTURE names the retained TSV capture (descriptor/control/SID bytes).
#![cfg(windows)]

use fs_ntfs::security_descriptor::{SecurityDescriptor, Sid};
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

#[test]
fn windows_authored_descriptors_match_security_apis_and_are_captured() {
    let mut capture = String::from(
        "# Windows Advapi32 capture: sddl\tdescriptor_hex\tcontrol\towner_hex\tgroup_hex\n",
    );
    // Windows authors these bytes; ACL contents are only preserved by this slice.
    let cases = [
        "O:SYG:BA",
        "O:SY",
        "G:BA",
        "O:SYG:SYD:",
        "O:SYG:BAD:P(A;;FA;;;SY)",
        "O:SYG:BAD:AI(A;ID;FR;;;WD)S:(AU;SAFA;FR;;;WD)",
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
            assert_eq!(decoded.as_bytes(), bytes);
            capture.push_str(&format!(
                "{sddl}\t{}\t{control}\t{owner_hex}\t{group_hex}\n",
                hex(bytes)
            ));
        }
    }
    let path = std::env::var_os("NTFS_SD_CAPTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("ntfs-security-descriptors.tsv"));
    std::fs::write(path, capture).expect("retain Windows oracle capture");
}
