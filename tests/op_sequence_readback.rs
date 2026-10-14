//! First deterministic slice of #484: create, offset writes, truncate,
//! and a refused duplicate create. Every successful operation must execute;
//! unsupported operations fail rather than disappearing from the workload.
mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::{fsck, mkfs, read, write};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const STEPS: usize = 120;
const SIZE: u64 = 64 * 1024 * 1024;

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
    for (path, expected) in model {
        let output = std::process::Command::new("ntfscat")
            .args([image, path])
            .output()
            .expect("ntfscat missing: install ntfs-3g for test:op-sequence-oracle");
        assert!(
            output.status.success(),
            "ntfscat {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(&output.stdout, expected, "ntfs-3g contents of {path}");
    }
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
