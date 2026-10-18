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

use crate::attr_io::{find_attribute, AttrType};
use crate::data_runs::{decode_runs, DataRun};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The archive's first eight bytes.
pub const ARCHIVE_MAGIC: [u8; 8] = *b"NTRESCUE";
/// The archive layout this module writes. A reader refuses any other.
pub const ARCHIVE_VERSION: u32 = 2;
/// Bytes in the archive header.
pub const HEADER_LEN: usize = 32;
/// Bytes in a record header; a data record's payload follows it.
pub const RECORD_HEADER_LEN: usize = 28;
/// A record whose payload is the source's bytes over its span.
pub const KIND_DATA: u32 = 1;
/// A record naming a span the source could not read. It has no payload.
pub const KIND_UNREADABLE: u32 = 2;
/// The bulk read size [`ScanOptions::default`] uses.
pub const DEFAULT_CHUNK: usize = 1024 * 1024;

/// The class of source space a record's span was in. A record never spans
/// two classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RangeClass {
    /// The scan knew no layout for the span: a linear scan's every record.
    Unclassified = 0,
    /// NTFS metadata: the boot sector and its backup, `$MFT` and `$MFTMirr`.
    Metadata = 1,
    /// Clusters `$Bitmap` marks allocated.
    Allocated = 2,
    /// Clusters `$Bitmap` marks free.
    Free = 3,
    /// The bytes past the volume's last whole cluster.
    Tail = 4,
}

impl RangeClass {
    /// Every class, in the order of its code.
    pub const ALL: [RangeClass; 5] = [
        RangeClass::Unclassified,
        RangeClass::Metadata,
        RangeClass::Allocated,
        RangeClass::Free,
        RangeClass::Tail,
    ];

    /// The class's code in a record header.
    pub fn code(self) -> u32 {
        self as u32
    }
}

/// What a scan archived of one class of space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClassBytes {
    /// Bytes held in data records of the class.
    pub archived: u64,
    /// Bytes named by unreadable records of the class.
    pub unreadable: u64,
}

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
    /// Archived and unreadable bytes per class, indexed by
    /// [`RangeClass::code`]; read them with [`ScanReport::class`].
    pub classes: [ClassBytes; 5],
}

impl ScanReport {
    /// Whether the archive holds every byte of the source.
    pub fn is_complete(&self) -> bool {
        self.unreadable_bytes == 0 && self.readable_bytes == self.source_size
    }

