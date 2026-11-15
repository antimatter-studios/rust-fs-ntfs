//! `$Secure:$SDS` data-stream serialization plus the SDH/SII hash.
//!
//! The `$SDS` stream is the canonical store of every distinct security
//! descriptor on the volume. Each entry consists of a 20-byte header
//! (hash + security_id + offset + size) immediately followed by the SD
//! payload (MS-DTYP §2.4.6 SECURITY_DESCRIPTOR_RELATIVE), zero-padded
//! to a 16-byte boundary. NTFS keeps an exact byte-for-byte mirror of
//! every entry at `entry_offset + 0x40000` so the cache survives a
//! torn-write at the primary copy.
//!
//! References:
//!   * The Linux-NTFS documentation's `$Secure` page (`$SDS`, and the
//!     `$SDH` and `$SII` view indexes).
//!   * MS-DTYP §2.4.6 (`SECURITY_DESCRIPTOR_RELATIVE`).
//!
//! No documentation this crate uses specifies the hash function below —
//! it was observed via byte-diff against Microsoft `Format-Volume`'s
//! `$Secure:$SDS` output (NTFS v3.1 reference). All 12 sampled entries
//! from a fresh reference matched a 32-bit rotate-3-and-add over the
//! SD bytes; that is the algorithm implemented here.

/// One SD entry destined for `$SDS`. `security_id` is the monotonic
/// ID (Microsoft's allocation starts at 0x100); the offset and full
/// entry size are derived in [`build_sds`].
#[derive(Clone, Debug)]
pub struct SdEntry<'a> {
    pub security_id: u32,
    pub sd: &'a [u8],
}

/// SDS entry header size (hash + security_id + offset + size).
pub const SDS_HEADER_LEN: u32 = 20;

// Field offsets within the 20-byte SDS entry header (all little-endian).
const SDS_HDR_HASH_OFF: usize = 0; // u32 SDH/SII hash
const SDS_HDR_SECURITY_ID_OFF: usize = 4; // u32 security identifier
const SDS_HDR_ENTRY_OFFSET_OFF: usize = 8; // u64 byte offset of this entry in $SDS
const SDS_HDR_ENTRY_SIZE_OFF: usize = 16; // u32 unpadded entry size (header + SD)
const SDS_HDR_SD_DATA_OFF: usize = 20; // SD payload starts immediately after the header

/// Round `n` up to the next multiple of 16. NTFS pads SDS entries to 16-byte
/// boundaries so successive entries remain naturally aligned in the $SDS stream.
fn align_to_16(n: usize) -> usize {
    (n + 15) & !15
}

/// NTFS mirrors every `$SDS` entry at `offset + MIRROR_GAP`. Per the
/// public Microsoft layout this is exactly 256 KiB.
pub const SDS_MIRROR_GAP: u64 = 0x40000;

/// SDH/SII hash of a security-descriptor blob.
///
/// No documentation this crate uses specifies the SDH hash algorithm;
/// this implementation was derived purely by observing
/// Microsoft `Format-Volume`'s `$Secure:$SDS` byte output and
/// brute-checking 32-bit hash candidates against the resulting
/// SD bytes (12 entries from a fresh NTFS v3.1 reference all matched).
pub fn sdh_hash(sd: &[u8]) -> u32 {
    let mut h: u32 = 0;
    let mut chunks = sd.chunks_exact(4);
    for c in &mut chunks {
        let w = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        h = h.rotate_left(3).wrapping_add(w);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let mut buf = [0u8; 4];
        buf[..rem.len()].copy_from_slice(rem);
        let w = u32::from_le_bytes(buf);
        h = h.rotate_left(3).wrapping_add(w);
    }
    h
}

/// Total in-stream length of a single SDS entry (header + SD), padded
/// to a 16-byte boundary. Microsoft pads with zero bytes; ntfs.sys
/// reads `sds_size` (unpadded) but the next entry begins at the
/// padded offset.
fn sds_entry_total_len(sd_len: usize) -> usize {
    align_to_16(SDS_HEADER_LEN as usize + sd_len)
}

