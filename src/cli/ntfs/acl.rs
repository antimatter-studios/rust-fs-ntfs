//! `fs.ntfs acl show`: a file's persisted security descriptor as JSON.
//!
//! Read-only. The descriptor is decoded by the library's bounded
//! `SecurityDescriptor` view and reported as stored: owner and group SIDs,
//! the raw control bits, and each ACL's ACEs in on-disk order with their raw
//! type and flags. Nothing is evaluated, canonicalised or reordered, and an
//! ACE type this tool does not name is reported by number with its bytes.
//!
//! A DACL has three states Windows distinguishes and so does this output:
//! absent (`SE_DACL_PRESENT` clear), null (present with offset zero, which
//! grants everyone everything) and empty (present with zero ACEs, which
//! grants nobody anything).

use fs_core::cli::Json;
use fs_ntfs::attr_io::AttrType;
use fs_ntfs::error::{Error, Kind};
use fs_ntfs::read;
use fs_ntfs::security_descriptor::{Acl, SecurityDescriptor, Sid};

use super::device::Device;

const SE_DACL_PRESENT: u16 = 0x0004;
const SE_SACL_PRESENT: u16 = 0x0010;

/// $Secure is a fixed system record on every NTFS 3.x volume.
const SECURE_RECORD: u64 = 9;
/// An $SDS entry header: hash, security id, offset, length.
const SDS_HEADER: usize = 20;

/// A SID in the `S-R-A-S1-...` form Windows prints.
pub fn sid_string(sid: Sid<'_>) -> String {
    let authority = sid.identifier_authority();
    let mut text = if authority >= 1 << 32 {
        format!("S-{}-0x{:012X}", sid.revision(), authority)
    } else {
        format!("S-{}-{}", sid.revision(), authority)
    };
    for sub in sid.subauthorities() {
        text.push_str(&format!("-{sub}"));
    }
    text
}

