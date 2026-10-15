//! The MFT inventory's file references, graded by Windows (#609).
//!
//! The fixture is a volume Windows formatted and populated
//! (`test-disks/build-windows-native-read-fixtures.ps1`): it created eight
//! files, deleted them and created eight more, so the freed slots were
//! reused with bumped sequence numbers. Beside it,
//! `test-disks/ntfs-inventory.fileids.txt` holds the reference
//! `fsutil file queryfileid` reported for every surviving file. Each must
//! appear, bit for bit, in a bounded, resumed inventory of the image.

use fs_ntfs::block_io::PathIo;
use fs_ntfs::mft_inventory::{inventory_batch_io, InventoryCursor};
use std::collections::BTreeSet;
use std::path::Path;

const WINDOWS_IMG: &str = "test-disks/ntfs-inventory.img";
const WINDOWS_IDS: &str = "test-disks/ntfs-inventory.fileids.txt";

/// The low 64 bits of each `File ID is 0x...` line; NTFS leaves the high
/// half of the 128-bit ID zero.
fn windows_references() -> Vec<(String, u64)> {
    let text = std::fs::read_to_string(WINDOWS_IDS).expect("read the fsutil answers");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let (name, answer) = line.split_once('\t').expect("name<TAB>answer");
            let hex = answer
                .rsplit("0x")
                .next()
                .expect("a hexadecimal file ID")
                .trim();
            assert!(hex.len() <= 32, "file ID {hex} is longer than 128 bits");
            let id = u128::from_str_radix(hex, 16).expect("hexadecimal file ID");
            assert_eq!(id >> 64, 0, "{name}: NTFS file IDs have a zero high half");
            (name.to_string(), id as u64)
        })
        .collect()
}

#[test]
fn inventory_references_are_the_ones_windows_reports() {
    for path in [WINDOWS_IMG, WINDOWS_IDS] {
        assert!(
            Path::new(path).exists(),
            "missing {path}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
        );
    }
    let before = std::fs::read(WINDOWS_IMG).expect("read image");
    let mut io = PathIo::open_ro(Path::new(WINDOWS_IMG)).expect("open_ro");
    let mut cursor = InventoryCursor::START;
    let mut ours = BTreeSet::new();
    loop {
        let batch = inventory_batch_io(&mut io, cursor, 5).expect("batch");
        assert!(batch.slots_visited <= 5);
        ours.extend(batch.entries.iter().map(|e| e.file_reference));
        match batch.next {
            Some(next) => cursor = next,
            None => break,
        }
    }
    drop(io);
    let windows = windows_references();
    assert!(windows.len() >= 9, "eight reused files and a directory");
    for (name, reference) in &windows {
        assert!(
            ours.contains(reference),
            "{name}: Windows reports {reference:#018x}, which the inventory does not return"
        );
    }
    assert!(
        windows.iter().any(|(_, r)| r >> 48 > 1),
        "slot reuse left at least one sequence above one"
    );
    assert_eq!(before, std::fs::read(WINDOWS_IMG).expect("read image"));
}
