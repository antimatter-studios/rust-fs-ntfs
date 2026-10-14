//! `rust-ntfs layout` — print how a file's unnamed `$DATA` is stored.
//!
//! The attribute header's flags, compression unit and four sizes, then the
//! run list as ranges of VCNs that are data or hole, then a digest of the
//! value. Cluster numbers are left out, and adjacent runs of one kind are
//! printed as one range, so two volumes that store the same file the same
//! way print the same text wherever their clusters happen to be. That is
//! what lets a file Windows wrote be compared with one this crate wrote.

use fs_ntfs::attr_io::{self, attr_flags, attr_off, AttrType};
use fs_ntfs::block_io::PathIo;
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rust-ntfs layout <image> <path>

Prints how the unnamed $DATA stream of `<path>` is stored: its flags,
compression unit, sizes, its runs as VCN ranges of data and holes, and a
digest of its value. Cluster numbers are not printed.
";

/// `$STANDARD_INFORMATION`'s file-attributes word, from the value's start.
const SI_FILE_ATTRIBUTES: usize = 0x20;
/// `FILE_ATTRIBUTE_SPARSE_FILE`.
const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
/// The non-resident header's compression-unit exponent.
const NONRES_COMPRESSION_UNIT: usize = 0x22;
/// The extended header's `total_allocated` (compressed or sparse only).
const NONRES_TOTAL_ALLOCATED: usize = 0x40;

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("layout", run_inner(args))
}

fn run_inner(args: Vec<String>) -> Result<(), crate::cli::CliError> {
    if crate::cli::asked_for_help(&args, USAGE) {
        return Ok(());
    }
    let args = crate::cli::positionals(&args, 2, USAGE)?;
    let (image, path) = (&args[0], &args[1]);
    let mut io = PathIo::open_ro(Path::new(image)).map_err(|e| format!("open {image}: {e}"))?;
    let rec = fs_ntfs::read::resolve_path(&mut io, path).map_err(|e| format!("{path}: {e}"))?;
    let (_, record) = fs_ntfs::mft_io::read_mft_record_io(&mut io, rec)
        .map_err(|e| format!("{path}: record {rec}: {e}"))?;
    let value = fs_ntfs::read::read_attribute_value(&mut io, rec, AttrType::Data, None)
        .map_err(|e| format!("{path}: read $DATA: {e}"))?;
    let text = describe(&record, &value).map_err(|e| format!("{path}: {e}"))?;
    print!("{text}");
    Ok(())
}

/// The layout of the record's unnamed `$DATA`, one `name=value` per line.
fn describe(record: &[u8], value: &[u8]) -> Result<String, String> {
    let loc = attr_io::find_attribute(record, AttrType::Data, None)
        .ok_or("no unnamed $DATA in the file's base record")?;
    let u16_at = |off: usize| -> Result<u16, String> {
        record
            .get(loc.attr_offset + off..loc.attr_offset + off + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or_else(|| format!("$DATA header is cut short at +{off:#x}"))
    };
    let u64_at = |off: usize| -> Result<u64, String> {
        record
            .get(loc.attr_offset + off..loc.attr_offset + off + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
            .ok_or_else(|| format!("$DATA header is cut short at +{off:#x}"))
    };
    let flags = u16_at(attr_off::FLAGS)?;
    let mut out = String::new();
    if loc.is_resident {
        let _ = writeln!(out, "resident=yes");
        let _ = writeln!(out, "flags={flags:#06x}");
        let _ = writeln!(out, "data_size={}", loc.resident_value_length.unwrap_or(0));
    } else {
        let mpo = u16_at(attr_off::NONRES_MAPPING_PAIRS_OFFSET)? as usize;
        let _ = writeln!(out, "resident=no");
        let _ = writeln!(out, "flags={flags:#06x}");
        let _ = writeln!(out, "compression_unit={}", u16_at(NONRES_COMPRESSION_UNIT)?);
        let _ = writeln!(out, "mapping_pairs_offset={mpo:#x}");
        let _ = writeln!(out, "data_size={}", u64_at(attr_off::NONRES_DATA_LENGTH)?);
        let _ = writeln!(
            out,
            "allocated_size={}",
            u64_at(attr_off::NONRES_ALLOCATED_LENGTH)?
        );
        let _ = writeln!(
            out,
            "initialized_size={}",
            u64_at(attr_off::NONRES_INITIALIZED_LENGTH)?
        );
        if flags & (attr_flags::COMPRESSION_MASK | attr_flags::SPARSE) != 0 {
            let _ = writeln!(out, "total_allocated={}", u64_at(NONRES_TOTAL_ALLOCATED)?);
        }
        let _ = writeln!(
            out,
            "highest_vcn={}",
            u64_at(attr_off::NONRES_LAST_VCN)? as i64
        );
        let pairs = record
            .get(loc.attr_offset + mpo..loc.attr_offset + loc.attr_length)
            .ok_or("$DATA mapping pairs run past the record")?;
        let runs = fs_ntfs::data_runs::decode_runs(pairs).map_err(|e| e.to_string())?;
        for (kind, start, end) in ranges(&runs) {
            let _ = writeln!(out, "{kind} {start}..{end}");
        }
    }
    let _ = writeln!(out, "si_sparse={}", yes_no(si_sparse(record)?));
    let _ = writeln!(out, "value_fnv1a64={:#018x}", fnv1a64(value));
    Ok(out)
}

/// The run list as `(kind, first VCN, end VCN)`, adjacent runs of one
/// kind merged: where two data runs meet is a matter of which clusters
/// were free, not of how the file is stored.
fn ranges(runs: &[fs_ntfs::data_runs::DataRun]) -> Vec<(&'static str, u64, u64)> {
    let mut out: Vec<(&'static str, u64, u64)> = Vec::new();
    for run in runs {
        let kind = if run.lcn.is_some() { "data" } else { "hole" };
        let end = run.starting_vcn + run.length;
        match out.last_mut() {
            Some(last) if last.0 == kind && last.2 == run.starting_vcn => last.2 = end,
            _ => out.push((kind, run.starting_vcn, end)),
        }
    }
    out
}

/// Whether `$STANDARD_INFORMATION` marks the file sparse.
fn si_sparse(record: &[u8]) -> Result<bool, String> {
    let si = attr_io::find_attribute(record, AttrType::StandardInformation, None)
        .ok_or("no $STANDARD_INFORMATION")?;
    let at = attr_io::resident_value_start(&si).ok_or("$STANDARD_INFORMATION is not resident")?
        + SI_FILE_ATTRIBUTES;
    let word = record
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or("$STANDARD_INFORMATION is cut short")?;
    Ok(word & FILE_ATTRIBUTE_SPARSE_FILE != 0)
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

/// FNV-1a, 64-bit: enough to tell two values apart in a comparison, with
/// no dependency for it.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_ntfs::data_runs::DataRun;

    fn run(starting_vcn: u64, length: u64, lcn: Option<u64>) -> DataRun {
        DataRun {
            starting_vcn,
            length,
            lcn,
        }
    }

    #[test]
    fn adjacent_runs_of_one_kind_are_one_range() {
        let runs = [
            run(0, 2, Some(100)),
            run(2, 14, Some(500)),
            run(16, 32, None),
            run(48, 16, Some(7)),
        ];
        assert_eq!(
            ranges(&runs),
            vec![("data", 0, 16), ("hole", 16, 48), ("data", 48, 64)]
        );
    }

    #[test]
    fn the_digest_is_fnv1a64() {
        // The published FNV-1a 64 test vectors.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
