//! The rescue scan (#720), graded against bytes Windows wrote.
//!
//! The sources are the partitions Windows left mid-write and the copies
//! Windows recovered from them (`test-disks/windows-interrupted-{1,6}*`).
//! Only the read-back tests read an archive through this crate, and they
//! grade it against the test's own parse. The test parses the
//! layout `docs/rescue-archive.md` documents, checks every CRC-32 with an
//! independent implementation (`crc32fast`), rebuilds an image from the data
//! records with its own file writes, and reads each rebuilt copy Windows
//! recovered with the independent `ntfs` crate, against the size and SHA-256
//! Windows' `Get-FileHash` recorded for every file on it.

mod common;

use fs_ntfs::rescue::{
    read_archive, resume, scan, scan_to_path, FileSource, RangeClass, ReadFailure, RescueSource,
    ScanOptions,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const SECTOR: u64 = 512;
/// The scan's default bulk read.
const CHUNK: u64 = 1024 * 1024;

/// Each capture's SHA-256, as `tests/windows_interrupted_fixture.rs` records it.
const CAPTURES: [(&str, &str); 4] = [
    (
        "windows-interrupted-1",
        "c45659e95104eca5f793c3f21061f42705a01783e62d1c7a46c63e4089db0b9a",
    ),
    (
        "windows-interrupted-1.recovered",
        "16beeb97f2177b8abd02287c8803bf4faa460a7ee701ab842f8ef552ef3bc14a",
    ),
    (
        "windows-interrupted-6",
        "7393334b8b7c1faba047d8fd4081d0cd07e9c9de0d59d9e21e6b954845170442",
    ),
    (
        "windows-interrupted-6.recovered",
        "c4b18810f6aa6130073583d8343b355c31a9f4f94cf1740cb59b76af10b50308",
    ),
];

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn file_sha256(path: &str) -> String {
    let mut file = File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; CHUNK as usize];
    loop {
        let n = file
            .read(&mut buf)
            .unwrap_or_else(|e| panic!("read {path}: {e}"));
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    hex(&hash.finalize())
}

fn capture_sha256(name: &str) -> &'static str {
    CAPTURES
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("{name} has no recorded SHA-256"))
        .1
}

fn unpack(name: &str) -> String {
    let path = common::temp_image_path(name.replace('.', "_"));
    let out = File::create(&path).expect("create image copy");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    assert_eq!(
        file_sha256(&path),
        capture_sha256(name),
        "{name} is not the recorded capture"
    );
    path
}

fn new_file(path: &str) -> File {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap_or_else(|e| panic!("create {path}: {e}"))
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().unwrap())
}

fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().unwrap())
}

struct Record {
    offset: u64,
    len: u64,
    kind: u32,
    /// The class of space the span was in: 0 unclassified, 1 NTFS
    /// metadata, 2 allocated, 3 free, 4 the tail past the last cluster.
    class: u32,
}

struct Archive {
    sector: u32,
    size: u64,
    records: Vec<Record>,
}

impl Archive {
    /// The unreadable records, as (offset, length) in bytes.
    fn unreadable(&self) -> Vec<(u64, u64)> {
        self.records
            .iter()
            .filter(|r| r.kind == 2)
            .map(|r| (r.offset, r.len))
            .collect()
    }

    /// The records tile the source, taken in offset order: no gap and no
    /// overlap. The scan may archive them in another order.
    fn assert_tiles(&self) {
        let mut sorted: Vec<&Record> = self.records.iter().collect();
        sorted.sort_by_key(|r| r.offset);
        let mut next = 0;
        for r in sorted {
            assert!(r.len > 0, "an empty record at {}", r.offset);
            assert_eq!(
                r.offset, next,
                "a record starts at {} where {next} was expected",
                r.offset
            );
            next += r.len;
        }
        assert_eq!(next, self.size, "the records end short of the source");
    }
}

/// Read as many bytes as the file has, up to `buf.len()`.
fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        let m = r.read(&mut buf[n..]).expect("read the archive");
        if m == 0 {
            break;
        }
        n += m;
    }
    n
}

/// Parse an archive by its documented layout, checking every CRC-32 with
/// `crc32fast` before handing a data record's payload to `payload`.
fn parse(path: &str, mut payload: impl FnMut(u64, &[u8])) -> Archive {
    let mut r = BufReader::new(File::open(path).expect("open the archive"));
    let mut head = [0u8; 32];
    r.read_exact(&mut head).expect("the archive header");
    assert_eq!(&head[0..8], b"NTRESCUE", "the archive's magic");
    assert_eq!(le32(&head[8..12]), 2, "the archive's version");
    assert_eq!(le32(&head[24..28]), 0, "the header's reserved bytes");
    assert_eq!(
        le32(&head[28..32]),
        crc32fast::hash(&head[..28]),
        "the header's CRC-32"
    );
    let mut archive = Archive {
        sector: le32(&head[12..16]),
        size: le64(&head[16..24]),
        records: Vec::new(),
    };
    let mut bytes = Vec::new();
    loop {
        let mut rh = [0u8; 28];
        match read_up_to(&mut r, &mut rh) {
            0 => break,
            28 => {}
            n => panic!("the archive ends {n} bytes into a record header"),
        }
        let (offset, len, kind) = (le64(&rh[0..8]), le64(&rh[8..16]), le32(&rh[16..20]));
        let class = le32(&rh[20..24]);
        assert!(class <= 4, "the record at {offset} has class {class}");
        let mut crc = crc32fast::Hasher::new();
        crc.update(&rh[..24]);
        match kind {
            1 => {
                assert!(len <= 64 << 20, "a {len}-byte data record at {offset}");
                bytes.resize(len as usize, 0);
                r.read_exact(&mut bytes).expect("a data record's payload");
                crc.update(&bytes);
            }
            2 => {}
            k => panic!("the record at {offset} has kind {k}"),
        }
        assert_eq!(
            le32(&rh[24..28]),
            crc.finalize(),
            "the CRC-32 of the record at {offset}+{len}"
        );
        if kind == 1 {
            payload(offset, &bytes);
        }
        archive.records.push(Record {
            offset,
            len,
            kind,
            class,
        });
    }
    archive
}

