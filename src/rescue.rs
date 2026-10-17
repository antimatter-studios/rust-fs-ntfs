//! Sector rescue: a read-only scan that copies every readable byte of a
//! failing source into an external, append-only archive, and records every
//! byte it could not read.
//!
//! The archive is evidence, not a volume. A data record holds bytes the
//! source returned; an unreadable record names a span the source refused
//! and carries no payload, so nothing in the archive stands in for a byte
//! the source never gave. The layout, and what a reader must refuse, is
//! `docs/rescue-archive.md`.
//!
//! The scan reads in bulk. A bulk read that fails is retried one sector at
//! a time, so one bad sector costs that sector and not the chunk around
//! it, and adjacent unreadable sectors become one span, across chunk
//! boundaries too. Each record is flushed before the scan reads past it,
//! so an interruption can lose only the record being written.
//!
//! The source is only ever read. A rescue archive is never a repair plan:
//! nothing here writes to a volume.
//!
//! The behaviour (bulk reads retried per sector, append-only checksummed
//! records) follows the rescue scanner of Slate's MIT-licensed tools;
//! neither its code nor its archive format is used (docs/provenance.md).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The archive's first eight bytes.
pub const ARCHIVE_MAGIC: [u8; 8] = *b"NTRESCUE";
/// The archive layout this module writes. A reader refuses any other.
pub const ARCHIVE_VERSION: u32 = 1;
/// Bytes in the archive header.
pub const HEADER_LEN: usize = 32;
/// Bytes in a record header; a data record's payload follows it.
pub const RECORD_HEADER_LEN: usize = 24;
/// A record whose payload is the source's bytes over its span.
pub const KIND_DATA: u32 = 1;
/// A record naming a span the source could not read. It has no payload.
pub const KIND_UNREADABLE: u32 = 2;
/// The bulk read size [`ScanOptions::default`] uses.
pub const DEFAULT_CHUNK: usize = 1024 * 1024;

const MAX_CHUNK: usize = 64 * 1024 * 1024;
const EMPTY_SOURCE: &str = "the source is empty: there is nothing to rescue";

/// Why a read of the source failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadFailure {
    /// The medium could not return these bytes: a media error, which the
    /// scan records as an unreadable span and moves past.
    Unreadable(String),
    /// Anything else (the source vanished, a short read, a bad handle).
    /// The scan stops, because carrying on would record healthy bytes as
    /// lost.
    Failed(String),
}

/// A source to rescue: positioned reads that say whether a failure was the
/// medium's.
pub trait RescueSource {
    /// The source's length in bytes.
    fn size(&self) -> u64;
    /// Fill `buf` from `offset`. On failure the contents of `buf` are not
    /// to be trusted, even where the source wrote some of it.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure>;
}

/// An image file or a device, opened read-only.
pub struct FileSource {
    file: File,
    size: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file =
            File::open(path).map_err(|e| format!("open '{}' read-only: {e}", path.display()))?;
        // A block device reports a length of 0 to stat; seeking to the end
        // measures a device and a file alike.
        let size = file
            .seek(SeekFrom::End(0))
            .map_err(|e| format!("measure '{}': {e}", path.display()))?;
        Ok(Self { file, size })
    }
}

impl RescueSource for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        let read = self
            .file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(buf));
        read.map_err(|e| {
            let what = format!("read {} bytes at {offset}: {e}", buf.len());
            if is_media_error(&e) {
                ReadFailure::Unreadable(what)
            } else {
                ReadFailure::Failed(what)
            }
        })
    }
}

/// Whether an I/O error is the medium failing to return data, as opposed to
/// anything else going wrong.
fn is_media_error(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EIO)
    }
    // Microsoft's System Error Codes: ERROR_CRC (23), ERROR_SECTOR_NOT_FOUND
    // (27), ERROR_READ_FAULT (30) and ERROR_IO_DEVICE (1117).
    #[cfg(windows)]
    {
        matches!(e.raw_os_error(), Some(23 | 27 | 30 | 1117))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// How the scan reads its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanOptions {
    /// The unit a failed bulk read is retried in, and the granularity of an
    /// unreadable span: a power of two from 512 to 4096.
    pub sector_size: u32,
    /// Bytes per bulk read: a whole number of sectors, at most 64 MiB.
    pub chunk_size: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            sector_size: 512,
            chunk_size: DEFAULT_CHUNK,
        }
    }
}