/// Serialise one entry into `out` at `offset`, returning the unpadded
/// `sds_size` value (header + SD, what the entry header reports).
fn write_one_entry(out: &mut Vec<u8>, offset: u64, entry: &SdEntry<'_>) -> u32 {
    let hash = sdh_hash(entry.sd);
    let unpadded = SDS_HEADER_LEN as usize + entry.sd.len();
    let total = sds_entry_total_len(entry.sd.len());

    let needed = offset as usize + total;
    if out.len() < needed {
        out.resize(needed, 0);
    }

    let at = offset as usize;
    out[at + SDS_HDR_HASH_OFF..at + SDS_HDR_HASH_OFF + 4].copy_from_slice(&hash.to_le_bytes());
    out[at + SDS_HDR_SECURITY_ID_OFF..at + SDS_HDR_SECURITY_ID_OFF + 4]
        .copy_from_slice(&entry.security_id.to_le_bytes());
    out[at + SDS_HDR_ENTRY_OFFSET_OFF..at + SDS_HDR_ENTRY_OFFSET_OFF + 8]
        .copy_from_slice(&offset.to_le_bytes());
    out[at + SDS_HDR_ENTRY_SIZE_OFF..at + SDS_HDR_ENTRY_SIZE_OFF + 4]
        .copy_from_slice(&(unpadded as u32).to_le_bytes());
    out[at + SDS_HDR_SD_DATA_OFF..at + SDS_HDR_SD_DATA_OFF + entry.sd.len()]
        .copy_from_slice(entry.sd);
    // Bytes at + unpadded .. at + total stay zero (alignment pad).

    unpadded as u32
}

/// Build the full `$SDS` data-stream bytes for the given entries.
///
/// Microsoft maintains a duplicate copy of every entry at
/// `offset + 0x40000` (256 KiB) inside the same data stream — the
/// "mirror" copy. The returned buffer covers from offset 0 through
/// the end of the last mirrored entry, so reading it as a normal data
/// stream yields both copies in their canonical positions.
///
/// For S2 we ship a single entry but the API takes a slice so S3+ can
/// extend without touching this function.
pub fn build_sds(entries: &[SdEntry<'_>]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut cursor: u64 = 0;

    let mut written: Vec<(u64, usize)> = Vec::with_capacity(entries.len());

    for entry in entries {
        let total = sds_entry_total_len(entry.sd.len());
        let _ = write_one_entry(&mut out, cursor, entry);
        written.push((cursor, total));
        cursor += total as u64;
    }

    // Mirror every entry at offset+0x40000. Each mirror entry carries
    // the SAME sds_offset value as its primary (Microsoft's reference
    // does NOT bump the header offset for the mirror copy — the mirror
    // is bit-identical to the primary's bytes).
    for (off, total) in &written {
        let mirror_off = off + SDS_MIRROR_GAP;
        let needed = mirror_off as usize + *total;
        if out.len() < needed {
            out.resize(needed, 0);
        }
        // Copy primary's bytes verbatim. We can't slice-overlap so
        // pull into a temp.
        let src: Vec<u8> = out[*off as usize..*off as usize + *total].to_vec();
        out[mirror_off as usize..mirror_off as usize + *total].copy_from_slice(&src);
    }

    out
}

/// Which half of a 256 KiB region pair a copy sits in: `$SDS` alternates
/// a primary region with the mirror region that copies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdsCopy {
    Primary,
    Mirror,
}

/// One copy of one `$SDS` entry, as stored: where it is, what its header
/// claims, and what the claim was checked against. Nothing in it is
/// repaired; a copy that fails a check is still reported, so a caller can
/// see it rather than be handed the other copy silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogEntry<'a> {
    /// Byte offset of this copy in the `$SDS` stream.
    pub physical: u64,
    /// The offset its header claims: a primary's own, and for a mirror its
    /// primary's, since a mirror is a byte-for-byte copy.
    pub logical: u64,
    pub security_id: u32,
    /// The unpadded size the header declares: header plus descriptor.
    pub declared_len: u32,
    /// The hash the header stores.
    pub stored_hash: u32,
    /// [`sdh_hash`] of the descriptor bytes as found.
    pub computed_hash: u32,
    pub copy: SdsCopy,
    /// The descriptor bytes as stored, which may be malformed: parse them
    /// with [`crate::security_descriptor::SecurityDescriptor`].
    pub descriptor: &'a [u8],
}

impl CatalogEntry<'_> {
    /// The stored hash is the one the descriptor bytes give.
    pub fn hash_verified(&self) -> bool {
        self.stored_hash == self.computed_hash
    }

    /// The offset field names this copy's primary position.
    pub fn offset_verified(&self) -> bool {
        match self.copy {
            SdsCopy::Primary => self.logical == self.physical,
            SdsCopy::Mirror => self.logical.checked_add(SDS_MIRROR_GAP) == Some(self.physical),
        }
    }
}

/// Why a catalog walk stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogErrorKind {
    /// The stream ends inside an entry.
    Truncated,
    /// The declared size is below the 20-byte header.
    LengthBelowHeader,
    /// The entry would run past its 256 KiB region into the next one.
    CrossesRegion,
    /// After a region's last entry, bytes that are not zero padding.
    NotPadding,
    /// Neither the primary copy of the asked-for entry nor its mirror
    /// verifies (from [`find_descriptor`] only).
    Unverified,
}