    /// What the scan archived of `class`.
    pub fn class(&self, class: RangeClass) -> ClassBytes {
        self.classes[class as usize]
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
    let order =
        ntfs_order(source, size).unwrap_or_else(|| vec![(0, size, RangeClass::Unclassified)]);
    let mut out = Records::new(archive, size);
    let mut buf = vec![0u8; options.chunk_size];
    for (start, end, class) in order {
        out.class = class;
        scan_span(source, &mut out, &mut buf, start, end, options.sector_size)?;
    }
    out.finish()
}

/// Resume an interrupted scan of `source` in `archive`, an archive of that
/// source opened for reading and writing.
///
/// Before the archive is changed, each data record's span is read from the
/// source again and compared with its payload. A sector that reads and
/// differs refuses the resume, naming its offset, with the archive untouched:
/// the source has changed, or is another one (the boot sector is the first
/// record, so another volume is refused too). A sector that no longer reads
/// is no difference, so a failing medium can still be resumed.
///
/// The archive is then read back to its last whole record and the torn record
/// after it, if any, is cut away; nothing before the cut is rewritten. The
/// scan then runs in the order it would have, but reads only the ranges of
/// the source no data record holds: the ranges the interrupted pass never
/// reached, and the spans it recorded as unreadable. A span that reads now
/// gains a data record; the earlier unreadable record stays, as history.
///
/// The layout reads an NTFS scan starts with are served from the archive's
/// data records where those hold the bytes, so no range already archived is
/// read from the source again after the comparison. The report counts what
/// this pass appended.
pub fn resume<S: RescueSource + ?Sized>(
    source: &mut S,
    archive: &mut File,
    options: ScanOptions,
) -> Result<ScanReport, String> {
    options.validate()?;
    archive
        .seek(SeekFrom::Start(0))
        .map_err(|e| format!("rewind the archive: {e}"))?;
    let contents = read_archive(&mut io::BufReader::new(&mut *archive))?;
    let size = source.size();
    if contents.source_size != size {
        return Err(format!(
            "the archive is of a {}-byte source; this source is {size} bytes",
            contents.source_size
        ));
    }
    if contents.sector_size != options.sector_size {
        return Err(format!(
            "the archive was scanned in {}-byte sectors, not {}",
            contents.sector_size, options.sector_size
        ));
    }
    compare_retained(source, archive, &contents.records, options)?;
    archive
        .set_len(contents.whole_len)
        .and_then(|()| archive.sync_all())
        .map_err(|e| format!("cut the torn record from the archive: {e}"))?;
    let mut held: Vec<ArchivedRecord> = contents
        .records
        .into_iter()
        .filter(|r| r.kind == KIND_DATA)
        .collect();
    held.sort_by_key(|r| r.offset);
    let order = ntfs_order(
        &mut Archived {
            source: &mut *source,
            archive: &mut *archive,
            held: &held,
        },
        size,
    )
    .unwrap_or_else(|| vec![(0, size, RangeClass::Unclassified)]);
    archive
        .seek(SeekFrom::Start(contents.whole_len))
        .map_err(|e| format!("seek to the archive's end: {e}"))?;
    let mut out = Records::new(archive, size);
    let mut buf = vec![0u8; options.chunk_size];
    for (start, end, class) in order {
        out.class = class;
        for (a, b) in missing(&held, start, end) {
            scan_span(source, &mut out, &mut buf, a, b, options.sector_size)?;
        }
    }
    out.finish()
}

/// Read each data record's span of `source` again, in archive order, and
/// compare it with the record's payload. A bulk read the medium refuses is
/// retried a sector at a time, and a sector that still does not read is
/// skipped: an unreadable sector is no difference.
fn compare_retained<S: RescueSource + ?Sized>(
    source: &mut S,
    archive: &mut File,
    records: &[ArchivedRecord],
    options: ScanOptions,
) -> Result<(), String> {
    let sector = options.sector_size as usize;
    let mut now = vec![0u8; options.chunk_size];
    let mut kept = vec![0u8; options.chunk_size];
    for r in records.iter().filter(|r| r.kind == KIND_DATA) {
        let end = r.offset + r.len;
        let mut at = r.offset;
        while at < end {
            let n = (end - at).min(now.len() as u64) as usize;
            let from = r.position + RECORD_HEADER_LEN as u64 + (at - r.offset);
            archive
                .seek(SeekFrom::Start(from))
                .and_then(|_| archive.read_exact(&mut kept[..n]))
                .map_err(|e| format!("read the archived {}+{}: {e}", r.offset, r.len))?;
            match source.read_at(at, &mut now[..n]) {
                Ok(()) => same_sectors(at, &now[..n], &kept[..n], sector)?,
                Err(ReadFailure::Unreadable(_)) => {
                    for (i, old) in kept[..n].chunks(sector).enumerate() {
                        let offset = at + (i * sector) as u64;
                        let buf = &mut now[..old.len()];
                        match source.read_at(offset, buf) {
                            Ok(()) => same_sectors(offset, buf, old, sector)?,
                            Err(ReadFailure::Unreadable(_)) => {}
                            Err(ReadFailure::Failed(e)) => {
                                return Err(format!("the comparison with the archive stopped: {e}"))
                            }
                        }
                    }
                }
                Err(ReadFailure::Failed(e)) => {
                    return Err(format!("the comparison with the archive stopped: {e}"))
                }
            }
            at += n as u64;
        }
    }
    Ok(())
}

/// Refuse `now`, read from the source at `offset`, if it differs from `kept`,
/// the archive's copy, naming the offset of the first sector that differs.
fn same_sectors(offset: u64, now: &[u8], kept: &[u8], sector: usize) -> Result<(), String> {
    match now.iter().zip(kept).position(|(a, b)| a != b) {
        None => Ok(()),
        Some(i) => {
            let byte = offset + i as u64;
            let at = byte - byte % sector as u64;
            Err(format!(
                "the source's sector at byte {at} differs from the archive's copy: \
                 the source changed since it was archived, or is another source"
            ))
        }
    }
}

/// The parts of `start..end` no record of `held`, sorted by offset, covers.
fn missing(held: &[ArchivedRecord], start: u64, end: u64) -> Vec<(u64, u64)> {
    let mut gaps = Vec::new();
    let mut at = start;
    for r in held {
        if at >= end || r.offset >= end {
            break;
        }
        if r.offset > at {
            gaps.push((at, r.offset));
        }
        at = at.max(r.offset + r.len);
    }
    if at < end {
        gaps.push((at, end));
    }
    gaps
}

/// The source as a resumed scan reads its layout: bytes the archive's data
/// records hold come from the archive, and only the rest from the source.
struct Archived<'a, S: ?Sized> {
    source: &'a mut S,
    archive: &'a mut File,
    /// The archive's data records, sorted by offset.
    held: &'a [ArchivedRecord],
}