impl ScanOptions {
    fn validate(&self) -> Result<(), String> {
        let sector = self.sector_size;
        if !sector.is_power_of_two() || !(512..=4096).contains(&sector) {
            return Err(format!(
                "sector size {sector} is not a power of two from 512 to 4096"
            ));
        }
        let chunk = self.chunk_size;
        if chunk == 0 || chunk > MAX_CHUNK || !chunk.is_multiple_of(sector as usize) {
            return Err(format!(
                "chunk size {chunk} is not a whole number of {sector}-byte sectors up to {MAX_CHUNK}"
            ));
        }
        Ok(())
    }
}

/// What a scan archived.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub source_size: u64,
    /// Bytes held in data records.
    pub readable_bytes: u64,
    /// Bytes named by unreadable records.
    pub unreadable_bytes: u64,
    pub data_records: u64,
    /// Unreadable records: each is a maximal run of unreadable sectors.
    pub unreadable_spans: u64,
    /// Bulk reads that failed and were retried sector by sector.
    pub failed_bulk_reads: u64,
}

impl ScanReport {
    /// Whether the archive holds every byte of the source.
    pub fn is_complete(&self) -> bool {
        self.unreadable_bytes == 0 && self.readable_bytes == self.source_size
    }
}

/// Scan `source` into a new archive at `archive`, which must not exist (so
/// it can never be the source). The source is opened read-only.
pub fn scan_to_path(
    source: &Path,
    archive: &Path,
    options: ScanOptions,
) -> Result<ScanReport, String> {
    options.validate()?;
    let mut input = FileSource::open(source)?;
    if input.size() == 0 {
        return Err(EMPTY_SOURCE.into());
    }
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(archive)
        .map_err(|e| format!("create the archive '{}': {e}", archive.display()))?;
    sync_parent(archive)?;
    scan(&mut input, &mut out, options)
}

/// Scan `source` into `archive`, an empty file opened for writing. The
/// header is written and flushed first, then each record as it is made.
pub fn scan<S: RescueSource + ?Sized>(
    source: &mut S,
    archive: &mut File,
    options: ScanOptions,
) -> Result<ScanReport, String> {
    options.validate()?;
    let size = source.size();
    if size == 0 {
        return Err(EMPTY_SOURCE.into());
    }
    write_header(archive, options.sector_size, size)?;
    let mut out = Records::new(archive, size);
    let sector = options.sector_size as usize;
    let mut buf = vec![0u8; options.chunk_size];
    let mut offset = 0u64;
    while offset < size {
        let n = (size - offset).min(options.chunk_size as u64) as usize;
        let chunk = &mut buf[..n];
        match source.read_at(offset, chunk) {
            Ok(()) => out.data(offset, chunk)?,
            Err(ReadFailure::Unreadable(_)) => {
                out.report.failed_bulk_reads += 1;
                retry_by_sector(source, &mut out, offset, chunk, sector)?;
            }
            Err(ReadFailure::Failed(e)) => return Err(format!("the scan stopped: {e}")),
        }
        offset += n as u64;
    }
    out.finish()
}

/// Read a chunk whose bulk read failed one sector at a time, archiving each
/// readable run and recording each unreadable sector.
fn retry_by_sector<S: RescueSource + ?Sized>(
    source: &mut S,
    out: &mut Records<'_>,
    offset: u64,
    chunk: &mut [u8],
    sector: usize,
) -> Result<(), String> {
    // Where the run of readable sectors being gathered starts in `chunk`.
    let mut run: Option<usize> = None;
    let mut at = 0;
    while at < chunk.len() {
        let len = sector.min(chunk.len() - at);
        let pos = offset + at as u64;
        match source.read_at(pos, &mut chunk[at..at + len]) {
            Ok(()) => {
                run.get_or_insert(at);
            }
            Err(ReadFailure::Unreadable(_)) => {
                if let Some(start) = run.take() {
                    out.data(offset + start as u64, &chunk[start..at])?;
                }
                out.unreadable(pos, len as u64)?;
            }
            Err(ReadFailure::Failed(e)) => return Err(format!("the scan stopped: {e}")),
        }
        at += len;
    }
    if let Some(start) = run {
        out.data(offset + start as u64, &chunk[start..])?;
    }
    Ok(())
}

/// The record stream: appends records in source order, holding back the
/// current unreadable span until it ends so adjacent sectors become one.
struct Records<'a> {
    archive: &'a mut File,
    pending: Option<(u64, u64)>,
    report: ScanReport,
}

