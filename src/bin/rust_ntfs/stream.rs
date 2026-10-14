//! `rust-ntfs stream` — write or delete one of a file's named streams.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rust-ntfs stream <image> <path> <name> [data-source]
       rust-ntfs stream <image> <path> <name> --delete

Writes bytes into the named $DATA stream <name> of an existing file,
creating the stream when the file has none of that name, or deletes it.
A file whose record has no room for the stream is given an
$ATTRIBUTE_LIST and extension records, as Windows does.

Data-source flags (exactly one required unless --delete):
  --content STR     Literal UTF-8 text (no trailing newline).
  --bytes N         Generate N bytes of data using --pattern (default zeros).
  --from FILE       Read bytes from FILE on the host filesystem.
  --pattern PAT     One of: zeros, ones, incrementing. Default: zeros.

Examples:
  rust-ntfs stream vol.img /f.txt notes --content 'hello'
  rust-ntfs stream vol.img /f.txt notes --delete
";

pub fn run(args: Vec<String>) -> ExitCode {
    crate::cli::finish("stream", run_inner(args))
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
        fs_ntfs::write::delete_named_stream(Path::new(&image), &path, &name)
            .map_err(|e| format!("delete {path}:{name}: {e}"))?;
        println!("deleted {path}:{name}");
        return Ok(());
    }
    let data = crate::write::parse_data_source(&mut rest)
        .map_err(|e| crate::cli::CliError::Usage(format!("{e}\n\n{USAGE}")))?;
    fs_ntfs::write::write_named_stream(Path::new(&image), &path, &name, &data)
        .map_err(|e| format!("write {path}:{name}: {e}"))?;
    println!("wrote {} bytes to {path}:{name}", data.len());
    Ok(())
}
