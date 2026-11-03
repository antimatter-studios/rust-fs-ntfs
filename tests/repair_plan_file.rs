//! The repair plan file, checked against its documented layout with no
//! fixture (#905). `tests/windows_interrupted_fixture.rs` saves real plans
//! and damages them, which the whole-file checksum catches first. This file
//! builds the bytes from docs/repair-plan-file.md by hand, so that the writer
//! is compared with the document, and so that a file whose checksum holds
//! reaches the structural refusals behind it.
#![cfg(feature = "plan-file")]

use fs_ntfs::block_io::BlockIo;
use fs_ntfs::fsck::FsckOptions;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::repair_plan::{
    load_plan, plan_fsck_io, save_plan, PlanUnit, PlannedWrite, RepairPlan,
};
use sha2::{Digest, Sha256};

const VOLUME_SIZE: u64 = 64 * 1024 * 1024;
const SERIAL: u64 = 0x1122_3344_5566_7788;

/// Where each field of a two-unit file sits, from the documented layout.
const UNIT_COUNT_AT: usize = 12;
const LENGTH_AT: usize = 16;
const FIRST_UNIT_AT: usize = 40;
const HEADER_AND_TRAILER: usize = 40 + 32;

struct MemDev(Vec<u8>);

impl BlockIo for MemDev {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let at = offset as usize;
        let there = self.0.get(at..at + buf.len()).ok_or("read past the end")?;
        buf.copy_from_slice(there);
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let at = offset as usize;
        let there = self
            .0
            .get_mut(at..at + buf.len())
            .ok_or("write past the end")?;
        there.copy_from_slice(buf);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.0.len() as u64
    }
}

fn write(offset: u64, before: &[u8], after: &[u8]) -> PlannedWrite {
    PlannedWrite {
        offset,
        before: before.to_vec(),
        after: after.to_vec(),
    }
}

/// Two units, the first of two writes, with images of different lengths.
fn two_units() -> Vec<PlanUnit> {
    vec![
        PlanUnit {
            writes: vec![write(4096, b"abcd", b"ABCD"), write(8192, b"ef", b"EF")],
        },
        PlanUnit {
            writes: vec![write(12288, b"ghijkl", b"GHIJKL")],
        },
    ]
}

/// A unit's bytes as the document lays them out.
fn unit_bytes(unit: &PlanUnit) -> Vec<u8> {
    let mut hash = Sha256::new();
    for w in &unit.writes {
        hash.update(&w.before);
    }
    let mut out = (unit.writes.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&hash.finalize());
    for w in &unit.writes {
        out.extend_from_slice(&w.offset.to_le_bytes());
        out.extend_from_slice(&(w.before.len() as u32).to_le_bytes());
        out.extend_from_slice(&w.before);
        out.extend_from_slice(&w.after);
    }
    out
}

/// Sets the file-length field and appends the trailer, so that whatever
/// `body` holds reaches the reader with its length and checksum right.
fn sealed(mut body: Vec<u8>) -> Vec<u8> {
    let length = (body.len() + 32) as u64;
    body[LENGTH_AT..LENGTH_AT + 8].copy_from_slice(&length.to_le_bytes());
    let sum = Sha256::digest(&body);
    body.extend_from_slice(&sum);
    body
}

/// The unsealed bytes of a version 1 file holding `units`.
fn body_of(units: &[PlanUnit]) -> Vec<u8> {
    let mut out = b"NTFSPLAN".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&VOLUME_SIZE.to_le_bytes());
    out.extend_from_slice(&SERIAL.to_le_bytes());
    for unit in units {
        out.extend_from_slice(&unit_bytes(unit));
    }
    out
}

/// A freshly formatted volume and a plan made from it, whose units a test
/// replaces: a plan holds a report only a repair can make.
fn volume_and_plan() -> (MemDev, RepairPlan) {
    let mut dev = MemDev(vec![0u8; VOLUME_SIZE as usize]);
    format_filesystem(&mut dev, VOLUME_SIZE, 4096, 4096, None, Some(SERIAL)).expect("format");
    let plan = plan_fsck_io(&mut dev, FsckOptions::default()).expect("plan");
    (dev, plan)
}

fn refused_naming(what: &str, bytes: &[u8], names: &str) {
    match load_plan(bytes) {
        Ok(_) => panic!("{what}: loaded"),
        Err(e) => assert!(
            e.to_string().contains(names),
            "{what}: refused as {e}, not naming {names:?}"
        ),
    }
}

#[test]
fn save_plan_writes_the_documented_layout_and_load_plan_reads_it_back() {
    let (mut dev, mut plan) = volume_and_plan();
    let before = dev.0.clone();
    for units in [Vec::new(), two_units()] {
        plan.units = units.clone();
        let saved = save_plan(&plan, &mut dev).expect("save");
        assert!(
            saved == sealed(body_of(&units)),
            "{} units: save_plan differs from the documented layout",
            units.len()
        );
        let loaded = load_plan(&saved).expect("load");
        assert_eq!(loaded.source_size, VOLUME_SIZE);
        assert_eq!(loaded.volume_serial, SERIAL);
        assert_eq!(loaded.units, units);
    }
    assert_eq!(
        sealed(body_of(&[])).len(),
        HEADER_AND_TRAILER,
        "a plan of no units"
    );
    assert!(dev.0 == before, "saving a plan wrote to its source");
}