impl<S: RescueSource + ?Sized> RescueSource for Archived<'_, S> {
    fn size(&self) -> u64 {
        self.source.size()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        let end = offset + buf.len() as u64;
        let mut at = offset;
        for r in self.held {
            let (a, b) = (r.offset, r.offset + r.len);
            if at >= end || a >= end {
                break;
            }
            if b <= at {
                continue;
            }
            if a > at {
                self.source
                    .read_at(at, &mut buf[(at - offset) as usize..(a - offset) as usize])?;
                at = a;
            }
            let upto = b.min(end);
            let part = &mut buf[(at - offset) as usize..(upto - offset) as usize];
            let from = r.position + RECORD_HEADER_LEN as u64 + (at - a);
            self.archive
                .seek(SeekFrom::Start(from))
                .and_then(|_| self.archive.read_exact(part))
                .map_err(|e| {
                    ReadFailure::Failed(format!("read the archived {a}+{}: {e}", r.len))
                })?;
            at = upto;
        }
        if at < end {
            self.source
                .read_at(at, &mut buf[(at - offset) as usize..])?;
        }
        Ok(())
    }
}

/// Archive `start..end` of the source in bulk reads of `buf`'s length.
fn scan_span<S: RescueSource + ?Sized>(
    source: &mut S,
    out: &mut Records<'_>,
    buf: &mut [u8],
    start: u64,
    end: u64,
    sector_size: u32,
) -> Result<(), String> {
    let mut offset = start;
    while offset < end {
        let n = (end - offset).min(buf.len() as u64) as usize;
        let chunk = &mut buf[..n];
        match source.read_at(offset, chunk) {
            Ok(()) => out.data(offset, chunk)?,
            Err(ReadFailure::Unreadable(_)) => {
                out.report.failed_bulk_reads += 1;
                retry_by_sector(source, out, offset, chunk, sector_size as usize)?;
            }
            Err(ReadFailure::Failed(e)) => return Err(format!("the scan stopped: {e}")),
        }
        offset += n as u64;
    }
    Ok(())
}

/// A half-open span of the source and the class of space it is in.
type Span = (u64, u64, RangeClass);