/// Rebuild an image from the archive's data records with plain file writes.
fn rebuild(archive: &str) -> (String, Archive) {
    let path = common::temp_image_path("rescue_rebuilt");
    let mut img = new_file(&path);
    let parsed = parse(archive, |offset, bytes| {
        img.seek(SeekFrom::Start(offset))
            .expect("seek the rebuilt image");
        img.write_all(bytes).expect("write the rebuilt image");
    });
    img.set_len(parsed.size).expect("size the rebuilt image");
    (path, parsed)
}

/// Windows' manifest of the files on a recovered copy: path, size, SHA-256.
fn manifest(k: &str) -> Vec<(String, u64, String)> {
    let path = format!("test-disks/windows-interrupted-{k}.recovered.manifest");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 3, "{path}: {l:?}");
            (f[0].to_string(), f[1].parse().unwrap(), f[2].to_string())
        })
        .collect()
}

/// Every file Windows listed on recovered copy `k`, read from `img` by the
/// `ntfs` crate, has the size and SHA-256 Windows recorded.
fn files_match_windows(k: &str, img: &str) {
    let want = manifest(k);
    assert!(
        want.len() > 200,
        "snapshot {k}: the manifest lost its lines"
    );
    let (ntfs, mut reader) = common::open(img);
    for (path, size, sha) in &want {
        let bytes = common::read_file_all(&ntfs, &mut reader, &format!("/{path}"));
        assert_eq!(
            (bytes.len() as u64, hex(&Sha256::digest(&bytes))),
            (*size, sha.clone()),
            "snapshot {k}: /{path}, read by the ntfs crate from the rebuilt image, \
             against Windows' Get-FileHash"
        );
    }
}

/// A source with chosen sectors unreadable. A read touching one fails, as a
/// device's does, after filling the buffer up to the bad sector and with
/// garbage beyond it, so a scan that trusted a failed read would archive
/// wrong bytes.
struct Faulty {
    inner: FileSource,
    bad: BTreeSet<u64>,
    reads: u64,
}

impl RescueSource for Faulty {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        self.reads += 1;
        let first = offset / SECTOR;
        let last = (offset + buf.len() as u64 - 1) / SECTOR;
        if let Some(&bad) = self.bad.range(first..=last).next() {
            let good = ((bad - first) * SECTOR) as usize;
            if good > 0 {
                self.inner.read_at(offset, &mut buf[..good])?;
            }
            buf[good..].fill(0xA5);
            return Err(ReadFailure::Unreadable(format!(
                "injected bad sector {bad}"
            )));
        }
        self.inner.read_at(offset, buf)
    }
}

