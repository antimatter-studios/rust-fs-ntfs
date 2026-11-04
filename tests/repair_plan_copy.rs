#![cfg(feature = "plan-file")]
//! What applying a repair plan leaves when it stops, and a copy-repair of a
//! source that cannot be written. No fixture: the plans and sources are
//! built here, so each failure lands on a chosen write or flush.

mod common;

use fs_ntfs::block_io::BlockIo;
use fs_ntfs::repair_plan::{apply_plan_units, copy_repair, PlanUnit, PlannedWrite, SavedPlan};
use std::path::Path;

const SIZE: usize = 4096;
const SERIAL: u64 = 0x1122_3344_5566_7788;
const SERIAL_OFFSET: usize = 0x48;

fn write(offset: u64, fill: u8) -> PlannedWrite {
    PlannedWrite {
        offset,
        before: vec![0; 4],
        after: vec![fill; 4],
    }
}

/// Three units: two writes, two writes, one write, all over zeroed bytes.
fn plan() -> SavedPlan {
    SavedPlan {
        source_size: SIZE as u64,
        volume_serial: SERIAL,
        units: vec![
            PlanUnit {
                writes: vec![write(512, 1), write(600, 2)],
            },
            PlanUnit {
                writes: vec![write(1024, 3), write(1100, 4)],
            },
            PlanUnit {
                writes: vec![write(2048, 5)],
            },
        ],
    }
}

fn source_bytes() -> Vec<u8> {
    let mut bytes = vec![0; SIZE];
    bytes[SERIAL_OFFSET..SERIAL_OFFSET + 8].copy_from_slice(&SERIAL.to_le_bytes());
    bytes
}

/// `bytes` with each of `writes` made.
fn holding(mut bytes: Vec<u8>, writes: &[&PlannedWrite]) -> Vec<u8> {
    for write in writes {
        let at = write.offset as usize;
        bytes[at..at + write.after.len()].copy_from_slice(&write.after);
    }
    bytes
}

/// A target that records each write's offset, and each flush as `None`, in
/// the order they reach it, and fails one write or one flush.
struct StoppingTarget {
    bytes: Vec<u8>,
    events: Vec<Option<u64>>,
    fail_write_at: Option<u64>,
    fail_flush: Option<usize>,
    flushes: usize,
}

impl StoppingTarget {
    fn new() -> Self {
        Self {
            bytes: source_bytes(),
            events: Vec::new(),
            fail_write_at: None,
            fail_flush: None,
            flushes: 0,
        }
    }
}

impl BlockIo for StoppingTarget {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), String> {
        let at = offset as usize;
        buffer.copy_from_slice(&self.bytes[at..at + buffer.len()]);
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buffer: &[u8]) -> Result<(), String> {
        if self.fail_write_at == Some(offset) {
            return Err(format!("injected write failure at {offset}"));
        }
        let at = offset as usize;
        self.bytes[at..at + buffer.len()].copy_from_slice(buffer);
        self.events.push(Some(offset));
        Ok(())
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn sync(&mut self) -> Result<(), String> {
        let this = self.flushes;
        self.flushes += 1;
        if self.fail_flush == Some(this) {
            return Err(format!("injected failure of flush {this}"));
        }
        self.events.push(None);
        Ok(())
    }
}

#[test]
fn a_write_that_fails_inside_a_unit_stops_there_and_undoes_nothing() {
    let plan = plan();
    let mut target = StoppingTarget::new();
    target.fail_write_at = Some(1100);

    let refused = apply_plan_units(&plan, &mut target)
        .unwrap_err()
        .to_string();

    assert!(
        refused.contains("plan unit 1 write 1") && refused.contains("injected write failure"),
        "the error does not name the unit, write and cause: {refused}"
    );
    assert_eq!(
        target.events,
        [Some(512), Some(600), None, Some(1024)],
        "the apply went on past the failed write, or flushed the part of its unit"
    );
    let (first, second) = (&plan.units[0], &plan.units[1]);
    assert_eq!(
        target.bytes,
        holding(
            source_bytes(),
            &[&first.writes[0], &first.writes[1], &second.writes[0]]
        ),
        "the target holds other bytes than the first unit and the write before the failure"
    );
}

