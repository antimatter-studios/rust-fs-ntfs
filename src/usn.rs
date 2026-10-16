//! Read-only decoding of `USN_RECORD_V2` change-journal records.
//!
//! Layout follows Microsoft's documented `USN_RECORD_V2` structure
//! (winioctl.h): a 60-byte fixed header followed by a UTF-16LE file name
//! located by `FileNameOffset` and `FileNameLength`. Every field is
//! little-endian. File references stay full 64-bit values (48-bit MFT
//! segment plus 16-bit sequence) so callers can detect slot reuse.
//!
//! Decoding is bounded by the caller's buffer and by the record's own
//! `RecordLength`; nothing is read past either. Versions other than 2.0
//! are refused explicitly rather than guessed at.

use std::fmt;

/// Size of the fixed `USN_RECORD_V2` header before `FileName`.
pub const USN_RECORD_V2_HEADER_LEN: usize = 60;

const OFF_RECORD_LENGTH: usize = 0;
const OFF_MAJOR_VERSION: usize = 4;
const OFF_MINOR_VERSION: usize = 6;
const OFF_FILE_REFERENCE: usize = 8;
const OFF_PARENT_REFERENCE: usize = 16;
const OFF_USN: usize = 24;
const OFF_TIMESTAMP: usize = 32;
const OFF_REASON: usize = 40;
const OFF_SOURCE_INFO: usize = 44;
const OFF_SECURITY_ID: usize = 48;
const OFF_FILE_ATTRIBUTES: usize = 52;
const OFF_FILE_NAME_LENGTH: usize = 56;
const OFF_FILE_NAME_OFFSET: usize = 58;

/// One decoded `USN_RECORD_V2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnRecordV2 {
    /// `RecordLength`: total bytes this record occupies, including padding.
    pub record_length: u32,
    /// Full 64-bit `FileReferenceNumber`.
    pub file_reference: u64,
    /// Full 64-bit `ParentFileReferenceNumber`.
    pub parent_file_reference: u64,
    /// The record's update sequence number.
    pub usn: i64,
    /// `TimeStamp` in Windows FILETIME units (100 ns since 1601-01-01 UTC).
    pub timestamp: i64,
    /// `Reason` flags (`USN_REASON_*`), all 32 bits kept.
    pub reason: u32,
    /// `SourceInfo` flags (`USN_SOURCE_*`).
    pub source_info: u32,
    pub security_id: u32,
    pub file_attributes: u32,
    /// File name as raw UTF-16LE code units (unpaired surrogates kept).
    pub file_name_utf16: Vec<u16>,
}

impl UsnRecordV2 {
    /// MFT segment number: the low 48 bits of `file_reference`.
    pub fn file_segment(&self) -> u64 {
        self.file_reference & 0x0000_FFFF_FFFF_FFFF
    }

    /// Sequence number: the high 16 bits of `file_reference`.
    pub fn file_sequence(&self) -> u16 {
        (self.file_reference >> 48) as u16
    }

    /// Lossy UTF-8 rendering of the file name, for display only.
    pub fn file_name_lossy(&self) -> String {
        String::from_utf16_lossy(&self.file_name_utf16)
    }
}

/// Why a buffer could not be decoded as a `USN_RECORD_V2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsnDecodeError {
    /// The buffer ends before the bytes the record claims or needs.
    Truncated { needed: usize, available: usize },
    /// `MajorVersion.MinorVersion` is not 2.0.
    UnsupportedVersion { major: u16, minor: u16 },
    /// `RecordLength` is smaller than the fixed header.
    RecordLengthTooSmall { record_length: u32 },
    /// The file name is not inside the record, or has an odd byte length.
    BadFileName {
        offset: u16,
        length: u16,
        record_length: u32,
    },
}

impl fmt::Display for UsnDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, available } => write!(
                f,
                "truncated USN record: needs {needed} bytes, {available} available"
            ),
            Self::UnsupportedVersion { major, minor } => {
                write!(f, "unsupported USN record version {major}.{minor}")
            }
            Self::RecordLengthTooSmall { record_length } => write!(
                f,
                "USN record length {record_length} is below the {USN_RECORD_V2_HEADER_LEN}-byte header"
            ),
            Self::BadFileName {
                offset,
                length,
                record_length,
            } => write!(
                f,
                "USN file name (offset {offset}, length {length}) does not fit record length {record_length}"
            ),
        }
    }
}

impl std::error::Error for UsnDecodeError {}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

