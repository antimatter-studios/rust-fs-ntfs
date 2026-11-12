//! Linux ownership and mode, kept where WSL keeps them: the `$LXUID`,
//! `$LXGID` and `$LXMOD` extended attributes (#599).
//!
//! Microsoft's [WSL file permissions] page names the attributes and what
//! each holds: the owner's user ID, the owner's group ID, and the file mode
//! with its type and permission octals. It does not give their bytes. Each
//! value is four bytes, a little-endian `u32`, and the kernel oracle in
//! `tests/kernel_ntfs3.rs` grades that: Linux's ntfs3 reports the owner,
//! group and mode this module wrote, and this module reads the ones ntfs3
//! wrote for `chown` and `chmod`.
//!
//! The attributes sit beside the file's native security descriptor and
//! never replace it: Windows keeps enforcing its own access control list,
//! and a Linux reader takes ownership and mode from here. Nothing in this
//! module maps an identity to a SID or enforces a permission.
//!
//! A device, FIFO or socket needs its WSL reparse tag beside `$LXMOD`, and a
//! device its `$LXDEV`, so such a mode is refused rather than written as a
//! regular file that claims to be one (#1196).
//!
//! [WSL file permissions]: https://learn.microsoft.com/en-us/windows/wsl/file-permissions

use crate::ea_io::Ea;
use crate::error::Error;

/// The owner's user ID.
pub const LXUID: &[u8] = b"$LXUID";
/// The owner's group ID.
pub const LXGID: &[u8] = b"$LXGID";
/// The file mode: type and permission bits, as `st_mode`.
pub const LXMOD: &[u8] = b"$LXMOD";

/// The type bits of a mode.
pub const S_IFMT: u32 = 0o170_000;
/// A regular file's type bits.
pub const S_IFREG: u32 = 0o100_000;
/// A directory's type bits.
pub const S_IFDIR: u32 = 0o040_000;
/// The permission bits, with set-user-ID, set-group-ID and sticky.
pub const PERMISSION_BITS: u32 = 0o7777;

/// A file's Linux ownership and mode. `None` is an attribute the file does
/// not carry: a reader then falls back to its own default for that field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnixMetadata {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: Option<u32>,
}

impl UnixMetadata {
    /// True when no field is set.
    pub fn is_empty(&self) -> bool {
        self.uid.is_none() && self.gid.is_none() && self.mode.is_none()
    }
}

/// The ownership and mode a file's EAs carry.
///
/// A value of the wrong length is refused, naming the attribute: read as a
/// truncated or padded number it would hand a reader an owner nobody set.
pub fn from_eas(eas: &[Ea]) -> Result<UnixMetadata, Error> {
    let mut out = UnixMetadata::default();
    for ea in eas {
        let field = if ea.name.eq_ignore_ascii_case(LXUID) {
            &mut out.uid
        } else if ea.name.eq_ignore_ascii_case(LXGID) {
            &mut out.gid
        } else if ea.name.eq_ignore_ascii_case(LXMOD) {
            &mut out.mode
        } else {
            continue;
        };
        let bytes: [u8; 4] = ea.value.as_slice().try_into().map_err(|_| {
            Error::invalid(format!(
                "{} holds {} bytes; a WSL ownership or mode attribute holds 4",
                String::from_utf8_lossy(&ea.name),
                ea.value.len()
            ))
        })?;
        *field = Some(u32::from_le_bytes(bytes));
    }
    Ok(out)
}

/// The mode to store for a file of the given kind.
///
/// Permission bits alone take the file's type. Type bits must name the
/// file's own kind; any other type is refused, as is a bit outside the type
/// and permission fields.
pub fn checked_mode(mode: u32, is_dir: bool) -> Result<u32, Error> {
    if mode & !(S_IFMT | PERMISSION_BITS) != 0 {
        return Err(Error::invalid(format!(
            "mode {mode:#o} sets bits outside the file type and permissions"
        )));
    }
    let kind = if is_dir { S_IFDIR } else { S_IFREG };
    match mode & S_IFMT {
        0 => Ok(mode | kind),
        t if t == kind => Ok(mode),
        t if t == S_IFREG || t == S_IFDIR => Err(Error::invalid(format!(
            "mode {mode:#o} names a {}, but the file is a {}",
            if t == S_IFDIR {
                "directory"
            } else {
                "regular file"
            },
            if is_dir { "directory" } else { "regular file" }
        ))),
        _ => Err(Error::refused(format!(
            "mode {mode:#o} names a device, FIFO, socket or symbolic link, which needs a \
             WSL reparse point this crate does not write yet (#1196)"
        ))),
    }
}