#[test]
fn a_flush_that_fails_stops_the_apply_before_the_next_unit() {
    let plan = plan();
    let mut target = StoppingTarget::new();
    target.fail_flush = Some(1);

    let refused = apply_plan_units(&plan, &mut target)
        .unwrap_err()
        .to_string();

    assert!(
        refused.contains("plan unit 1: flushing") && refused.contains("injected failure"),
        "the error does not name the unit and cause: {refused}"
    );
    assert_eq!(
        target.events,
        [Some(512), Some(600), None, Some(1024), Some(1100)],
        "a unit was written after the flush before it failed"
    );
}

/// A directory of its own for one test, so the test can see every name a
/// copy-repair leaves in it.
#[cfg(unix)]
fn new_directory(stem: &str) -> std::path::PathBuf {
    let directory = std::path::PathBuf::from(common::temp_image_path(stem));
    std::fs::create_dir(&directory).unwrap();
    directory
}

/// The names in `directory`, sorted.
#[cfg(all(unix, feature = "cli"))]
fn names_in(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// `plan` as a plan file, built from the layout in docs/repair-plan-file.md.
#[cfg(all(unix, feature = "cli"))]
fn plan_file_bytes(plan: &SavedPlan) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut units = Vec::new();
    for unit in &plan.units {
        units.extend_from_slice(&(unit.writes.len() as u32).to_le_bytes());
        let mut before = Sha256::new();
        for write in &unit.writes {
            before.update(&write.before);
        }
        units.extend_from_slice(&before.finalize());
        for write in &unit.writes {
            units.extend_from_slice(&write.offset.to_le_bytes());
            units.extend_from_slice(&(write.before.len() as u32).to_le_bytes());
            units.extend_from_slice(&write.before);
            units.extend_from_slice(&write.after);
        }
    }
    let mut file = b"NTFSPLAN".to_vec();
    file.extend_from_slice(&1u32.to_le_bytes());
    file.extend_from_slice(&(plan.units.len() as u32).to_le_bytes());
    file.extend_from_slice(&((40 + units.len() + 32) as u64).to_le_bytes());
    file.extend_from_slice(&plan.source_size.to_le_bytes());
    file.extend_from_slice(&plan.volume_serial.to_le_bytes());
    file.extend_from_slice(&units);
    let sum = Sha256::digest(&file);
    file.extend_from_slice(&sum);
    file
}