/// The order to archive an NTFS source in: the boot sector and its backup,
/// `$MFT` and `$MFTMirr`, then the clusters `$Bitmap` marks allocated, then
/// the free ones, then the tail past the last whole cluster. The spans tile
/// the source exactly once.
///
/// `None` when the boot sector does not parse as NTFS, or `$MFT`'s record
/// 0, 1 or 6 or `$Bitmap` cannot be read in full: the scan is then linear,
/// because it never guesses a layout. These reads touch no record and no
/// report, so a source with no readable layout is scanned exactly as it
/// would have been without them.
fn ntfs_order<S: RescueSource + ?Sized>(source: &mut S, size: u64) -> Option<Vec<Span>> {
    let mut boot = [0u8; 512];
    if size < boot.len() as u64 {
        return None;
    }
    source.read_at(0, &mut boot).ok()?;
    if &boot[3..11] != b"NTFS    " {
        return None;
    }
    let params = crate::mft_io::parse_boot_params_from_bytes(&boot).ok()?;
    let cluster = params.cluster_size;
    let bps = params.bytes_per_sector;
    let record = usize::try_from(params.file_record_size).ok()?;
    let clusters = params.volume_bytes().min(size) / cluster;
    if clusters == 0 {
        return None;
    }

    // $MFT's own record maps $MFT; records 1 and 6 are found through it.
    let mut mft = vec![0u8; record];
    source
        .read_at(params.mft_lcn.checked_mul(cluster)?, &mut mft)
        .ok()?;
    let mft_runs = unnamed_data_runs(&mut mft, bps, clusters)?;
    let nth_record =
        |source: &mut S, n: u64| read_runs(source, &mft_runs, cluster, n * record as u64, record);
    let mirr_runs = unnamed_data_runs(&mut nth_record(source, 1)?, bps, clusters)?;
    let bitmap_runs = unnamed_data_runs(&mut nth_record(source, 6)?, bps, clusters)?;
    let bitmap = read_runs(
        source,
        &bitmap_runs,
        cluster,
        0,
        usize::try_from(clusters.div_ceil(8)).ok()?,
    )?;

    let mut metadata = vec![(0, cluster)];
    // The backup boot sector is the sector after the volume's last.
    let backup = params.volume_bytes();
    if backup < size {
        metadata.push((backup, (backup + u64::from(bps)).min(size)));
    }
    for run in mft_runs.iter().chain(&mirr_runs) {
        let lcn = run.lcn?;
        metadata.push((lcn * cluster, (lcn + run.length) * cluster));
    }
    metadata.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (a, b) in metadata {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }

    let mut order: Vec<Span> = merged
        .iter()
        .map(|&(a, b)| (a, b, RangeClass::Metadata))
        .collect();
    let allocated = |c: u64| bitmap[(c / 8) as usize] >> (c % 8) & 1 == 1;
    for (class, want) in [(RangeClass::Allocated, true), (RangeClass::Free, false)] {
        let mut c = 0;
        while c < clusters {
            if allocated(c) != want {
                c += 1;
                continue;
            }
            let first = c;
            while c < clusters && allocated(c) == want {
                c += 1;
            }
            outside(&merged, first * cluster, c * cluster, class, &mut order);
        }
    }
    outside(
        &merged,
        clusters * cluster,
        size,
        RangeClass::Tail,
        &mut order,
    );
    Some(order)
}

/// Push the parts of `start..end` that no span of `taken` covers.
fn outside(taken: &[(u64, u64)], start: u64, end: u64, class: RangeClass, order: &mut Vec<Span>) {
    let mut at = start;
    for &(a, b) in taken {
        if b <= at || a >= end {
            continue;
        }
        if a > at {
            order.push((at, a, class));
        }
        at = at.max(b);
    }
    if at < end {
        order.push((at, end, class));
    }
}

/// The runs of an MFT record's unnamed, non-resident `$DATA`, each inside
/// the volume's `clusters`.
fn unnamed_data_runs(record: &mut [u8], bps: u16, clusters: u64) -> Option<Vec<DataRun>> {
    crate::mft_io::apply_fixup_on_read(record, bps).ok()?;
    let at = find_attribute(record, AttrType::Data, None)?;
    if at.is_resident {
        return None;
    }
    let pairs = at.attr_offset + usize::from(at.non_resident_mapping_pairs_offset?);
    let runs = decode_runs(record.get(pairs..at.attr_offset + at.attr_length)?).ok()?;
    let inside = |r: &DataRun| {
        r.lcn
            .and_then(|lcn| lcn.checked_add(r.length))
            .is_some_and(|end| end <= clusters)
    };
    (!runs.is_empty() && runs.iter().all(inside)).then_some(runs)
}

/// Read `len` bytes of a non-resident value from byte `start`, through its
/// runs. `None` when a byte is in no run or in a sparse one, or a read fails.
fn read_runs<S: RescueSource + ?Sized>(
    source: &mut S,
    runs: &[DataRun],
    cluster: u64,
    start: u64,
    len: usize,
) -> Option<Vec<u8>> {
    let mut out = vec![0u8; len];
    let mut done = 0;
    while done < len {
        let at = start + done as u64;
        let vcn = at / cluster;
        let run = runs
            .iter()
            .find(|r| vcn >= r.starting_vcn && vcn - r.starting_vcn < r.length)?;
        let lcn = run.lcn? + (vcn - run.starting_vcn);
        let run_end = run
            .starting_vcn
            .checked_add(run.length)?
            .checked_mul(cluster)?;
        let n = ((run_end - at) as usize).min(len - done);
        source
            .read_at(lcn * cluster + at % cluster, &mut out[done..done + n])
            .ok()?;
        done += n;
    }
    Some(out)
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
/// current unreadable span until it ends so adjacent sectors of one class
/// become one.
struct Records<'a> {
    archive: &'a mut File,
    /// The class of space the records being made are in.
    class: RangeClass,
    pending: Option<(u64, u64, RangeClass)>,
    report: ScanReport,
}

