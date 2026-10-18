//! Disk execution for #682; expected state comes only from #680's model.
use super::{common, generator, model, reference_tools, shrink, SIZE};
use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::{error::Error, facade::Filesystem, fsck, mkfs, read, write};
use generator::{Generator, Geometry, Op, Regression, OPCODES};
use model::{Ea, Model, NEED_EA};
use ntfs::{NtfsAttributeType, NtfsFile, NtfsReadSeek};
use shrink::Mismatch;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The state the disk is compared with. `Model` is the only real one; a
/// test may wrap it to inject a known divergence (#684), never edit it.
pub trait Expected: Clone {
    fn tree(&self) -> &Model;
    fn apply(&mut self, op: &Op) -> Result<(), model::Error>;
    fn stat(&self, path: &str) -> Result<model::Stat, model::Error> {
        self.tree().stat(path)
    }
}

impl Expected for Model {
    fn tree(&self) -> &Model {
        self
    }

    fn apply(&mut self, op: &Op) -> Result<(), model::Error> {
        op.apply(self)
    }
}

/// The image every sequence starts on, recorded in each captured pin.
pub fn geometry() -> Geometry {
    Geometry {
        size: SIZE,
        cluster: 4096,
        record: 1024,
        label: "OPSEQ".into(),
        serial: 682,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Observed {
    record: u64,
    stat: read::Stat,
    names: Vec<(String, u64, bool)>,
    bytes: Vec<u8>,
    /// Named `$DATA` streams by stored name, with their complete bytes.
    streams: Vec<(String, Vec<u8>)>,
    /// Extended attributes by stored name, with flags and values (#732).
    eas: Vec<Ea>,
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
fn compare(io: &mut PathIo, model: &impl Expected) -> BTreeMap<String, Observed> {
    let mut all = Vec::new();
    paths(model.tree(), "/", &mut all);
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
        let mut streams: Vec<_> = write::list_named_streams_io(io, &path)
            .expect("list modeled streams")
            .into_iter()
            .map(|name| {
                let bytes = read::read_attribute_value(io, record, AttrType::Data, Some(&name))
                    .expect("named stream contents");
                (name, bytes)
            })
            .collect();
        streams.sort();
        let mut wanted = model.tree().streams(&path).unwrap();
        wanted.sort();
        assert_eq!(streams, wanted, "named streams of {path}");
        let mut eas: Vec<Ea> = write::list_eas_io(io, &path)
            .expect("list modeled EAs")
            .into_iter()
            .map(|ea| (ea.name, ea.flags, ea.value))
            .collect();
        eas.sort();
        assert_eq!(eas, model.tree().eas(&path).unwrap(), "EAs of {path}");
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
                .tree()
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
                model.tree().read(&path).unwrap(),
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
                streams,
                eas,
            },
        );
    }
    snapshot
}

/// Every named `$DATA` stream of `file` with its complete bytes, as the
/// upstream `ntfs` parser reads them. A stream split across extension
/// records is one stream; the parser follows the `$ATTRIBUTE_LIST`.
fn independent_streams(reader: &mut common::Reader, file: &NtfsFile) -> Vec<(String, Vec<u8>)> {
    let mut names = Vec::new();
    let mut attributes = file.attributes();
    while let Some(item) = attributes.next(reader) {
        let item = item.unwrap();
        let attribute = item.to_attribute().unwrap();
        let name = attribute.name().unwrap().to_string_lossy();
        if attribute.ty().unwrap() == NtfsAttributeType::Data
            && !name.is_empty()
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    let mut streams: Vec<_> = names
        .into_iter()
        .map(|name| {
            let item = file.data(reader, &name).unwrap().unwrap();
            let attribute = item.to_attribute().unwrap();
            let mut value = attribute.value(reader).unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                match value.read(reader, &mut chunk).unwrap() {
                    0 => break,
                    n => bytes.extend_from_slice(&chunk[..n]),
                }
            }
            (name, bytes)
        })
        .collect();
    streams.sort();
    streams
}

