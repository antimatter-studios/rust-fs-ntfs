//! First deterministic slice of #484: create, offset writes, truncate,
//! and a refused duplicate create. Every successful operation must execute;
//! unsupported operations fail rather than disappearing from the workload.
mod common;
#[allow(dead_code)]
#[path = "op_sequence_model/generator.rs"]
mod generator;
#[path = "op_sequence_model/harness.rs"]
mod harness;
#[path = "op_sequence_model/model.rs"]
mod model;
#[path = "op_sequence_model/shrink.rs"]
mod shrink;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::{fsck, mkfs, read, write};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const STEPS: usize = 120;
const SIZE: u64 = 64 * 1024 * 1024;
/// The committed seeds and captured regressions, replayed by the gate.
const CORPUS: &str = "fuzz/corpus/operation_sequence";

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

fn reference_tools(image: &str, model: &BTreeMap<String, Vec<u8>>, artifacts: &Path) {
    let fixed = std::process::Command::new("ntfsfix")
        .args(["-n", image])
        .output()
        .expect("ntfsfix missing: install ntfs-3g for test:op-sequence-oracle");
    let mut report = fixed.stdout;
    report.extend_from_slice(&fixed.stderr);
    fs::write(artifacts.join("ntfsfix.txt"), &report).expect("save oracle verdict");
    assert!(
        fixed.status.success(),
        "ntfsfix -n: {}",
        String::from_utf8_lossy(&report)
    );
    for (index, (path, expected)) in model.iter().enumerate() {
        let output = std::process::Command::new("ntfscat")
            .args([image, path])
            .output()
            .expect("ntfscat missing: install ntfs-3g for test:op-sequence-oracle");
        fs::write(
            artifacts.join(format!("ntfscat-{index}.bin")),
            &output.stdout,
        )
        .unwrap();
        fs::write(
            artifacts.join(format!("ntfscat-{index}.txt")),
            &output.stderr,
        )
        .unwrap();
        assert!(
            output.status.success(),
            "ntfscat {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(&output.stdout, expected, "ntfs-3g contents of {path}");
    }
}

/// The capture command (#684), run from the repository root:
/// `cargo run --release --example op_sequence_capture -- INPUT NAME`.
#[allow(dead_code)]
pub fn capture(original: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    shrink::quiet_panics();
    harness::capture(original, name, Path::new(CORPUS), |model| model)
}

/// Shared with libFuzzer: valid JSON replays concretely; other bytes use the
/// live generator. The gate above always decodes its JSON strictly.
#[allow(dead_code)]
pub fn fuzz_input(input: &[u8]) {
    // Raw bytes choose the image they run on (#1024), a pin replays on its
    // own, and geometry.txt beside every finding names it.
    let (replayed, geometry, rest) = match generator::decode_regression(input) {
        Ok((operations, pinned)) => (
            Some(operations),
            pinned.map_or_else(harness::geometry, |pinned| pinned.geometry),
            input,
        ),
        Err(_) => {
            let (geometry, rest) = generator::decode_geometry(input);
            (None, geometry, rest)
        }
    };
    // Keep totals across completed inputs as well as the current prefix's
    // per-opcode accepted/refused counts when a later input crashes, and
    // the geometries the formatter refused, as the model expects (#1025).
    use generator::OPCODES;
    static TOTALS: std::sync::Mutex<(harness::Counts, usize)> =
        std::sync::Mutex::new((([0; OPCODES], [0; OPCODES]), 0));
    let histogram = |totals: &(harness::Counts, usize)| {
        fs::write(
            "tmp/op-sequence-model/explorer/refusal-histogram.json",
            serde_json::to_vec(&(&totals.0 .0, &totals.0 .1, totals.1)).unwrap(),
        )
        .unwrap();
    };
    if harness::refused_geometry("explorer", &geometry).is_some() {
        fs::write("tmp/op-sequence-model/explorer/input.bin", input).unwrap();
        let mut totals = TOTALS.lock().unwrap();
        totals.1 += 1;
        histogram(&totals);
        return;
    }
    let mut h = harness::Harness::on("explorer", &geometry);
    fs::write("tmp/op-sequence-model/explorer/input.bin", input).unwrap();
    match replayed {
        Some(operations) => {
            for operation in operations {
                h.step(operation);
            }
        }
        None => {
            h.generated(rest);
        }
    }
    h.finish();
    let mut totals = TOTALS.lock().unwrap();
    for kind in 0..OPCODES {
        totals.0 .0[kind] += h.counts.0[kind];
        totals.0 .1[kind] += h.counts.1[kind];
    }
    histogram(&totals);
}

/// A geometry the formatter refuses (#1025) runs nothing: the explorer
/// records the formatter's reason and counts it in its histogram.
#[test]
fn the_explorer_counts_a_refused_geometry_with_its_reason() {
    fuzz_input(&[0, 255, 0, 1, 2, 3]);
    let artifacts = Path::new("tmp/op-sequence-model/explorer");
    let geometry = fs::read_to_string(artifacts.join("geometry.txt")).unwrap();
    assert!(geometry.contains("\t131072\t"), "{geometry}");
    let refusals = fs::read_to_string(artifacts.join("refusals.tsv")).unwrap();
    assert!(
        refusals.starts_with("format\t") && refusals.contains("invalid cluster_size 131072"),
        "{refusals}"
    );
    let histogram: (Vec<usize>, Vec<usize>, usize) =
        serde_json::from_slice(&fs::read(artifacts.join("refusal-histogram.json")).unwrap())
            .expect("accepted and refused operations, then refused geometries");
    assert!(histogram.2 >= 1, "{histogram:?}");
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
fn every_committed_corpus_seed_names_its_geometry() {
    for entry in fs::read_dir(CORPUS).expect("restore the committed operation corpus") {
        let seed = entry.unwrap().path();
        if seed.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let (_, pinned) = generator::decode_regression(&fs::read(&seed).unwrap()).unwrap();
        let pinned = pinned.unwrap_or_else(|| panic!("{} has no geometry", seed.display()));
        assert_eq!(
            model::format_refusal(
                pinned.geometry.size,
                pinned.geometry.cluster,
                pinned.geometry.record
            ),
            None,
            "{} must name a supported geometry",
            seed.display()
        );
    }
}

fn boundary_corpus_replay(cluster: u32, record: u32) {
    let stem = format!("geometry-c{cluster}-r{record}-v1");
    let seed = Path::new(CORPUS).join(format!("{stem}.json"));
    let (operations, pinned) = generator::decode_regression(
        &fs::read(&seed).unwrap_or_else(|error| panic!("missing seed {}: {error}", seed.display())),
    )
    .unwrap();
    let geometry = pinned
        .expect("boundary seed must name its geometry")
        .geometry;
    assert_eq!((geometry.cluster, geometry.record), (cluster, record));
    assert!(!operations.is_empty(), "empty boundary seed");
    // Separate from the all-corpus replay so every boundary is a fixed CI
    // case, with its own fresh image, independent reader and final remount.
    let mut h = harness::Harness::on(&format!("boundary-{stem}"), &geometry);
    for operation in operations {
        assert!(h.step(operation), "boundary operation must execute");
    }
    h.finish();
}

#[test]
fn corpus_c512_r1024_survives_fresh_remount() {
    boundary_corpus_replay(512, 1024);
}

#[test]
fn corpus_c512_r4096_survives_fresh_remount() {
    boundary_corpus_replay(512, 4096);
}

#[test]
fn corpus_c65536_r1024_survives_fresh_remount() {
    boundary_corpus_replay(65536, 1024);
}

#[test]
fn corpus_c65536_r4096_survives_fresh_remount() {
    boundary_corpus_replay(65536, 4096);
}

#[test]
fn committed_corpus_replays_every_seed() {
    let corpus = Path::new(CORPUS);
    for required in [
        "basic-write-v1.json",
        "namespace-v1.json",
        "refusals-v1.json",
        "geometry-c512-r4096-v1.json",
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
    for seed in &seeds {
        // Shown only if this test fails: which seed, and what it pins.
        eprintln!("replaying {}", seed.display());
        let (operations, pinned) = generator::decode_regression(&fs::read(seed).unwrap()).unwrap();
        assert!(!operations.is_empty(), "empty seed {}", seed.display());
        // A pin replays on the image it was captured on; a plain seed on
        // the default one.
        let geometry = match pinned {
            Some(pinned) => {
                eprintln!("pinned: {} after operation {}", pinned.kind, pinned.step);
                pinned.geometry
            }
            None => harness::geometry(),
        };
        // Evidence lands in tmp/op-sequence-model/corpus-<file stem>/.
        let stem = seed.file_stem().unwrap().to_str().unwrap();
        let mut h = harness::Harness::on(&format!("corpus-{stem}"), &geometry);
        assert_eq!(
            fs::read_to_string(format!("tmp/op-sequence-model/corpus-{stem}/geometry.txt"))
                .unwrap(),
            format!(
                "{}\t{}\t{}\t{}\n",
                geometry.size, geometry.cluster, geometry.record, geometry.serial
            ),
            "{} must replay on the geometry it names",
            seed.display()
        );
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
    // The nine namespace and data opcodes; streams have their own case.
    assert!(
        counts.0[..9].iter().all(|n| *n > 0),
        "accepted: {:?}",
        counts.0
    );
    assert!(
        counts.1[..9].iter().all(|n| *n > 0),
        "refused: {:?}",
        counts.1
    );
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

/// Named streams (#731): `$UpCase` collisions, hard-link sharing, a stream
/// past the resident limit and atomic refusals, through every reopen.
#[test]
fn generated_streams_and_refusals_survive_reopen() {
    use generator::Op;
    let mut operations = generator::decode_replay(
        r#"{"version":1,"operations":[
        {"op":"create","parent":"/","name":"a"},
        {"op":"write","path":"/a","data":[1,2,3]},
        {"op":"write_stream","path":"/a","stream":"s","data":[10,11]},
        {"op":"write_stream","path":"/a","stream":"S","data":[12]},
        {"op":"write_stream","path":"/a","stream":"σ","data":[]},
        {"op":"write_stream","path":"/a","stream":"Σ","data":[5,6]},
        {"op":"mkdir","parent":"/","name":"d"},
        {"op":"link","path":"/a","parent":"/d","name":"é"},
        {"op":"delete_stream","path":"/d/é","stream":"S"},
        {"op":"delete_stream","path":"/a","stream":"s"},
        {"op":"delete_stream","path":"/a","stream":""},
        {"op":"write_stream","path":"/a","stream":"a:b","data":[1]},
        {"op":"write_stream","path":"/a","stream":"x.","data":[1]},
        {"op":"write_stream","path":"/a","stream":"","data":[1]},
        {"op":"write_stream","path":"/missing","stream":"s","data":[1]},
        {"op":"write_stream","path":"/a/x","stream":"s","data":[1]},
        {"op":"write_at","path":"/a","offset":3,"data":[4]}
    ]}"#
        .as_bytes(),
    )
    .unwrap();
    // Larger than a 1024-byte record can hold resident.
    let big: Vec<u8> = (0..3000).map(|i| (i % 253) as u8 + 1).collect();
    operations.push(Op::WriteStream {
        path: "/a".into(),
        stream: "big".into(),
        data: big.clone(),
    });
    let mut h = harness::Harness::new("streams");
    for op in operations {
        h.step(op);
    }
    // Under the volume's $UpCase, S is s and Σ is σ: one stream each,
    // spelled as first written, shared by both names of the file.
    let expected = vec![("big".to_string(), big), ("σ".to_string(), vec![5, 6])];
    assert_eq!(h.model.streams("/a").unwrap(), expected);
    assert_eq!(h.model.streams("/d/é").unwrap(), expected);
    assert_eq!(h.model.read("/a").unwrap(), &[1, 2, 3, 4]);
    let (accepted, refused) = h.counts;
    assert_eq!((accepted[9], refused[9]), (5, 5), "stream writes");
    assert_eq!((accepted[10], refused[10]), (1, 2), "stream deletes");

    // The same opcodes from raw generator bytes: a fresh stream on /a,
    // then the deletion of its first stream, then refusals on a missing
    // path and on a file used as a directory.
    let mut input = Vec::new();
    for (verb, target, name, length, data) in [
        (9, 2, 20, 2u16, vec![7, 8]),
        (10, 2, 19, 0, vec![]),
        (9, 0, 0, 1, vec![9]),
        (10, 1, 0, 0, vec![]),
    ] {
        input.extend([verb, target, 2, name]);
        input.extend(length.to_le_bytes());
        input.extend([0, 0]);
        input.extend(data);
    }
    let counts = h.generated(&input);
    assert_eq!(
        (counts.0[9], counts.1[9]),
        (1, 1),
        "generated stream writes"
    );
    assert_eq!(
        (counts.0[10], counts.1[10]),
        (1, 1),
        "generated stream deletes"
    );
    assert!(h.model.streams("/a").unwrap().len() == 2);
    h.finish();
}

/// Extended attributes (#732): case collisions, hard-link sharing, sets on
/// both sides of the resident limit and at and past 64 KiB, and atomic
/// refusals, through every reopen.
#[test]
fn generated_eas_and_refusals_survive_reopen() {
    use generator::Op;
    let mut operations = generator::decode_replay(
        r#"{"version":1,"operations":[
        {"op":"create","parent":"/","name":"a"},
        {"op":"write","path":"/a","data":[1,2,3]},
        {"op":"write_ea","path":"/a","name":"user.x","flags":0,"value":[10,11]},
        {"op":"write_ea","path":"/a","name":"USER.x","flags":128,"value":[12]},
        {"op":"write_ea","path":"/a","name":"z","flags":0,"value":[]},
        {"op":"mkdir","parent":"/","name":"d"},
        {"op":"link","path":"/a","parent":"/d","name":"é"},
        {"op":"remove_ea","path":"/d/é","name":"Z"},
        {"op":"remove_ea","path":"/a","name":"z"},
        {"op":"remove_ea","path":"/a","name":""},
        {"op":"write_ea","path":"/a","name":"","flags":0,"value":[1]},
        {"op":"write_ea","path":"/missing","name":"a","flags":0,"value":[1]},
        {"op":"write_ea","path":"/a/x","name":"a","flags":0,"value":[1]},
        {"op":"remove_ea","path":"/missing","name":"a"},
        {"op":"write_at","path":"/a","offset":3,"data":[4]}
    ]}"#
        .as_bytes(),
    )
    .unwrap();
    let value = |len: usize, seed: usize| -> Vec<u8> {
        (0..len).map(|i| ((i * 7 + seed) % 251) as u8).collect()
    };
    let write = |path: &str, name: &str, flags: u8, value: Vec<u8>| Op::WriteEa {
        path: path.into(),
        name: name.into(),
        flags,
        value,
    };
    let remove = |path: &str, name: &str| Op::RemoveEa {
        path: path.into(),
        name: name.into(),
    };
    operations.extend([
        // 254 bytes is the longest name, and n and N are one name.
        write("/a", &"n".repeat(254), 0, vec![1]),
        write("/a", &"N".repeat(254), 128, vec![2]),
        write("/a", &"n".repeat(255), 0, vec![3]),
        // More than a 1024-byte record holds resident: into clusters.
        write("/a", "big", 0, value(3000, 1)),
        // USER.X, the long name and BIG pack to 12 + 260 + 3008 bytes, so
        // 63,000 more is past 64 KiB and 62,000 is not.
        write("/a", "huge", 0, value(63_000, 2)),
        write("/a", "huge", 128, value(62_000, 3)),
        // A set that shrinks, then grows to exactly 65,535 packed bytes:
        // 260 + 62,009 + 5 + 6 + 3,255. One byte more is refused.
        remove("/a", "BIG"),
        write("/a", "user.x", 0, value(3255, 4)),
        write("/a", "user.x", 0, value(3256, 5)),
        // Removing every EA gives the clusters back, and so does the last
        // name of a file that still has them.
        Op::Create {
            parent: "/".into(),
            name: "b".into(),
        },
        write("/b", "big", 0, value(5000, 6)),
        remove("/b", "big"),
        write("/b", "big", 0, value(5000, 7)),
        Op::Unlink { path: "/b".into() },
        Op::Unlink {
            path: "/d/é".into(),
        },
    ]);
    let mut h = harness::Harness::new("eas");
    for op in operations {
        h.step(op);
    }
    let expected = vec![
        (b"HUGE".to_vec(), 128, value(62_000, 3)),
        ("N".repeat(254).into_bytes(), 128, vec![2]),
        (b"USER.X".to_vec(), 0, value(3255, 4)),
    ];
    assert_eq!(h.model.eas("/a").unwrap(), expected);
    assert_eq!(h.model.read("/a").unwrap(), &[1, 2, 3, 4]);
    let (accepted, refused) = h.counts;
    assert_eq!((accepted[11], refused[11]), (10, 6), "EA writes");
    assert_eq!((accepted[12], refused[12]), (3, 3), "EA removals");

    // The same opcodes from raw generator bytes, on /a's full set: a fresh
    // EA is refused, the removal of its first EA makes room for it, then
    // refusals on a missing path, a file used as a directory and a value
    // no set can hold.
    let mut input = Vec::new();
    for (verb, target, flags, name, length, class, data) in [
        (11, 2, 1, 13, 2u16, 0, vec![7, 8]),
        (12, 2, 0, 12, 0, 0, vec![]),
        (11, 2, 1, 13, 2, 0, vec![7, 8]),
        (11, 0, 0, 0, 1, 0, vec![9]),
        (12, 1, 0, 0, 0, 0, vec![]),
        (11, 2, 0, 0, u16::MAX, 3, vec![]),
    ] {
        input.extend([verb, target, flags, name]);
        input.extend(length.to_le_bytes());
        input.extend([class, 0]);
        input.extend(data);
    }
    let counts = h.generated(&input);
    assert_eq!((counts.0[11], counts.1[11]), (1, 3), "generated EA writes");
    assert_eq!(
        (counts.0[12], counts.1[12]),
        (1, 1),
        "generated EA removals"
    );
    assert_eq!(
        h.model.eas("/a").unwrap()[0],
        (b"EA3".to_vec(), model::NEED_EA, vec![7, 8])
    );
    assert_eq!(h.model.eas("/a").unwrap().len(), 3);
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

/// Test-only divergence (#684): after a truncate that shrinks a file, this
/// wrapper keeps reporting the old size. The real model is never changed.
#[derive(Clone)]
struct StaleTruncate {
    model: model::Model,
    stale: BTreeMap<usize, usize>,
}

impl harness::Expected for StaleTruncate {
    fn tree(&self) -> &model::Model {
        &self.model
    }

    fn apply(&mut self, op: &generator::Op) -> Result<(), model::Error> {
        use generator::Op;
        let before = match op {
            Op::Write { path, .. } | Op::WriteAt { path, .. } | Op::Truncate { path, .. } => {
                self.model.stat(path).ok()
            }
            _ => None,
        };
        op.apply(&mut self.model)?;
        if let Some(before) = before {
            match op {
                Op::Truncate { size, .. } if (*size as usize) < before.size => {
                    self.stale.insert(before.id, before.size);
                }
                _ => {
                    self.stale.remove(&before.id);
                }
            }
        }
        Ok(())
    }

    fn stat(&self, path: &str) -> Result<model::Stat, model::Error> {
        let mut stat = self.model.stat(path)?;
        if let Some(size) = self.stale.get(&stat.id) {
            stat.size = *size;
        }
        Ok(stat)
    }
}

fn stale_truncate(model: model::Model) -> StaleTruncate {
    StaleTruncate {
        model,
        stale: BTreeMap::new(),
    }
}

/// Eighteen operations of every kind, one refusal among them. Only three
/// matter to the divergence: `/a` is created, written once, then shrunk.
fn stale_truncate_workload() -> Vec<generator::Op> {
    let mut operations = generator::decode_replay(
        br#"{"version":1,"operations":[
        {"op":"mkdir","parent":"/","name":"d"},
        {"op":"create","parent":"/d","name":"f"},
        {"op":"create","parent":"/","name":"a"},
        {"op":"write","path":"/d/f","data":[10,20,30]},
        {"op":"link","path":"/d/f","parent":"/","name":"alias"},
        {"op":"create","parent":"/","name":"b"},
        {"op":"truncate","path":"/b","size":300},
        {"op":"write_at","path":"/b","offset":100,"data":[1,2,3]},
        {"op":"rename","path":"/d","name":"moved","replace":false},
        {"op":"rmdir","path":"/moved"},
        {"op":"mkdir","parent":"/","name":"e"},
        {"op":"rmdir","path":"/e"},
        {"op":"unlink","path":"/alias"},
        {"op":"rename","path":"/b","name":"c","replace":false},
        {"op":"truncate","path":"/a","size":1000},
        {"op":"create","parent":"/","name":"z"},
        {"op":"unlink","path":"/moved/f"}
    ]}"#,
    )
    .unwrap();
    let data = (0..4096).map(|i| (i % 251) as u8 + 1).collect();
    operations.insert(
        5,
        generator::Op::Write {
            path: "/a".into(),
            data,
        },
    );
    operations
}

