//! `rust-ntfs security-id` — point a file at a security descriptor
//! already in `$Secure`.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rust-ntfs security-id <image> <path> <security-id>

Sets the security id in `<path>`'s $STANDARD_INFORMATION, so the file
takes the descriptor `$Secure` holds under that id. The id is decimal or
0x-prefixed hexadecimal, and must already name a descriptor in
`$Secure:$SII`: this adds none.

Example:
  rust-ntfs security-id vol.img /f.txt 0x100
";

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("security-id", run_inner(args))
}

fn run_inner(args: Vec<String>) -> Result<(), crate::cli::CliError> {
    if crate::cli::asked_for_help(&args, USAGE) {
        return Ok(());
    }
    let args = crate::cli::positionals(&args, 3, USAGE)?;
    let (image, path, text) = (&args[0], &args[1], &args[2]);
    let id = match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse(),
    }
    .map_err(|_| {
        crate::cli::CliError::Usage(format!(
            "security id {text:?} is not a 32-bit number\n\n{USAGE}"
        ))
    })?;
    fs_ntfs::write::set_security_id(Path::new(image), path, id)
        .map_err(|e| format!("security-id {path}: {e}"))?;
    println!("gave {path} security id {id:#x}");
    Ok(())
}
