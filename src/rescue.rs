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
use std::io::{self, Read, Seek, SeekFrom};
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
    let _ = (source, archive);
    Err("the rescue scan is not implemented yet (#720)".into())
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

#[cfg(test)]
mod tests {
    use super::*;

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
