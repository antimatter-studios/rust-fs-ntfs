//! `rust-ntfs reparse` — give an existing file a symbolic-link reparse
//! point, or take its reparse point away.

use fs_ntfs::record_build::{build_symlink_reparse_data, reparse_tag};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rust-ntfs reparse <image> <path> <target>
       rust-ntfs reparse <image> <path> --remove

Gives the existing file `<path>` an IO_REPARSE_TAG_SYMLINK reparse point
to `<target>`, relative to the file's directory, or with --remove takes
its reparse point away. A file whose record has no room for it, or that
has an $ATTRIBUTE_LIST, takes it through the list, as Windows does.
`symlink` creates a new link instead.
";

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("reparse", run_inner(args))
}

fn run_inner(args: Vec<String>) -> Result<(), crate::cli::CliError> {
    if crate::cli::asked_for_help(&args, USAGE) {
        return Ok(());
    }
    if args.len() == 3 && args[2] == "--remove" {
        let (image, path) = (&args[0], &args[1]);
        fs_ntfs::write::remove_reparse_point(Path::new(image), path)
            .map_err(|e| format!("remove the reparse point of {path}: {e}"))?;
        println!("removed the reparse point of {path}");
        return Ok(());
    }
    let args = crate::cli::positionals(&args, 3, USAGE)?;
    let (image, path, target) = (&args[0], &args[1], &args[2]);
    // Windows spells a relative link's names with `\`, as `symlink` does.
    let data = build_symlink_reparse_data(&target.replace('/', "\\"), None, true);
    fs_ntfs::write::write_reparse_point(Path::new(image), path, reparse_tag::SYMLINK, &data)
        .map_err(|e| format!("reparse {path} -> {target}: {e}"))?;
    println!("gave {path} a symbolic link to {target}");
    Ok(())
}
