//! Disk execution for #682; expected state comes only from #680's model.
use super::{common, generator, model, reference_tools, SIZE};
use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::{error::Error, facade::Filesystem, fsck, mkfs, read, write};
use generator::{Generator, Op};
use model::Model;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub struct Observed {
    record: u64,
    stat: read::Stat,
    names: Vec<(String, u64, bool)>,
    bytes: Vec<u8>,
}

fn join(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches('/'))
}

fn paths(model: &Model, path: &str, out: &mut Vec<String>) {
    out.push(path.into());
    if model.stat(path).unwrap().directory {
        for (name, _) in model.list(path).unwrap() {
            paths(model, &join(path, &name), out);
        }
    }
}

/// Also retains times, flags and exact directory references: a refusal must
/// leave all observable target/parent state unchanged, beyond modeled fields.
fn compare(io: &mut PathIo, model: &Model) -> BTreeMap<String, Observed> {
    let mut all = Vec::new();
    paths(model, "/", &mut all);
    let mut snapshot = BTreeMap::new();
    let mut identities = BTreeMap::new();
    let mut records = BTreeMap::new();
    for path in all {
        let expected = model.stat(&path).unwrap();
        let record = read::resolve_path(io, &path).expect("resolve modeled path");
        if let Some(previous) = identities.insert(expected.id, record) {
            assert_eq!(previous, record, "hard-link identity of {path}");
        }
        if let Some(previous) = records.insert(record, expected.id) {
            assert_eq!(previous, expected.id, "distinct model identities at {path}");
        }
        let stat = read::read_stat(io, record).expect("stat modeled path");
        assert_eq!(stat.is_dir, expected.directory, "kind of {path}");
        assert_eq!(stat.size, expected.size as u64, "size of {path}");
        assert_eq!(
            usize::from(stat.link_count),
            expected.links,
            "links of {path}"
        );
        let mut names = Vec::new();
        let mut bytes = Vec::new();
        if expected.directory {
            names = read::read_dir_entries(io, record)
                .expect("list modeled directory")
                .into_iter()
                .filter(read::listed_to_hosts)
                .map(|e| (e.name, e.record_number, e.is_dir))
                .collect();
            names.sort();
            let mut wanted: Vec<_> = model
                .list(&path)
                .unwrap()
                .into_iter()
                .map(|(name, _)| {
                    let directory = model.stat(&join(&path, &name)).unwrap().directory;
                    (name, directory)
                })
                .collect();
            wanted.sort();
            assert_eq!(
                names
                    .iter()
                    .map(|(n, _, d)| (n.clone(), *d))
                    .collect::<Vec<_>>(),
                wanted,
                "names/kinds in {path}"
            );
        } else {
            bytes = read::read_attribute_value(io, record, AttrType::Data, None)
                .expect("logical contents");
            assert_eq!(
                bytes,
                model.read(&path).unwrap(),
                "complete logical contents of {path}"
            );
        }
        snapshot.insert(
            path,
            Observed {
                record,
                stat,
                names,
                bytes,
            },
        );
    }
    snapshot
}

fn execute(io: &mut PathIo, op: &Op) -> Result<(), Error> {
    match op {
        Op::Create { parent, name } => write::create_file_io(io, parent, name).map(|_| ()),
        Op::Mkdir { parent, name } => write::mkdir_io(io, parent, name).map(|_| ()),
        Op::Write { path, data } => {
            write::write_file_contents_io(io, path, data).map(|n| assert_eq!(n, data.len() as u64))
        }
        Op::WriteAt { path, offset, data } => {
            write::write_at_io(io, path, u64::from(*offset), data)
                .map(|n| assert_eq!(n, data.len() as u64))
        }
        Op::Truncate { path, size } => {
            write::truncate_io(io, path, u64::from(*size)).map(|n| assert_eq!(n, u64::from(*size)))
        }
        Op::Rename {
            path,
            name,
            replace,
        } => write::rename_replace_io(io, path, name, *replace),
        Op::Unlink { path } => write::unlink_io(io, path),
        Op::Rmdir { path } => write::rmdir_io(io, path),
        Op::Link { path, parent, name } => write::link_io(io, path, parent, name),
    }
}

fn known_refusal(op: &Op, error: &Error) -> bool {
    // The original four-seed gate documents W2.1's bounded mapping capacity.
    matches!(
        op,
        Op::Write { .. } | Op::WriteAt { .. } | Op::Truncate { .. }
    ) && error.kind() == fs_ntfs::error::Kind::Io
        && error.contains("exceed attr capacity")
}

pub struct Harness {
    pub image: String,
    pub model: Model,
    artifacts: PathBuf,
    operations: Vec<Op>,
    refusals: Vec<String>,
    pub counts: ([usize; 9], [usize; 9]),
    started: Instant,
}

