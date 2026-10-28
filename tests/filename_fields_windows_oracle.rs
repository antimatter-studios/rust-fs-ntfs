//! Issue #629: the three FILE_NAME fields Windows duplicates -- data size,
//! allocated size and file attributes -- read raw out of the images that
//! `test-disks/capture-filename-fields.ps1` (CI job `filename-fields-capture`)
//! produced, through the independent `ntfs` crate rather than this crate.
//!
//! For every manifest row it reads the FILE_NAME in the file's own MFT
//! record and the FILE_NAME key in the root directory index, writes both
//! beside the Get-Item / fsutil values to `tmp/fn-fields/raw-comparison.tsv`,
//! and grades them against `common::fn_fields_windows::WINDOWS`, the table
//! `field_exhaustion_fn` holds this crate's writer to. Every row of that
//! table must be observed exactly once, so neither side can drift alone.
//!
//! The eager index copy must also agree with what the Windows API reports:
//! the length and attributes for every file, and the allocation of the
//! non-resident positive control big.bin.
//!
//! Provenance is asserted too: each image is the one the manifest names, the
//! six steps ran in order on a named Windows build, every file the manifest
//! lists is the file decoded, and the readonly step really set the readonly
//! attribute.

mod common;

use common::fn_fields_windows::{observed, Fields, WINDOWS};
use ntfs::structured_values::NtfsFileName;
use ntfs::Ntfs;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const PRODUCER: &str =
    "CI job filename-fields-capture (test-disks/capture-filename-fields.ps1 on elevated Windows)";
const STEPS: [&str; 6] = [
    "create",
    "write-close",
    "reopen",
    "readonly",
    "rename",
    "create-refresh",
];
const READONLY: u32 = 0x1;

struct Row {
    step: usize,
    operation: String,
    hash: String,
    path: String,
    length: String,
    allocation: String,
    attributes: String,
    windows: String,
}

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("NTFS_FN_FIELDS_DIR").unwrap_or_else(|_| "test-disks".into()))
}

fn require(path: &Path) {
    assert!(
        path.is_file(),
        "missing {}; produced by {PRODUCER}",
        path.display()
    );
}

fn manifest() -> Vec<Row> {
    let path = dir().join("ntfs-fn-fields.manifest.tsv");
    require(&path);
    let text = std::fs::read_to_string(&path).expect("read manifest");
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("step\toperation\timage_sha256\tpath\tlength\tallocation\tattributes\twindows"),
        "manifest header"
    );
    lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 8, "manifest row has eight columns: {line:?}");
            Row {
                step: f[0].parse().expect("step number"),
                operation: f[1].into(),
                hash: f[2].into(),
                path: f[3].into(),
                length: f[4].into(),
                allocation: f[5].into(),
                attributes: f[6].into(),
                windows: f[7].into(),
            }
        })
        .collect()
}

