//! Prints how many extents of a file's unnamed `$DATA` its
//! `$ATTRIBUTE_LIST` names, and the MFT record each is in, so that a
//! Windows scenario can show the run list it ships really was split
//! across records before Windows grades it (#801). One line per extent,
//! `<starting VCN> <record>`, in the list's order; a file with no list
//! prints nothing and exits 1.
//!
//! Windows' `chkdsk` and `Get-FileHash` are the oracle; this only proves
//! the scenario built the shape it is meant to grade.
//!
//! Usage: `cargo run --release --example listed_data_extents -- IMAGE PATH`

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::PathIo;
use fs_ntfs::read;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [image, file] = args.as_slice() else {
        eprintln!("usage: listed_data_extents IMAGE PATH");
        return ExitCode::from(2);
    };
    let mut io = match PathIo::open_ro(Path::new(image)) {
        Ok(io) => io,
        Err(e) => {
            eprintln!("open {image}: {e}");
            return ExitCode::from(2);
        }
    };
    let rec = match read::resolve_path(&mut io, file) {
        Ok(rec) => rec,
        Err(e) => {
            eprintln!("resolve {file}: {e}");
            return ExitCode::from(2);
        }
    };
    let list =
        match read::read_attribute_value_if_present(&mut io, rec, AttrType::AttributeList, None) {
            Ok(Some(list)) => list,
            Ok(None) => return ExitCode::from(1),
            Err(e) => {
                eprintln!("read {file}'s $ATTRIBUTE_LIST: {e}");
                return ExitCode::from(2);
            }
        };
    let entries = match read::parse_attribute_list(&list) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("parse {file}'s $ATTRIBUTE_LIST: {e}");
            return ExitCode::from(2);
        }
    };
    for e in entries
        .iter()
        .filter(|e| e.type_code == AttrType::Data as u32 && e.name.is_none())
    {
        println!("{} {}", e.starting_vcn, e.record_number);
    }
    ExitCode::SUCCESS
}