#[test]
fn a_scan_of_each_windows_capture_rebuilds_it_byte_for_byte() {
    for (name, sha) in CAPTURES {
        let src = unpack(name);
        let archive = common::temp_image_path("rescue_archive");
        let report = scan_to_path(Path::new(&src), Path::new(&archive), ScanOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(report.is_complete(), "{name}: {report:?}");
        assert_eq!(report.unreadable_spans, 0, "{name}: {report:?}");
        assert_eq!(report.failed_bulk_reads, 0, "{name}: {report:?}");

        let (rebuilt, parsed) = rebuild(&archive);
        assert_eq!(parsed.sector, 512, "{name}: the archive's sector size");
        assert_eq!(
            parsed.size, report.source_size,
            "{name}: the archive's size"
        );
        parsed.assert_tiles();
        assert!(parsed.unreadable().is_empty(), "{name}");
        assert_eq!(
            file_sha256(&rebuilt),
            sha,
            "{name}: the image rebuilt from the archive"
        );
        assert_eq!(
            file_sha256(&src),
            sha,
            "{name}: the scan changed its source"
        );
        if let Some(k) = name
            .strip_prefix("windows-interrupted-")
            .and_then(|k| k.strip_suffix(".recovered"))
        {
            files_match_windows(k, &rebuilt);
        }
        for path in [&src, &archive, &rebuilt] {
            std::fs::remove_file(path).expect("remove a scratch file");
        }
    }
}

#[test]
fn unreadable_sectors_are_recorded_exactly_and_every_other_byte_is_kept() {
    let name = "windows-interrupted-1.recovered";
    let src = unpack(name);
    let sectors = std::fs::metadata(&src).unwrap().len() / SECTOR;
    let per_chunk = CHUNK / SECTOR;
    // Half-open sector ranges: the first sector, a lone sector inside a
    // chunk, a run across the boundary of the first two chunks, a dead
    // region two and a half chunks long, and the last sector.
    let spans = [
        (0, 1),
        (1000, 1001),
        (per_chunk - 2, per_chunk + 4),
        (5 * per_chunk, 7 * per_chunk + per_chunk / 2),
        (sectors - 1, sectors),
    ];
    let mut source = Faulty {
        inner: FileSource::open(Path::new(&src)).unwrap(),
        bad: spans.iter().flat_map(|&(a, b)| a..b).collect(),
        reads: 0,
    };
    let archive = common::temp_image_path("rescue_archive");
    let mut out = new_file(&archive);
    let report = scan(&mut source, &mut out, ScanOptions::default()).expect("scan");
    drop(out);

    let mut capture = File::open(&src).unwrap();
    let mut want = Vec::new();
    let parsed = parse(&archive, |offset, bytes| {
        want.resize(bytes.len(), 0);
        capture.seek(SeekFrom::Start(offset)).unwrap();
        capture.read_exact(&mut want).unwrap();
        assert!(
            bytes == &want[..],
            "the data record at {offset}+{} differs from the capture",
            bytes.len()
        );
    });
    parsed.assert_tiles();
    let lost: Vec<(u64, u64)> = spans
        .iter()
        .map(|&(a, b)| (a * SECTOR, (b - a) * SECTOR))
        .collect();
    assert_eq!(
        parsed.unreadable(),
        lost,
        "the unreadable records, as (offset, length) in bytes"
    );
    let lost_bytes: u64 = lost.iter().map(|s| s.1).sum();
    assert_eq!(report.unreadable_bytes, lost_bytes, "{report:?}");
    assert_eq!(report.readable_bytes, report.source_size - lost_bytes);
    assert_eq!(report.unreadable_spans, 5, "{report:?}");
    assert!(!report.is_complete());

    // A linear scan knows no layout: every record, and every byte the
    // report counts, is unclassified.
    for r in &parsed.records {
        assert_eq!(
            r.class, 0,
            "the record at {}+{} has a class",
            r.offset, r.len
        );
    }
    let none = report.class(RangeClass::Unclassified);
    assert_eq!(none.archived, report.readable_bytes, "{report:?}");
    assert_eq!(none.unreadable, lost_bytes, "{report:?}");
    for class in [
        RangeClass::Metadata,
        RangeClass::Allocated,
        RangeClass::Free,
        RangeClass::Tail,
    ] {
        let bytes = report.class(class);
        assert_eq!(
            bytes.archived + bytes.unreadable,
            0,
            "{class:?}: {report:?}"
        );
    }

    // Chunks 0, 1, 5, 6 and 7 and the last, shorter one hold a bad sector.
    // Only those are read again, one sector at a time.
    let chunks = sectors.div_ceil(per_chunk);
    let last_chunk = sectors - (chunks - 1) * per_chunk;
    assert_eq!(report.failed_bulk_reads, 6, "{report:?}");
    assert_eq!(
        source.reads,
        1 + chunks + 5 * per_chunk + last_chunk,
        "the boot-sector read that found no layout, then bulk reads plus \
         one read per sector of each failed chunk"
    );
    assert_eq!(
        file_sha256(&src),
        capture_sha256(name),
        "the scan changed its source"
    );
}

#[test]
fn a_source_that_ends_inside_a_sector_keeps_its_last_bytes() {
    let path = common::temp_image_path("rescue_tail");
    let bytes: Vec<u8> = (0..5 * 512 + 100).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let mut source = Faulty {
        inner: FileSource::open(Path::new(&path)).unwrap(),
        bad: BTreeSet::from([2]),
        reads: 0,
    };
    let archive = common::temp_image_path("rescue_archive");
    let mut out = new_file(&archive);
    let options = ScanOptions {
        sector_size: 512,
        chunk_size: 1024,
    };
    let report = scan(&mut source, &mut out, options).expect("scan");
    drop(out);

    let mut got = vec![0u8; bytes.len()];
    let parsed = parse(&archive, |offset, b| {
        got[offset as usize..offset as usize + b.len()].copy_from_slice(b)
    });
    parsed.assert_tiles();
    assert_eq!(parsed.unreadable(), vec![(1024, 512)]);
    assert_eq!(&got[..1024], &bytes[..1024]);
    assert_eq!(&got[1536..], &bytes[1536..]);
    let tail = parsed.records.last().unwrap();
    assert_eq!((tail.offset, tail.len, tail.kind), (2048, 612, 1));
    assert_eq!(report.failed_bulk_reads, 1, "{report:?}");
}

#[test]
fn a_failure_that_is_not_the_medium_stops_the_scan() {
    struct Vanishing;
    impl RescueSource for Vanishing {
        fn size(&self) -> u64 {
            4 * CHUNK
        }
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
            if offset >= 2 * CHUNK {
                return Err(ReadFailure::Failed("the device went away".into()));
            }
            buf.fill(0x5A);
            Ok(())
        }
    }
    let archive = common::temp_image_path("rescue_archive");
    let mut out = new_file(&archive);
    let err = scan(&mut Vanishing, &mut out, ScanOptions::default())
        .expect_err("a failure other than the medium's stops the scan");
    drop(out);
    assert!(err.contains("the device went away"), "{err}");
    // What was read before the failure is archived; nothing after it is
    // recorded as unreadable.
    let parsed = parse(&archive, |_, b| assert!(b.iter().all(|&x| x == 0x5A)));
    assert_eq!(parsed.records.len(), 2);
    assert!(parsed.unreadable().is_empty());
}

#[test]
fn an_existing_file_is_never_taken_as_the_archive() {
    let name = "windows-interrupted-6";
    let src = unpack(name);
    let existing = common::temp_image_path("rescue_existing");
    std::fs::write(&existing, b"not an archive").unwrap();
    let err = scan_to_path(
        Path::new(&src),
        Path::new(&existing),
        ScanOptions::default(),
    )
    .expect_err("an existing file is refused as the archive");
    assert!(err.contains("create the archive"), "{err}");
    assert_eq!(std::fs::read(&existing).unwrap(), b"not an archive");

    let err = scan_to_path(Path::new(&src), Path::new(&src), ScanOptions::default())
        .expect_err("the source is refused as its own archive");
    assert!(err.contains("create the archive"), "{err}");
    assert_eq!(
        file_sha256(&src),
        capture_sha256(name),
        "the refused scan changed its source"
    );
}

