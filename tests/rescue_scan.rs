//! The rescue scan (#720), graded against bytes Windows wrote.
//!
//! The sources are the partitions Windows left mid-write and the copies
//! Windows recovered from them (`test-disks/windows-interrupted-{1,6}*`).
//! Nothing here reads an archive through this crate. The test parses the
//! layout `docs/rescue-archive.md` documents, checks every CRC-32 with an
//! independent implementation (`crc32fast`), rebuilds an image from the data
//! records with its own file writes, and reads each rebuilt copy Windows
//! recovered with the independent `ntfs` crate, against the size and SHA-256
//! Windows' `Get-FileHash` recorded for every file on it.

mod common;

use fs_ntfs::rescue::{
    scan, scan_to_path, FileSource, RangeClass, ReadFailure, RescueSource, ScanOptions,
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

    /// The records tile the source: in order, with no gap and no overlap.
    fn assert_tiles(&self) {
        let mut next = 0;
        for r in &self.records {
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
        chunks + 5 * per_chunk + last_chunk,
        "bulk reads plus one read per sector of each failed chunk"
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
