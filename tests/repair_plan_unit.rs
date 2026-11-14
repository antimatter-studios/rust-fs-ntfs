#![cfg(feature = "plan-file")]

use fs_ntfs::block_io::BlockIo;
use fs_ntfs::repair_plan::{
    check_unit_on_device, PlanSourceError, PlanUnit, PlannedWrite, SavedPlan,
};

struct ReadOnlyDevice {
    bytes: Vec<u8>,
    reads: Vec<(u64, usize)>,
    fail_at: Option<u64>,
}

impl BlockIo for ReadOnlyDevice {
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
        panic!("checking a unit must never attempt a write");
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn sync(&mut self) -> Result<(), String> {
        panic!("checking a unit must never attempt a flush");
    }
}

fn device_and_plan() -> (ReadOnlyDevice, SavedPlan) {
    let device = ReadOnlyDevice {
        bytes: vec![0; 32],
        reads: Vec::new(),
        fail_at: None,
    };
    let plan = SavedPlan {
        clear_dirty_from: None,
        source_size: device.size(),
        volume_serial: 0,
        units: vec![
            PlanUnit {
                writes: vec![
                    PlannedWrite {
                        offset: 8,
                        before: vec![0; 4],
                        after: vec![1, 2, 3, 4],
                    },
                    PlannedWrite {
                        offset: 10,
                        before: vec![3, 4, 0, 0],
                        after: vec![5, 6, 7, 8],
                    },
                    PlannedWrite {
                        offset: 9,
                        before: vec![2, 5, 6, 7, 8, 0],
                        after: vec![9; 6],
                    },
                ],
            },
            PlanUnit {
                writes: vec![PlannedWrite {
                    offset: 9,
                    before: vec![9; 6],
                    after: vec![10; 6],
                }],
            },
            PlanUnit {
                writes: vec![PlannedWrite {
                    offset: 20,
                    before: vec![0],
                    after: vec![11],
                }],
            },
        ],
    };
    (device, plan)
}

fn apply_earlier_units(device: &mut ReadOnlyDevice, plan: &SavedPlan, unit: usize) {
    for earlier in &plan.units[..unit] {
        for write in &earlier.writes {
            let start = write.offset as usize;
            device.bytes[start..start + write.after.len()].copy_from_slice(&write.after);
        }
    }
}

#[test]
fn overlapping_writes_use_the_latest_after_image_only_within_the_checked_unit() {
    let (mut device, plan) = device_and_plan();
    let before = device.bytes.clone();

    assert_eq!(check_unit_on_device(&plan, 0, &mut device), Ok(()));
    assert_eq!(device.bytes, before);
    assert_eq!(device.reads, [(8, 4), (10, 4), (9, 6)]);
}

#[test]
fn a_later_unit_reads_the_device_after_earlier_units_have_landed() {
    let (mut device, plan) = device_and_plan();
    apply_earlier_units(&mut device, &plan, 1);
    let before = device.bytes.clone();

    assert_eq!(check_unit_on_device(&plan, 1, &mut device), Ok(()));
    assert_eq!(device.bytes, before);
    assert_eq!(device.reads, [(9, 6)]);
}

#[test]
fn earlier_units_are_not_overlaid_to_hide_a_changed_device() {
    for changed in [0, 4, 10] {
        let (mut device, plan) = device_and_plan();
        apply_earlier_units(&mut device, &plan, 1);
        device.bytes[12] = changed;
        let before = device.bytes.clone();

        assert_eq!(
            check_unit_on_device(&plan, 1, &mut device),
            Err(PlanSourceError::DeviceBeforeImage {
                unit: 1,
                write: 0,
                start: 9,
                end: 15,
            })
        );
        assert_eq!(device.bytes, before);
        assert_eq!(device.reads, [(9, 6)]);
    }
}

#[test]
fn first_middle_and_last_units_refuse_the_changed_range_without_writing() {
    for unit in 0..3 {
        let (mut device, plan) = device_and_plan();
        apply_earlier_units(&mut device, &plan, unit);
        let write = &plan.units[unit].writes[0];
        device.bytes[write.offset as usize] ^= 0xff;
        let before = device.bytes.clone();
        let end = write.offset + write.before.len() as u64;
        let error = check_unit_on_device(&plan, unit, &mut device).unwrap_err();

        assert_eq!(
            error,
            PlanSourceError::DeviceBeforeImage {
                unit,
                write: 0,
                start: write.offset,
                end,
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "plan unit {unit} write 0: the device does not hold its before-image in bytes {}..{end}",
                write.offset
            )
        );
        assert_eq!(device.bytes, before);
        assert_eq!(device.reads, [(write.offset, write.before.len())]);
    }
}

#[test]
fn a_later_write_mismatch_stops_before_reading_any_following_write() {
    let (mut device, plan) = device_and_plan();
    device.bytes[12] = 99;
    device.fail_at = Some(9);
    let before = device.bytes.clone();

    assert_eq!(
        check_unit_on_device(&plan, 0, &mut device),
        Err(PlanSourceError::DeviceBeforeImage {
            unit: 0,
            write: 1,
            start: 10,
            end: 14,
        })
    );
    assert_eq!(device.bytes, before);
    assert_eq!(device.reads, [(8, 4), (10, 4)]);
}

#[test]
fn read_failures_stop_the_unit_without_writing_or_reading_later_writes() {
    let (mut device, plan) = device_and_plan();
    device.fail_at = Some(10);
    let before = device.bytes.clone();

    assert_eq!(
        check_unit_on_device(&plan, 0, &mut device),
        Err(PlanSourceError::Read("injected read failure at 10".into()))
    );
    assert_eq!(device.bytes, before);
    assert_eq!(device.reads, [(8, 4), (10, 4)]);
}

#[test]
fn an_out_of_range_unit_is_refused_without_reading_the_device() {
    for unit in [3, usize::MAX] {
        let (mut device, plan) = device_and_plan();
        let before = device.bytes.clone();
        let error = check_unit_on_device(&plan, unit, &mut device).unwrap_err();

        assert_eq!(error, PlanSourceError::NoSuchUnit { unit, units: 3 });
        assert_eq!(
            error.to_string(),
            format!("the plan has 3 units, no unit {unit}")
        );
        assert_eq!(device.bytes, before);
        assert!(device.reads.is_empty());
    }
}