impl<'a> Records<'a> {
    fn new(archive: &'a mut File, size: u64) -> Self {
        Self {
            archive,
            class: RangeClass::Unclassified,
            pending: None,
            report: ScanReport {
                source_size: size,
                ..ScanReport::default()
            },
        }
    }

    fn data(&mut self, offset: u64, payload: &[u8]) -> Result<(), String> {
        self.close_span()?;
        let class = self.class;
        self.append(offset, payload.len() as u64, KIND_DATA, class, payload)?;
        self.report.readable_bytes += payload.len() as u64;
        self.report.classes[class as usize].archived += payload.len() as u64;
        self.report.data_records += 1;
        Ok(())
    }

    fn unreadable(&mut self, offset: u64, len: u64) -> Result<(), String> {
        let class = self.class;
        match &mut self.pending {
            Some((start, span, of)) if *of == class && *start + *span == offset => *span += len,
            _ => {
                self.close_span()?;
                self.pending = Some((offset, len, class));
            }
        }
        Ok(())
    }

    fn close_span(&mut self) -> Result<(), String> {
        if let Some((offset, len, class)) = self.pending.take() {
            self.append(offset, len, KIND_UNREADABLE, class, &[])?;
            self.report.unreadable_bytes += len;
            self.report.classes[class as usize].unreadable += len;
            self.report.unreadable_spans += 1;
        }
        Ok(())
    }

    fn append(
        &mut self,
        offset: u64,
        len: u64,
        kind: u32,
        class: RangeClass,
        payload: &[u8],
    ) -> Result<(), String> {
        let mut head = [0u8; RECORD_HEADER_LEN];
        head[0..8].copy_from_slice(&offset.to_le_bytes());
        head[8..16].copy_from_slice(&len.to_le_bytes());
        head[16..20].copy_from_slice(&kind.to_le_bytes());
        head[20..24].copy_from_slice(&class.code().to_le_bytes());
        let crc = crc32(&[&head[..24], payload]);
        head[24..28].copy_from_slice(&crc.to_le_bytes());
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

/// One whole record read back from an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchivedRecord {
    /// Where the record's header starts in the archive.
    pub position: u64,
    /// The record's span of the source.
    pub offset: u64,
    pub len: u64,
    /// [`KIND_DATA`] or [`KIND_UNREADABLE`].
    pub kind: u32,
    pub class: RangeClass,
}

/// An archive read back to its last whole record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveContents {
    pub sector_size: u32,
    pub source_size: u64,
    /// Every whole record, in archive order.
    pub records: Vec<ArchivedRecord>,
    /// The archive's length up to the end of its last whole record: where a
    /// resumed scan appends.
    pub whole_len: u64,
    /// Bytes past [`ArchiveContents::whole_len`]: a record an interruption
    /// cut short. Zero when the archive ends on a whole record.
    pub torn_bytes: u64,
}