fn ace_type_name(ace_type: u8) -> Option<&'static str> {
    Some(match ace_type {
        0x00 => "access_allowed",
        0x01 => "access_denied",
        0x02 => "system_audit",
        0x03 => "system_alarm",
        0x05 => "access_allowed_object",
        0x06 => "access_denied_object",
        0x07 => "system_audit_object",
        0x09 => "access_allowed_callback",
        0x0A => "access_denied_callback",
        0x11 => "system_mandatory_label",
        _ => return None,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn acl_json(control: u16, present_bit: u16, offset: u32, acl: Option<Acl<'_>>) -> Json {
    let state = if control & present_bit == 0 {
        "absent"
    } else if offset == 0 {
        "null"
    } else {
        match acl {
            Some(a) if a.ace_count() == 0 => "empty",
            Some(_) => "present",
            None => "null",
        }
    };
    let mut fields = vec![("state", Json::from(state))];
    if let Some(acl) = acl {
        fields.push(("revision", Json::from(u64::from(acl.revision()))));
        let aces: Vec<Json> = acl
            .aces()
            .enumerate()
            .map(|(index, ace)| {
                let bytes = ace.as_bytes();
                Json::object([
                    ("index", Json::from(index as u64)),
                    ("type", Json::from(u64::from(ace.ace_type))),
                    ("type_name", Json::from(ace_type_name(ace.ace_type))),
                    ("flags", Json::from(u64::from(ace.flags))),
                    ("bytes", Json::from(hex(bytes))),
                ])
            })
            .collect();
        fields.push(("aces", Json::from(aces)));
    }
    Json::object(fields)
}

/// The JSON report for one self-relative descriptor.
pub fn descriptor_json(bytes: &[u8]) -> Result<Json, String> {
    let sd = SecurityDescriptor::parse(bytes)
        .map_err(|e| format!("security descriptor: {:?} at offset {}", e.kind, e.offset))?;
    let h = sd.header;
    Ok(Json::object([
        ("revision", Json::from(u64::from(h.revision))),
        ("control", Json::from(u64::from(h.control))),
        ("owner", Json::from(sd.owner.map(sid_string))),
        ("group", Json::from(sd.group.map(sid_string))),
        (
            "dacl",
            acl_json(h.control, SE_DACL_PRESENT, h.dacl_offset, sd.dacl()),
        ),
        (
            "sacl",
            acl_json(h.control, SE_SACL_PRESENT, h.sacl_offset, sd.sacl()),
        ),
        ("bytes", Json::from(hex(bytes))),
    ]))
}

/// The descriptor bytes a file resolves to: its own $SECURITY_DESCRIPTOR
/// attribute when it carries one, else the $Secure:$SDS entry its
/// $STANDARD_INFORMATION security id names.
pub fn file_descriptor(dev: &mut Device, path: &str) -> Result<Vec<u8>, Error> {
    let record = read::resolve_path(dev, path)?;
    if let Some(own) =
        read::read_attribute_value_if_present(dev, record, AttrType::SecurityDescriptor, None)?
    {
        return Ok(own);
    }
    let Some(id) = fs_ntfs::write::read_security_id_io(dev, path)? else {
        return Err(Error::new(
            Kind::NotFound,
            format!("{path}: no $SECURITY_DESCRIPTOR and no security id"),
        ));
    };
    let sds =
        read::read_attribute_value_if_present(dev, SECURE_RECORD, AttrType::Data, Some("$SDS"))?
            .ok_or_else(|| Error::new(Kind::NotFound, "$Secure has no $SDS stream"))?;
    sds_lookup(&sds, id)
        .ok_or_else(|| Error::new(Kind::NotFound, format!("security id {id} not in $SDS")))
}

/// Walks $SDS entries (16-byte aligned) for the one carrying `id`.
fn sds_lookup(sds: &[u8], id: u32) -> Option<Vec<u8>> {
    let mut at = 0usize;
    while at + SDS_HEADER <= sds.len() {
        let field = |o: usize| u32::from_le_bytes(sds[at + o..at + o + 4].try_into().unwrap());
        let entry_id = field(4);
        let len = field(16) as usize;
        if len < SDS_HEADER || at + len > sds.len() {
            at = (at + 16) & !15;
            continue;
        }
        if entry_id == id {
            return Some(sds[at + SDS_HEADER..at + len].to_vec());
        }
        at = (at + len + 15) & !15;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERYONE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
    const SYSTEM: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];

    /// A self-relative descriptor: header, owner, group, then the DACL.
    fn descriptor(control: u16, dacl: Option<&[u8]>) -> Vec<u8> {
        let mut b = vec![1u8, 0];
        b.extend_from_slice(&(control | 0x8000).to_le_bytes());
        let owner = 20u32;
        let group = owner + 12;
        let dacl_off = if dacl.is_some() { group + 12 } else { 0 };
        for v in [owner, group, 0, dacl_off] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&SYSTEM);
        b.extend_from_slice(&EVERYONE);
        if let Some(d) = dacl {
            b.extend_from_slice(d);
        }
        b
    }

    fn acl(aces: &[Vec<u8>]) -> Vec<u8> {
        let size = 8 + aces.iter().map(Vec::len).sum::<usize>();
        let mut b = vec![2u8, 0];
        b.extend_from_slice(&(size as u16).to_le_bytes());
        b.extend_from_slice(&(aces.len() as u16).to_le_bytes());
        b.extend_from_slice(&[0, 0]);
        for a in aces {
            b.extend_from_slice(a);
        }
        b
    }

    fn ace(ace_type: u8, flags: u8, mask: u32, sid: &[u8]) -> Vec<u8> {
        let mut b = vec![ace_type, flags];
        b.extend_from_slice(&((8 + sid.len()) as u16).to_le_bytes());
        b.extend_from_slice(&mask.to_le_bytes());
        b.extend_from_slice(sid);
        b
    }

    fn dacl_state(json: &Json) -> String {
        json.get("dacl").unwrap().get("state").unwrap().to_text()
    }

    #[test]
    fn absent_null_and_empty_dacls_are_distinct() {
        let absent = descriptor_json(&descriptor(0, None)).unwrap();
        let null = descriptor_json(&descriptor(SE_DACL_PRESENT, None)).unwrap();
        let empty = descriptor_json(&descriptor(SE_DACL_PRESENT, Some(&acl(&[])))).unwrap();
        assert_eq!(dacl_state(&absent), "absent");
        assert_eq!(dacl_state(&null), "null");
        assert_eq!(dacl_state(&empty), "empty");
        assert!(null.get("dacl").unwrap().get("aces").is_none());
        assert!(empty.get("dacl").unwrap().get("aces").is_some());
    }

    #[test]
    fn owner_and_group_are_the_persisted_sids() {
        let json = descriptor_json(&descriptor(0, None)).unwrap();
        assert_eq!(json.get("owner").unwrap().to_text(), "S-1-5-18");
        assert_eq!(json.get("group").unwrap().to_text(), "S-1-1-0");
    }

    #[test]
    fn aces_keep_their_order_flags_and_unknown_types() {
        let aces = [
            ace(0x01, 0x03, 0x0012_0089, &EVERYONE),
            ace(0x00, 0x10, 0x001F_01FF, &SYSTEM),
            ace(0x13, 0x00, 0, &SYSTEM),
        ];
        let json = descriptor_json(&descriptor(SE_DACL_PRESENT, Some(&acl(&aces)))).unwrap();
        let dacl = json.get("dacl").unwrap();
        assert_eq!(dacl.get("state").unwrap().to_text(), "present");
        let text = dacl.get("aces").unwrap().to_text();
        let first = text.find("access_denied").unwrap();
        let second = text.find("access_allowed").unwrap();
        assert!(first < second, "on-disk order is kept: {text}");
        assert!(
            text.contains(&hex(&aces[2])),
            "unknown ACE bytes kept: {text}"
        );
    }

    #[test]
    fn sds_lookup_finds_the_entry_by_security_id() {
        let sd = descriptor(0, None);
        let mut sds = Vec::new();
        for (id, body) in [(0x100u32, vec![0u8; 8]), (0x101, sd.clone())] {
            let start = sds.len();
            let len = (SDS_HEADER + body.len()) as u32;
            sds.extend_from_slice(&0u32.to_le_bytes());
            sds.extend_from_slice(&id.to_le_bytes());
            sds.extend_from_slice(&(start as u64).to_le_bytes());
            sds.extend_from_slice(&len.to_le_bytes());
            sds.extend_from_slice(&body);
            sds.resize((sds.len() + 15) & !15, 0);
        }
        assert_eq!(sds_lookup(&sds, 0x101), Some(sd));
        assert_eq!(sds_lookup(&sds, 0x102), None);
    }
}