#[test]
fn save_plan_refuses_a_write_it_cannot_represent_and_a_source_with_no_boot_sector() {
    let (mut dev, mut plan) = volume_and_plan();
    plan.units = vec![PlanUnit {
        writes: vec![write(4096, b"abcd", b"AB")],
    }];
    let e = save_plan(&plan, &mut dev).expect_err("images of different lengths");
    assert!(e.contains("4 bytes before and 2 after"), "{e}");

    plan.units = Vec::new();
    let e = save_plan(&plan, &mut MemDev(vec![0u8; 0x4f])).expect_err("no serial number to read");
    assert!(e.contains("read past the end"), "{e}");
}

#[test]
fn every_prefix_of_a_plan_file_is_refused_as_truncated() {
    let file = sealed(body_of(&two_units()));
    for cut in 0..file.len() {
        refused_naming(&format!("cut at {cut}"), &file[..cut], "truncated");
    }
}

#[test]
fn every_changed_byte_is_refused_naming_what_is_wrong() {
    let file = sealed(body_of(&two_units()));
    for at in 0..file.len() {
        let mut changed = file.clone();
        changed[at] ^= 0x01;
        let names = match at {
            0..=7 => "not a repair plan file",
            8..=11 => "version",
            // The length field is checked before the checksum. The file's
            // length is even, so each of these says it is longer than it is.
            16..=23 => "truncated",
            _ => "checksum",
        };
        refused_naming(&format!("byte {at} changed"), &changed, names);
    }
}

#[test]
fn a_file_that_is_not_the_length_or_version_it_says_is_refused() {
    let file = sealed(body_of(&two_units()));

    refused_naming("empty", b"", "truncated");
    refused_naming("a short other file", b"MZ", "not a repair plan file");
    refused_naming("another file", &[0x55; 4096], "not a repair plan file");

    let mut longer = file.clone();
    longer.push(0);
    refused_naming("a byte past its length", &longer, "trailing bytes");

    for version in [0u32, 2, u32::MAX] {
        let mut other = body_of(&two_units());
        other[8..12].copy_from_slice(&version.to_le_bytes());
        refused_naming(
            &format!("version {version}"),
            &sealed(other),
            &format!("format version {version} is not supported"),
        );
    }

    // Each is refused from the field alone, before the checksum it would fail.
    for length in [0u64, HEADER_AND_TRAILER as u64 - 1] {
        let mut short = file.clone();
        short[LENGTH_AT..LENGTH_AT + 8].copy_from_slice(&length.to_le_bytes());
        refused_naming(&format!("length {length}"), &short, "malformed");
    }
    let mut huge = file.clone();
    huge[LENGTH_AT..LENGTH_AT + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    refused_naming("a length no file can have", &huge, "truncated");
}

#[test]
fn a_file_whose_checksum_holds_and_whose_structure_does_not_is_refused() {
    let units = two_units();
    let write_count_at = FIRST_UNIT_AT;
    let hash_at = write_count_at + 4;
    let first_length_at = hash_at + 32 + 8;
    let last_before_at = sealed(body_of(&units)).len() - 32 - 12;

    let edited = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut body = body_of(&units);
        edit(&mut body);
        sealed(body)
    };
    let set_u32 = |at: usize, value: u32| {
        edited(&move |b: &mut Vec<u8>| b[at..at + 4].copy_from_slice(&value.to_le_bytes()))
    };

    for (what, count) in [("one more", 3), ("every", u32::MAX)] {
        refused_naming(
            &format!("{what} unit than it holds"),
            &set_u32(UNIT_COUNT_AT, count),
            "runs past the end",
        );
    }
    for count in [0, 1] {
        refused_naming(
            &format!("{count} of its 2 units"),
            &set_u32(UNIT_COUNT_AT, count),
            "bytes between the last unit and the checksum",
        );
    }
    refused_naming(
        "a write count past the end",
        &set_u32(write_count_at, u32::MAX),
        "runs past the end",
    );
    refused_naming(
        "an image length past the end",
        &set_u32(first_length_at, u32::MAX),
        "runs past the end",
    );
    refused_naming(
        "unit 0's hash changed",
        &edited(&|b: &mut Vec<u8>| b[hash_at] ^= 0x01),
        "unit 0: before-image does not match its hash",
    );
    refused_naming(
        "unit 1's before-image changed",
        &edited(&|b: &mut Vec<u8>| b[last_before_at] ^= 0x01),
        "unit 1: before-image does not match its hash",
    );
}