/// A catalog error at a byte offset in the `$SDS` stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogError {
    pub kind: CatalogErrorKind,
    pub at: u64,
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            CatalogErrorKind::Truncated => "the stream ends inside an entry",
            CatalogErrorKind::LengthBelowHeader => "an entry declares less than its header",
            CatalogErrorKind::CrossesRegion => "an entry runs past its 256 KiB region",
            CatalogErrorKind::NotPadding => "bytes after a region's last entry are not padding",
            CatalogErrorKind::Unverified => "neither copy of the entry verifies",
        };
        write!(f, "$SDS: {what} at offset {:#x}", self.at)
    }
}

impl std::error::Error for CatalogError {}

/// Every entry copy in a `$SDS` stream, in stream order: each region's
/// entries from its start, 16-byte aligned, until a zero header, whose
/// region must then hold only zero padding; then the next region, primary
/// and mirror alike. A malformed entry is a located error, after which the
/// walk ends; the end of the stream ends it once.
///
/// Nothing is allocated, and no on-disk length is used before it is
/// bounded by the stream and the region.
pub fn catalog(stream: &[u8]) -> Catalog<'_> {
    Catalog {
        stream,
        at: 0,
        done: false,
    }
}

/// The iterator [`catalog`] returns.
#[derive(Clone, Debug)]
pub struct Catalog<'a> {
    stream: &'a [u8],
    at: usize,
    done: bool,
}

impl<'a> Catalog<'a> {
    fn fail(
        &mut self,
        kind: CatalogErrorKind,
        at: usize,
    ) -> Option<Result<CatalogEntry<'a>, CatalogError>> {
        self.done = true;
        Some(Err(CatalogError {
            kind,
            at: at as u64,
        }))
    }
}

impl<'a> Iterator for Catalog<'a> {
    type Item = Result<CatalogEntry<'a>, CatalogError>;

    fn next(&mut self) -> Option<Self::Item> {
        let gap = SDS_MIRROR_GAP as usize;
        while !self.done {
            let s = self.stream;
            let at = self.at;
            if at >= s.len() {
                self.done = true;
                return None;
            }
            let region = at / gap;
            let region_end = region
                .checked_add(1)
                .and_then(|r| r.checked_mul(gap))
                .unwrap_or(usize::MAX);
            let header_end = at.saturating_add(SDS_HEADER_LEN as usize);
            let header = &s[at..header_end.min(s.len())];
            if header.iter().all(|&b| b == 0) {
                // The region's last entry was the one before: the rest of
                // the region is padding, and the walk goes on at the next.
                let tail = &s[at..region_end.min(s.len())];
                if let Some(i) = tail.iter().position(|&b| b != 0) {
                    return self.fail(CatalogErrorKind::NotPadding, (at + i) & !15);
                }
                self.at = region_end;
                continue;
            }
            if header_end > region_end {
                return self.fail(CatalogErrorKind::CrossesRegion, at);
            }
            if header_end > s.len() {
                return self.fail(CatalogErrorKind::Truncated, at);
            }
            let u32_at = |o: usize| u32::from_le_bytes(s[at + o..at + o + 4].try_into().unwrap());
            let declared_len = u32_at(SDS_HDR_ENTRY_SIZE_OFF);
            let len = declared_len as usize;
            if len < SDS_HEADER_LEN as usize {
                return self.fail(CatalogErrorKind::LengthBelowHeader, at);
            }
            let end = at.saturating_add(len);
            if end > region_end {
                return self.fail(CatalogErrorKind::CrossesRegion, at);
            }
            if end > s.len() {
                return self.fail(CatalogErrorKind::Truncated, at);
            }
            let descriptor = &s[at + SDS_HDR_SD_DATA_OFF..end];
            let logical = u64::from_le_bytes(
                s[at + SDS_HDR_ENTRY_OFFSET_OFF..at + SDS_HDR_ENTRY_OFFSET_OFF + 8]
                    .try_into()
                    .unwrap(),
            );
            self.at = align_to_16(end);
            return Some(Ok(CatalogEntry {
                physical: at as u64,
                logical,
                security_id: u32_at(SDS_HDR_SECURITY_ID_OFF),
                declared_len,
                stored_hash: u32_at(SDS_HDR_HASH_OFF),
                computed_hash: sdh_hash(descriptor),
                copy: if region.is_multiple_of(2) {
                    SdsCopy::Primary
                } else {
                    SdsCopy::Mirror
                },
                descriptor,
            }));
        }
        None
    }
}