/// The 512-byte sectors `$Bitmap`'s data is in, found by the independent
/// `ntfs` crate reading MFT record 6's unnamed `$DATA`.
fn bitmap_sectors(img: &str) -> BTreeSet<u64> {
    use ntfs::NtfsReadSeek;
    let (ntfs, mut reader) = common::open(img);
    let file = ntfs.file(&mut reader, 6).expect("MFT record 6");
    let item = file
        .data(&mut reader, "")
        .expect("$Bitmap has a $DATA attribute")
        .expect("$Bitmap's $DATA");
    let attr = item.to_attribute().expect("to_attribute");
    let mut value = attr.value(&mut reader).expect("$Bitmap's value");
    let mut sectors = BTreeSet::new();
    let mut buf = [0u8; SECTOR as usize];
    loop {
        let at = value.data_position().value();
        let n = value.read(&mut reader, &mut buf).expect("read $Bitmap");
        if n == 0 {
            break;
        }
        let at = at.expect("$Bitmap has no sparse run").get();
        sectors.extend(at / SECTOR..(at + n as u64).div_ceil(SECTOR));
    }
    assert!(!sectors.is_empty(), "{img}: $Bitmap has no data");
    sectors
}

#[test]
fn a_source_whose_bitmap_is_unreadable_is_scanned_linearly() {
    let name = "windows-interrupted-1.recovered";
    let src = unpack(name);
    let bad = bitmap_sectors(&src);
    let lost = bad.len() as u64 * SECTOR;
    let mut source = Faulty {
        inner: FileSource::open(Path::new(&src)).unwrap(),
        bad,
        reads: 0,
    };
    let archive = common::temp_image_path("rescue_archive");
    let mut out = new_file(&archive);
    let report = scan(&mut source, &mut out, ScanOptions::default()).expect("scan");
    drop(out);
    let parsed = parse(&archive, |_, _| {});
    parsed.assert_tiles();
    let mut next = 0;
    for r in &parsed.records {
        assert_eq!(
            r.offset, next,
            "without a readable $Bitmap the scan is linear: the record at {} is out of order",
            r.offset
        );
        next += r.len;
        assert_eq!(
            r.class, 0,
            "without a readable $Bitmap the record at {}+{} has a class",
            r.offset, r.len
        );
    }
    assert_eq!(report.unreadable_bytes, lost, "{report:?}");
    let none = report.class(RangeClass::Unclassified);
    assert_eq!(none.archived, report.readable_bytes, "{report:?}");
    assert_eq!(none.unreadable, lost, "{report:?}");
    std::fs::remove_file(&archive).expect("remove the archive");
    std::fs::remove_file(&src).expect("remove the capture copy");
}