impl Harness {
    pub fn new(name: &str) -> Self {
        let image = common::temp_image_path(format!("opseq_{name}"));
        File::create(&image).unwrap().set_len(SIZE).unwrap();
        let mut io = PathIo::open_rw(Path::new(&image)).unwrap();
        mkfs::format_filesystem(&mut io, SIZE, 4096, 1024, Some("OPSEQ"), Some(682)).unwrap();
        io.sync().unwrap();
        drop(io);
        let (oracle, mut reader) = common::open(&image);
        let table = common::read_file_all(&oracle, &mut reader, "/$UpCase");
        let model = Model::new(
            table
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect(),
        )
        .unwrap();
        let artifacts = PathBuf::from(format!("tmp/op-sequence-model/{name}"));
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(
            artifacts.join("geometry.txt"),
            format!("{SIZE}\t4096\t1024\t682\n"),
        )
        .unwrap();
        fs::write(artifacts.join("active-image.txt"), &image).unwrap();
        fs::write(
            artifacts.join("reproducer.json"),
            generator::encode(&[]).unwrap(),
        )
        .unwrap();
        fs::write(artifacts.join("refusals.tsv"), "").unwrap();
        fs::write(
            artifacts.join("outcome-counts.json"),
            "[[0,0,0,0,0,0,0,0,0],[0,0,0,0,0,0,0,0,0]]",
        )
        .unwrap();
        Self {
            image,
            model,
            artifacts,
            operations: Vec::new(),
            refusals: Vec::new(),
            counts: ([0; 9], [0; 9]),
            started: Instant::now(),
        }
    }

    pub fn step(&mut self, op: Op) -> bool {
        assert!(
            self.started.elapsed() < Duration::from_secs(90),
            "sequence deadline"
        );
        self.operations.push(op.clone());
        // Persist the concrete prefix BEFORE the disk call, including refusals.
        fs::write(
            self.artifacts.join("reproducer.json"),
            generator::encode(&self.operations).unwrap(),
        )
        .unwrap();
        let mut io = PathIo::open_rw(Path::new(&self.image)).unwrap();
        let before = compare(&mut io, &self.model);
        let mut candidate = self.model.clone();
        let expected = op.apply(&mut candidate);
        let result = execute(&mut io, &op);
        io.sync().expect("sync operation, including refusal");
        let accepted = result.is_ok();
        let kind = match &op {
            Op::Create { .. } => 0,
            Op::Mkdir { .. } => 1,
            Op::Write { .. } => 2,
            Op::WriteAt { .. } => 3,
            Op::Truncate { .. } => 4,
            Op::Rename { .. } => 5,
            Op::Unlink { .. } => 6,
            Op::Rmdir { .. } => 7,
            Op::Link { .. } => 8,
        };
        if accepted {
            self.counts.0[kind] += 1;
        } else {
            self.counts.1[kind] += 1;
        }
        fs::write(
            self.artifacts.join("outcome-counts.json"),
            serde_json::to_vec(&self.counts).unwrap(),
        )
        .unwrap();
        match result {
            Ok(()) => {
                assert!(
                    expected.is_ok(),
                    "disk accepted model refusal: {op:?}: {expected:?}"
                );
                self.model = candidate;
            }
            Err(error) => {
                self.refusals.push(format!(
                    "{}\t{op:?}\t{:?}\t{error}",
                    self.operations.len(),
                    error.kind()
                ));
                fs::write(
                    self.artifacts.join("refusals.tsv"),
                    self.refusals.join("\n"),
                )
                .unwrap();
                assert!(
                    expected.is_err() || known_refusal(&op, &error),
                    "unexpected disk refusal: {op:?}: {error}"
                );
            }
        }
        let live = compare(&mut io, &self.model);
        if !accepted {
            assert_eq!(live, before, "refusal changed observable state: {op:?}");
        }
        drop(io);
        let reopened = self.verify();
        assert_eq!(reopened, live, "fresh reopen differs after {op:?}");
        if self.operations.len().is_multiple_of(8) {
            self.check();
        }
        accepted
    }