/// MS-FSCC 2.4.15 `FILE_FULL_EA_INFORMATION` entries, decoded here rather
/// than by `ea_io` (#732): `NextEntryOffset` (u32), `Flags` (u8),
/// `EaNameLength` (u8), `EaValueLength` (u16), the name and its NUL, then
/// the value. Each entry starts four-byte aligned; the final entry's offset
/// reaches the end of the buffer, and nothing follows it but its padding.
fn full_ea_information(bytes: &[u8]) -> Vec<Ea> {
    let mut entries = Vec::new();
    let mut at = 0;
    loop {
        let header = bytes
            .get(at..at + 8)
            .unwrap_or_else(|| panic!("EA entry at {at} runs past {} bytes", bytes.len()));
        let next = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let name_end = at + 8 + usize::from(header[5]);
        let end = name_end + 1 + usize::from(u16::from_le_bytes([header[6], header[7]]));
        assert!(
            end <= bytes.len(),
            "EA entry at {at} ends at {end}, past {} bytes",
            bytes.len()
        );
        let name = bytes[at + 8..name_end].to_vec();
        assert!(!name.contains(&0), "EA name at {at} holds a NUL");
        assert_eq!(bytes[name_end], 0, "EA name at {at} is not NUL-terminated");
        entries.push((name, header[4], bytes[name_end + 1..end].to_vec()));
        if next == 0 {
            assert!(
                bytes.len() <= end.next_multiple_of(4),
                "{} bytes follow the last EA entry",
                bytes.len() - end
            );
            return entries;
        }
        assert!(
            next.is_multiple_of(4) && next >= end - at,
            "EA entry at {at} has NextEntryOffset {next}"
        );
        at += next;
        if at == bytes.len() {
            return entries;
        }
    }
}

#[test]
fn full_ea_information_accepts_the_on_disk_terminal_entry_size() {
    // Windows writes the aligned size in the final $EA entry's offset field;
    // query buffers instead use zero for the final entry.
    let bytes = [12, 0, 0, 0, 0, 1, 1, 0, b'x', 0, 0x42, 0];
    assert_eq!(
        full_ea_information(&bytes),
        vec![(b"x".to_vec(), 0, vec![0x42])]
    );
}

/// The EAs of `file`, from the raw `$EA` and `$EA_INFORMATION` values the
/// upstream `ntfs` parser reads, following `$ATTRIBUTE_LIST`s and runs.
/// `$EA_INFORMATION` must describe the decoded set, and a file with no EAs
/// has neither attribute.
fn independent_eas(reader: &mut common::Reader, file: &NtfsFile) -> Vec<Ea> {
    let (mut ea, mut information) = (None, None);
    let mut attributes = file.attributes();
    while let Some(item) = attributes.next(reader) {
        let item = item.unwrap();
        let attribute = item.to_attribute().unwrap();
        let slot = match attribute.ty().unwrap() {
            NtfsAttributeType::EA => &mut ea,
            NtfsAttributeType::EAInformation => &mut information,
            _ => continue,
        };
        if slot.is_some() {
            continue;
        }
        let mut value = attribute.value(reader).unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            match value.read(reader, &mut chunk).unwrap() {
                0 => break,
                n => bytes.extend_from_slice(&chunk[..n]),
            }
        }
        *slot = Some(bytes);
    }
    let (ea, information) = match (ea, information) {
        (None, None) => return Vec::new(),
        (Some(ea), Some(information)) => (ea, information),
        (ea, information) => panic!(
            "$EA present: {}, $EA_INFORMATION present: {}",
            ea.is_some(),
            information.is_some()
        ),
    };
    let mut entries = full_ea_information(&ea);
    // EaPackedLength, NeedEaCount and EaQueryLength, from the entries.
    let packed: usize = entries.iter().map(|(n, _, v)| 5 + n.len() + v.len()).sum();
    let need = entries.iter().filter(|(_, f, _)| f & NEED_EA != 0).count();
    let query: usize = entries
        .iter()
        .map(|(n, _, v)| (8 + n.len() + 1 + v.len()).next_multiple_of(4))
        .sum();
    assert_eq!(information.len(), 8, "$EA_INFORMATION length");
    assert_eq!(
        (
            usize::from(u16::from_le_bytes([information[0], information[1]])),
            usize::from(u16::from_le_bytes([information[2], information[3]])),
            u32::from_le_bytes(information[4..8].try_into().unwrap()) as usize,
        ),
        (packed, need, query),
        "$EA_INFORMATION against its $EA"
    );
    entries.sort();
    entries
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
        Op::WriteStream { path, stream, data } => {
            write::write_named_stream_io(io, path, stream, data)
        }
        Op::DeleteStream { path, stream } => write::delete_named_stream_io(io, path, stream),
        Op::WriteEa {
            path,
            name,
            flags,
            value,
        } => write::write_ea_io(io, path, name.as_bytes(), value, *flags),
        Op::RemoveEa { path, name } => write::remove_ea_io(io, path, name.as_bytes()),
    }
}

