//! First deterministic slice of #484: create, offset writes, truncate,
//! and a refused duplicate create. Every successful operation must execute;
//! unsupported operations fail rather than disappearing from the workload.
#[path = "op_sequence_model/runner.rs"]
mod runner;

use runner::{common, generator, harness, reference_tools, SIZE};

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::{fsck, mkfs, read, write};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const STEPS: usize = 120;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut n = self.0;
        n = (n ^ (n >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        n = (n ^ (n >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        n ^ (n >> 31)
    }
}

fn verify(image: &str, model: &BTreeMap<String, Vec<u8>>, initialized: &BTreeMap<String, usize>) {
    // A fresh descriptor and fresh independent parser, with no writer state.
    let mut io = PathIo::open_rw(Path::new(image)).expect("reopen image");
    let (oracle, mut reader) = common::open(image);
    let root = read::resolve_path(&mut io, "/").expect("root");
    let mut names: Vec<_> = read::read_dir_entries(&mut io, root)
        .expect("list root")
        .into_iter()
        .filter(read::listed_to_hosts)
        .map(|e| format!("/{}", e.name))
        .collect();
    names.sort();
    assert_eq!(names, model.keys().cloned().collect::<Vec<_>>());
    let mut theirs = common::list_names(&oracle, &mut reader, "/");
    theirs.retain(|name| !name.starts_with('$') && name != ".");
    theirs.sort();
    assert_eq!(
        theirs,
        model.keys().map(|p| p[1..].to_string()).collect::<Vec<_>>()
    );
    for (path, expected) in model {
        let record = read::resolve_path(&mut io, path).expect("resolve file");
        let stat = read::read_stat(&mut io, record).expect("stat");
        assert_eq!(stat.size, expected.len() as u64, "size of {path}");
        let actual =
            read::read_attribute_value(&mut io, record, fs_ntfs::attr_io::AttrType::Data, None)
                .expect("read data");
        assert_eq!(&actual, expected, "driver contents of {path}");
        let parsed = common::read_file_all(&oracle, &mut reader, path);
        assert_eq!(parsed.len(), expected.len(), "independent size of {path}");
        // The upstream `ntfs` reader follows allocated runs but does not
        // honor $DATA's initialized_size. After a shrink followed by a grow,
        // NTFS reads the uninitialized tail as zeros even if freed clusters
        // still contain the old bytes. Compare the parser over initialized
        // bytes; ntfs-3g checks the complete logical value below.
        let initialized_len = initialized[path].min(expected.len());
        assert_eq!(
            &parsed[..initialized_len],
            &expected[..initialized_len],
            "independent ntfs parser initialized contents of {path}"
        );
    }
    let report = fsck::check_io(&mut io).expect("check_io");
    assert!(
        report.findings.is_empty(),
        "check_io: {:?}",
        report.findings
    );
}

fn sequence(seed: u64) {
    let started = Instant::now();
    let artifacts = std::path::PathBuf::from(format!("tmp/op-sequence-model/{seed:016x}"));
    fs::create_dir_all(&artifacts).expect("artifact directory");
    let mut trace = File::create(artifacts.join("reproducer.txt")).expect("reproducer");
    writeln!(trace, "seed\t{seed}\ngeometry\t{SIZE}\t4096\t1024").unwrap();
    let image = common::temp_image_path(format!("opseq_{seed:016x}"));
    let file = File::create(&image).expect("create image");
    file.set_len(SIZE).expect("image length");
    drop(file);
    let mut io = PathIo::open_rw(Path::new(&image)).expect("open writer");
    mkfs::format_filesystem(&mut io, SIZE, 4096, 1024, Some("OPSEQ"), Some(seed)).expect("format");
    let mut model = BTreeMap::new();
    let mut initialized = BTreeMap::new();
    for name in ["a", "b", "c"] {
        writeln!(trace, "create\t/\t{name}").unwrap();
        trace.flush().unwrap();
        write::create_file_io(&mut io, "/", name).expect("create");
        let path = format!("/{name}");
        model.insert(path.clone(), Vec::new());
        initialized.insert(path, 0);
    }
    let mut rng = Rng(seed);
    let mut capacity_refusals = 0usize;
    for step in 0..STEPS {
        let path = format!("/{}", ["a", "b", "c"][(rng.next() % 3) as usize]);
        let expected = model.get_mut(&path).unwrap();
        if step % 3 == 2 {
            let length = (rng.next() % 12289) as usize;
            let previous_length = expected.len();
            writeln!(trace, "truncate\t{path}\t{length}").unwrap();
            trace.flush().unwrap();
            write::truncate_io(&mut io, &path, length as u64).expect("truncate must execute");
            expected.resize(length, 0);
            if length < previous_length {
                let initialized_len = initialized.get_mut(&path).unwrap();
                *initialized_len = (*initialized_len).min(length);
            }
        } else {
            let offset = (rng.next() % 16384) as usize;
            let length = (rng.next() % 4096 + 1) as usize;
            let fill = rng.next();
            let mut bytes_rng = Rng(fill);
            let data: Vec<_> = (0..length).map(|_| bytes_rng.next() as u8).collect();
            writeln!(trace, "write_at\t{path}\t{offset}\t{length}\t{fill}").unwrap();
            trace.flush().unwrap();
            match write::write_at_io(&mut io, &path, offset as u64, &data) {
                Ok(written) => {
                    assert_eq!(written, length as u64);
                    expected.resize(expected.len().max(offset + length), 0);
                    expected[offset..offset + length].copy_from_slice(&data);
                    let initialized_len = initialized.get_mut(&path).unwrap();
                    *initialized_len = (*initialized_len).max(offset + length);
                }
                Err(error) if error.to_string().contains("exceed attr capacity") => {
                    // Attribute-record expansion is W2.1. The allocator
                    // must refuse this growth cleanly until that lands.
                    capacity_refusals += 1;
                    writeln!(
                        trace,
                        "refused\twrite_at\t{path}\t{offset}\t{length}\t{error}"
                    )
                    .unwrap();
                    trace.flush().unwrap();
                }
                Err(error) => panic!("write_at must succeed or hit the known W2.1 limit: {error}"),
            }
        }
        io.sync().expect("sync");
        drop(io);
        verify(&image, &model, &initialized);
        io = PathIo::open_rw(Path::new(&image)).expect("reopen writer");
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "seed {seed}, operation {step}: 90-second sequence deadline exceeded"
        );
    }
    writeln!(trace, "create\t/\ta\t# expected refusal").unwrap();
    trace.flush().unwrap();
    let refusal = write::create_file_io(&mut io, "/", "a").expect_err("duplicate must refuse");
    io.sync().expect("sync refusal");
    drop(io);
    verify(&image, &model, &initialized);
    fs::write(
        artifacts.join("refusals.tsv"),
        format!(
            "operation\tcount\tkind\ncreate-existing\t1\t{:?}\nwrite_at-mapping-capacity\t{}\tIo\n",
            refusal.kind(),
            capacity_refusals
        ),
    )
    .expect("refusal histogram");
    if std::env::var_os("OPSEQ_REFERENCE_TOOLS").is_some() {
        reference_tools(&image, &model, &artifacts);
    }
    assert!(
        started.elapsed() < Duration::from_secs(90),
        "sequence deadline exceeded"
    );
}

#[test]
fn seeded_readback_0() {
    sequence(0);
}
#[test]
fn seeded_readback_1() {
    sequence(1);
}
#[test]
fn seeded_readback_484() {
    sequence(484);
}
#[test]
fn seeded_readback_boundary() {
    sequence(u64::MAX);
}

#[test]
fn committed_corpus_replays_every_seed() {
    let corpus = Path::new("fuzz/corpus/operation_sequence");
    for required in [
        "basic-write-v1.json",
        "namespace-v1.json",
        "refusals-v1.json",
    ] {
        assert!(corpus.join(required).is_file(), "missing seed {required}");
    }
    let mut seeds: Vec<_> = fs::read_dir(corpus)
        .expect("restore the committed operation corpus")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    seeds.sort();
    assert!(!seeds.is_empty(), "no operation sequences selected");
    for (index, seed) in seeds.iter().enumerate() {
        let operations = generator::decode_replay(&fs::read(seed).unwrap()).unwrap();
        assert!(!operations.is_empty(), "empty seed {}", seed.display());
        let mut h = harness::Harness::new(&format!("corpus-{index}"));
        for operation in operations {
            h.step(operation);
        }
        h.finish();
    }
    fs::write(
        "tmp/op-sequence-model/corpus-executed.json",
        serde_json::to_vec(&seeds).unwrap(),
    )
    .unwrap();
}

#[test]
fn generated_namespace_operations_and_refusals_survive_reopen() {
    let mut input = Vec::new();
    for (verb, target, parent, name, length, offset, data) in [
        (0, 2, 2, 0, 0u16, 0u16, vec![]), // create /a
        (1, 2, 2, 2, 0, 0, vec![]),       // mkdir /b
        (2, 2, 2, 0, 4096, 0, vec![7; 4096]),
        (3, 2, 2, 0, 3, 6000, vec![1, 2, 3]),
        (4, 2, 2, 0, 9000, 0, vec![]),
        (8, 2, 2, 4, 0, 0, vec![]),  // link /a -> /é
        (5, 2, 2, 20, 0, 0, vec![]), // rename /a -> /fresh7
        (6, 2, 2, 0, 0, 0, vec![]),  // unlink /fresh7
        (7, 6, 2, 0, 0, 0, vec![]),  // rmdir /b
    ] {
        input.extend([verb, target, parent, name]);
        input.extend(length.to_le_bytes());
        input.extend(offset.to_le_bytes());
        input.extend(data);
    }
    // Every opcode also reaches the disk with an absent target/parent.
    for verb in 0..9 {
        let write = verb == 2 || verb == 3;
        input.extend([verb, 0, 0, 0, u8::from(write), 0, 0, 0]);
        if write {
            input.push(42);
        }
    }
    let mut h = harness::Harness::new("generated");
    let counts = h.generated(&input);
    assert!(counts.0.iter().all(|n| *n > 0), "accepted: {:?}", counts.0);
    assert!(counts.1.iter().all(|n| *n > 0), "refused: {:?}", counts.1);
    assert_eq!(h.model.read("/é").unwrap().len(), 9000);
    assert_eq!(h.model.stat("/é").unwrap().links, 1);
    h.finish();
}

#[test]
fn replay_checks_nested_state_and_refusal_atomicity() {
    use generator::Op;
    let operations = generator::decode_replay(
        br#"{"version":1,"operations":[
        {"op":"mkdir","parent":"/","name":"d"},
        {"op":"create","parent":"/d","name":"f"},
        {"op":"write","path":"/d/f","data":[10,20,30]},
        {"op":"link","path":"/d/f","parent":"/","name":"alias"},
        {"op":"rename","path":"/d","name":"moved","replace":false},
        {"op":"create","parent":"/moved","name":"F"},
        {"op":"rmdir","path":"/moved"},
        {"op":"unlink","path":"/moved"},
        {"op":"rmdir","path":"/alias"},
        {"op":"rename","path":"/alias","name":"moved","replace":true},
        {"op":"create","parent":"/alias","name":"child"},
        {"op":"truncate","path":"/moved/f","size":1},
        {"op":"write_at","path":"/alias","offset":4,"data":[40]},
        {"op":"create","parent":"/","name":"other"},
        {"op":"rename","path":"/alias","name":"other","replace":true}
    ]}"#,
    )
    .unwrap();
    let mut h = harness::Harness::new("nested-replay");
    for op in operations {
        h.step(op);
    }
    assert_eq!(h.model.read("/other").unwrap(), &[10, 0, 0, 0, 40]);
    assert_eq!(h.model.stat("/other").unwrap().links, 2);
    h.step(Op::Unlink {
        path: "/moved/f".into(),
    });
    h.step(Op::Rmdir {
        path: "/moved".into(),
    });
    h.finish();
}

#[test]
fn persisted_byte_mutation_is_detected_without_writer_state() {
    let mut h = harness::Harness::new("byte-control");
    h.step(generator::Op::Create {
        parent: "/".into(),
        name: "a".into(),
    });
    h.step(generator::Op::Write {
        path: "/a".into(),
        data: vec![1, 2, 3],
    });
    write::write_at(Path::new(&h.image), "/a", 0, &[99]).unwrap();
    File::open(&h.image).unwrap().sync_all().unwrap();
    assert!(std::panic::catch_unwind(|| h.verify()).is_err());
}

#[test]
fn persisted_name_mutation_is_detected_without_writer_state() {
    let mut h = harness::Harness::new("name-control");
    h.step(generator::Op::Create {
        parent: "/".into(),
        name: "a".into(),
    });
    write::rename(Path::new(&h.image), "/a", "wrong").unwrap();
    File::open(&h.image).unwrap().sync_all().unwrap();
    assert!(std::panic::catch_unwind(|| h.verify()).is_err());
}
