#![cfg(feature = "plan-file")]

use fs_ntfs::block_io::BlockIo;
use fs_ntfs::repair_plan::{check_plan_source, PlanSourceError, PlanUnit, PlannedWrite, SavedPlan};

struct ReadOnlySource {
    bytes: Vec<u8>,
    reads: Vec<(u64, usize)>,
    fail_at: Option<u64>,
}

impl BlockIo for ReadOnlySource {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), String> {
        self.reads.push((offset, buffer.len()));
        if self.fail_at == Some(offset) {
            return Err(format!("injected read failure at {offset}"));
        }
        let start = usize::try_from(offset).map_err(|error| error.to_string())?;
        let end = start.checked_add(buffer.len()).ok_or("read overflow")?;
        buffer.copy_from_slice(self.bytes.get(start..end).ok_or("read past end")?);
        Ok(())
    }

    fn write_all_at(&mut self, _offset: u64, _buffer: &[u8]) -> Result<(), String> {
        panic!("checking a plan must never attempt a write");
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn sync(&mut self) -> Result<(), String> {
        panic!("checking a plan must never attempt a flush");
    }
}

fn source_and_plan() -> (ReadOnlySource, SavedPlan) {
    let serial = 0x1234_5678_9abc_def0_u64;
    let mut bytes = vec![0; 1024];
    bytes[0x48..0x50].copy_from_slice(&serial.to_le_bytes());
    let source = ReadOnlySource {
        bytes,
        reads: Vec::new(),
        fail_at: None,
    };
    let plan = SavedPlan {
        source_size: source.size(),
        volume_serial: serial,
        clear_dirty_from: None,
        units: vec![
            PlanUnit {
                writes: vec![
                    PlannedWrite {
                        offset: 128,
                        before: vec![0; 4],
                        after: vec![1, 2, 3, 4],
                    },
                    PlannedWrite {
                        offset: 130,
                        before: vec![3, 4, 0, 0],
                        after: vec![5, 6, 7, 8],
                    },
                ],
            },
            PlanUnit {
                writes: vec![PlannedWrite {
                    offset: 129,
                    before: vec![2, 5, 6, 7, 8, 0],
                    after: vec![9; 6],
                }],
            },
        ],
    };
    (source, plan)
}

#[test]
fn overlapping_writes_observe_latest_after_images_within_and_across_units() {
    let (mut source, plan) = source_and_plan();
    let before = source.bytes.clone();

    assert_eq!(check_plan_source(&plan, &mut source), Ok(()));
    assert_eq!(source.bytes, before);
    assert_eq!(source.reads, [(0x48, 8), (128, 4), (130, 4), (129, 6)]);
}

#[test]
fn size_mismatch_is_refused_before_any_reads() {
    for size in [512, 1536] {
        let (mut source, plan) = source_and_plan();
        source.bytes.resize(size, 0);
        source.fail_at = Some(0x48);

        assert_eq!(
            check_plan_source(&plan, &mut source),
            Err(PlanSourceError::Size {
                source: size as u64,
                plan: plan.source_size,
            })
        );
        assert!(source.reads.is_empty());
    }
}

#[test]
fn serial_mismatch_is_refused_before_any_before_image_reads() {
    let (mut source, plan) = source_and_plan();
    source.bytes[0x48] ^= 0xff;
    source.bytes[128] ^= 0xff;
    let serial = u64::from_le_bytes(source.bytes[0x48..0x50].try_into().unwrap());

    assert_eq!(
        check_plan_source(&plan, &mut source),
        Err(PlanSourceError::Serial {
            source: serial,
            plan: plan.volume_serial,
        })
    );
    assert_eq!(source.reads, [(0x48, 8)]);
}

#[test]
fn first_before_image_mismatch_names_its_first_byte_and_stops_reading() {
    let (mut source, plan) = source_and_plan();
    source.bytes[130] = 1;
    source.bytes[131] = 1;
    source.bytes[134] = 1;

    assert_eq!(
        check_plan_source(&plan, &mut source),
        Err(PlanSourceError::BeforeImage {
            unit: 0,
            write: 0,
            offset: 130,
        })
    );
    assert_eq!(source.reads, [(0x48, 8), (128, 4)]);
}

#[test]
fn a_later_mismatch_within_a_unit_names_its_write_and_byte() {
    let (mut source, plan) = source_and_plan();
    source.bytes[132] = 1;

    assert_eq!(
        check_plan_source(&plan, &mut source),
        Err(PlanSourceError::BeforeImage {
            unit: 0,
            write: 1,
            offset: 132,
        })
    );
    assert_eq!(source.reads, [(0x48, 8), (128, 4), (130, 4)]);
}

#[test]
fn a_later_unit_mismatch_names_its_unit_write_and_byte() {
    let (mut source, plan) = source_and_plan();
    source.bytes[134] = 1;

    assert_eq!(
        check_plan_source(&plan, &mut source),
        Err(PlanSourceError::BeforeImage {
            unit: 1,
            write: 0,
            offset: 134,
        })
    );
    assert_eq!(source.reads, [(0x48, 8), (128, 4), (130, 4), (129, 6)]);
}

#[test]
fn serial_read_failure_is_refused_without_writes() {
    let (mut source, plan) = source_and_plan();
    source.fail_at = Some(0x48);

    assert_eq!(
        check_plan_source(&plan, &mut source),
        Err(PlanSourceError::Read("injected read failure at 72".into()))
    );
    assert_eq!(source.reads, [(0x48, 8)]);
}

#[test]
fn before_image_read_failure_is_refused_without_writes() {
    let (mut source, plan) = source_and_plan();
    source.fail_at = Some(130);

    assert_eq!(
        check_plan_source(&plan, &mut source),
        Err(PlanSourceError::Read("injected read failure at 130".into()))
    );
    assert_eq!(source.reads, [(0x48, 8), (128, 4), (130, 4)]);
}