/// Refusals the model does not predict, because they depend on where the
/// driver put a file's attributes, which the model never knows. Each is a
/// documented driver limit, and every one must still leave the volume
/// exactly as it was.
fn known_refusal(op: &Op, error: &Error) -> bool {
    use fs_ntfs::error::Kind;
    // The original four-seed gate documents W2.1's bounded mapping capacity.
    let capacity = matches!(
        op,
        Op::Write { .. } | Op::WriteAt { .. } | Op::Truncate { .. }
    ) && error.kind() == Kind::Io
        && error.contains("exceed attr capacity");
    // A stream its base record cannot hold gives the file an
    // $ATTRIBUTE_LIST (#480), and the paths that add or drop a base-record
    // attribute, such as a name, refuse such a file (#468).
    let listed = error.kind() == Kind::Refused && error.contains("carries an $ATTRIBUTE_LIST");
    // What `attr_list` documents it does not move to make room (#480),
    // when a stream or an EA set goes through a file's list (#660).
    let unmoved = matches!(
        op,
        Op::WriteStream { .. } | Op::DeleteStream { .. } | Op::WriteEa { .. } | Op::RemoveEa { .. }
    ) && error.kind() == Kind::Refused
        && (error.contains("this driver does not move")
            || error.contains("is an attribute this driver moves"));
    capacity || listed || unmoved
}

/// A geometry the formatter refuses (#1025), with its reason, which must
/// be the one the model expects; `None` when both expect it formatted.
/// The refusal goes in `name`'s artifacts with nothing run, replacing
/// whatever an earlier input left there.
pub fn refused_geometry(name: &str, geometry: &Geometry) -> Option<Error> {
    let expected = model::format_refusal(geometry.size, geometry.cluster, geometry.record);
    let planned = mkfs::plan_format(
        geometry.size,
        geometry.cluster,
        geometry.record,
        Some(&geometry.label),
        Some(geometry.serial),
    );
    let error = match (planned, expected) {
        (Ok(_), None) => return None,
        (Ok(_), Some(reason)) => {
            panic!("the formatter planned {geometry:?}, refused by the model: {reason}")
        }
        (Err(error), None) => panic!("unexpected format refusal of {geometry:?}: {error}"),
        (Err(error), Some(reason)) => {
            assert!(
                error.to_string().contains(reason),
                "{geometry:?} refused with {error}, not {reason}"
            );
            error
        }
    };
    let artifacts = PathBuf::from(format!("tmp/op-sequence-model/{name}"));
    fs::create_dir_all(&artifacts).unwrap();
    fs::write(
        artifacts.join("geometry.txt"),
        format!(
            "{}\t{}\t{}\t{}\n",
            geometry.size, geometry.cluster, geometry.record, geometry.serial
        ),
    )
    .unwrap();
    fs::write(
        artifacts.join("refusals.tsv"),
        format!("format\t{:?}\t{error}\n", error.kind()),
    )
    .unwrap();
    fs::write(
        artifacts.join("reproducer.json"),
        generator::encode(&[]).unwrap(),
    )
    .unwrap();
    let zero: Counts = ([0; OPCODES], [0; OPCODES]);
    fs::write(
        artifacts.join("outcome-counts.json"),
        serde_json::to_vec(&zero).unwrap(),
    )
    .unwrap();
    Some(error)
}

/// Accepted then refused operations, one slot per opcode.
pub type Counts = ([usize; OPCODES], [usize; OPCODES]);

pub struct Harness<M = Model> {
    pub image: String,
    pub model: M,
    artifacts: PathBuf,
    operations: Vec<Op>,
    refusals: Vec<String>,
    pub counts: Counts,
    /// Refused operations by operation and reason (#1031).
    pub reasons: BTreeMap<(String, String), usize>,
    started: Instant,
}

impl Harness {
    pub fn new(name: &str) -> Self {
        Self::with(name, |model| model)
    }

    /// A harness whose image is formatted at `geometry`, such as a pin's.
    pub fn on(name: &str, geometry: &Geometry) -> Self {
        Self::formatted(name, geometry, |model| model)
    }
}

impl<M: Expected> Harness<M> {
    /// `wrap` receives the real model of the freshly formatted image.
    pub fn with(name: &str, wrap: impl FnOnce(Model) -> M) -> Self {
        Self::formatted(name, &geometry(), wrap)
    }