/// Read an archive back to its last whole record, checking every CRC-32.
///
/// A record cut short at the end of the archive, in its header or its
/// payload, was being written when the scan was interrupted: it is left
/// out and its bytes counted in [`ArchiveContents::torn_bytes`]. A whole
/// record that breaks a rule of `docs/rescue-archive.md` is damage and is
/// refused, naming the archive byte its header starts at.
pub fn read_archive<R: Read>(archive: &mut R) -> Result<ArchiveContents, String> {
    let mut head = [0u8; HEADER_LEN];
    let got = read_up_to(archive, &mut head)?;
    if got < HEADER_LEN {
        return Err(format!("the archive ends {got} bytes into its header"));
    }
    if head[0..8] != ARCHIVE_MAGIC {
        return Err("the archive's magic is not NTRESCUE".into());
    }
    let version = le32(&head[8..12]);
    if version != ARCHIVE_VERSION {
        return Err(format!(
            "the archive is version {version}; only version {ARCHIVE_VERSION} is read"
        ));
    }
    if le32(&head[24..28]) != 0 {
        return Err("the archive header's reserved bytes are not zero".into());
    }
    if le32(&head[28..32]) != crc32(&[&head[..28]]) {
        return Err("the archive header fails its CRC-32".into());
    }
    let mut contents = ArchiveContents {
        sector_size: le32(&head[12..16]),
        source_size: u64::from_le_bytes(head[16..24].try_into().expect("8 bytes")),
        records: Vec::new(),
        whole_len: HEADER_LEN as u64,
        torn_bytes: 0,
    };
    let mut payload = Vec::new();
    loop {
        let position = contents.whole_len;
        let mut rh = [0u8; RECORD_HEADER_LEN];
        let got = read_up_to(archive, &mut rh)?;
        if got < RECORD_HEADER_LEN {
            contents.torn_bytes = got as u64;
            return Ok(contents);
        }
        let damaged = |why: String| format!("the record at archive byte {position} {why}");
        let offset = u64::from_le_bytes(rh[0..8].try_into().expect("8 bytes"));
        let len = u64::from_le_bytes(rh[8..16].try_into().expect("8 bytes"));
        let kind = le32(&rh[16..20]);
        let code = le32(&rh[20..24]);
        let class = RangeClass::ALL
            .into_iter()
            .find(|c| c.code() == code)
            .ok_or_else(|| damaged(format!("has class {code}")))?;
        if len == 0 {
            return Err(damaged("has a zero length".into()));
        }
        if offset
            .checked_add(len)
            .is_none_or(|end| end > contents.source_size)
        {
            return Err(damaged(format!(
                "runs past the source's {} bytes",
                contents.source_size
            )));
        }
        let payload_len = match kind {
            KIND_DATA if len <= MAX_CHUNK as u64 => len as usize,
            KIND_DATA => return Err(damaged(format!("holds {len} bytes, past {MAX_CHUNK}"))),
            KIND_UNREADABLE => 0,
            _ => return Err(damaged(format!("has kind {kind}"))),
        };
        payload.resize(payload_len, 0);
        let got = read_up_to(archive, &mut payload)?;
        if got < payload_len {
            contents.torn_bytes = (RECORD_HEADER_LEN + got) as u64;
            return Ok(contents);
        }
        if le32(&rh[24..28]) != crc32(&[&rh[..24], &payload]) {
            return Err(damaged(format!("for {offset}+{len} fails its CRC-32")));
        }
        contents.records.push(ArchivedRecord {
            position,
            offset,
            len,
            kind,
            class,
        });
        contents.whole_len += (RECORD_HEADER_LEN + payload_len) as u64;
    }
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    /// The image's length: the archived source's size.
    pub bytes: u64,
    /// The SHA-256 of the image's bytes.
    pub sha256: [u8; 32],
    /// The `(offset, length)` source spans no data record held, sorted by
    /// offset: written as zeros and listed in the error manifest at
    /// [`export_manifest_path`]. Empty for a complete image.
    pub missing: Vec<(u64, u64)>,
    /// For an incomplete image, whether its `$Volume` dirty flag was set,
    /// so Windows checks the volume before trusting it: `Err` names why it
    /// could not be (the `$Volume` record missing or not NTFS), and the
    /// error manifest says so too. `None` for a complete image, whose flag
    /// is left exactly as the source had it.
    pub dirty_flag: Option<Result<(), String>>,
}

impl ExportReport {
    /// Whether every byte of the image came from a data record. An image
    /// with any missing span is never complete, however few bytes it lacks.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// How an export treats an archive that lacks some source bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportOptions {
    /// Export an incomplete archive anyway, writing its missing spans as
    /// zeros and listing them in an error manifest beside the image. Off by
    /// default: an incomplete archive is refused.
    pub allow_incomplete: bool,
}

/// Where [`export_to_path`] writes before renaming into `destination`.
pub fn export_temp_path(destination: &Path) -> std::path::PathBuf {
    let mut name = destination.as_os_str().to_owned();
    name.push(".partial");
    name.into()
}

/// Where an incomplete export lists its missing spans: beside the image,
/// one `offset length` line per span in bytes, after `#` comment lines.
pub fn export_manifest_path(destination: &Path) -> std::path::PathBuf {
    let mut name = destination.as_os_str().to_owned();
    name.push(".missing.txt");
    name.into()
}