/// The smallest sequence that still shows the divergence: a file must hold
/// a byte before a truncate can shrink it, and zero is the smallest size.
fn stale_truncate_minimum() -> Vec<generator::Op> {
    use generator::Op;
    vec![
        Op::Create {
            parent: "/".into(),
            name: "a".into(),
        },
        Op::Write {
            path: "/a".into(),
            data: vec![1],
        },
        Op::Truncate {
            path: "/a".into(),
            size: 0,
        },
    ]
}

#[test]
fn mismatch_kind_ignores_paths_values_and_operands() {
    for (message, kind) in [
        (
            "assertion `left == right` failed: size of /a/b\n  left: 1\n right: 0",
            "size of",
        ),
        (
            "assertion `left == right` failed: names/kinds in /d\n  left: []",
            "names/kinds in",
        ),
        (
            "disk accepted model refusal: Create { parent: \"/\", name: \"a\" }: Exists",
            "disk accepted model refusal",
        ),
        (
            "fresh reopen differs after Truncate { path: \"/a\", size: 0 }",
            "fresh reopen differs after Truncate",
        ),
        ("check_io: [Finding]", "check_io"),
        (
            "called `Result::unwrap()` on an `Err` value: Io",
            "called `Result::unwrap()` on an `Err` value",
        ),
        (
            "assertion failed: report.findings.is_empty()",
            "report.findings.is_empty()",
        ),
        ("sequence deadline", "sequence deadline"),
        ("", "panic"),
    ] {
        assert_eq!(shrink::kind(message), kind, "{message:?}");
    }
    let payload = std::panic::catch_unwind(|| std::panic::panic_any(7u8)).unwrap_err();
    assert_eq!(shrink::kind_of(&*payload), "panic");
    let payload = std::panic::catch_unwind(|| panic!("size of {}", "/x")).unwrap_err();
    assert_eq!(shrink::kind_of(&*payload), "size of");
}