    fn formatted(name: &str, geometry: &Geometry, wrap: impl FnOnce(Model) -> M) -> Self {
        let image = common::temp_image_path(format!("opseq_{name}"));
        File::create(&image)
            .unwrap()
            .set_len(geometry.size)
            .unwrap();
        let mut io = PathIo::open_rw(Path::new(&image)).unwrap();
        mkfs::format_filesystem(
            &mut io,
            geometry.size,
            geometry.cluster,
            geometry.record,
            Some(&geometry.label),
            Some(geometry.serial),
        )
        .unwrap();
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
            format!(
                "{}\t{}\t{}\t{}\n",
                geometry.size, geometry.cluster, geometry.record, geometry.serial
            ),
        )
        .unwrap();
        fs::write(artifacts.join("active-image.txt"), &image).unwrap();
        fs::write(
            artifacts.join("reproducer.json"),
            generator::encode(&[]).unwrap(),
        )
        .unwrap();
        fs::write(artifacts.join("refusals.tsv"), "").unwrap();
        let zero: Counts = ([0; OPCODES], [0; OPCODES]);
        fs::write(
            artifacts.join("outcome-counts.json"),
            serde_json::to_vec(&zero).unwrap(),
        )
        .unwrap();
        Self {
            image,
            model: wrap(model),
            artifacts,
            operations: Vec::new(),
            refusals: Vec::new(),
            counts: ([0; OPCODES], [0; OPCODES]),
            reasons: BTreeMap::new(),
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
        let expected = candidate.apply(&op);
        let result = execute(&mut io, &op);
        io.sync().expect("sync operation, including refusal");
        let accepted = result.is_ok();
        let kind = op.opcode();
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
                let operation = serde_json::to_value(&op).unwrap()["op"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                *self
                    .reasons
                    .entry((operation, format!("{:?}", error.kind())))
                    .or_default() += 1;
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
            assert_eq!(
                independent_streams(&mut reader, &file),
                actual.streams,
                "independent streams {path}"
            );
            assert_eq!(
                independent_eas(&mut reader, &file),
                actual.eas,
                "independent EAs {path}"
            );
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

    pub fn generated(&mut self, input: &[u8]) -> Counts {
        fs::write(self.artifacts.join("input.bin"), input).unwrap();
        let mut generator = Generator::new(input);
        let mut counts = ([0; OPCODES], [0; OPCODES]);
        while let Some(op) = generator.next(self.model.tree()) {
            let kind = op.opcode();
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
                    files.insert(path.clone(), entry.bytes);
                }
                for (index, (stream, expected)) in entry.streams.iter().enumerate() {
                    let output = std::process::Command::new("ntfscat")
                        .args(["-n", stream, &self.image, &path])
                        .output()
                        .expect("ntfscat missing: install ntfs-3g for test:op-sequence-oracle");
                    let evidence = format!("ntfscat-{}-stream-{index}", entry.record);
                    fs::write(
                        self.artifacts.join(format!("{evidence}.bin")),
                        &output.stdout,
                    )
                    .unwrap();
                    fs::write(
                        self.artifacts.join(format!("{evidence}.txt")),
                        &output.stderr,
                    )
                    .unwrap();
                    assert!(
                        output.status.success(),
                        "ntfscat -n {stream} {path}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    assert_eq!(
                        &output.stdout, expected,
                        "ntfs-3g stream {stream} of {path}"
                    );
                }
                if !entry.eas.is_empty() {
                    // The raw $EA, attribute type 0xE0, decoded as above.
                    let output = std::process::Command::new("ntfscat")
                        .args(["-a", "0xE0", &self.image, &path])
                        .output()
                        .expect("ntfscat missing: install ntfs-3g for test:op-sequence-oracle");
                    let evidence = format!("ntfscat-{}-ea", entry.record);
                    fs::write(
                        self.artifacts.join(format!("{evidence}.bin")),
                        &output.stdout,
                    )
                    .unwrap();
                    fs::write(
                        self.artifacts.join(format!("{evidence}.txt")),
                        &output.stderr,
                    )
                    .unwrap();
                    assert!(
                        output.status.success(),
                        "ntfscat -a 0xE0 {path}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let mut eas = full_ea_information(&output.stdout);
                    eas.sort();
                    assert_eq!(eas, entry.eas, "ntfs-3g EAs of {path}");
                }
            }
            reference_tools(&self.image, &files, &self.artifacts);
            // The named samples keep their image beside ntfsfix.txt and
            // geometry.txt for Windows chkdsk to grade (#1034, #1035).
            let name = self.artifacts.file_name().unwrap().to_string_lossy();
            let retained = std::env::var("OPSEQ_RETAIN_SAMPLES").unwrap_or_default();
            if retained.split(',').any(|sample| sample == name) {
                fs::copy(&self.image, self.artifacts.join("sample.img"))
                    .expect("retain operation sample image");
            }
        }
        assert!(
            self.started.elapsed() < Duration::from_secs(90),
            "sequence deadline"
        );
    }
}

impl<M> Drop for Harness<M> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            fs::copy(&self.image, self.artifacts.join("failure.img"))
                .expect("retain failed operation image");
        }
        // Fuzz workers stay alive across inputs; don't accumulate image files.
        let _ = fs::remove_file(&self.image);
    }
}

