//! Decodes the Windows-frozen hard-link images from
//! `tests/scripts/hardlink-readonly-index-capture.ps1` with the independent
//! `ntfs` 0.4 crate and reports, per stage, every FILE_NAME copy of the
//! linked file: the ones in its own MFT record and the index-entry keys in
//! both parent directories. The test asserts provenance, identity and the
//! control; it never asserts how Windows propagates the read-only bit, it
//! writes what it observed to `observations.tsv` for review.
#![cfg(windows)]

use ntfs::structured_values::NtfsFileName;
use ntfs::{Ntfs, NtfsAttributeType, NtfsFile};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

struct Offset<R> {
    inner: R,
    base: u64,
}

impl<R: Read> Read for Offset<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for Offset<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let p = match pos {
            SeekFrom::Start(n) => self.inner.seek(SeekFrom::Start(self.base + n))?,
            other => self.inner.seek(other)?,
        };
        Ok(p - self.base)
    }
}

type Fs = Offset<BufReader<File>>;

fn dir<'n>(ntfs: &'n Ntfs, fs: &mut Fs, from: &NtfsFile<'n>, name: &str) -> NtfsFile<'n> {
    let index = from.directory_index(fs).expect("directory index");
    let mut entries = index.entries();
    while let Some(entry) = entries.next(fs) {
        let entry = entry.expect("index entry");
        let key = entry.key().expect("key").expect("file name key");
        if key.name().to_string_lossy() == name {
            return entry.to_file(ntfs, fs).expect("entry to file");
        }
    }
    panic!("directory {name} not found");
}

/// Name, reference, flags, namespace, data size and allocated size.
type NameCopy = (String, u64, u32, u8, u64, u64);

/// Every index key, retaining the fields Windows duplicated into it.
fn keys(fs: &mut Fs, d: &NtfsFile) -> Vec<NameCopy> {
    let index = d.directory_index(fs).expect("directory index");
    let mut entries = index.entries();
    let mut out = Vec::new();
    while let Some(entry) = entries.next(fs) {
        let entry = entry.expect("index entry");
        let key = entry.key().expect("key").expect("file name key");
        out.push((
            key.name().to_string_lossy(),
            entry.file_reference().file_record_number(),
            key.file_attributes().bits(),
            key.namespace() as u8,
            key.data_size(),
            key.allocated_size(),
        ));
    }
    out
}

/// Every own-record copy, with its parent reference and duplicated fields.
fn own_names(fs: &mut Fs, file: &NtfsFile) -> Vec<NameCopy> {
    let mut attrs = file.attributes();
    let mut out = Vec::new();
    while let Some(item) = attrs.next(fs) {
        let item = item.expect("attribute item");
        let attr = item.to_attribute().expect("attribute");
        if attr.ty().expect("type") == NtfsAttributeType::FileName {
            let name = attr
                .structured_value::<_, NtfsFileName>(fs)
                .expect("FILE_NAME");
            out.push((
                name.name().to_string_lossy(),
                name.parent_directory_reference().file_record_number(),
                name.file_attributes().bits(),
                name.namespace() as u8,
                name.data_size(),
                name.allocated_size(),
            ));
        }
    }
    out
}