#[test]
fn a_windows_capture_is_archived_metadata_and_allocated_before_free() {
    for (name, sha) in CAPTURES {
        let src = unpack(name);
        let archive = common::temp_image_path("rescue_archive");
        let report = scan_to_path(Path::new(&src), Path::new(&archive), ScanOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(report.is_complete(), "{name}: {report:?}");
        let (rebuilt, parsed) = rebuild(&archive);
        parsed.assert_tiles();
        let classes: Vec<u32> = parsed.records.iter().map(|r| r.class).collect();
        assert!(
            !classes.contains(&0),
            "{name}: a source with a readable layout left a record unclassified"
        );
        for class in [1, 2, 3] {
            assert!(
                classes.contains(&class),
                "{name}: no record of class {class}"
            );
        }
        let first_free = classes.iter().position(|&c| c == 3).unwrap();
        if let Some(late) = classes[first_free..].iter().position(|&c| c == 1 || c == 2) {
            let r = &parsed.records[first_free + late];
            panic!(
                "{name}: the class {} record at {}+{} follows the first free record",
                r.class, r.offset, r.len
            );
        }
        let mut seen = 0;
        for &c in &classes {
            assert!(c >= seen, "{name}: class {c} after class {seen}");
            seen = c;
        }
        assert_eq!(
            file_sha256(&rebuilt),
            sha,
            "{name}: the image rebuilt from the archive"
        );
        for path in [&src, &archive, &rebuilt] {
            std::fs::remove_file(path).expect("remove a scratch file");
        }
    }
}

/// The 512-byte sectors of every cluster `$Bitmap` marks free, as the
/// independent `ntfs` crate reads `$Bitmap` (MFT record 6's unnamed `$DATA`)
/// and the volume's geometry from the boot sector.
fn free_sectors(img: &str) -> BTreeSet<u64> {
    use ntfs::NtfsReadSeek;
    let (ntfs, mut reader) = common::open(img);
    let cluster = u64::from(ntfs.cluster_size());
    let clusters = ntfs.size() / cluster;
    let file = ntfs.file(&mut reader, 6).expect("MFT record 6");
    let item = file
        .data(&mut reader, "")
        .expect("$Bitmap has a $DATA attribute")
        .expect("$Bitmap's $DATA");
    let attr = item.to_attribute().expect("to_attribute");
    let mut value = attr.value(&mut reader).expect("$Bitmap's value");
    let mut bitmap = vec![0u8; clusters.div_ceil(8) as usize];
    let mut done = 0;
    while done < bitmap.len() {
        let n = value
            .read(&mut reader, &mut bitmap[done..])
            .expect("read $Bitmap");
        assert!(n > 0, "{img}: $Bitmap ends before cluster {}", done * 8);
        done += n;
    }
    let per_cluster = cluster / SECTOR;
    let free: BTreeSet<u64> = (0..clusters)
        .filter(|&c| bitmap[(c / 8) as usize] >> (c % 8) & 1 == 0)
        .flat_map(|c| c * per_cluster..(c + 1) * per_cluster)
        .collect();
    assert!(!free.is_empty(), "{img}: no free cluster");
    free
}

#[test]
fn a_scan_with_every_free_cluster_unreadable_keeps_every_file() {
    for (name, _) in CAPTURES {
        let Some(k) = name
            .strip_prefix("windows-interrupted-")
            .and_then(|k| k.strip_suffix(".recovered"))
        else {
            continue;
        };
        let src = unpack(name);
        let bad = free_sectors(&src);
        let lost = bad.len() as u64 * SECTOR;
        let mut source = Faulty {
            inner: FileSource::open(Path::new(&src)).unwrap(),
            bad,
            reads: 0,
        };
        let archive = common::temp_image_path("rescue_archive");
        let mut out = new_file(&archive);
        let report = scan(&mut source, &mut out, ScanOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        drop(out);

        let (rebuilt, parsed) = rebuild(&archive);
        parsed.assert_tiles();
        for r in parsed.records.iter().filter(|r| r.kind == 2) {
            assert_eq!(
                r.class, 3,
                "{name}: the unreadable record at {}+{} is not free space",
                r.offset, r.len
            );
        }
        assert_eq!(report.unreadable_bytes, lost, "{name}: {report:?}");
        for class in [
            RangeClass::Unclassified,
            RangeClass::Metadata,
            RangeClass::Allocated,
            RangeClass::Tail,
        ] {
            assert_eq!(
                report.class(class).unreadable,
                0,
                "{name}: {class:?} bytes reported unreadable: {report:?}"
            );
        }
        let free = report.class(RangeClass::Free);
        assert_eq!(
            (free.archived, free.unreadable),
            (0, lost),
            "{name}: the free space Windows' $Bitmap names: {report:?}"
        );
        files_match_windows(k, &rebuilt);
        for path in [&src, &archive, &rebuilt] {
            std::fs::remove_file(path).expect("remove a scratch file");
        }
    }
}

/// A source that stops the scan, as an unplugged device does, at the first
/// read that touches a free cluster: the scan ends after the allocated pass.
struct StopsAtFree {
    inner: FileSource,
    free: BTreeSet<u64>,
}

const STOPPED: &str = "the source stopped at free space";

impl RescueSource for StopsAtFree {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        let first = offset / SECTOR;
        let last = (offset + buf.len() as u64 - 1) / SECTOR;
        if self.free.range(first..=last).next().is_some() {
            return Err(ReadFailure::Failed(STOPPED.into()));
        }
        self.inner.read_at(offset, buf)
    }
}

#[test]
fn a_scan_stopped_after_the_allocated_pass_keeps_every_file() {
    for (name, _) in CAPTURES {
        let Some(k) = name
            .strip_prefix("windows-interrupted-")
            .and_then(|k| k.strip_suffix(".recovered"))
        else {
            continue;
        };
        let src = unpack(name);
        let mut source = StopsAtFree {
            inner: FileSource::open(Path::new(&src)).unwrap(),
            free: free_sectors(&src),
        };
        let archive = common::temp_image_path("rescue_archive");
        let mut out = new_file(&archive);
        let err = scan(&mut source, &mut out, ScanOptions::default())
            .expect_err("a source that stops at free space stops the scan");
        drop(out);
        assert!(err.contains(STOPPED), "{name}: {err}");

        let (rebuilt, parsed) = rebuild(&archive);
        assert!(parsed.unreadable().is_empty(), "{name}");
        let classes: BTreeSet<u32> = parsed.records.iter().map(|r| r.class).collect();
        assert_eq!(
            classes,
            BTreeSet::from([1, 2]),
            "{name}: the classes archived before the free pass"
        );
        files_match_windows(k, &rebuilt);
        for path in [&src, &archive, &rebuilt] {
            std::fs::remove_file(path).expect("remove a scratch file");
        }
    }
}

/// The first 8 MiB of a Windows capture, with three sectors unreadable,
/// scanned in 256 KiB chunks: an archive with both kinds of record that is
/// small enough to cut at every record boundary. Returns the archive's
/// bytes and where each record starts in them, by the documented layout,
/// with the archive's length last.
fn cut_fixture() -> (Vec<u8>, Archive, Vec<u64>) {
    let src = unpack("windows-interrupted-1");
    let prefix = common::temp_image_path("rescue_prefix");
    let mut head = vec![0u8; 8 << 20];
    File::open(&src)
        .and_then(|mut f| f.read_exact(&mut head))
        .expect("read the capture's first 8 MiB");
    new_file(&prefix)
        .write_all(&head)
        .expect("write the prefix");
    let mut source = Faulty {
        inner: FileSource::open(Path::new(&prefix)).unwrap(),
        bad: BTreeSet::from([100, 101, 9000]),
        reads: 0,
    };
    let archive = common::temp_image_path("rescue_archive");
    let options = ScanOptions {
        sector_size: 512,
        chunk_size: 256 << 10,
    };
    scan(&mut source, &mut new_file(&archive), options).expect("scan the prefix");
    let parsed = parse(&archive, |_, _| {});
    let bytes = std::fs::read(&archive).expect("read the archive");
    let mut starts = vec![32u64];
    for r in &parsed.records {
        let payload = if r.kind == 1 { r.len } else { 0 };
        starts.push(starts.last().unwrap() + 28 + payload);
    }
    assert_eq!(
        *starts.last().unwrap(),
        bytes.len() as u64,
        "the archive's length"
    );
    assert!(
        !parsed.unreadable().is_empty(),
        "the fixture has unreadable records"
    );
    for path in [&src, &prefix, &archive] {
        std::fs::remove_file(path).expect("remove a scratch file");
    }
    (bytes, parsed, starts)
}

#[test]
fn an_archive_cut_short_reads_back_to_its_last_whole_record() {
    let (bytes, parsed, starts) = cut_fixture();
    let n = parsed.records.len();
    for k in 0..=n {
        let mut cuts = vec![starts[k]];
        if k < n {
            // Inside the header, one byte short of it, and inside the payload.
            cuts.extend([starts[k] + 1, starts[k] + 27, starts[k + 1] - 1]);
            if parsed.records[k].kind == 1 {
                cuts.push(starts[k] + 28 + parsed.records[k].len / 2);
            }
        }
        for cut in cuts {
            let got = read_archive(&mut &bytes[..cut as usize])
                .unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
            assert_eq!(got.sector_size, parsed.sector, "cut at {cut}");
            assert_eq!(got.source_size, parsed.size, "cut at {cut}");
            assert_eq!(
                got.whole_len, starts[k],
                "cut at {cut}: the whole records' end"
            );
            assert_eq!(
                got.torn_bytes,
                cut - starts[k],
                "cut at {cut}: the torn bytes"
            );
            assert_eq!(got.records.len(), k, "cut at {cut}: the whole records");
            for (i, (r, want)) in got.records.iter().zip(&parsed.records).enumerate() {
                assert_eq!(
                    (r.position, r.offset, r.len, r.kind, r.class.code()),
                    (starts[i], want.offset, want.len, want.kind, want.class),
                    "cut at {cut}: record {i}"
                );
            }
        }
    }
}

#[test]
fn a_whole_record_with_a_bad_checksum_is_refused_as_damage() {
    let (mut bytes, parsed, starts) = cut_fixture();
    let i = (parsed.records.len() / 2..parsed.records.len())
        .find(|&i| parsed.records[i].kind == 1)
        .expect("a data record in the archive's second half");
    let at = starts[i] + 28 + parsed.records[i].len / 2;
    bytes[at as usize] ^= 0x01;
    let err = read_archive(&mut &bytes[..])
        .expect_err("a flipped payload byte is damage, not an interruption");
    assert!(
        err.contains(&format!("archive byte {}", starts[i])),
        "the refusal names the damaged record: {err}"
    );
    // Cut inside that record, the damage is a torn tail like any other.
    let got = read_archive(&mut &bytes[..at as usize]).expect("the cut archive");
    assert_eq!(got.records.len(), i);
}

/// A source of the right size that counts the reads it is asked for and
/// serves none of them.
struct Untouched {
    size: u64,
    reads: u64,
}

impl RescueSource for Untouched {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        self.reads += 1;
        Err(ReadFailure::Failed(format!(
            "a refused resume read {offset}+{}",
            buf.len()
        )))
    }
}