/// Replay concrete operations on a fresh image. The first mismatch is its
/// kind and the number of operations executed when it was seen (#684).
pub fn replay<M: Expected>(
    name: &str,
    operations: &[Op],
    wrap: impl FnOnce(Model) -> M,
) -> Option<Mismatch> {
    replay_on(name, &geometry(), operations, wrap)
}

/// `replay` on an image formatted at `geometry`, such as a pin's (#1024).
pub fn replay_on<M: Expected>(
    name: &str,
    geometry: &Geometry,
    operations: &[Op],
    wrap: impl FnOnce(Model) -> M,
) -> Option<Mismatch> {
    let mut h = Harness::formatted(name, geometry, wrap);
    let outcome = shrink::quietly(|| {
        panic::catch_unwind(AssertUnwindSafe(|| {
            for op in operations {
                h.step(op.clone());
            }
            h.finish();
        }))
    });
    let payload = outcome.err()?;
    Some(Mismatch {
        kind: shrink::kind_of(&*payload),
        step: h.operations.len(),
    })
}

/// Raw generator bytes become the operations the harness ran from them,
/// up to and including any that failed.
fn concretize<M: Expected>(
    name: &str,
    geometry: &Geometry,
    input: &[u8],
    wrap: impl FnOnce(Model) -> M,
) -> Vec<Op> {
    let mut h = Harness::formatted(name, geometry, wrap);
    let _ = shrink::quietly(|| panic::catch_unwind(AssertUnwindSafe(|| h.generated(input))));
    h.operations.clone()
}

/// Minimize a saved failing input, JSON replay or raw generator bytes, and
/// pin it as `<corpus>/<name>-v1.json` with the mismatch it reproduces.
/// The input is only ever read: a failed capture leaves it as it was.
pub fn capture<M: Expected>(
    original: &Path,
    name: &str,
    corpus: &Path,
    wrap: impl Fn(Model) -> M,
) -> Result<PathBuf, String> {
    let destination = shrink::destination(corpus, name)?;
    let kept = |why: &str| {
        format!(
            "{}: {why}; it is unchanged and nothing was pinned",
            original.display()
        )
    };
    let input = fs::read(original).map_err(|e| format!("{}: {e}", original.display()))?;
    let scratch = format!("capture-{name}");
    let scratch_dir = Path::new("tmp/op-sequence-model").join(&scratch);
    if let (Ok(file), Ok(dir)) = (fs::canonicalize(original), fs::canonicalize(&scratch_dir)) {
        if file.starts_with(dir) {
            return Err(kept("the capture's replays write here: move it first"));
        }
    }
    // Raw bytes choose their image (#1024); a replay keeps its pin's.
    let (operations, geometry) = match generator::decode_regression(&input) {
        Ok((operations, pinned)) => (
            operations,
            pinned.map_or_else(geometry, |pinned| pinned.geometry),
        ),
        Err(_) => {
            let (geometry, input) = generator::decode_geometry(&input);
            if let Some(reason) = refused_geometry(&scratch, &geometry) {
                return Err(kept(&format!(
                    "the formatter refuses its geometry: {reason}"
                )));
            }
            (concretize(&scratch, &geometry, input, &wrap), geometry)
        }
    };
    let found = replay_on(&scratch, &geometry, &operations, &wrap)
        .ok_or_else(|| kept("it replays without a mismatch"))?;
    let minimal = shrink::shrink(&operations, &found, |candidate| {
        replay_on(&scratch, &geometry, candidate, &wrap)
    })
    .map_err(|e| kept(&e))?;
    let regression = Regression {
        kind: found.kind,
        step: minimal.len(),
        geometry,
        original: original.display().to_string(),
        original_operations: operations.len(),
    };
    shrink::pin(&destination, &minimal, &regression)
}