/// Write the fields `meta` sets into `eas`, replacing any value already
/// there. A field that is `None` leaves its attribute as it is.
pub fn apply(meta: &UnixMetadata, eas: &mut Vec<Ea>) {
    for (name, value) in [(LXUID, meta.uid), (LXGID, meta.gid), (LXMOD, meta.mode)] {
        if let Some(value) = value {
            crate::ea_io::upsert(
                eas,
                Ea {
                    flags: 0,
                    name: name.to_vec(),
                    value: value.to_le_bytes().to_vec(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ea(name: &[u8], value: &[u8]) -> Ea {
        Ea {
            flags: 0,
            name: name.to_vec(),
            value: value.to_vec(),
        }
    }

    #[test]
    fn reads_each_attribute_as_a_little_endian_u32() {
        let eas = [
            ea(b"USER.OTHER", b"x"),
            ea(LXUID, &1234u32.to_le_bytes()),
            ea(LXGID, &5678u32.to_le_bytes()),
            ea(LXMOD, &0o100_640u32.to_le_bytes()),
        ];
        assert_eq!(
            from_eas(&eas).unwrap(),
            UnixMetadata {
                uid: Some(1234),
                gid: Some(5678),
                mode: Some(0o100_640),
            }
        );
    }

    #[test]
    fn a_file_without_the_attributes_has_none() {
        let meta = from_eas(&[ea(b"USER.OTHER", b"x")]).unwrap();
        assert!(meta.is_empty());
    }

    #[test]
    fn a_value_of_the_wrong_length_is_refused_by_name() {
        for value in [&[7u8, 0][..], &[0u8; 8][..], &[][..]] {
            let err = from_eas(&[ea(LXUID, value)]).unwrap_err();
            assert!(err.message().contains("$LXUID"), "{}", err.message());
        }
    }

    #[test]
    fn apply_replaces_set_fields_and_keeps_the_rest() {
        let mut eas = vec![
            ea(LXUID, &1u32.to_le_bytes()),
            ea(LXGID, &2u32.to_le_bytes()),
            ea(b"USER.OTHER", b"x"),
        ];
        apply(
            &UnixMetadata {
                uid: Some(1000),
                gid: None,
                mode: Some(0o100_600),
            },
            &mut eas,
        );
        let meta = from_eas(&eas).unwrap();
        assert_eq!(meta.uid, Some(1000));
        assert_eq!(meta.gid, Some(2));
        assert_eq!(meta.mode, Some(0o100_600));
        assert!(eas.iter().any(|e| e.name == b"USER.OTHER"));
        assert!(eas.iter().all(|e| e.flags == 0));
    }

    #[test]
    fn permission_bits_take_the_file_type() {
        assert_eq!(checked_mode(0o640, false).unwrap(), 0o100_640);
        assert_eq!(checked_mode(0o2750, true).unwrap(), 0o042_750);
        assert_eq!(checked_mode(0o100_644, false).unwrap(), 0o100_644);
    }

    #[test]
    fn a_mode_of_another_kind_is_refused() {
        assert_eq!(
            checked_mode(0o040_755, false).unwrap_err().kind(),
            crate::error::Kind::Invalid
        );
        assert_eq!(
            checked_mode(0o100_644, true).unwrap_err().kind(),
            crate::error::Kind::Invalid
        );
        for special in [0o020_644, 0o060_644, 0o010_644, 0o140_644, 0o120_777] {
            assert_eq!(
                checked_mode(special, false).unwrap_err().kind(),
                crate::error::Kind::Refused,
                "{special:#o}"
            );
        }
        assert!(checked_mode(0o1_000_644, false).is_err());
    }
}