impl<'a> Records<'a> {
    fn new(archive: &'a mut File, size: u64) -> Self {
        Self {
            archive,
            pending: None,
            report: ScanReport {
                source_size: size,
                ..ScanReport::default()
            },
        }
    }

    fn data(&mut self, offset: u64, payload: &[u8]) -> Result<(), String> {
        self.close_span()?;
        self.append(offset, payload.len() as u64, KIND_DATA, payload)?;
        self.report.readable_bytes += payload.len() as u64;
        self.report.data_records += 1;
        Ok(())
    }

    fn unreadable(&mut self, offset: u64, len: u64) -> Result<(), String> {
        match &mut self.pending {
            Some((start, span)) if *start + *span == offset => *span += len,
            _ => {
                self.close_span()?;
                self.pending = Some((offset, len));
            }
        }
        Ok(())
    }

    fn close_span(&mut self) -> Result<(), String> {
        if let Some((offset, len)) = self.pending.take() {
            self.append(offset, len, KIND_UNREADABLE, &[])?;
            self.report.unreadable_bytes += len;
            self.report.unreadable_spans += 1;
        }
        Ok(())
    }

    fn append(&mut self, offset: u64, len: u64, kind: u32, payload: &[u8]) -> Result<(), String> {
        let mut head = [0u8; RECORD_HEADER_LEN];
        head[0..8].copy_from_slice(&offset.to_le_bytes());
        head[8..16].copy_from_slice(&len.to_le_bytes());
        head[16..20].copy_from_slice(&kind.to_le_bytes());
        let crc = crc32(&[&head[..20], payload]);
        head[20..24].copy_from_slice(&crc.to_le_bytes());
        let what = |e: io::Error| format!("append the record for {offset}+{len}: {e}");
        self.archive.write_all(&head).map_err(what)?;
        self.archive.write_all(payload).map_err(what)?;
        self.archive.sync_data().map_err(what)
    }

    fn finish(mut self) -> Result<ScanReport, String> {
        self.close_span()?;
        self.archive
            .sync_all()
            .map_err(|e| format!("flush the archive: {e}"))?;
        Ok(self.report)
    }
}

fn write_header(archive: &mut File, sector_size: u32, size: u64) -> Result<(), String> {
    let mut head = [0u8; HEADER_LEN];
    head[0..8].copy_from_slice(&ARCHIVE_MAGIC);
    head[8..12].copy_from_slice(&ARCHIVE_VERSION.to_le_bytes());
    head[12..16].copy_from_slice(&sector_size.to_le_bytes());
    head[16..24].copy_from_slice(&size.to_le_bytes());
    // 24..28 is reserved and zero.
    let crc = crc32(&[&head[..28]]);
    head[28..32].copy_from_slice(&crc.to_le_bytes());
    archive
        .write_all(&head)
        .and_then(|()| archive.sync_all())
        .map_err(|e| format!("write the archive header: {e}"))
}

/// Make a newly created archive's directory entry durable, so a power cut
/// cannot leave records in a file no directory names.
fn sync_parent(archive: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let parent = match archive.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("flush the directory '{}': {e}", parent.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = archive;
        Ok(())
    }
}

/// CRC-32 as zlib, gzip and PNG compute it (reflected polynomial
/// 0xEDB88320, initial value and final XOR all ones), over `parts` in turn.
fn crc32(parts: &[&[u8]]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut bit = 0;
            while bit < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                bit += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    let mut crc = !0u32;
    for part in parts {
        for &byte in *part {
            crc = TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_published_check_value() {
        // The check value every CRC-32 (IEEE) catalogue lists for "123456789".
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xCBF4_3926);
    }

    #[test]
    fn options_refuse_a_sector_size_or_chunk_the_scan_cannot_use() {
        let bad = |sector_size, chunk_size| {
            ScanOptions {
                sector_size,
                chunk_size,
            }
            .validate()
            .is_err()
        };
        assert!(bad(1000, 1 << 20));
        assert!(bad(256, 1 << 20));
        assert!(bad(8192, 1 << 20));
        assert!(bad(512, 0));
        assert!(bad(512, 1000));
        assert!(bad(512, MAX_CHUNK + 512));
        assert!(!bad(4096, 4096));
        assert!(ScanOptions::default().validate().is_ok());
    }
}