/// How a primary copy's mirror, 256 KiB on, compares with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorState {
    /// Byte for byte the primary's entry.
    Same,
    /// Present, and not the primary's bytes.
    Different,
    /// The stream ends before the mirror would.
    Missing,
}

/// Compare `primary`'s bytes in `stream` with the copy one mirror gap on.
pub fn mirror_state(stream: &[u8], primary: &CatalogEntry<'_>) -> MirrorState {
    let len = primary.declared_len as usize;
    let at = primary.physical as usize;
    let Some(mirror) = at.checked_add(SDS_MIRROR_GAP as usize) else {
        return MirrorState::Missing;
    };
    match (
        stream.get(at..at + len),
        stream.get(mirror..mirror.saturating_add(len)),
    ) {
        (Some(p), Some(m)) if mirror.checked_add(len).is_some() => {
            if p == m {
                MirrorState::Same
            } else {
                MirrorState::Different
            }
        }
        _ => MirrorState::Missing,
    }
}

/// The descriptor bytes security id `id` names: its primary copy when that
/// verifies (hash and offset), else its mirror when that one does. `None`
/// when no copy carries the id; [`CatalogErrorKind::Unverified`] when the
/// id's copies all fail, since choosing one would hand the caller bytes
/// nothing vouches for; any walk error before the id is found.
pub fn find_descriptor(stream: &[u8], id: u32) -> Result<Option<&[u8]>, CatalogError> {
    let mut unverified = None;
    let mut mirror_only = None;
    for entry in catalog(stream) {
        let entry = entry?;
        if entry.security_id != id {
            continue;
        }
        let verified = entry.hash_verified() && entry.offset_verified();
        match entry.copy {
            SdsCopy::Primary if verified => return Ok(Some(entry.descriptor)),
            SdsCopy::Primary => {
                if unverified.is_none() {
                    unverified = Some(entry.physical);
                }
            }
            SdsCopy::Mirror if verified => {
                if unverified.is_some_and(|p| p.checked_add(SDS_MIRROR_GAP) == Some(entry.physical))
                {
                    return Ok(Some(entry.descriptor));
                }
                if mirror_only.is_none() {
                    mirror_only = Some(entry.descriptor);
                }
            }
            SdsCopy::Mirror => {}
        }
    }
    if let Some(at) = unverified {
        return Err(CatalogError {
            kind: CatalogErrorKind::Unverified,
            at,
        });
    }
    Ok(mirror_only)
}

/// `$Secure` is MFT record 9 on every NTFS 3.x volume.
const SECURE_RECORD: u64 = 9;

/// The security descriptor `file_path` resolves to on the volume at
/// `image`, as stored. See [`file_descriptor_io`].
pub fn file_descriptor(
    image: &std::path::Path,
    file_path: &str,
) -> Result<Vec<u8>, crate::error::Error> {
    let mut io = crate::block_io::PathIo::open_ro(image)?;
    file_descriptor_io(&mut io, file_path)
}