/// Export the source a complete archive holds to a new image at
/// `destination`.
///
/// An archive with any source byte no data record holds is refused before
/// anything is created, naming the first missing span. Otherwise the image
/// is written to [`export_temp_path`], flushed, read back against every
/// data record, and only then renamed into place. An existing destination
/// is refused, and the archive is only ever read.
pub fn export_to_path(archive: &Path, destination: &Path) -> Result<ExportReport, String> {
    export_with(archive, destination, ExportOptions::default())
}

/// [`export_to_path`], with `options` saying whether an incomplete archive
/// exports.
///
/// With [`ExportOptions::allow_incomplete`], an archive lacking some source
/// bytes exports with those spans as zeros, and an error manifest listing
/// every span by byte offset and length is written to
/// [`export_manifest_path`] before the image is renamed into place. The
/// report then names the spans and [`ExportReport::is_complete`] is false.
/// An existing manifest is refused like an existing destination.
pub fn export_with(
    archive: &Path,
    destination: &Path,
    options: ExportOptions,
) -> Result<ExportReport, String> {
    let input = File::open(archive)
        .map_err(|e| format!("open the archive '{}': {e}", archive.display()))?;
    let contents = read_archive(&mut io::BufReader::new(&input))?;
    let mut held: Vec<ArchivedRecord> = contents
        .records
        .iter()
        .filter(|r| r.kind == KIND_DATA)
        .copied()
        .collect();
    held.sort_by_key(|r| r.offset);
    let gaps: Vec<(u64, u64)> = missing(&held, 0, contents.source_size)
        .into_iter()
        .map(|(start, end)| (start, end - start))
        .collect();
    if let (Some(&(start, len)), false) = (gaps.first(), options.allow_incomplete) {
        return Err(format!(
            "the archive holds no data for source bytes {start}+{len}; only a complete archive exports"
        ));
    }
    if destination.symlink_metadata().is_ok() {
        return Err(format!(
            "the destination '{}' exists; an export never replaces a file",
            destination.display()
        ));
    }
    let manifest = export_manifest_path(destination);
    if !gaps.is_empty() && manifest.symlink_metadata().is_ok() {
        return Err(format!(
            "the error manifest '{}' exists; an export never replaces a file",
            manifest.display()
        ));
    }
    let temp = export_temp_path(destination);
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("create '{}': {e}", temp.display()))?;
    let written =
        write_image(&input, &held, &gaps, &mut out, contents.source_size).and_then(|report| {
            drop(out);
            if destination.symlink_metadata().is_ok() {
                return Err(format!(
                    "the destination '{}' appeared during the export",
                    destination.display()
                ));
            }
            // The manifest lands first: an incomplete image never sits
            // where a reader could take it for whole without its list of
            // missing spans beside it.
            if !report.is_complete() {
                write_manifest(&manifest, &report)?;
            }
            std::fs::rename(&temp, destination).map_err(|e| {
                if !report.is_complete() {
                    let _ = std::fs::remove_file(&manifest);
                }
                format!("rename into '{}': {e}", destination.display())
            })?;
            Ok(report)
        });
    match written {
        Ok(report) => {
            sync_parent(destination)?;
            Ok(report)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// Write `report`'s missing spans to the error manifest at `path` through a
/// flushed temporary file, refusing to replace one that exists.
fn write_manifest(path: &Path, report: &ExportReport) -> Result<(), String> {
    let total: u64 = report.missing.iter().map(|&(_, len)| len).sum();
    let mut text = format!(
        "# rescue export error manifest: {} spans, {total} of {} image bytes held by no data record, written as zeros\n# offset length\n",
        report.missing.len(),
        report.bytes
    );
    match &report.dirty_flag {
        Some(Ok(())) => text.push_str(
            "# the volume's dirty flag is set, so Windows checks it before trusting it\n",
        ),
        Some(Err(e)) => text.push_str(&format!(
            "# the volume's dirty flag is not set: {}\n",
            e.replace('\n', " ")
        )),
        None => {}
    }
    for &(offset, len) in &report.missing {
        text.push_str(&format!("{offset} {len}\n"));
    }
    let temp = export_temp_path(path);
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut out| {
            out.write_all(text.as_bytes())?;
            out.sync_all()
        })
        .map_err(|e| format!("write the error manifest '{}': {e}", temp.display()))
        .and_then(|()| {
            if path.symlink_metadata().is_ok() {
                return Err(format!(
                    "the error manifest '{}' appeared during the export",
                    path.display()
                ));
            }
            std::fs::rename(&temp, path)
                .map_err(|e| format!("rename into '{}': {e}", path.display()))
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Write each data record of `archive` into `out` at its offset, leaving
/// the `gaps` no record holds as zeros, flush, read every record's span
/// back against its payload and every gap back as zeros, and hash the
/// image.
fn write_image(
    mut archive: &File,
    held: &[ArchivedRecord],
    gaps: &[(u64, u64)],
    out: &mut File,
    size: u64,
) -> Result<ExportReport, String> {
    use sha2::{Digest, Sha256};
    let write_err = |e: io::Error| format!("write the image: {e}");
    let mut payload = Vec::new();
    let mut payload_of = |r: &ArchivedRecord, payload: &mut Vec<u8>| {
        payload.resize(r.len as usize, 0);
        archive
            .seek(SeekFrom::Start(r.position + RECORD_HEADER_LEN as u64))
            .and_then(|_| archive.read_exact(payload))
            .map_err(|e| format!("read the record at archive byte {}: {e}", r.position))
    };
    out.set_len(size).map_err(write_err)?;
    for r in held {
        payload_of(r, &mut payload)?;
        out.seek(SeekFrom::Start(r.offset))
            .and_then(|_| out.write_all(&payload))
            .map_err(write_err)?;
    }
    out.sync_all().map_err(write_err)?;
    let mut back = Vec::new();
    for r in held {
        payload_of(r, &mut payload)?;
        back.resize(payload.len(), 0);
        out.seek(SeekFrom::Start(r.offset))
            .and_then(|_| out.read_exact(&mut back))
            .map_err(|e| format!("read the image back: {e}"))?;
        if back != payload {
            return Err(format!(
                "the image disagrees with the record at archive byte {} over source bytes {}+{}",
                r.position, r.offset, r.len
            ));
        }
    }
    let mut zeros = vec![0u8; DEFAULT_CHUNK];
    for &(offset, len) in gaps {
        let mut at = offset;
        while at < offset + len {
            let n = (offset + len - at).min(zeros.len() as u64) as usize;
            out.seek(SeekFrom::Start(at))
                .and_then(|_| out.read_exact(&mut zeros[..n]))
                .map_err(|e| format!("read the image back: {e}"))?;
            if zeros[..n].iter().any(|&b| b != 0) {
                return Err(format!(
                    "the image holds non-zero bytes in the missing span {offset}+{len}"
                ));
            }
            at += n as u64;
        }
    }
    // An incomplete NTFS image is marked dirty the way Windows marks one:
    // `$Volume`'s VOLUME_INFORMATION flag, mirrored into `$MFTMirr`. A
    // `$Volume` record lost to a gap is not fabricated; the report says
    // why the flag is not set.
    let dirty_flag = (!gaps.is_empty()).then(|| {
        crate::fsck::set_dirty_io(&mut ImageIo { file: out, size })
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    let mut hash = Sha256::new();
    out.seek(SeekFrom::Start(0)).map_err(write_err)?;
    let mut buf = vec![0u8; DEFAULT_CHUNK];
    let mut left = size;
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        out.read_exact(&mut buf[..n])
            .map_err(|e| format!("read the image back: {e}"))?;
        hash.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(ExportReport {
        bytes: size,
        sha256: hash.finalize().into(),
        missing: gaps.to_vec(),
        dirty_flag,
    })
}

/// [`BlockIo`](crate::block_io::BlockIo) over the image an export is
/// writing, for setting its dirty flag in place.
struct ImageIo<'a> {
    file: &'a mut File,
    size: u64,
}

impl crate::block_io::BlockIo for ImageIo<'_> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(buf))
            .map_err(|e| format!("read the image @{offset} len {}: {e}", buf.len()))
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.write_all(buf))
            .map_err(|e| format!("write the image @{offset} len {}: {e}", buf.len()))
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn sync(&mut self) -> Result<(), String> {
        self.file
            .sync_all()
            .map_err(|e| format!("sync the image: {e}"))
    }
}

/// Read as many bytes as `r` has, up to `buf.len()`.
fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(m) => n += m,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read the archive: {e}")),
        }
    }
    Ok(n)
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().expect("4 bytes"))
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
