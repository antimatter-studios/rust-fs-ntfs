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
    can_sync: bool,
    fail_write_at: Option<u64>,
    fail_flush: Option<usize>,
    flushes: usize,
}

impl StoppingTarget {
    fn new() -> Self {
        Self {
            bytes: source_bytes(),
            events: Vec::new(),
            can_sync: true,
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

    fn can_sync(&self) -> bool {
        self.can_sync
    }
}

fn assert_unordered_target_is_refused(plan: &SavedPlan) {
    let mut target = StoppingTarget::new();
    target.can_sync = false;

    let result = apply_plan_units(plan, &mut target);

    assert!(
        target.events.is_empty(),
        "a target that cannot order writes was written or flushed: {:?}",
        target.events
    );
    assert_eq!(target.bytes, source_bytes(), "refusal changed the target");
    assert_eq!(target.flushes, 0, "refusal flushed the target");
    let refused = result
        .expect_err("a target that cannot order writes was accepted")
        .to_string();
    assert!(
        refused.contains("#518") && refused.contains("order"),
        "the refusal does not explain the ordering requirement and name #518: {refused}"
    );
}

#[test]
fn a_target_that_cannot_order_writes_is_refused_before_any_write_or_flush() {
    assert_unordered_target_is_refused(&plan());
}

#[test]
fn an_empty_plan_also_refuses_a_target_that_cannot_order_writes() {
    let mut empty = plan();
    empty.units.clear();
    assert_unordered_target_is_refused(&empty);
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