/// Decode the `USN_RECORD_V2` at the start of `buf`.
///
/// Reads at most `RecordLength` bytes and never past `buf`. Returns the
/// record; advance by `record.record_length` to reach the next one.
pub fn decode_usn_record_v2(buf: &[u8]) -> Result<UsnRecordV2, UsnDecodeError> {
    if buf.len() < OFF_MINOR_VERSION + 2 {
        return Err(UsnDecodeError::Truncated {
            needed: OFF_MINOR_VERSION + 2,
            available: buf.len(),
        });
    }
    let record_length = u32_at(buf, OFF_RECORD_LENGTH);
    let major = u16_at(buf, OFF_MAJOR_VERSION);
    let minor = u16_at(buf, OFF_MINOR_VERSION);
    if (major, minor) != (2, 0) {
        return Err(UsnDecodeError::UnsupportedVersion { major, minor });
    }
    if (record_length as usize) < USN_RECORD_V2_HEADER_LEN {
        return Err(UsnDecodeError::RecordLengthTooSmall { record_length });
    }
    let len = record_length as usize;
    if buf.len() < len {
        return Err(UsnDecodeError::Truncated {
            needed: len,
            available: buf.len(),
        });
    }
    let rec = &buf[..len];
    let name_len = u16_at(rec, OFF_FILE_NAME_LENGTH);
    let name_off = u16_at(rec, OFF_FILE_NAME_OFFSET);
    let name_start = name_off as usize;
    let name_end = name_start + name_len as usize;
    if name_start < USN_RECORD_V2_HEADER_LEN || name_end > len || !name_len.is_multiple_of(2) {
        return Err(UsnDecodeError::BadFileName {
            offset: name_off,
            length: name_len,
            record_length,
        });
    }
    let file_name_utf16 = rec[name_start..name_end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(UsnRecordV2 {
        record_length,
        file_reference: u64_at(rec, OFF_FILE_REFERENCE),
        parent_file_reference: u64_at(rec, OFF_PARENT_REFERENCE),
        usn: u64_at(rec, OFF_USN) as i64,
        timestamp: u64_at(rec, OFF_TIMESTAMP) as i64,
        reason: u32_at(rec, OFF_REASON),
        source_info: u32_at(rec, OFF_SOURCE_INFO),
        security_id: u32_at(rec, OFF_SECURITY_ID),
        file_attributes: u32_at(rec, OFF_FILE_ATTRIBUTES),
        file_name_utf16,
    })
}

/// Decode consecutive `USN_RECORD_V2` records filling `buf`, stopping at
/// `max_records`. Returns the records and the byte offset where decoding
/// stopped, so a caller can resume.
pub fn decode_usn_records_v2(
    buf: &[u8],
    max_records: usize,
) -> Result<(Vec<UsnRecordV2>, usize), UsnDecodeError> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < buf.len() && out.len() < max_records {
        let rec = decode_usn_record_v2(&buf[pos..])?;
        pos += rec.record_length as usize;
        out.push(rec);
    }
    Ok((out, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(name: &str, file_ref: u64, parent_ref: u64, reason: u32) -> Vec<u8> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let name_bytes = name.len() * 2;
        let len = (USN_RECORD_V2_HEADER_LEN + name_bytes).div_ceil(8) * 8;
        let mut b = vec![0u8; len];
        b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        b[8..16].copy_from_slice(&file_ref.to_le_bytes());
        b[16..24].copy_from_slice(&parent_ref.to_le_bytes());
        b[24..32].copy_from_slice(&0x1234_5678_9abcu64.to_le_bytes());
        b[32..40].copy_from_slice(&0x01DA_0000_0000_0001u64.to_le_bytes());
        b[40..44].copy_from_slice(&reason.to_le_bytes());
        b[52..56].copy_from_slice(&0x20u32.to_le_bytes());
        b[56..58].copy_from_slice(&(name_bytes as u16).to_le_bytes());
        b[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            b[60 + i * 2..62 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        b
    }

    #[test]
    fn full_reference_reason_and_name_survive() {
        let file_ref = 0xBEEF_0000_1234_5678u64;
        let b = build("é.txt", file_ref, 0x0005_0000_0000_0005, 0x8000_0102);
        let r = decode_usn_record_v2(&b).unwrap();
        assert_eq!(r.file_reference, file_ref);
        assert_eq!(r.file_segment(), 0x1234_5678);
        assert_eq!(r.file_sequence(), 0xBEEF);
        assert_eq!(r.parent_file_reference, 0x0005_0000_0000_0005);
        assert_eq!(r.reason, 0x8000_0102);
        assert_eq!(r.usn, 0x1234_5678_9abc);
        assert_eq!(r.file_attributes, 0x20);
        assert_eq!(r.file_name_lossy(), "é.txt");
    }

    #[test]
    fn unsupported_versions_are_refused() {
        for (major, minor) in [(3u16, 0u16), (4, 0), (2, 1)] {
            let mut b = build("a", 1, 5, 1);
            b[4..6].copy_from_slice(&major.to_le_bytes());
            b[6..8].copy_from_slice(&minor.to_le_bytes());
            assert_eq!(
                decode_usn_record_v2(&b),
                Err(UsnDecodeError::UnsupportedVersion { major, minor })
            );
        }
    }

    #[test]
    fn truncated_buffers_are_refused() {
        let b = build("name", 1, 5, 1);
        for cut in [0, 7, 59, b.len() - 1] {
            assert!(matches!(
                decode_usn_record_v2(&b[..cut]),
                Err(UsnDecodeError::Truncated { .. })
            ));
        }
    }

    #[test]
    fn name_outside_record_is_refused() {
        let mut b = build("name", 1, 5, 1);
        b[56..58].copy_from_slice(&200u16.to_le_bytes());
        assert!(matches!(
            decode_usn_record_v2(&b),
            Err(UsnDecodeError::BadFileName { .. })
        ));
        let mut b = build("name", 1, 5, 1);
        b[0..4].copy_from_slice(&40u32.to_le_bytes());
        assert_eq!(
            decode_usn_record_v2(&b),
            Err(UsnDecodeError::RecordLengthTooSmall { record_length: 40 })
        );
    }

    #[test]
    fn consecutive_records_stop_at_the_bound() {
        let mut buf = build("one", 1, 5, 1);
        let first = buf.len();
        buf.extend(build("two", 2, 5, 2));
        buf.extend(build("three", 3, 5, 4));
        let (recs, pos) = decode_usn_records_v2(&buf, 1).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(pos, first);
        let (recs, pos) = decode_usn_records_v2(&buf, 10).unwrap();
        assert_eq!(recs.len(), 3);
        assert_eq!(pos, buf.len());
        assert_eq!(recs[2].file_name_lossy(), "three");
    }
}
