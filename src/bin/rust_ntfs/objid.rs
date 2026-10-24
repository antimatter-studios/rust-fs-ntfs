//! `rust-ntfs objid` — give a file an object id, its `$OBJECT_ID`, or
//! take it away.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rust-ntfs objid <image> <path> <object-id>
       rust-ntfs objid <image> <path> --remove

Gives `<path>` the 16-byte object id `<object-id>`, written as 32
hexadecimal digits, in its `$OBJECT_ID` attribute, or with --remove takes
it away. Removing an object id from a file that has none fails.
";

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("objid", run_inner(args))
}

fn run_inner(args: Vec<String>) -> Result<(), crate::cli::CliError> {
    if crate::cli::asked_for_help(&args, USAGE) {
        return Ok(());
    }
    if args.len() == 3 && args[2] == "--remove" {
        let (image, path) = (&args[0], &args[1]);
        let removed = fs_ntfs::write::remove_object_id(Path::new(image), path)
            .map_err(|e| format!("objid {path} --remove: {e}"))?;
        if !removed {
            return Err(format!("objid {path} --remove: {path} has no object id").into());
        }
        println!("removed the object id of {path}");
        return Ok(());
    }
    let args = crate::cli::positionals(&args, 3, USAGE)?;
    let (image, path, hex) = (&args[0], &args[1], &args[2]);
    let id = parse_id(hex).ok_or_else(|| {
        crate::cli::CliError::Usage(format!(
            "object id {hex:?} is not 32 hexadecimal digits\n\n{USAGE}"
        ))
    })?;
    fs_ntfs::write::write_object_id(Path::new(image), path, &id)
        .map_err(|e| format!("objid {path}: {e}"))?;
    println!("gave {path} object id {hex}");
    Ok(())
}

fn parse_id(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    let mut id = [0u8; 16];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(id)
}