#[test]
fn resuming_a_damaged_archive_is_refused_and_leaves_it_unchanged() {
    let (bytes, parsed, starts) = cut_fixture();
    let options = ScanOptions {
        sector_size: 512,
        chunk_size: 256 << 10,
    };
    // A data record and an unreadable record, neither the last, so a
    // flipped length cannot pass for a torn tail.
    let n = parsed.records.len();
    let data = (1..n - 1)
        .find(|&i| parsed.records[i].kind == 1)
        .expect("a data record before the last");
    let lost = (1..n - 1)
        .find(|&i| parsed.records[i].kind == 2)
        .expect("an unreadable record before the last");
    // The header's magic, version, sector size, source size, reserved bytes
    // and checksum; then each record's offset, length, kind, class, checksum
    // and, for the data record, its payload.
    let mut flips: Vec<(String, u64)> = [0, 8, 12, 16, 24, 28]
        .into_iter()
        .map(|at| (format!("header byte {at}"), at))
        .collect();
    for i in [data, lost] {
        for (part, at) in [
            ("offset", 0),
            ("length", 8),
            ("kind", 16),
            ("class", 20),
            ("checksum", 24),
        ] {
            flips.push((format!("record {i}'s {part}"), starts[i] + at));
        }
    }
    flips.push((
        format!("record {data}'s payload"),
        starts[data] + 28 + parsed.records[data].len / 2,
    ));

    let archive = common::temp_image_path("rescue_resume_damaged");
    for (what, at) in flips {
        let mut damaged = bytes.clone();
        damaged[at as usize] ^= 0x01;
        new_file(&archive)
            .write_all(&damaged)
            .expect("write the damaged archive");
        let before = file_sha256(&archive);
        let mut source = Untouched {
            size: parsed.size,
            reads: 0,
        };
        let mut out = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&archive)
            .expect("open the damaged archive");
        let got = resume(&mut source, &mut out, options);
        drop(out);
        assert!(
            got.is_err(),
            "{what} flipped: the resume was not refused: {got:?}"
        );
        assert_eq!(
            source.reads, 0,
            "{what} flipped: the refused resume read the source"
        );
        assert_eq!(
            file_sha256(&archive),
            before,
            "{what} flipped: the refused resume changed the archive"
        );
        std::fs::remove_file(&archive).expect("remove the damaged archive");
    }
}

/// A source that logs every read it is asked for, as (offset, length).
struct Logged {
    inner: Faulty,
    log: Vec<(u64, u64)>,
}

impl RescueSource for Logged {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        self.log.push((offset, buf.len() as u64));
        self.inner.read_at(offset, buf)
    }
}

