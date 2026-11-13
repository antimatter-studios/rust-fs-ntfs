//! `rust-ntfs times` — set a file's four `$STANDARD_INFORMATION` times.

use fs_ntfs::write::FileTimes;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rust-ntfs times <image> <path> <creation> <modification> <change> <access>

Sets the times in `<path>`'s $STANDARD_INFORMATION. Each is a FILETIME:
100-nanosecond intervals since 1601-01-01 UTC, in decimal. `-` leaves
that time as it is. <change> is the time the MFT record last changed.

Example:
  rust-ntfs times vol.img /f.txt 126000000000000000 - - 133000000000000001
";

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("times", run_inner(args))
}

fn run_inner(args: Vec<String>) -> Result<(), crate::cli::CliError> {
    if crate::cli::asked_for_help(&args, USAGE) {
        return Ok(());
    }
    let args = crate::cli::positionals(&args, 6, USAGE)?;
    let (image, path) = (&args[0], &args[1]);
    let time = |text: &str| -> Result<Option<u64>, crate::cli::CliError> {
        if text == "-" {
            return Ok(None);
        }
        text.parse().map(Some).map_err(|_| {
            crate::cli::CliError::Usage(format!(
                "time {text:?} is neither a decimal FILETIME nor `-`\n\n{USAGE}"
            ))
        })
    };
    let times = FileTimes {
        creation: time(&args[2])?,
        modification: time(&args[3])?,
        mft_record_modification: time(&args[4])?,
        access: time(&args[5])?,
    };
    fs_ntfs::write::set_times(Path::new(image), path, times)
        .map_err(|e| format!("times {path}: {e}"))?;
    println!("set the times of {path}");
    Ok(())
}