/// The security descriptor `file_path` resolves to, as stored: its own
/// `$SECURITY_DESCRIPTOR` attribute when it carries one, else the `$SDS`
/// copy its `$STANDARD_INFORMATION` security id names, read through
/// [`find_descriptor`]: a verified primary, else its verified mirror, and
/// an error rather than a copy nothing vouches for (#606).
pub fn file_descriptor_io<T: crate::block_io::BlockIo + ?Sized>(
    io: &mut T,
    file_path: &str,
) -> Result<Vec<u8>, crate::error::Error> {
    use crate::attr_io::AttrType;
    use crate::error::{Error, Kind};
    let record = crate::read::resolve_path(io, file_path)?;
    if let Some(own) = crate::read::read_attribute_value_if_present(
        io,
        record,
        AttrType::SecurityDescriptor,
        None,
    )? {
        return Ok(own);
    }
    let Some(id) = crate::write::read_security_id_io(io, file_path)? else {
        return Err(Error::new(
            Kind::NotFound,
            format!("{file_path}: no $SECURITY_DESCRIPTOR and no security id"),
        ));
    };
    let sds = crate::read::read_attribute_value_if_present(
        io,
        SECURE_RECORD,
        AttrType::Data,
        Some("$SDS"),
    )?
    .ok_or_else(|| Error::new(Kind::NotFound, "$Secure has no $SDS stream"))?;
    match find_descriptor(&sds, id) {
        Ok(Some(descriptor)) => Ok(descriptor.to_vec()),
        Ok(None) => Err(Error::new(
            Kind::NotFound,
            format!("security id {id} not in $SDS"),
        )),
        Err(e) => Err(Error::new(Kind::Io, format!("security id {id}: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode an ASCII hex string into bytes.
    fn hex_decode(s: &str) -> Vec<u8> {
        let bytes = s.as_bytes();
        assert!(bytes.len().is_multiple_of(2), "odd hex length");
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for chunk in bytes.chunks_exact(2) {
            let hi = hex_nibble(chunk[0]);
            let lo = hex_nibble(chunk[1]);
            out.push((hi << 4) | lo);
        }
        out
    }

    fn hex_nibble(b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => panic!("bad hex digit {b}"),
        }
    }

    // --- align_to_16 ----------------------------------------------------------

    #[test]
    fn align_to_16_already_aligned_is_unchanged() {
        assert_eq!(align_to_16(0), 0);
        assert_eq!(align_to_16(16), 16);
        assert_eq!(align_to_16(32), 32);
    }

    #[test]
    fn align_to_16_rounds_up_to_next_multiple() {
        assert_eq!(align_to_16(1), 16);
        assert_eq!(align_to_16(15), 16);
        assert_eq!(align_to_16(17), 32);
    }

    // --- sds_entry_total_len --------------------------------------------------

    #[test]
    fn sds_entry_total_len_pads_to_16_boundary() {
        // header=20 + sd=0 = 20 → pad to 32
        assert_eq!(sds_entry_total_len(0), 32);
        // header=20 + sd=12 = 32 → already aligned
        assert_eq!(sds_entry_total_len(12), 32);
        // header=20 + sd=52 = 72 → pad to 80
        assert_eq!(sds_entry_total_len(52), 80);
    }

    #[test]
    fn sdh_hash_matches_reference() {
        // External anchor: SD blob + expected hash captured from a
        // fresh Microsoft `Format-Volume` output (NTFS v3.1
        // reference). 100 bytes, security_id=0x100. This is the
        // canonical system-metafile SD on a fresh-formatted volume
        // (matches `SD_SYSFILE_RW` modulo the leading length /
        // revision bytes).
        let sd_100 = hex_decode("01000480480000005400000000000000140000000200340002000000000014008900120001010000000000051200000000001800890012000102000000000005200000002002000001010000000000051400000001020000000000052000000020020000");
        assert_eq!(sdh_hash(&sd_100), 0x32fee6cb);
    }

    #[test]
    fn sdh_hash_stable_round_trip() {
        // Regression guard: hashing a known multi-byte payload twice
        // must yield the same value, and the algorithm's defining
        // formula (h = rotl(h, 3) + w_le) must be re-derivable from
        // the bytes. If a future commit accidentally swaps endianness
        // or changes the rotation, this test catches it without
        // needing an external dump.
        let bytes: Vec<u8> = (0..40u8).collect();
        let h1 = sdh_hash(&bytes);
        let h2 = sdh_hash(&bytes);
        assert_eq!(h1, h2, "hash must be deterministic");

        // Hand-compute on a 4-byte input.
        let single = [0x01u8, 0x02, 0x03, 0x04];
        // rotl(0, 3) = 0; 0 + 0x04030201_LE = 0x04030201
        assert_eq!(sdh_hash(&single), 0x04030201);

        // Two words: w1 = 0x04030201_LE, h = rotl(0, 3) + w1 = 0x04030201.
        // rotl(0x04030201, 3) = 0x20181008.  w2 = 0x08070605_LE.
        // h2 = 0x20181008 + 0x08070605 = 0x281f160d.
        let two = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(sdh_hash(&two), 0x281f160d);
    }

    #[test]
    fn build_sds_single_entry_layout() {
        // Use a small 72-byte SD-shaped payload. Bytes are arbitrary;
        // this asserts only the framing (header values, padding, mirror).
        let sd: Vec<u8> = (0..72u8).collect();
        let entries = [SdEntry {
            security_id: 0x100,
            sd: &sd,
        }];
        let bytes = build_sds(&entries);

        // Primary copy at offset 0: header + SD = 92 bytes; padded to 96.
        assert_eq!(bytes.len(), SDS_MIRROR_GAP as usize + 96);

        let hash = sdh_hash(&sd);
        assert_eq!(&bytes[0..4], &hash.to_le_bytes());
        assert_eq!(&bytes[4..8], &0x100u32.to_le_bytes());
        assert_eq!(&bytes[8..16], &0u64.to_le_bytes());
        assert_eq!(&bytes[16..20], &92u32.to_le_bytes());
        assert_eq!(&bytes[20..92], &sd[..]);
        // Pad bytes 92..96 zero.
        assert_eq!(&bytes[92..96], &[0u8; 4]);

        // Mirror copy: identical 96 bytes at +0x40000.
        let m = SDS_MIRROR_GAP as usize;
        assert_eq!(&bytes[m..m + 96], &bytes[0..96]);
    }

    // --- sdh_hash: remainder byte paths -----------------------------------

    #[test]
    fn sdh_hash_empty_input_returns_zero() {
        assert_eq!(sdh_hash(&[]), 0);
    }

    #[test]
    fn sdh_hash_single_byte_remainder() {
        // 1 byte, no full 4-byte chunk: buf=[0x42,0,0,0], w=0x42, h=0+0x42=0x42.
        assert_eq!(sdh_hash(&[0x42]), 0x42);
    }

    #[test]
    fn sdh_hash_two_byte_remainder() {
        // [0x01, 0x02]: buf=[0x01,0x02,0,0], w=0x0201, h=0+0x0201=0x0201.
        assert_eq!(sdh_hash(&[0x01, 0x02]), 0x0201);
    }

    #[test]
    fn sdh_hash_three_byte_remainder() {
        // [0x01, 0x02, 0x03]: buf=[0x01,0x02,0x03,0], w=0x00030201.
        assert_eq!(sdh_hash(&[0x01, 0x02, 0x03]), 0x00030201);
    }

    #[test]
    fn sdh_hash_five_bytes_full_chunk_plus_one_remainder() {
        // Bytes 0-3: full chunk → h = rotl(0, 3) + w0 = w0.
        // Byte 4: remainder buf=[b4, 0, 0, 0].
        let bytes = [0x01u8, 0x02, 0x03, 0x04, 0x05];
        let w0: u32 = u32::from_le_bytes([0x01, 0x02, 0x03, 0x04]);
        let h1 = 0u32.rotate_left(3).wrapping_add(w0);
        let w1 = u32::from_le_bytes([0x05, 0, 0, 0]);
        let expected = h1.rotate_left(3).wrapping_add(w1);
        assert_eq!(sdh_hash(&bytes), expected);
    }

    // --- sdh_hash: determinism and stability --------------------------------

    #[test]
    fn sdh_hash_deterministic_for_same_input() {
        let bytes: Vec<u8> = (0..100u8).collect();
        assert_eq!(sdh_hash(&bytes), sdh_hash(&bytes));
    }

    // --- build_sds: edge cases ---------------------------------------------

    #[test]
    fn build_sds_empty_entries_returns_empty_buffer() {
        let bytes = build_sds(&[]);
        assert!(bytes.is_empty());
    }

    #[test]
    fn build_sds_two_entries_sequential_primary_layout() {
        let sd0: Vec<u8> = vec![0xAAu8; 12]; // entry0: 20+12=32 = aligned
        let sd1: Vec<u8> = vec![0xBBu8; 8]; // entry1: 20+8=28 → 32
        let entries = [
            SdEntry {
                security_id: 0x100,
                sd: &sd0,
            },
            SdEntry {
                security_id: 0x101,
                sd: &sd1,
            },
        ];
        let bytes = build_sds(&entries);

        // Entry 0 at offset 0: 32 bytes.
        let hash0 = sdh_hash(&sd0);
        assert_eq!(&bytes[0..4], &hash0.to_le_bytes(), "entry0 hash");
        assert_eq!(&bytes[4..8], &0x100u32.to_le_bytes(), "entry0 security_id");
        assert_eq!(&bytes[8..16], &0u64.to_le_bytes(), "entry0 self-offset");
        assert_eq!(&bytes[16..20], &32u32.to_le_bytes(), "entry0 unpadded size");

        // Entry 1 at offset 32: 32 bytes.
        let hash1 = sdh_hash(&sd1);
        assert_eq!(&bytes[32..36], &hash1.to_le_bytes(), "entry1 hash");
        assert_eq!(
            &bytes[36..40],
            &0x101u32.to_le_bytes(),
            "entry1 security_id"
        );
        assert_eq!(&bytes[40..48], &32u64.to_le_bytes(), "entry1 self-offset");
        assert_eq!(&bytes[48..52], &28u32.to_le_bytes(), "entry1 unpadded size");
    }

    #[test]
    fn build_sds_two_entries_both_mirrors_present() {
        let sd: Vec<u8> = vec![0xCCu8; 12]; // 32-byte padded entry
        let entries = [
            SdEntry {
                security_id: 0x100,
                sd: &sd,
            },
            SdEntry {
                security_id: 0x101,
                sd: &sd,
            },
        ];
        let bytes = build_sds(&entries);
        let m = SDS_MIRROR_GAP as usize;
        // Mirror of entry 0 at +0x40000.
        assert_eq!(&bytes[m..m + 32], &bytes[0..32], "entry0 mirror");
        // Mirror of entry 1 at 32 + 0x40000.
        assert_eq!(&bytes[m + 32..m + 64], &bytes[32..64], "entry1 mirror");
    }

    #[test]
    fn build_sds_entry_offset_field_reflects_primary_position() {
        // The mirror copy must report the same sds_offset as the primary.
        let sd: Vec<u8> = vec![0x11u8; 12];
        let entries = [
            SdEntry {
                security_id: 0x100,
                sd: &sd,
            },
            SdEntry {
                security_id: 0x101,
                sd: &sd,
            },
        ];
        let bytes = build_sds(&entries);
        let m = SDS_MIRROR_GAP as usize;
        // Primary entry1 sds_offset = 32.
        let primary_off = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        // Mirror entry1 sds_offset must also = 32 (NOT 32 + 0x40000).
        let mirror_off = u64::from_le_bytes(bytes[m + 40..m + 48].try_into().unwrap());
        assert_eq!(primary_off, 32);
        assert_eq!(
            mirror_off, 32,
            "mirror sds_offset must equal primary position"
        );
    }

    // --- catalog: reading entries back (#606) ------------------------------

    /// Two entries, 64 and 32 bytes padded, as the formatter lays them, with
    /// their mirrors.
    fn two_entry_stream() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let sd0: Vec<u8> = (0..44u8).collect();
        let sd1: Vec<u8> = vec![0x5Au8; 9];
        let bytes = build_sds(&[
            SdEntry {
                security_id: 0x100,
                sd: &sd0,
            },
            SdEntry {
                security_id: 0x101,
                sd: &sd1,
            },
        ]);
        (bytes, sd0, sd1)
    }

    /// One entry written at `at`, its offset field `logical`: what a later
    /// region's entry, or its mirror, holds.
    fn put_entry(stream: &mut Vec<u8>, at: usize, logical: u64, id: u32, sd: &[u8]) {
        let end = at + 20 + sd.len();
        if stream.len() < end {
            stream.resize(end, 0);
        }
        stream[at..at + 4].copy_from_slice(&sdh_hash(sd).to_le_bytes());
        stream[at + 4..at + 8].copy_from_slice(&id.to_le_bytes());
        stream[at + 8..at + 16].copy_from_slice(&logical.to_le_bytes());
        stream[at + 16..at + 20].copy_from_slice(&((20 + sd.len()) as u32).to_le_bytes());
        stream[at + 20..end].copy_from_slice(sd);
    }

    #[test]
    fn the_catalog_reads_every_copy_with_its_place_and_both_hashes() {
        let (bytes, sd0, sd1) = two_entry_stream();
        let entries: Vec<_> = catalog(&bytes).collect::<Result<_, _>>().unwrap();
        let gap = SDS_MIRROR_GAP;
        let places: Vec<_> = entries
            .iter()
            .map(|e| (e.physical, e.logical, e.security_id, e.copy))
            .collect();
        assert_eq!(
            places,
            [
                (0, 0, 0x100, SdsCopy::Primary),
                (64, 64, 0x101, SdsCopy::Primary),
                (gap, 0, 0x100, SdsCopy::Mirror),
                (gap + 64, 64, 0x101, SdsCopy::Mirror),
            ]
        );
        for e in &entries {
            assert!(e.hash_verified() && e.offset_verified(), "{e:?}");
            assert_eq!(e.stored_hash, e.computed_hash);
        }
        assert_eq!(entries[0].descriptor, &sd0[..]);
        assert_eq!(entries[1].descriptor, &sd1[..]);
        assert_eq!(
            entries[1].declared_len, 29,
            "the unpadded size, not the padded one"
        );
        assert_eq!(mirror_state(&bytes, &entries[0]), MirrorState::Same);
        assert_eq!(find_descriptor(&bytes, 0x101).unwrap(), Some(&sd1[..]));
        assert_eq!(find_descriptor(&bytes, 0x999).unwrap(), None);
    }

    #[test]
    fn an_empty_stream_ends_at_once_and_ends_once() {
        let mut c = catalog(&[]);
        assert!(c.next().is_none());
        assert!(c.next().is_none());
    }

    #[test]
    fn a_zero_region_tail_moves_the_walk_to_the_next_region() {
        let gap = SDS_MIRROR_GAP as usize;
        let sd = vec![0x77u8; 12];
        let mut stream = Vec::new();
        put_entry(&mut stream, 0, 0, 0x100, &sd);
        put_entry(&mut stream, gap, 0, 0x100, &sd);
        put_entry(&mut stream, 2 * gap, 2 * gap as u64, 0x200, &sd);
        put_entry(&mut stream, 3 * gap, 2 * gap as u64, 0x200, &sd);
        let found: Vec<_> = catalog(&stream)
            .map(|e| {
                let e = e.unwrap();
                (e.physical, e.security_id, e.copy, e.offset_verified())
            })
            .collect();
        assert_eq!(
            found,
            [
                (0, 0x100, SdsCopy::Primary, true),
                (gap as u64, 0x100, SdsCopy::Mirror, true),
                (2 * gap as u64, 0x200, SdsCopy::Primary, true),
                (3 * gap as u64, 0x200, SdsCopy::Mirror, true),
            ]
        );
        assert_eq!(find_descriptor(&stream, 0x200).unwrap(), Some(&sd[..]));
    }

    #[test]
    fn an_entry_crossing_its_region_is_a_located_error_and_the_walk_stops() {
        let gap = SDS_MIRROR_GAP as usize;
        let mut stream = vec![0u8; gap + 64];
        // 20 header bytes and gap - 12 descriptor bytes end 8 bytes past
        // the region, inside its mirror.
        put_entry(&mut stream, 0, 0, 0x100, &vec![0x11; gap - 12]);
        let mut c = catalog(&stream);
        assert_eq!(
            c.next(),
            Some(Err(CatalogError {
                kind: CatalogErrorKind::CrossesRegion,
                at: 0,
            }))
        );
        assert!(c.next().is_none(), "an error ends the walk, once");
    }

    #[test]
    fn bytes_after_a_regions_last_entry_must_be_padding() {
        let gap = SDS_MIRROR_GAP as usize;
        let mut stream = Vec::new();
        put_entry(&mut stream, 0, 0, 0x100, &[0x11; 12]);
        stream.resize(gap + 64, 0);
        stream[gap - 8] = 1;
        let mut c = catalog(&stream);
        assert!(matches!(c.next(), Some(Ok(e)) if e.security_id == 0x100));
        assert_eq!(
            c.next(),
            Some(Err(CatalogError {
                kind: CatalogErrorKind::NotPadding,
                at: (gap - 16) as u64,
            }))
        );
        assert!(c.next().is_none());
    }

    #[test]
    fn a_truncated_entry_is_an_error_and_a_short_length_is_refused() {
        let (bytes, _, _) = two_entry_stream();
        let mut c = catalog(&bytes[..64 + 24]);
        assert!(matches!(c.next(), Some(Ok(e)) if e.security_id == 0x100));
        assert_eq!(
            c.next(),
            Some(Err(CatalogError {
                kind: CatalogErrorKind::Truncated,
                at: 64,
            }))
        );
        assert!(c.next().is_none());
        let mut short = bytes.clone();
        short[16..20].copy_from_slice(&12u32.to_le_bytes());
        assert_eq!(
            catalog(&short).next(),
            Some(Err(CatalogError {
                kind: CatalogErrorKind::LengthBelowHeader,
                at: 0,
            }))
        );
    }

    #[test]
    fn a_torn_primary_is_read_from_its_mirror_and_never_invented() {
        let (bytes, sd0, _) = two_entry_stream();
        let mut torn = bytes.clone();
        torn[20] ^= 0xff;
        let entries: Vec<_> = catalog(&torn).collect::<Result<_, _>>().unwrap();
        assert!(!entries[0].hash_verified(), "the torn primary is visible");
        assert!(entries[2].hash_verified(), "its mirror still verifies");
        assert_eq!(mirror_state(&torn, &entries[0]), MirrorState::Different);
        assert_eq!(find_descriptor(&torn, 0x100).unwrap(), Some(&sd0[..]));
        // Both copies torn: no descriptor is chosen.
        let gap = SDS_MIRROR_GAP as usize;
        torn[gap + 20] ^= 0xff;
        assert_eq!(
            find_descriptor(&torn, 0x100),
            Err(CatalogError {
                kind: CatalogErrorKind::Unverified,
                at: 0,
            })
        );
        // A mirror the stream does not reach is missing, not the same.
        let first = catalog(&bytes[..SDS_MIRROR_GAP as usize])
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(
            mirror_state(&bytes[..SDS_MIRROR_GAP as usize], &first),
            MirrorState::Missing
        );
    }

    #[test]
    fn a_wrong_offset_field_is_visible_beside_a_good_hash() {
        let (bytes, _, _) = two_entry_stream();
        let mut moved = bytes.clone();
        moved[8..16].copy_from_slice(&999u64.to_le_bytes());
        let first = catalog(&moved).next().unwrap().unwrap();
        assert!(first.hash_verified());
        assert!(!first.offset_verified());
    }
}