#[test]
fn a_resumed_scan_reads_again_only_ranges_with_no_data_record() {
    // The first 8 MiB of a Windows capture. Sectors 100 and 101 fail on the
    // first pass only; sector 9000 fails on both.
    let src = unpack("windows-interrupted-1");
    let prefix = common::temp_image_path("rescue_resume_prefix");
    let mut head = vec![0u8; 8 << 20];
    File::open(&src)
        .and_then(|mut f| f.read_exact(&mut head))
        .expect("read the capture's first 8 MiB");
    new_file(&prefix)
        .write_all(&head)
        .expect("write the prefix");
    let options = ScanOptions {
        sector_size: 512,
        chunk_size: 256 << 10,
    };
    let healed: BTreeSet<u64> = BTreeSet::from([100, 101]);
    let dead: BTreeSet<u64> = BTreeSet::from([9000]);
    let archive = common::temp_image_path("rescue_resume_archive");
    let mut first = Faulty {
        inner: FileSource::open(Path::new(&prefix)).unwrap(),
        bad: healed.union(&dead).copied().collect(),
        reads: 0,
    };
    scan(&mut first, &mut new_file(&archive), options).expect("the first pass");

    // Interrupt it: cut the archive halfway through the payload of the first
    // data record after its last unreadable record.
    let whole = parse(&archive, |_, _| {});
    let mut starts = vec![32u64];
    for r in &whole.records {
        let payload = if r.kind == 1 { r.len } else { 0 };
        starts.push(starts.last().unwrap() + 28 + payload);
    }
    let last_lost = whole
        .records
        .iter()
        .rposition(|r| r.kind == 2)
        .expect("the first pass recorded unreadable sectors");
    let k = (last_lost + 1..whole.records.len())
        .find(|&i| whole.records[i].kind == 1)
        .expect("a data record after the last unreadable one");
    let cut = starts[k] + 28 + whole.records[k].len / 2;
    let before = std::fs::read(&archive).expect("read the first pass's archive");
    OpenOptions::new()
        .write(true)
        .open(&archive)
        .and_then(|f| f.set_len(cut))
        .expect("cut the archive");
    let kept: Vec<(u64, u64)> = whole.records[..k]
        .iter()
        .filter(|r| r.kind == 1)
        .map(|r| (r.offset, r.offset + r.len))
        .collect();

    let mut second = Logged {
        inner: Faulty {
            inner: FileSource::open(Path::new(&prefix)).unwrap(),
            bad: dead.clone(),
            reads: 0,
        },
        log: Vec::new(),
    };
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&archive)
        .expect("open the cut archive");
    resume(&mut second, &mut out, options).expect("the resumed pass");
    drop(out);

    // The resume first reads each kept record's span again, in archive
    // order, to compare it with the archive (#983). Only the scan after that
    // comparison is held to the ranges no data record covers.
    let mut i = 0;
    for &(a, b) in &kept {
        let mut at = a;
        while at < b {
            assert_eq!(
                second.log.get(i).map(|r| r.0),
                Some(at),
                "the comparison did not read the kept record {a}..{b} in order"
            );
            at += second.log[i].1;
            i += 1;
        }
        assert_eq!(at, b, "the comparison read past the kept record {a}..{b}");
    }
    for &(offset, len) in &second.log[i..] {
        for &(a, b) in &kept {
            assert!(
                offset + len <= a || offset >= b,
                "the resumed pass read {offset}+{len}, which the data record \
                 {a}..{b} already holds"
            );
        }
    }
    let after = std::fs::read(&archive).expect("read the resumed archive");
    assert!(
        after.len() as u64 > starts[k]
            && after[..starts[k] as usize] == before[..starts[k] as usize],
        "the resume changed the archive's whole records"
    );

    // Every byte but sector 9000 is in exactly one data record, holding the
    // capture's bytes; sector 9000 is in none.
    let mut held: Vec<(u64, u64)> = Vec::new();
    let parsed = parse(&archive, |offset, bytes| {
        assert!(
            bytes == &head[offset as usize..offset as usize + bytes.len()],
            "the data record at {offset}+{} differs from the capture",
            bytes.len()
        );
        held.push((offset, offset + bytes.len() as u64));
    });
    held.sort();
    let mut next = 0;
    for &(a, b) in &held {
        if next == 9000 * SECTOR {
            next += SECTOR;
        }
        assert_eq!(
            a, next,
            "a data record starts at {a} where {next} was expected"
        );
        next = b;
    }
    assert_eq!(
        next, parsed.size,
        "the data records end short of the source"
    );

    // The first pass's unreadable records stay as history; the dead sector
    // is unreadable again, with no payload.
    let lost = parsed.unreadable();
    assert_eq!(
        lost[..lost.len() - 1],
        whole.unreadable()[..],
        "the first pass's unreadable records"
    );
    assert_eq!(
        lost[lost.len() - 1],
        (9000 * SECTOR, SECTOR),
        "the resumed pass's unreadable record"
    );
    for path in [&src, &prefix, &archive] {
        std::fs::remove_file(path).expect("remove a scratch file");
    }
}

/// A 1 MiB source that is no NTFS volume, archived in 64 KiB records, with
/// the archive cut halfway through its last record's payload. Returns the
/// source's path, its bytes and the archive's path.
fn cut_linear_archive(name: &str) -> (String, Vec<u8>, String) {
    let bytes: Vec<u8> = (0..1u32 << 20)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let src = common::temp_image_path(format!("{name}_source"));
    new_file(&src).write_all(&bytes).expect("write the source");
    let archive = common::temp_image_path(format!("{name}_archive"));
    let options = ScanOptions {
        sector_size: 512,
        chunk_size: 64 << 10,
    };
    let mut source = FileSource::open(Path::new(&src)).unwrap();
    scan(&mut source, &mut new_file(&archive), options).expect("the first pass");
    let len = std::fs::metadata(&archive).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&archive)
        .and_then(|f| f.set_len(len - (32 << 10)))
        .expect("cut the archive");
    (src, bytes, archive)
}

