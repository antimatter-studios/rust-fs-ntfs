//! `fsck.ntfs`: check an NTFS volume, and repair the one thing the library
//! knows how to repair safely.
//!
//! WHAT IT CHECKS is `fs_ntfs::fsck::check_io`: the dirty flag, whether
//! `$LogFile` records anything to replay (empty, clean, or records),
//! `$MFTMirr` against `$MFT`, and the header of every MFT record the bitmap
//! marks in use. It does not follow attributes, indexes or runs. The report
//! says so in `checks` and `scope`, so nobody reads exit 0 as "chkdsk would
//! agree": Windows' chkdsk is the authority.
//!
//! WHAT IT REPAIRS, with `-y` (or `-p`): the dirty flag, and only when
//! `$LogFile` is empty or its restart area records nothing to redo or undo
//! (#375); a clean log is kept as it is. Any other log may hold
//! transactions this library cannot replay (#137), and clearing the flag
//! over them would discard them, so that is refused and reported, and
//! nothing is written. Nothing else is repaired.
//!
//! EXIT STATUS IS fsck(8)'s, because scripts and the `fsck` front-end read
//! it: 0 clean, 1 errors corrected, 4 errors left uncorrected, 8 an
//! operational error (the target could not be opened, or is not an NTFS
//! volume), 16 a wrong command line.
//!
//! Without `-y` nothing is written, as with `-n`.

use std::ffi::OsString;

use clap::{value_parser, Arg, ArgAction, ArgMatches, Command as Cmd};

use fs_core::cli::{CliError, Json, Outcome, Tool};
use fs_ntfs::fsck::{check_io, repair_dirty_io, CheckFinding, CheckReport, LogfileState};

/// fsck(8): no errors.
pub const CLEAN: u8 = 0;
/// fsck(8): filesystem errors corrected.
pub const CORRECTED: u8 = 1;
/// fsck(8): filesystem errors left uncorrected.
pub const UNCORRECTED: u8 = 4;
/// fsck(8): operational error.
pub const OPERATIONAL: u8 = 8;
/// fsck(8): usage or syntax error.
pub const USAGE: u8 = 16;

/// What the check covers, in the order it reports them.
const CHECKS: &[&str] = &["dirty_flag", "logfile", "mft_mirror", "mft_records"];

/// The sentence that stops exit 0 being read as more than it is.
const SCOPE: &str = "the dirty flag, whether $LogFile is empty, $MFTMirr against $MFT, and the \
                     header of every in-use MFT record; not a full structural check (attributes, \
                     indexes and cluster allocation are not examined): Windows' chkdsk is the \
                     authority";

pub const TOOL: Tool = Tool {
    name: "fsck.ntfs",
    verb: "fsck",
    section: 8,
    usage_exit: USAGE,
    about: "Check an NTFS volume's dirty flag, log, MFT mirror and MFT records",
    command,
    run,
};

fn command() -> Cmd {
    Cmd::new("fsck.ntfs")
        .about("Check an NTFS volume's dirty flag, log, MFT mirror and MFT records")
        .long_about(
            "Check an NTFS image or device: the dirty flag, whether $LogFile is empty, \
             $MFTMirr against $MFT, and the header of every in-use MFT record. This is not \
             a full structural check, and the report says so: Windows' chkdsk is the \
             authority.\n\n\
             Nothing is written unless -y (or -p) is given, and then only the dirty flag is \
             cleared, and only when $LogFile is empty: a log that holds records may hold \
             transactions this library cannot replay.\n\n\
             The report is JSON on stdout (--text for people).\n\n\
             Exit status, as fsck(8): 0 clean, 1 errors corrected, 4 errors left \
             uncorrected, 8 operational error, 16 usage error.",
        )
        .arg(
            Arg::new("device")
                .value_name("TARGET")
                .help("The image file or device to check")
                .value_parser(value_parser!(OsString))
                .required(true),
        )
        .arg(
            Arg::new("no")
                .short('n')
                .long("no")
                .help("Check only; open read-only and change nothing (the default)")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("yes")
                .short('y')
                .long("yes")
                .help("Repair what can be repaired: the dirty flag, when $LogFile is empty")
                .action(ArgAction::SetTrue)
                .conflicts_with("no"),
        )
        .arg(
            Arg::new("preen")
                .short('p')
                .visible_short_alias('a')
                .long("preen")
                .help("Repair automatically, as -y (the one repair made here is a safe one)")
                .action(ArgAction::SetTrue)
                .conflicts_with("no"),
        )
        .arg(
            Arg::new("force")
                .short('f')
                .long("force")
                .help("Check even a volume marked clean (accepted: every check is always made)")
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .help("Accepted for compatibility; the report already lists every finding")
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("offset")
                .long("offset")
                .value_name("BYTES")
                .help(
                    "Where the filesystem starts in TARGET, for a partition in a whole-disk image",
                )
                .value_parser(value_parser!(u64)),
        )
        .args(fs_core::cli::format_args())
        .after_help(
            "Examples:\n  fsck.ntfs disk.img                 check, change nothing\n  \
             fsck.ntfs -fn disk.img             the same, as fsck(8) front-ends spell it\n  \
             fsck.ntfs -y disk.img              clear the dirty flag if that is safe\n  \
             fsck.ntfs --text disk.img; echo $?",
        )
}

