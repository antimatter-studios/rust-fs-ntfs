//! `fs.ntfs SOURCE rescue ARCHIVE [--resume]`: copy every readable byte of
//! a failing source into a rescue archive, recording each unreadable span,
//! and pick an interrupted archive up again. The scan itself is
//! `fs_ntfs::rescue`; this is its command line.
//!
//! The exit status says how far the rescue got: 0 when the archive holds
//! every byte of the source, [`EXIT_INCOMPLETE`] when some spans are still
//! unreadable, 1 when the run failed or was refused.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::BufReader;
use std::path::Path;

use clap::{value_parser, Arg, ArgAction, ArgMatches, Command as Cmd};
use fs_core::cli::{CliError, Json, Outcome};
use fs_ntfs::rescue::{self, FileSource, RangeClass, ReadFailure, RescueSource, ScanOptions};

/// Exit status: the rescue ran and the archive still lacks some of the
/// source.
pub const EXIT_INCOMPLETE: u8 = 4;

pub fn command() -> Cmd {
    Cmd::new("rescue")
        .about("Copy every readable byte of a failing source into a resumable rescue archive")
        .arg(
            Arg::new("archive")
                .value_name("ARCHIVE")
                .help("The rescue archive: created, or with --resume picked up again")
                .value_parser(value_parser!(OsString))
                .required(true),
        )
        .arg(
            Arg::new("resume")
                .long("resume")
                .action(ArgAction::SetTrue)
                .help("Carry on an interrupted archive of this source instead of starting one"),
        )
        .arg(
            Arg::new("sector-size")
                .long("sector-size")
                .value_name("BYTES")
                .help("Retry failed reads in sectors of this size (512 to 4096); default: the boot sector's")
                .value_parser(value_parser!(u32)),
        )
        .after_help(
            "Examples:\n  fs.ntfs /dev/rdisk4 rescue disk4.ntrescue\n  \
             fs.ntfs /dev/rdisk4 rescue disk4.ntrescue --resume\n  \
             fs.ntfs --offset 1048576 whole-disk.img rescue part1.ntrescue\n\n\
             The source is only ever read. Metadata is read first, then allocated \
             clusters, then free ones. An existing archive is refused unless --resume \
             is given, and a resume first checks every archived sector still reads the \
             same from the source. Exit status: 0 the archive holds every byte, 4 some \
             spans are still unreadable, 1 the rescue failed or was refused.",
        )
}

/// The bytes of `inner` from `offset` on: a partition inside a whole disk.
struct Window<S> {
    inner: S,
    offset: u64,
}

impl<S: RescueSource> RescueSource for Window<S> {
    fn size(&self) -> u64 {
        self.inner.size() - self.offset
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ReadFailure> {
        self.inner.read_at(self.offset + offset, buf)
    }
}

pub fn run(target: &OsString, offset: u64, sub: &ArgMatches) -> Result<Outcome, CliError> {
    let source = Path::new(target);
    let archive = Path::new(
        sub.get_one::<OsString>("archive")
            .expect("clap requires the archive"),
    );
    let resume = sub.get_flag("resume");
    let options = ScanOptions {
        sector_size: sub.get_one::<u32>("sector-size").copied(),
        ..ScanOptions::default()
    };

    let inner = FileSource::open(source).map_err(CliError::failed)?;
    if offset >= inner.size() {
        return Err(CliError::failed(format!(
            "the offset {offset} is not inside '{}', which is {} bytes",
            source.display(),
            inner.size()
        )));
    }
    let mut input = Window { inner, offset };

    let report = if resume {
        let mut out = OpenOptions::new()
            .read(true)
            .write(true)
            .open(archive)
            .map_err(|e| {
                CliError::failed(format!("open the archive '{}': {e}", archive.display()))
            })?;
        rescue::resume(&mut input, &mut out, options)
    } else {
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(archive)
            .map_err(|e| {
                CliError::failed(format!(
                    "create the archive '{}': {e} (pass --resume to carry on an existing one)",
                    archive.display()
                ))
            })?;
        rescue::scan(&mut input, &mut out, options)
    }
    .map_err(CliError::failed)?;

    // A resume counts only what it appended, so whether the archive is now
    // whole is read back from the archive itself.
    let contents = File::open(archive)
        .map_err(|e| e.to_string())
        .and_then(|f| rescue::read_archive(&mut BufReader::new(f)))
        .map_err(|e| CliError::failed(format!("read back '{}': {e}", archive.display())))?;
    let missing = missing_bytes(&contents);
    let complete = missing == 0;

    let classes = Json::object(RangeClass::ALL.iter().map(|&class| {
        let bytes = report.class(class);
        (
            class_name(class),
            Json::object([
                ("archived", Json::from(bytes.archived)),
                ("unreadable", Json::from(bytes.unreadable)),
            ]),
        )
    }));
    let json = Json::object([
        ("source", Json::from(source.display().to_string())),
        ("offset", Json::from(offset)),
        ("archive", Json::from(archive.display().to_string())),
        ("resumed", Json::from(resume)),
        ("source_size", Json::from(report.source_size)),
        ("sector_size", Json::from(contents.sector_size)),
        ("readable_bytes", Json::from(report.readable_bytes)),
        ("unreadable_bytes", Json::from(report.unreadable_bytes)),
        ("data_records", Json::from(report.data_records)),
        ("unreadable_spans", Json::from(report.unreadable_spans)),
        ("failed_bulk_reads", Json::from(report.failed_bulk_reads)),
        ("classes", classes),
        ("missing_bytes", Json::from(missing)),
        ("complete", Json::from(complete)),
    ]);
    let outcome = Outcome::report(json);
    Ok(if complete {
        outcome
    } else {
        outcome.with_code(EXIT_INCOMPLETE)
    })
}

/// Source bytes no data record in the archive holds.
fn missing_bytes(contents: &rescue::ArchiveContents) -> u64 {
    let mut held: Vec<(u64, u64)> = contents
        .records
        .iter()
        .filter(|r| r.kind == rescue::KIND_DATA)
        .map(|r| (r.offset, r.offset + r.len))
        .collect();
    held.sort_unstable();
    let mut covered = 0u64;
    let mut reach = 0u64;
    for (start, end) in held {
        let start = start.max(reach);
        if end > start {
            covered += end - start;
            reach = end;
        }
    }
    contents.source_size - covered.min(contents.source_size)
}

fn class_name(class: RangeClass) -> &'static str {
    match class {
        RangeClass::Unclassified => "unclassified",
        RangeClass::Metadata => "metadata",
        RangeClass::Allocated => "allocated",
        RangeClass::Free => "free",
        RangeClass::Tail => "tail",
    }
}
