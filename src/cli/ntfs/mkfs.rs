//! `mkfs.ntfs`: create a fresh NTFS filesystem.
//!
//! The formatter that shipped as the `mkfs_ntfs` target, moved into the
//! multi-call binary. Every flag it accepted is still accepted, and the
//! work is done by the one implementation it always shared with
//! `rust-ntfs format` (`super::format::execute`), so the two cannot drift.
//! `--size` is the shared spelling of what was `--create-size`, which
//! stays as an alias, and `-n` gains the long form `--dry-run`.
//!
//! What is new is the result: a JSON report on stdout, read back from the
//! volume that was written (`--text` prints nothing there, as the tool did
//! before). The progress lines on stderr are unchanged.
//!
//! Convention follows the standard formatters: the device or file must
//! already exist at the target size, unless `--size` is given for an
//! image file that does not exist yet.

use std::path::Path;

use clap::{Arg, ArgAction, ArgMatches, Command as Cmd};

use super::format::{self, Opts, DEFAULT_CLUSTER_SIZE, DEFAULT_MFT_RECORD_SIZE};
use crate::common::{CliError, Json, Outcome, Tool};
use fs_ntfs::block_io::PathIo;

pub const TOOL: Tool = Tool {
    name: "mkfs.ntfs",
    verb: "mkfs",
    section: 8,
    usage_exit: crate::common::output::EXIT_USAGE,
    about: "Create an NTFS filesystem on a device or an image file",
    command,
    run,
};

fn command() -> Cmd {
    Cmd::new("mkfs.ntfs")
        .about("Create an NTFS filesystem on a device or an image file")
        .long_about(
            "Create an NTFS filesystem on a device or a pre-sized image file.\n\n\
             The device or file must already exist at the target size (`truncate -s 64M \
             out.img`), unless --size is given for an image file that does not exist yet.\n\n\
             A JSON report of what was written, read back from the new volume, goes to \
             stdout; progress goes to stderr.",
        )
        // A repeated flag takes its last value, as the hand parser this
        // replaces did: scripts append a flag to override one earlier.
        .args_override_self(true)
        .arg(
            Arg::new("device")
                .value_name("TARGET")
                .help("Block device or image file to format")
                .required(true),
        )
        .arg(
            Arg::new("label")
                .short('L')
                .long("label")
                .value_name("LABEL")
                .help("Volume label, at most 32 UTF-16 code units")
                .value_parser(parse_label),
        )
        .arg(
            Arg::new("cluster-size")
                .short('c')
                .long("cluster-size")
                .value_name("BYTES")
                .help(format!(
                    "Cluster size in bytes: a power of two, 512..=65536. Default: \
                     {DEFAULT_CLUSTER_SIZE}."
                ))
                .value_parser(parse_number),
        )
        .arg(
            Arg::new("mft-record-size")
                .long("mft-record-size")
                .value_name("BYTES")
                .help(format!(
                    "MFT record size in bytes: a power of two, 2048..=16384 (512 cannot hold \
                     $Secure; 1024 cannot hold the populated root metadata). Default: \
                     {DEFAULT_MFT_RECORD_SIZE}."
                ))
                .value_parser(parse_mft_record_size),
        )
        .arg(
            Arg::new("serial")
                .long("serial")
                .value_name("HEX")
                .help("Volume serial number, 1 to 16 hex digits, 0x optional. Default: random.")
                .value_parser(format::parse_hex_u64),
        )
        .arg(
            Arg::new("quick")
                .short('Q')
                .long("quick")
                .visible_short_alias('f')
                .visible_alias("fast")
                .help(
                    "Quick format (accepted: what is written is always the quick-format \
                     layout, with no full-volume zero pass)",
                )
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("force")
                .short('F')
                .long("force")
                .help("Format even if the device looks in use (accepted; nothing is inspected yet)")
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("dry-run")
                .short('n')
                .long("dry-run")
                .help("Open the device and report, but write nothing")
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("quiet")
                .short('q')
                .long("quiet")
                .help("No progress on stderr")
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("size")
                .long("size")
                .visible_alias("create-size")
                .value_name("SIZE")
                .help(
                    "Create TARGET as an image file of SIZE bytes first, if it does not exist \
                     (K/M/G/T suffixes, 1024-based). Refused for block and character devices.",
                )
                .value_parser(format::parse_size),
        )
        .args(crate::common::format_args())
        .after_help(
            "Examples:\n  \
             mkfs.ntfs --size 64M --label BACKUP disk.img\n  \
             truncate -s 1G disk.img && mkfs.ntfs -c 4096 disk.img\n  \
             mkfs.ntfs -n disk.img                  open and report, write nothing\n  \
             mkfs.ntfs --text -q disk.img           print nothing on success",
        )
}

fn parse_label(v: &str) -> Result<String, String> {
    format::check_label(v)?;
    Ok(v.to_string())
}

fn parse_number(v: &str) -> Result<u32, String> {
    v.parse().map_err(|_| format!("not a valid number: {v}"))
}

fn parse_mft_record_size(v: &str) -> Result<u32, String> {
    format::check_mft_record_size(parse_number(v)?)
}

fn opts(matches: &ArgMatches) -> Opts {
    Opts {
        label: matches.get_one::<String>("label").cloned(),
        cluster_size: matches.get_one::<u32>("cluster-size").copied(),
        mft_record_size: matches.get_one::<u32>("mft-record-size").copied(),
        serial: matches.get_one::<u64>("serial").copied(),
        force: matches.get_count("force") > 0,
        quick: matches.get_count("quick") > 0,
        dry_run: matches.get_count("dry-run") > 0,
        quiet: matches.get_count("quiet") > 0,
        create_size: matches.get_one::<u64>("size").copied(),
        device: matches.get_one::<String>("device").cloned(),
    }
}

