//! `rust-ntfs ea` — set or remove one of a file's extended attributes.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rust-ntfs ea <image> <path> <name> [data-source]
       rust-ntfs ea <image> <path> <name> --delete

Sets the extended attribute <name> of an existing file to the given bytes,
adding it when the file has none of that name, or removes it. The name is
stored upper-cased, as Windows stores it. A file whose record has no room
for its EAs is given an $ATTRIBUTE_LIST and extension records, as Windows
does.

Data-source flags (exactly one required unless --delete):
  --content STR     Literal UTF-8 text (no trailing newline).
  --bytes N         Generate N bytes of data using --pattern (default zeros).
  --from FILE       Read bytes from FILE on the host filesystem.
  --pattern PAT     One of: zeros, ones, incrementing. Default: zeros.

Examples:
  rust-ntfs ea vol.img /f.txt USER.NOTE --content 'hello'
  rust-ntfs ea vol.img /f.txt USER.NOTE --delete
";

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("ea", run_inner(args))
}

fn run_inner(args: Vec<String>) -> Result<(), crate::cli::CliError> {
    if crate::cli::asked_for_help(&args, USAGE) {
        return Ok(());
    }
    if args.len() < 4 {
        return Err(crate::cli::CliError::Usage(format!(
            "expected <image> <path> <name> and a data source or --delete\n\n{USAGE}"
        )));
    }
    let mut iter = args.into_iter();
    let image = iter.next().unwrap();
    let path = iter.next().unwrap();
    let name = iter.next().unwrap();
    let mut rest: Vec<String> = iter.collect();

    if rest == ["--delete"] {
        fs_ntfs::write::remove_ea(Path::new(&image), &path, name.as_bytes())
            .map_err(|e| format!("remove EA {name} from {path}: {e}"))?;
        println!("removed EA {name} from {path}");
        return Ok(());
    }
    let data = crate::write::parse_data_source(&mut rest)
        .map_err(|e| crate::cli::CliError::Usage(format!("{e}\n\n{USAGE}")))?;
    fs_ntfs::write::write_ea(Path::new(&image), &path, name.as_bytes(), &data, 0)
        .map_err(|e| format!("write EA {name} on {path}: {e}"))?;
    println!("wrote {} bytes to EA {name} on {path}", data.len());
    Ok(())
}
