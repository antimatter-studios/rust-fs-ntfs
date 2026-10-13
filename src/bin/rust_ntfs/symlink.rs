//! `rust-ntfs symlink` — create a symbolic link: a file carrying an
//! IO_REPARSE_TAG_SYMLINK reparse point.

use fs_ntfs::facade::Filesystem;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rust-ntfs symlink <image> <parent> <basename> <target>

Creates `<parent>/<basename>`, a symbolic link to `<target>`. A target
with a drive letter, `\\??\\` or `\\\\` is absolute, as Windows reads it;
any other is relative to the link's directory.
";

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("symlink", run_inner(args))
}

fn run_inner(args: Vec<String>) -> Result<(), crate::cli::CliError> {
    if crate::cli::asked_for_help(&args, USAGE) {
        return Ok(());
    }
    let args = crate::cli::positionals(&args, 4, USAGE)?;
    let (image, parent, basename, target) = (&args[0], &args[1], &args[2], &args[3]);
    let fs = Filesystem::mount_rw(image).map_err(|e| format!("mount {image}: {e}"))?;
    fs.create_symlink(parent, basename, target, true)
        .map_err(|e| format!("symlink {parent}/{basename} -> {target}: {e}"))?;
    println!("linked {parent}/{basename} -> {target}");
    Ok(())
}