    pub fn verify(&self) -> BTreeMap<String, Observed> {
        let mut io = PathIo::open_ro(Path::new(&self.image)).unwrap();
        let snapshot = compare(&mut io, &self.model);
        // Independent descriptors/parser check namespace and metadata. Unlike
        // ntfscat, this parser ignores initialized_size; logical bytes above
        // and the final ntfs-3g hook cover zero-filled uninitialized tails.
        let (oracle, mut reader) = common::open(&self.image);
        for (path, actual) in &snapshot {
            let file = common::navigate(&oracle, &mut reader, path);
            assert_eq!(
                file.file_record_number(),
                actual.record,
                "independent identity {path}"
            );
            assert_eq!(
                file.is_directory(),
                actual.stat.is_dir,
                "independent kind {path}"
            );
            assert_eq!(
                file.hard_link_count(),
                actual.stat.link_count,
                "independent links {path}"
            );
            if actual.stat.is_dir {
                let mut names = common::list_names(&oracle, &mut reader, path);
                names.retain(|n| n != "." && n != ".." && (path != "/" || !n.starts_with('$')));
                names.sort();
                assert_eq!(
                    names,
                    actual
                        .names
                        .iter()
                        .map(|(n, _, _)| n.clone())
                        .collect::<Vec<_>>(),
                    "independent names {path}"
                );
            } else {
                let item = file.data(&mut reader, "").unwrap().unwrap();
                assert_eq!(
                    item.to_attribute().unwrap().value_length(),
                    actual.stat.size,
                    "independent size {path}"
                );
            }
        }
        snapshot
    }

    fn check(&self) {
        let mounted = Filesystem::mount(&self.image).expect("fresh whole-image mount");
        let snapshot = self.verify();
        for (path, entry) in snapshot {
            assert_eq!(
                mounted.stat(&path).unwrap().file_record_number,
                entry.record
            );
            if !entry.stat.is_dir {
                let mut bytes = vec![0; entry.bytes.len() + 1];
                let n = mounted.read_file(&path, 0, &mut bytes).unwrap();
                assert_eq!(n, entry.bytes.len(), "remount EOF {path}");
                assert_eq!(&bytes[..n], entry.bytes, "remount bytes {path}");
            }
        }
        let mut io = PathIo::open_ro(Path::new(&self.image)).unwrap();
        let report = fsck::check_io(&mut io).expect("check_io after remount");
        assert!(
            report.findings.is_empty(),
            "check_io: {:?}",
            report.findings
        );
    }

    pub fn generated(&mut self, input: &[u8]) -> ([usize; 9], [usize; 9]) {
        fs::write(self.artifacts.join("input.bin"), input).unwrap();
        let mut generator = Generator::new(input);
        let mut counts = ([0; 9], [0; 9]);
        while let Some(op) = generator.next(&self.model) {
            let kind = match &op {
                Op::Create { .. } => 0,
                Op::Mkdir { .. } => 1,
                Op::Write { .. } => 2,
                Op::WriteAt { .. } => 3,
                Op::Truncate { .. } => 4,
                Op::Rename { .. } => 5,
                Op::Unlink { .. } => 6,
                Op::Rmdir { .. } => 7,
                Op::Link { .. } => 8,
            };
            if self.step(op) {
                counts.0[kind] += 1;
            } else {
                counts.1[kind] += 1;
            }
            fs::write(
                self.artifacts.join("outcome-counts.json"),
                serde_json::to_vec(&counts).unwrap(),
            )
            .unwrap();
        }
        counts
    }

    pub fn finish(&self) {
        self.check();
        if std::env::var_os("OPSEQ_REFERENCE_TOOLS").is_some() {
            let mut files = BTreeMap::new();
            for (path, entry) in self.verify() {
                if entry.stat.is_dir {
                    let output = std::process::Command::new("ntfsls")
                        .args(["-a", "-p", &path, &self.image])
                        .output()
                        .expect("ntfsls missing: install ntfs-3g for test:op-sequence-oracle");
                    fs::write(
                        self.artifacts.join(format!("ntfsls-{}.txt", entry.record)),
                        [&output.stdout[..], &output.stderr[..]].concat(),
                    )
                    .unwrap();
                    assert!(
                        output.status.success(),
                        "ntfsls {path}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let mut names: Vec<_> = String::from_utf8(output.stdout)
                        .unwrap()
                        .lines()
                        .filter(|n| *n != "." && *n != "..")
                        .map(str::to_owned)
                        .collect();
                    names.sort();
                    assert_eq!(
                        names,
                        entry
                            .names
                            .into_iter()
                            .map(|(n, _, _)| n)
                            .collect::<Vec<_>>(),
                        "ntfs-3g names {path}"
                    );
                } else {
                    files.insert(path, entry.bytes);
                }
            }
            reference_tools(&self.image, &files, &self.artifacts);
        }
        assert!(
            self.started.elapsed() < Duration::from_secs(90),
            "sequence deadline"
        );
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if std::thread::panicking() {
            fs::copy(&self.image, self.artifacts.join("failure.img"))
                .expect("retain failed operation image");
        }
        // Fuzz workers stay alive across inputs; don't accumulate image files.
        let _ = fs::remove_file(&self.image);
    }
}