fn run(matches: &ArgMatches) -> Result<Outcome, CliError> {
    let opts = opts(matches);
    let device = opts.device.clone().expect("clap requires the device");
    let done = format::execute(&opts, &device, TOOL.name).map_err(CliError::failed)?;

    let mut report = vec![
        ("fs", Json::from("ntfs")),
        ("device", Json::from(device.as_str())),
        ("device_bytes", Json::from(done.device_bytes)),
        ("dry_run", Json::from(opts.dry_run)),
        ("formatted", Json::from(done.formatted)),
    ];
    if !done.formatted {
        report.push(("label", Json::from(opts.label.clone())));
        report.push(("block_size", Json::from(done.cluster_size)));
        report.push(("mft_record_size", Json::from(done.mft_record_size)));
        return Ok(Outcome::report(Json::object(report)).with_text(String::new()));
    }

    // The report is what the volume now SAYS, read back, not what was
    // asked for: a label or serial that did not reach the disk shows here.
    let mut io = PathIo::open_ro(Path::new(&device))
        .map_err(|e| CliError::failed(format!("read back {device} after formatting: {e}")))?;
    let vi = fs_ntfs::read::read_volume_info(&mut io)
        .map_err(|e| CliError::failed(format!("read back {device} after formatting: {e}")))?;
    report.extend([
        (
            "label",
            if vi.label.is_empty() {
                Json::Null
            } else {
                Json::from(vi.label.as_str())
            },
        ),
        ("block_size", Json::from(vi.cluster_size)),
        ("total_bytes", Json::from(vi.total_size)),
        ("total_clusters", Json::from(vi.total_clusters)),
        ("mft_record_size", Json::from(vi.file_record_size)),
        ("serial_number", Json::from(serial_text(vi.serial_number))),
    ]);
    // `--text` prints nothing on success, exactly as the tool always has:
    // its progress is on stderr, and a script checks the status.
    Ok(Outcome::report(Json::object(report)).with_text(String::new()))
}

/// The serial number as the 16 hex digits `--serial` takes.
pub fn serial_text(serial: u64) -> String {
    format!("{serial:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Result<Opts, clap::Error> {
        let mut full = vec!["mkfs.ntfs"];
        full.extend_from_slice(argv);
        command().try_get_matches_from(full).map(|m| opts(&m))
    }

    /// Every flag `rust-ntfs format` takes, `mkfs.ntfs` takes too: the two
    /// were one parser, and scripts written against either keep working.
    #[test]
    fn every_flag_the_hand_parser_takes_is_still_taken() {
        let opts = parse(&[
            "-L",
            "X",
            "-c",
            "4096",
            "--mft-record-size",
            "4096",
            "--serial",
            "0xdeadbeef",
            "-Q",
            "-f",
            "--fast",
            "--quick",
            "-F",
            "--force",
            "-n",
            "-q",
            "--quiet",
            "--create-size",
            "64M",
            "--cluster-size",
            "512",
            "--label",
            "Y",
            "/x/disk.img",
        ])
        .expect("parse");
        assert_eq!(opts.label.as_deref(), Some("Y"));
        assert_eq!(opts.cluster_size, Some(512));
        assert_eq!(opts.mft_record_size, Some(4096));
        assert_eq!(opts.serial, Some(0xdead_beef));
        assert!(opts.quick && opts.force && opts.dry_run && opts.quiet);
        assert_eq!(opts.create_size, Some(64 << 20));
        assert_eq!(opts.device.as_deref(), Some("/x/disk.img"));
    }

    #[test]
    fn size_is_the_shared_spelling_and_create_size_still_works() {
        for flag in ["--size", "--create-size"] {
            let opts = parse(&[flag, "64M", "/x/disk.img"]).expect("parse");
            assert_eq!(opts.create_size, Some(64 << 20), "{flag}");
        }
        assert!(parse(&["--dry-run", "/x/disk.img"]).expect("parse").dry_run);
    }

    #[test]
    fn undersized_mft_records_are_refused_while_parsing() {
        for bad in ["512", "1024"] {
            let err = parse(&["--mft-record-size", bad, "/x/disk.img"])
                .expect_err(bad)
                .to_string();
            assert!(err.contains("smallest supported size is 2048"), "{err}");
        }
        assert!(parse(&["--mft-record-size", "2048", "/x/disk.img"]).is_ok());
    }

    #[test]
    fn a_label_over_32_utf16_units_is_refused_while_parsing() {
        let long = "x".repeat(33);
        assert!(parse(&["-L", &long, "/x/disk.img"]).is_err());
        assert!(parse(&["-L", &"x".repeat(32), "/x/disk.img"]).is_ok());
        // An emoji is two UTF-16 units: sixteen of them fill the field.
        assert!(parse(&["-L", &"🙂".repeat(16), "/x/disk.img"]).is_ok());
        assert!(parse(&["-L", &"🙂".repeat(17), "/x/disk.img"]).is_err());
    }

    #[test]
    fn a_second_device_and_an_unknown_flag_are_refused() {
        assert!(parse(&["/x/a.img", "/x/b.img"]).is_err());
        assert!(parse(&["-Z", "/x/a.img"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn the_help_states_the_defaults_from_the_constants() {
        let help = command().render_long_help().to_string();
        assert!(
            help.contains(&format!("Default: {DEFAULT_CLUSTER_SIZE}.")),
            "{help}"
        );
        assert!(help.contains("512 cannot hold $Secure"), "{help}");
    }
}