fn run(matches: &ArgMatches) -> Result<Outcome, CliError> {
    let target = matches
        .get_one::<OsString>("device")
        .expect("clap requires the device");
    let offset = matches.get_one::<u64>("offset").copied().unwrap_or(0);
    let repair = matches.get_flag("yes") || matches.get_flag("preen");
    let name = target.to_string_lossy().into_owned();
    let operational = |e: CliError| e.with_code(OPERATIONAL);

    let (mut dev, _) = super::device::mount(target, offset, repair).map_err(operational)?;
    let before = check_io(&mut dev)
        .map_err(|e| CliError::failed(format!("{name}: {e}")).with_code(OPERATIONAL))?;

    // The one repair: the dirty flag, when the log holds nothing to lose.
    let mut dirty_refused = None;
    let mut repaired = 0u64;
    if repair && before.dirty {
        match repair_dirty_io(&mut dev) {
            Ok(true) => repaired += 1,
            Ok(false) => {}
            Err(why) => dirty_refused = Some(String::from(why)),
        }
    }
    let after = if repaired > 0 {
        check_io(&mut dev)
            .map_err(|e| CliError::failed(format!("{name}: {e}")).with_code(OPERATIONAL))?
    } else {
        before.clone()
    };

    let found = count(&before);
    let remaining = count(&after);
    let code = if remaining > 0 {
        UNCORRECTED
    } else if repaired > 0 {
        CORRECTED
    } else {
        CLEAN
    };

    let mut findings = Vec::new();
    if before.dirty {
        // Repairable unless the log may hold transactions: an empty log, or
        // one whose restart area records nothing to redo or undo, is safe
        // to clear the flag over (#375).
        let why = dirty_refused.clone().or_else(|| match &before.logfile {
            LogfileState::Pending(why) => Some(why.clone()),
            _ => None,
        });
        findings.push(Json::object([
            ("kind", Json::from("dirty")),
            ("repairable", Json::from(!before.logfile.needs_replay())),
            ("repaired", Json::from(before.dirty && !after.dirty)),
            ("why", Json::from(why)),
        ]));
    }
    findings.extend(before.findings.iter().map(finding));

    let report = Json::object([
        ("fs", Json::from("ntfs")),
        ("device", Json::from(name.as_str())),
        ("mode", Json::from(if repair { "repair" } else { "check" })),
        (
            "checks",
            Json::Arr(CHECKS.iter().map(|c| Json::from(*c)).collect()),
        ),
        ("scope", Json::from(SCOPE)),
        ("clean", Json::from(remaining == 0)),
        ("dirty", Json::from(after.dirty)),
        (
            "logfile",
            Json::from(match after.logfile {
                LogfileState::Empty => "empty",
                LogfileState::Clean(_) => "clean",
                LogfileState::Pending(_) => "records",
            }),
        ),
        ("exit", Json::from(u64::from(code))),
        ("found", Json::from(found)),
        ("repaired", Json::from(repaired)),
        ("remaining", Json::from(remaining)),
        (
            "scanned",
            Json::object([("mft_records", Json::from(before.records_scanned))]),
        ),
        ("findings", Json::Arr(findings.clone())),
    ]);

    let mut text = vec![match code {
        CLEAN => format!(
            "fsck.ntfs: {name}: clean ({} MFT records; checked {})",
            before.records_scanned,
            CHECKS.join(", ")
        ),
        CORRECTED => format!("fsck.ntfs: {name}: {found} problems found, all corrected"),
        _ => format!(
            "fsck.ntfs: {name}: {found} problems found, {repaired} repaired, {remaining} remaining"
        ),
    }];
    for f in &findings {
        text.push(format!("  {}", f.to_text().replace('\n', ", ")));
    }
    Ok(Outcome::report(report)
        .with_text(text.join("\n"))
        .with_code(code))
}

/// How many problems a check left: the dirty flag counts as one.
fn count(report: &CheckReport) -> u64 {
    u64::from(report.dirty) + report.findings.len() as u64
}

/// One finding as JSON: its `kind`, and what locates it.
fn finding(f: &CheckFinding) -> Json {
    match f {
        CheckFinding::MirrorMismatch { record } => Json::object([
            ("kind", Json::from("mft_mirror_mismatch")),
            ("record", Json::from(*record)),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::MirrorUnreadable { reason } => Json::object([
            ("kind", Json::from("mft_mirror_unreadable")),
            ("why", Json::from(reason.as_str())),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::BadRecord { record, reason } => Json::object([
            ("kind", Json::from("bad_mft_record")),
            ("record", Json::from(*record)),
            ("why", Json::from(reason.as_str())),
            ("repairable", Json::from(false)),
        ]),
    }
}
