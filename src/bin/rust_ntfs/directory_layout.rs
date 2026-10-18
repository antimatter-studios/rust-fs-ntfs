//! Matrix preconditions: the directory really has a list and the requested index shape.
use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::PathIo;
use fs_ntfs::{idx_block, mft_io, read};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "Usage: rust-ntfs directory-layout <image> <path> resident|spilled\n";

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("directory-layout", run_inner(args))
}

fn run_inner(args: Vec<String>) -> Result<(), crate::cli::CliError> {
    if crate::cli::asked_for_help(&args, USAGE) {
        return Ok(());
    }
    let args = crate::cli::positionals(&args, 3, USAGE)?;
    let spilled = match args[2].as_str() {
        "resident" => false,
        "spilled" => true,
        _ => return Err(crate::cli::CliError::Usage(USAGE.into())),
    };
    inspect(&args[0], &args[1], spilled).map_err(Into::into)
}

fn inspect(image: &str, path: &str, spilled: bool) -> Result<(), String> {
    let mut io = PathIo::open_ro(Path::new(image)).map_err(|e| e.to_string())?;
    let rec = read::resolve_path(&mut io, path).map_err(|e| e.to_string())?;
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).map_err(|e| e.to_string())?;
    if mft_io::record_flags(&record) & mft_io::MFT_FLAG_DIRECTORY == 0 {
        return Err(format!("{path}: not a directory"));
    }
    let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
        .map_err(|e| format!("{path}: missing $ATTRIBUTE_LIST: {e}"))?;
    let entries = read::parse_attribute_list(&list).map_err(|e| e.to_string())?;
    if !entries.iter().any(|e| e.record_number != rec) {
        return Err(format!("{path}: no extension records"));
    }
    // Check the actual attributes too, so a stale entry cannot prove a shape.
    for entry in &entries {
        let (_, held) =
            mft_io::read_mft_record_io(&mut io, entry.record_number).map_err(|e| e.to_string())?;
        if !attr_io::iter_attributes(&held).any(|loc| {
            loc.type_code == entry.type_code
                && loc.attribute_id == entry.attribute_id
                && attr_io::decode_attr_name(&held, &loc) == entry.name.as_deref().unwrap_or("")
        }) {
            return Err(format!("{path}: list names a missing attribute: {entry:?}"));
        }
    }
    for (ty, expected) in [
        (AttrType::IndexRoot, 1),
        (AttrType::IndexAllocation, usize::from(spilled)),
        (AttrType::Bitmap, usize::from(spilled)),
    ] {
        let count = entries.iter().filter(|e| e.type_code == ty as u32).count();
        if count != expected {
            return Err(format!(
                "{path}: {ty:?} has {count} entries, expected {expected}"
            ));
        }
    }
    let root = read::read_attribute_value(&mut io, rec, AttrType::IndexRoot, Some("$I30"))
        .map_err(|e| e.to_string())?;
    if root.get(28).map(|b| b & 1 != 0) != Some(spilled) {
        return Err(format!(
            "{path}: $INDEX_ROOT child flag disagrees with requested shape"
        ));
    }
    if spilled {
        let index = idx_block::load_for_directory_io(&mut io, rec).map_err(|e| e.to_string())?;
        if index.allocated_block_vcns().len() < 2 {
            return Err(format!(
                "{path}: index never split into multiple INDX blocks"
            ));
        }
    }
    println!("{path}: listed directory, spilled={spilled}");
    Ok(())
}