#[test]
fn a_resume_refuses_a_source_whose_retained_bytes_changed_naming_the_sector() {
    let (src, mut bytes, archive) = cut_linear_archive("rescue_resume_changed");
    // A byte inside the fifth record, in sector 585.
    bytes[300_001] ^= 0x01;
    std::fs::write(&src, &bytes).expect("change one byte of the source");
    let before = file_sha256(&archive);
    let mut source = FileSource::open(Path::new(&src)).unwrap();
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&archive)
        .expect("open the cut archive");
    let got = resume(
        &mut source,
        &mut out,
        ScanOptions {
            sector_size: 512,
            chunk_size: 64 << 10,
        },
    );
    drop(out);
    let err = got.expect_err("a source whose retained bytes changed was resumed");
    assert!(
        err.contains(&(585 * SECTOR).to_string()),
        "the refusal does not name the changed sector's offset {}: {err}",
        585 * SECTOR
    );
    assert_eq!(
        file_sha256(&archive),
        before,
        "the refused resume changed the archive"
    );
    for path in [&src, &archive] {
        std::fs::remove_file(path).expect("remove a scratch file");
    }
}

#[test]
fn a_retained_sector_that_no_longer_reads_does_not_refuse_the_resume() {
    let (src, mut bytes, archive) = cut_linear_archive("rescue_resume_unreadable");
    // Sector 585 changes and stops reading: an unreadable sector is no
    // difference.
    bytes[300_001] ^= 0x01;
    std::fs::write(&src, &bytes).expect("change one byte of the source");
    let mut source = Faulty {
        inner: FileSource::open(Path::new(&src)).unwrap(),
        bad: BTreeSet::from([585]),
        reads: 0,
    };
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&archive)
        .expect("open the cut archive");
    let got = resume(
        &mut source,
        &mut out,
        ScanOptions {
            sector_size: 512,
            chunk_size: 64 << 10,
        },
    );
    drop(out);
    got.expect("a retained sector that no longer reads refused the resume");
    let parsed = parse(&archive, |_, _| {});
    assert_eq!(
        parsed
            .records
            .iter()
            .filter(|r| r.kind == 1)
            .map(|r| r.len)
            .sum::<u64>(),
        parsed.size,
        "the resumed archive does not hold every byte of the source"
    );
    for path in [&src, &archive] {
        std::fs::remove_file(path).expect("remove a scratch file");
    }
}

/// Where each record of `parsed` starts in its archive, by the documented
/// layout, with the archive's length last.
fn record_starts(parsed: &Archive) -> Vec<u64> {
    let mut starts = vec![32u64];
    for r in &parsed.records {
        let payload = if r.kind == 1 { r.len } else { 0 };
        starts.push(starts.last().unwrap() + 28 + payload);
    }
    starts
}

/// The record boundaries an interrupted scan is resumed from: the header
/// alone, the first record of each class after the first, the middle record
/// and the whole archive. Sampled, not every boundary, so the test stays
/// inside its tier's budget.
fn sampled_boundaries(parsed: &Archive) -> BTreeSet<usize> {
    let n = parsed.records.len();
    let mut at = BTreeSet::from([0, n / 2, n]);
    for k in 1..n {
        if parsed.records[k].class != parsed.records[k - 1].class {
            at.insert(k);
        }
    }
    at
}

#[test]
fn an_archive_interrupted_anywhere_and_resumed_rebuilds_each_windows_capture() {
    for (name, sha) in CAPTURES {
        let src = unpack(name);
        let full = common::temp_image_path("rescue_resume_full");
        scan_to_path(Path::new(&src), Path::new(&full), ScanOptions::default())
            .unwrap_or_else(|e| panic!("{name}: the uninterrupted scan: {e}"));
        let whole = parse(&full, |_, _| {});
        let starts = record_starts(&whole);
        let bytes = std::fs::read(&full).expect("read the uninterrupted archive");
        assert_eq!(
            *starts.last().unwrap(),
            bytes.len() as u64,
            "{name}: the archive's length"
        );
        let classes: BTreeSet<u32> = whole.records.iter().map(|r| r.class).collect();
        assert!(
            classes.is_superset(&BTreeSet::from([1, 2, 3])),
            "{name}: the archive has no metadata, allocated and free records to cut between"
        );
        for k in sampled_boundaries(&whole) {
            // At the boundary, and one byte past it into a torn record.
            let mut cuts = vec![starts[k]];
            if k < whole.records.len() {
                cuts.push(starts[k] + 1);
            }
            for cut in cuts {
                let archive = common::temp_image_path("rescue_resume_cut");
                new_file(&archive)
                    .write_all(&bytes[..cut as usize])
                    .expect("write the interrupted archive");
                let mut source = FileSource::open(Path::new(&src)).unwrap();
                let mut out = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&archive)
                    .expect("open the interrupted archive");
                let report = resume(&mut source, &mut out, ScanOptions::default())
                    .unwrap_or_else(|e| panic!("{name}: resume from byte {cut}: {e}"));
                drop(out);
                assert!(
                    report.is_complete(),
                    "{name}: resume from byte {cut}: {report:?}"
                );

                let (rebuilt, parsed) = rebuild(&archive);
                parsed.assert_tiles();
                assert!(
                    parsed.unreadable().is_empty(),
                    "{name}: resume from byte {cut}"
                );
                assert_eq!(
                    file_sha256(&rebuilt),
                    sha,
                    "{name}: the image rebuilt from the archive resumed from byte {cut}, \
                     against the capture's SHA-256"
                );
                if let Some(k) = name
                    .strip_prefix("windows-interrupted-")
                    .and_then(|k| k.strip_suffix(".recovered"))
                {
                    files_match_windows(k, &rebuilt);
                }
                for path in [&archive, &rebuilt] {
                    std::fs::remove_file(path).expect("remove a scratch file");
                }
            }
        }
        for path in [&src, &full] {
            std::fs::remove_file(path).expect("remove a scratch file");
        }
    }
}
