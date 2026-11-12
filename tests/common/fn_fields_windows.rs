//! What Windows leaves in the two copies of a file's `$FILE_NAME` -- the one
//! in the file's own MFT record and the key of its entry in the parent
//! directory's index -- after each step of
//! `test-disks/capture-filename-fields.ps1` (issue #629).
//!
//! One table, read by both sides: `filename_fields_windows_oracle` grades
//! every Windows capture against it, and `field_exhaustion_fn` grades this
//! crate's writer against the same rows. An expectation can therefore only
//! change if Windows agrees with it.
//!
//! Observed on Windows Server 2025 build 26100 (security descriptor oracle
//! run 40731834415, `filename-fields-capture`, head 9e72bf16): the record
//! copy is lazy and is refreshed only by a rename; the index copy is eager.

/// The three duplicated fields of one `$FILE_NAME` copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fields {
    pub data_size: u64,
    pub allocated_size: u64,
    pub attributes: u32,
}

/// Both copies of one file's `$FILE_NAME` after one capture step.
#[derive(Debug)]
pub struct Observation {
    pub operation: &'static str,
    pub path: &'static str,
    pub record: Fields,
    pub index: Fields,
}

const fn fields(data_size: u64, allocated_size: u64, attributes: u32) -> Fields {
    Fields {
        data_size,
        allocated_size,
        attributes,
    }
}

const fn row(
    operation: &'static str,
    path: &'static str,
    record: Fields,
    index: Fields,
) -> Observation {
    Observation {
        operation,
        path,
        record,
        index,
    }
}

const EMPTY: Fields = fields(0, 0, 0x20);
const EMPTY_RO: Fields = fields(0, 0, 0x21);
const DATA: Fields = fields(200, 200, 0x20);
const BIG: Fields = fields(6000, 8192, 0x20);
const CTL: Fields = fields(2, 8, 0x20);

/// Every file of every step, as Windows wrote it.
pub const WINDOWS: [Observation; 25] = [
    row("create", "big.bin", EMPTY, EMPTY),
    row("create", "ctl.txt", EMPTY, EMPTY),
    row("create", "data.bin", EMPTY, EMPTY),
    row("create", "ro.txt", EMPTY, EMPTY),
    row("write-close", "big.bin", EMPTY, BIG),
    row("write-close", "ctl.txt", EMPTY, CTL),
    row("write-close", "data.bin", EMPTY, DATA),
    row("write-close", "ro.txt", EMPTY, EMPTY),
    row("reopen", "big.bin", EMPTY, BIG),
    row("reopen", "ctl.txt", EMPTY, CTL),
    row("reopen", "data.bin", EMPTY, DATA),
    row("reopen", "ro.txt", EMPTY, EMPTY),
    row("readonly", "big.bin", EMPTY, BIG),
    row("readonly", "ctl.txt", EMPTY, CTL),
    row("readonly", "data.bin", EMPTY, DATA),
    row("readonly", "ro.txt", EMPTY, EMPTY_RO),
    row("rename", "big.bin", EMPTY, BIG),
    row("rename", "ctl.txt", EMPTY, CTL),
    row("rename", "data2.bin", DATA, DATA),
    row("rename", "ro2.txt", EMPTY_RO, EMPTY_RO),
    row("create-refresh", "big.bin", EMPTY, BIG),
    row("create-refresh", "ctl.txt", EMPTY, CTL),
    row("create-refresh", "data2.bin", DATA, DATA),
    row("create-refresh", "ro2.txt", EMPTY_RO, EMPTY_RO),
    row("create-refresh", "sibling.txt", EMPTY, EMPTY),
];

/// The observation for `path` after `operation`; a pair Windows was never
/// observed at is refused rather than defaulted.
pub fn observed(operation: &str, path: &str) -> &'static Observation {
    WINDOWS
        .iter()
        .find(|o| o.operation == operation && o.path == path)
        .unwrap_or_else(|| panic!("no Windows observation of {path} after {operation}"))
}