#[test]
fn hardlink_readonly_index_copies_are_observed_from_windows_images() {
    let dir_var = std::env::var("NTFS_HARDLINK_CAPTURE").expect(
        "NTFS_HARDLINK_CAPTURE unset: run tests/scripts/hardlink-readonly-index-capture.ps1 \
         (workflow hardlink-readonly-index-oracle.yml) first",
    );
    let root = PathBuf::from(dir_var);
    let manifest = std::fs::read_to_string(root.join("manifest.tsv"))
        .expect("manifest.tsv missing: the Windows capture did not finish");
    let rows: Vec<Vec<&str>> = manifest.lines().map(|l| l.split('\t').collect()).collect();
    let head = rows.iter().find(|r| r[0] == "head").expect("head row");
    assert!(
        head.len() == 2 && head[1].len() == 40,
        "head SHA recorded: {head:?}"
    );
    let offset: u64 = rows.iter().find(|r| r[0] == "offset").expect("offset row")[1]
        .parse()
        .expect("offset");
    let ops: Vec<_> = rows.iter().filter(|r| r[0] == "op").collect();
    assert_eq!(
        ops.iter().map(|r| r[2]).collect::<Vec<_>>(),
        ["s0-created", "s1-readonly", "s2-reopen", "s3-rename"],
        "operation order"
    );
    // Windows' own file IDs: both links and the control, at every stage.
    let api: Vec<_> = rows.iter().filter(|r| r[0] == "api").collect();
    assert_eq!(api.len(), 12, "three paths per stage");
    for stage in api.chunks(3) {
        assert_eq!(stage[0][5], stage[1][5], "links share one Windows file ID");
        assert_ne!(stage[0][5], stage[2][5], "control is a different file");
    }
    let readonly = 0x1u32;
    let api_attrs = |stage: &str, i: usize| -> u32 {
        api.iter()
            .filter(|r| r[1] == stage)
            .nth(i)
            .expect("api row")[4]
            .parse()
            .expect("attrs")
    };
    assert_eq!(
        api_attrs("s0-created", 0) & readonly,
        0,
        "Windows: not read-only before"
    );
    assert_ne!(
        api_attrs("s1-readonly", 0) & readonly,
        0,
        "Windows: read-only after"
    );
    assert_eq!(
        api_attrs("s1-readonly", 2) & readonly,
        0,
        "Windows: control unchanged"
    );

    let mut report = vec![
        "stage\tcopy\tname\tparent_or_record\tfile_attributes\tnamespace\tdata_size\tallocated_size"
            .to_string(),
    ];
    let mut record_by_stage = Vec::new();
    for image in rows.iter().filter(|r| r[0] == "image") {
        let (stage, file, want) = (image[1], image[2], image[3]);
        let bytes = std::fs::read(root.join(file)).expect("stage image missing");
        assert_eq!(hex(&Sha256::digest(&bytes)), want, "{stage} image SHA-256");
        let mut fs = Offset {
            inner: BufReader::new(File::open(root.join(file)).expect("open image")),
            base: offset,
        };
        let mut ntfs = Ntfs::new(&mut fs).expect("ntfs 0.4 mounts the Windows image");
        ntfs.read_upcase_table(&mut fs).expect("upcase");
        let rootdir = ntfs.root_directory(&mut fs).expect("root");
        let alpha = dir(&ntfs, &mut fs, &rootdir, "alpha");
        let beta = dir(&ntfs, &mut fs, &rootdir, "beta");
        let a = keys(&mut fs, &alpha);
        let b = keys(&mut fs, &beta);
        let linked = a.iter().find(|k| k.0 == "linked.txt").expect("alpha key");
        let control = a
            .iter()
            .find(|k| k.0 == "control.txt")
            .expect("control key");
        let second_name = if stage == "s3-rename" {
            "renamed.txt"
        } else {
            "second.txt"
        };
        let second = b.iter().find(|k| k.0 == second_name).expect("beta key");
        assert_eq!(
            linked.1, second.1,
            "{stage}: both index keys name one MFT record"
        );
        assert_ne!(
            linked.1, control.1,
            "{stage}: control is a different record"
        );
        let file = ntfs.file(&mut fs, linked.1).expect("linked record");
        let own = own_names(&mut fs, &file);
        assert!(
            own.iter()
                .any(|n| n.0 == "linked.txt" && n.1 == alpha.file_record_number())
                && own
                    .iter()
                    .any(|n| n.0 == second_name && n.1 == beta.file_record_number()),
            "{stage}: own record holds both links: {own:?}"
        );
        record_by_stage.push((linked.1, control.2));
        for n in &own {
            report.push(format!(
                "{stage}\town-mft\t{}\t{}\t{:#x}\t{}\t{}\t{}",
                n.0, n.1, n.2, n.3, n.4, n.5
            ));
        }
        for (copy, k) in [
            ("alpha-index", linked),
            ("beta-index", second),
            ("control-index", control),
        ] {
            report.push(format!(
                "{stage}\t{copy}\t{}\t{}\t{:#x}\t{}\t{}\t{}",
                k.0, k.1, k.2, k.3, k.4, k.5
            ));
        }
    }
    assert_eq!(record_by_stage.len(), 4, "four frozen stages");
    assert!(
        record_by_stage.windows(2).all(|w| w[0] == w[1]),
        "record number and control index flags are stable: {record_by_stage:?}"
    );
    std::fs::write(root.join("observations.tsv"), report.join("\n") + "\n").expect("report");
    println!("{}", report.join("\n"));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