#[cfg(all(unix, feature = "cli"))]
#[test]
fn a_copy_repair_that_fails_while_copying_leaves_no_target_and_a_retry_succeeds() {
    // The failure is the kernel's own: fsck.ntfs runs under a file-size limit
    // smaller than the source, so a write of the copy fails with EFBIG, and
    // the limit binds only that process.
    use std::os::unix::process::CommandExt;
    const LIMIT: libc::rlim_t = 1 << 20;

    let directory = new_directory("copy-repair-failed-copy");
    let (source, plan_file, target) = (
        directory.join("source"),
        directory.join("plan"),
        directory.join("target"),
    );
    let mut pristine = vec![0u8; 3 << 20];
    pristine[SERIAL_OFFSET..SERIAL_OFFSET + 8].copy_from_slice(&SERIAL.to_le_bytes());
    std::fs::write(&source, &pristine).unwrap();
    let plan = SavedPlan {
        source_size: pristine.len() as u64,
        volume_serial: SERIAL,
        units: vec![PlanUnit {
            writes: vec![write(512, 1)],
        }],
    };
    std::fs::write(&plan_file, plan_file_bytes(&plan)).unwrap();

    let mut limited = std::process::Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"));
    limited
        .arg("fsck")
        .arg("--apply")
        .arg(&plan_file)
        .arg("--to")
        .arg(&target)
        .arg(&source);
    // SAFETY: both calls are async-signal-safe, and the limit is a local.
    unsafe {
        limited.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: LIMIT,
                rlim_max: LIMIT,
            };
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let failed = limited.output().expect("spawn fsck.ntfs");
    let said = String::from_utf8_lossy(&failed.stderr);
    assert!(
        !failed.status.success() && said.contains("File too large"),
        "fsck.ntfs did not fail on the file-size limit: {:?}: {said}",
        failed.status
    );

    assert!(
        target.symlink_metadata().is_err(),
        "a copy that failed left {} bytes at the target path",
        std::fs::metadata(&target).unwrap().len()
    );
    assert_eq!(
        names_in(&directory),
        ["plan", "source"],
        "a copy that failed left another name beside the target"
    );
    assert!(
        std::fs::read(&source).unwrap() == pristine,
        "a copy that failed changed its source"
    );

    copy_repair(&plan, &source, &target)
        .unwrap_or_else(|e| panic!("the retry of a copy that failed: {e}"));
    assert!(
        std::fs::read(&target).unwrap() == holding(pristine.clone(), &[&plan.units[0].writes[0]]),
        "the retry's target holds other bytes than the source with the unit applied"
    );
    assert!(
        std::fs::read(&source).unwrap() == pristine,
        "the retry changed its source"
    );
    assert_eq!(
        names_in(&directory),
        ["plan", "source", "target"],
        "a copy-repair left another name beside its target"
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

#[cfg(unix)]
#[test]
fn a_copy_repair_target_is_private_whatever_its_source_mode() {
    use std::os::unix::fs::PermissionsExt;

    let directory = new_directory("copy-repair-mode");
    let source = directory.join("source");
    std::fs::write(&source, source_bytes()).unwrap();
    // The umask is the process's. This test only asks for 022, the usual
    // one, and puts back whatever it found before asserting anything.
    // SAFETY: umask takes and returns a plain value and cannot fail.
    let umask = unsafe { libc::umask(0o022) };
    let mut modes = Vec::new();
    for source_mode in [0o600, 0o644] {
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(source_mode)).unwrap();
        let target = directory.join(format!("target-{source_mode:o}"));
        let repaired = copy_repair(&plan(), &source, &target);
        modes.push((
            source_mode,
            repaired.map(|()| std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777),
        ));
    }
    // SAFETY: as above.
    unsafe { libc::umask(umask) };

    for (source_mode, target_mode) in modes {
        let target_mode = target_mode
            .unwrap_or_else(|e| panic!("copy-repair of a source with mode {source_mode:o}: {e}"));
        assert_eq!(
            target_mode, 0o600,
            "a source with mode {source_mode:o} under umask 022 made a target with mode {target_mode:o}"
        );
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_copy_repair_works_from_a_source_that_refuses_writes_and_leaves_it_byte_for_byte() {
    // A source opened for writing is refused here, whatever is then written
    // to it, and the bytes are compared too for a test run with the rights
    // to open it anyway.
    let plan = plan();
    let source = common::temp_image_path("copy-repair-read-only-source");
    let target = common::temp_image_path("copy-repair-read-only-target");
    let pristine = source_bytes();
    std::fs::write(&source, &pristine).unwrap();
    set_read_only(&source, true);

    let repaired = copy_repair(&plan, Path::new(&source), Path::new(&target));

    let after = std::fs::read(&source).unwrap();
    set_read_only(&source, false);
    repaired.unwrap_or_else(|e| panic!("copy-repair of a read-only source: {e}"));
    assert!(after == pristine, "copy-repair changed its source");
    let every_write: Vec<&PlannedWrite> = plan.units.iter().flat_map(|u| &u.writes).collect();
    assert!(
        std::fs::read(&target).unwrap() == holding(pristine, &every_write),
        "the target holds other bytes than the source with every unit applied"
    );
}

/// Make `path` read-only, or writable again, on every platform the suite
/// runs on. Windows refuses to remove a read-only file, so a test undoes it.
fn set_read_only(path: &str, read_only: bool) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(read_only);
    std::fs::set_permissions(path, permissions).unwrap();
}