fn image(step: usize) -> PathBuf {
    let path = dir().join(format!("ntfs-fn-fields-step{step}.img"));
    require(&path);
    path
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read image");
    Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn raw(name: &NtfsFileName) -> Fields {
    Fields {
        data_size: name.data_size(),
        allocated_size: name.allocated_size(),
        attributes: name.file_attributes().bits(),
    }
}

/// The FILE_NAME in the file's MFT record and the FILE_NAME key of its entry
/// in the root directory index, found by name and bound by file reference.
fn decode(ntfs: &Ntfs, reader: &mut common::Reader, name: &str) -> (Fields, Fields) {
    let root = ntfs.root_directory(reader).expect("root directory");
    let index = root.directory_index(reader).expect("root index");
    let mut entries = index.entries();
    let mut found = Vec::new();
    while let Some(entry) = entries.next(reader) {
        let entry = entry.expect("index entry");
        let Some(key) = entry.key() else { continue };
        let key: NtfsFileName = key.expect("index key");
        if key.name().to_string_lossy() == name {
            found.push((entry.file_reference(), raw(&key)));
        }
    }
    assert_eq!(found.len(), 1, "{name} appears once in the root index");
    let (reference, index_fields) = found.pop().unwrap();
    let file = reference.to_file(ntfs, reader).expect("file record");
    let record = file
        .name(reader, None, None)
        .expect("record has a FILE_NAME")
        .expect("record FILE_NAME");
    assert_eq!(
        record.name().to_string_lossy(),
        name,
        "the record the index points at is the file the manifest names"
    );
    (raw(&record), index_fields)
}

#[test]
fn the_three_file_name_fields_match_the_expectations_for_all_six_windows_steps() {
    let rows = manifest();
    let mut evidence = String::from(
        "step\toperation\tpath\tpartition_offset\tapi_length\tapi_allocation\tapi_attributes\t\
         record_data_size\trecord_allocated_size\trecord_attributes\t\
         index_data_size\tindex_allocated_size\tindex_attributes\twindows\n",
    );
    let mut hashes = Vec::new();
    let mut decoded = Vec::new();
    for (i, operation) in STEPS.iter().enumerate() {
        let step = i + 1;
        let marker: Vec<&Row> = rows
            .iter()
            .filter(|r| r.step == step && r.path == "(image)")
            .collect();
        assert_eq!(marker.len(), 1, "step {step} has one image row");
        assert_eq!(
            marker[0].operation, *operation,
            "step {step} operation order"
        );
        assert!(
            !marker[0].windows.is_empty(),
            "step {step} names its Windows build"
        );
        let path = image(step);
        let hash = sha256(&path);
        assert_eq!(
            hash, marker[0].hash,
            "step {step} image is the one the manifest names"
        );
        hashes.push(hash.clone());

        let bytes = std::fs::read(&path).expect("read image");
        assert_eq!(
            &bytes[3..11],
            b"NTFS    ",
            "step {step} image is a bare volume at offset 0"
        );
        let (ntfs, mut reader) = common::open(path.to_str().expect("utf-8 path"));

        let files: Vec<&Row> = rows
            .iter()
            .filter(|r| r.step == step && r.path != "(image)")
            .collect();
        assert!(!files.is_empty(), "step {step} observed at least one file");
        for row in files {
            assert_eq!(
                row.hash, hash,
                "{} row is bound to step {step}'s image",
                row.path
            );
            let (record, index) = decode(&ntfs, &mut reader, &row.path);
            let attributes: u32 = row.attributes.parse().expect("API attributes");
            if *operation == "readonly" && row.path == "ro.txt" {
                assert_ne!(attributes & READONLY, 0, "Windows reports ro.txt readonly");
            }
            if row.path == "ctl.txt" {
                assert_eq!(attributes & READONLY, 0, "the control is never readonly");
            }

            decoded.push((step, *operation, row, record, index));
            writeln!(
                evidence,
                "{step}\t{operation}\t{}\t0\t{}\t{}\t{}\t{}\t{}\t{:#x}\t{}\t{}\t{:#x}\t{}",
                row.path,
                row.length,
                row.allocation,
                row.attributes,
                record.data_size,
                record.allocated_size,
                record.attributes,
                index.data_size,
                index.allocated_size,
                index.attributes,
                row.windows,
            )
            .unwrap();
        }
    }

    // The evidence is written before it is graded, so a failing run still
    // leaves the raw values behind for whoever reads it.
    let out = PathBuf::from("tmp/fn-fields");
    std::fs::create_dir_all(&out).expect("evidence directory");
    std::fs::write(out.join("raw-comparison.tsv"), evidence).expect("write evidence");

    for &(step, operation, row, record, index) in &decoded {
        let attributes: u32 = row.attributes.parse().expect("API attributes");
        let windows = observed(operation, &row.path);
        assert_eq!(
            record, windows.record,
            "step {step} {}: record FILE_NAME copy",
            row.path
        );
        assert_eq!(
            index, windows.index,
            "step {step} {}: root-index FILE_NAME copy",
            row.path
        );
        let length: u64 = row.length.parse().expect("API length");
        assert_eq!(
            length, index.data_size,
            "step {step} {}: the eager index copy holds the API length",
            row.path
        );
        assert_eq!(
            attributes, index.attributes,
            "step {step} {}: the eager index copy holds the API attributes",
            row.path
        );
        if row.path == "big.bin" && length > 0 {
            let allocation: u64 = row.allocation.parse().unwrap_or_else(|_| {
                panic!(
                    "step {step} big.bin: allocation {:?} is not a number; produced by {PRODUCER}",
                    row.allocation
                )
            });
            assert_eq!(
                allocation, index.allocated_size,
                "step {step} big.bin: the eager index copy holds the $DATA allocation fsutil reports"
            );
        }
    }

    for o in &WINDOWS {
        let seen = decoded
            .iter()
            .filter(|(_, op, row, _, _)| *op == o.operation && row.path == o.path)
            .count();
        assert_eq!(
            seen, 1,
            "{} after {} is observed exactly once by the capture",
            o.path, o.operation
        );
    }
    assert_eq!(
        decoded.len(),
        WINDOWS.len(),
        "the capture observes no file the table does not hold"
    );

    // Negative controls: a different step's image never matches this step's
    // manifest hash, and a name no step created is not found.
    for (a, ha) in hashes.iter().enumerate() {
        for (b, hb) in hashes.iter().enumerate() {
            if a != b {
                assert_ne!(
                    ha,
                    hb,
                    "steps {} and {} produced distinct images",
                    a + 1,
                    b + 1
                );
            }
        }
    }
    let (ntfs, mut reader) = common::open(image(1).to_str().expect("utf-8 path"));
    let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decode(&ntfs, &mut reader, "absent-control.txt")
    }));
    assert!(missing.is_err(), "an unknown name is refused, not decoded");
}