#[test]
fn shrinker_reduces_a_known_divergence_to_its_minimal_sequence() {
    let workload = stale_truncate_workload();
    let found = harness::replay("shrink-control", &workload, stale_truncate)
        .expect("the wrapper's deliberate divergence");
    assert_eq!(
        found,
        shrink::Mismatch {
            kind: "size of".into(),
            step: 16,
        },
        "the shrinking truncate of /a is the sixteenth operation"
    );
    let mut replays = 0;
    let minimal = shrink::shrink(&workload, &found, |candidate| {
        replays += 1;
        harness::replay("shrink-control", candidate, stale_truncate)
    })
    .unwrap();
    assert_eq!(minimal, stale_truncate_minimum());
    // About twenty replays: runs are deleted in halving chunks first.
    assert!(replays <= 30, "{replays} replays to shrink 16 operations");
    assert_eq!(
        harness::replay("shrink-control", &minimal, stale_truncate),
        Some(shrink::Mismatch {
            kind: "size of".into(),
            step: 3,
        })
    );
    // The divergence is the wrapper's: the real model agrees with the disk.
    assert_eq!(
        harness::replay("shrink-control", &minimal, |model| model),
        None
    );
}

#[test]
fn capture_pins_a_minimized_regression_and_never_discards_the_original() {
    let root = Path::new("tmp/op-sequence-model/capture-control");
    let _ = fs::remove_dir_all(root);
    let corpus = root.join("corpus");
    fs::create_dir_all(&corpus).unwrap();
    // Raw generator bytes, as libFuzzer saves them: a zero geometry header
    // (the default image, #1024), then create /a, write five bytes to it,
    // mkdir /b, and shrink /a to two bytes.
    let mut input = vec![0; generator::GEOMETRY_HEADER];
    for (frame, data) in [
        ([0, 2, 2, 0, 0, 0, 0, 0], &[][..]),
        ([2, 2, 2, 0, 5, 0, 0, 0], &[9, 8, 7, 6, 5][..]),
        ([1, 2, 2, 2, 0, 0, 0, 0], &[][..]),
        ([4, 2, 2, 0, 2, 0, 0, 0], &[][..]),
    ] {
        input.extend(frame);
        input.extend(data);
    }
    let original = root.join("crash-input.bin");
    fs::write(&original, &input).unwrap();
    let pinned =
        harness::capture(&original, "stale-truncate-size", &corpus, stale_truncate).unwrap();
    assert_eq!(pinned, corpus.join("stale-truncate-size-v1.json"));
    assert_eq!(fs::read(&original).unwrap(), input, "original kept");
    let captured = fs::read(&pinned).unwrap();
    let (operations, regression) = generator::decode_regression(&captured).unwrap();
    let mut minimum = stale_truncate_minimum();
    minimum[1] = generator::Op::Write {
        path: "/a".into(),
        data: vec![9],
    };
    assert_eq!(operations, minimum);
    let regression = regression.expect("the pinned mismatch");
    assert_eq!((regression.kind.as_str(), regression.step), ("size of", 3));
    assert_eq!(regression.geometry, harness::geometry());
    assert_eq!(regression.original, original.display().to_string());
    assert_eq!(regression.original_operations, 4);
    // The pinned file replays to exactly the mismatch it records.
    assert_eq!(
        harness::replay("capture-replay", &operations, stale_truncate),
        Some(shrink::Mismatch {
            kind: regression.kind,
            step: regression.step,
        })
    );

    // A name already in the corpus is never overwritten.
    let error =
        harness::capture(&original, "stale-truncate-size", &corpus, stale_truncate).unwrap_err();
    assert!(error.contains("exists"), "{error}");
    assert_eq!(fs::read(&pinned).unwrap(), captured);

    // An input that does not fail pins nothing and is left as it was.
    let passing = root.join("passing-v1.json");
    let bytes = generator::encode(&minimum).unwrap();
    fs::write(&passing, &bytes).unwrap();
    let error = harness::capture(&passing, "passing", &corpus, |model| model).unwrap_err();
    assert!(error.contains(&passing.display().to_string()), "{error}");
    assert_eq!(fs::read(&passing).unwrap(), bytes);
    assert!(!corpus.join("passing-v1.json").exists());

    // An input inside the capture's own scratch directory would be
    // overwritten by its replays, so it is refused before any of them.
    let scratch = Path::new("tmp/op-sequence-model/capture-inside");
    fs::create_dir_all(scratch).unwrap();
    let inside = scratch.join("reproducer.json");
    fs::write(&inside, &bytes).unwrap();
    let error = harness::capture(&inside, "inside", &corpus, stale_truncate).unwrap_err();
    assert!(error.contains(&inside.display().to_string()), "{error}");
    assert_eq!(fs::read(&inside).unwrap(), bytes);

    // Corpus names describe the finding: lowercase words and hyphens only.
    for name in ["", "Stale", "../escape", "a b", "trailing-"] {
        assert!(harness::capture(&original, name, &corpus, stale_truncate).is_err());
    }
}
