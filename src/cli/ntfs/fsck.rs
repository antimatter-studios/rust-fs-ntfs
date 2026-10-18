//! `fsck.ntfs`: check an NTFS volume, and repair the two things the library
//! knows how to repair safely.
//!
//! WHAT IT CHECKS is `fs_ntfs::fsck::check_io`: the dirty flag, whether
//! `$LogFile` records anything to replay (empty, clean, or records),
//! `$MFTMirr` against `$MFT`, the header of every MFT record the bitmap
//! marks in use, and `$Bitmap` against the clusters every run list in those
//! records claims (#517). It does not follow indexes, or look at attributes
//! beyond their run lists. The report says so in `checks` and `scope`, so
//! nobody reads exit 0 as "chkdsk would agree": Windows' chkdsk is the
//! authority.
//!
//! WHAT IT REPAIRS, with `-y` (or `-p`): a `$LogFile` that holds work, by
//! replaying it (#137); a clean log is kept as it is. A log that cannot be
//! replayed in full is refused and reported, and nothing is written.
//!
//! THE DIRTY FLAG IS LEFT SET unless `--clear-dirty` is given with `-y`
//! (#519), and then only once the log records nothing to redo or undo
//! (#375). The flag asks for a check of the whole volume, which Windows'
//! chkdsk makes and this does not; clearing it would tell Windows that check
//! had been made. ntfs-3g's `ntfsfix` leaves it set too unless given `-d`.
//! A flag left set is still a finding, so the exit status stays 4 and the
//! report says how to clear it. Nothing else is repaired.
//!
//! EXIT STATUS IS fsck(8)'s, because scripts and the `fsck` front-end read
//! it: 0 clean, 1 errors corrected, 4 errors left uncorrected, 8 an
//! operational error (the target could not be opened, or is not an NTFS
//! volume), 16 a wrong command line.
//!
//! WHAT A DRY RUN REPORTS, with `--dry-run`: every write `-y` would make
//! (with `--clear-dirty`, that too), worked out by the library's own repair
//! over an overlay that records its writes instead of making them
//! (`fs_ntfs::repair_plan`, #709). The plan is a list of units, each the
//! writes made between two flushes, each write by offset, length and the
//! SHA-256 of the bytes there before it and of the bytes it leaves. The
//! target is opened read-only, as with `-n`, and the exit status is `-n`'s.
//!
//! Without `-y` nothing is written, as with `-n`.

use std::ffi::OsString;

use clap::{value_parser, Arg, ArgAction, ArgMatches, Command as Cmd};

use fs_core::cli::{CliError, Json, Outcome, Tool};
use fs_ntfs::fsck::{
    check_io, repair_dirty_io, replay_logfile_io, CheckFinding, CheckReport, FsckOptions,
    LogfileState,
};
use fs_ntfs::repair_plan::{plan_fsck_io, RepairPlan};
use sha2::{Digest, Sha256};

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
const CHECKS: &[&str] = &[
    "dirty_flag",
    "logfile",
    "mft_mirror",
    "mft_records",
    "cluster_allocation",
];

/// Why `-y` leaves the dirty flag set, and how to clear it (#519).
const LEFT_FOR_CHKDSK: &str = "left set: the dirty flag asks for a check of the whole volume, \
                               which Windows' chkdsk makes at the next mount and this is not; \
                               pass --clear-dirty with -y to clear it on a volume known to be \
                               consistent";

/// The sentence that stops exit 0 being read as more than it is.
const SCOPE: &str = "the dirty flag, whether $LogFile is empty, $MFTMirr against $MFT, the \
                     header of every in-use MFT record, and $Bitmap against every run list; not \
                     a full structural check (indexes, and attributes beyond their run lists, \
                     are not examined): Windows' chkdsk is the authority";

pub const TOOL: Tool = Tool {
    name: "fsck.ntfs",
    verb: "fsck",
    section: 8,
    usage_exit: USAGE,
    about: "Check an NTFS volume's dirty flag, log, MFT mirror, MFT records and clusters",
    command,
    run,
};

fn command() -> Cmd {
    Cmd::new("fsck.ntfs")
        .about("Check an NTFS volume's dirty flag, log, MFT mirror, MFT records and clusters")
        .long_about(
            "Check an NTFS image or device: the dirty flag, whether $LogFile is empty, \
             $MFTMirr against $MFT, the header of every in-use MFT record, and $Bitmap \
             against the clusters every run list claims. This is not a full structural \
             check, and the report says so: Windows' chkdsk is the authority.\n\n\
             Nothing is written unless -y (or -p) is given. Then a $LogFile that holds \
             work is replayed; a log that cannot be replayed in full is left as it is, \
             with nothing written.\n\n\
             --dry-run works out every write -y would make (--clear-dirty's too, when \
             given) and makes none: TARGET is opened read-only. The report's plan lists \
             them in units, each the writes between two flushes, each by offset, length \
             and the SHA-256 of the bytes before and after it.\n\n\
             The dirty flag is left set for Windows' chkdsk, which runs at the next mount \
             of a volume that carries it: it asks for a check of the whole volume, which \
             this is not. --clear-dirty with -y clears it once the log holds nothing to \
             redo or undo, for a volume known to be consistent by other means.\n\n\
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
                .help("Repair what can be repaired: replay $LogFile")
                .action(ArgAction::SetTrue)
                .conflicts_with("no"),
        )
        .arg(
            Arg::new("preen")
                .short('p')
                .visible_short_alias('a')
                .long("preen")
                .help("Repair automatically, as -y (the repairs made here are safe ones)")
                .action(ArgAction::SetTrue)
                .conflicts_with("no"),
        )
        .arg(
            Arg::new("dry_run")
                .long("dry-run")
                .help("Report every write -y would make, and make none: open read-only, as -n")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["no", "yes", "preen"]),
        )
        .arg(
            Arg::new("clear_dirty")
                .short('d')
                .long("clear-dirty")
                .help(
                    "With -y or -p, also clear the dirty flag, which is otherwise left for \
                     Windows' chkdsk; with --dry-run, plan that too",
                )
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
             fsck.ntfs -y disk.img              replay $LogFile; the dirty flag stays set\n  \
             fsck.ntfs --dry-run disk.img       every write -y would make; none is made\n  \
             fsck.ntfs -y --clear-dirty disk.img   and clear the dirty flag too\n  \
             fsck.ntfs --text disk.img; echo $?",
        )
}

fn run(matches: &ArgMatches) -> Result<Outcome, CliError> {
    let target = matches
        .get_one::<OsString>("device")
        .expect("clap requires the device");
    let offset = matches.get_one::<u64>("offset").copied().unwrap_or(0);
    let repair = matches.get_flag("yes") || matches.get_flag("preen");
    let dry_run = matches.get_flag("dry_run");
    let clear_dirty = matches.get_flag("clear_dirty");
    if clear_dirty && !repair && !dry_run {
        return Err(CliError::usage(
            "--clear-dirty writes, so it needs -y or -p (or --dry-run, to see what it would \
             write): nothing is written without them",
        )
        .with_code(USAGE));
    }
    let name = target.to_string_lossy().into_owned();
    let operational = |e: CliError| e.with_code(OPERATIONAL);

    let (mut dev, _) = super::device::mount(target, offset, repair).map_err(operational)?;
    let before = check_io(&mut dev)
        .map_err(|e| CliError::failed(format!("{name}: {e}")).with_code(OPERATIONAL))?;

    // A dry run plans what -y would write, through the same repair, and
    // writes nothing (#709): the device is read-only.
    let plan =
        dry_run.then(|| plan_fsck_io(&mut dev, FsckOptions { clear_dirty }).map_err(String::from));

    // The log first: replayed in full, or not touched (#137).
    let mut replay_refused = None;
    let mut replayed = None;
    let mut repaired = 0u64;
    if repair && before.logfile.needs_replay() {
        match replay_logfile_io(&mut dev) {
            Ok(report) => {
                repaired += 1;
                replayed = Some(report);
            }
            Err(why) => replay_refused = Some(String::from(why)),
        }
    }
    // Then the dirty flag, when asked and the log holds nothing to lose
    // (#519): otherwise it is left for Windows' chkdsk.
    let mut dirty_refused = None;
    if repair && before.dirty && replay_refused.is_none() && clear_dirty {
        match repair_dirty_io(&mut dev) {
            Ok(true) => repaired += 1,
            Ok(false) => {}
            Err(why) => dirty_refused = Some(String::from(why)),
        }
    } else if repair && before.dirty && replay_refused.is_none() {
        dirty_refused = Some(LEFT_FOR_CHKDSK.to_string());
    } else if repair && before.dirty {
        dirty_refused = replay_refused.clone();
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
    if before.logfile.needs_replay() {
        let why = match &before.logfile {
            LogfileState::Pending(why) => why.clone(),
            _ => unreachable!("needs_replay is Pending"),
        };
        findings.push(Json::object([
            ("kind", Json::from("logfile")),
            ("repaired", Json::from(replayed.is_some())),
            ("replayed", Json::from(replayed.as_ref().map(|r| r.applied))),
            ("why", Json::from(replay_refused.clone().unwrap_or(why))),
        ]));
    }
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
            // Checking alone cannot know whether a log holding work will
            // replay; repairing says whether it did.
            (
                "repairable",
                Json::from(if repair {
                    before.dirty && !after.dirty
                } else {
                    !before.logfile.needs_replay()
                }),
            ),
            ("repaired", Json::from(before.dirty && !after.dirty)),
            ("why", Json::from(why)),
        ]));
    }
    // What the check found, and anything the check after a repair found
    // that it did not: a replay is what lets clusters be counted at all.
    findings.extend(
        before
            .findings
            .iter()
            .chain(
                after
                    .findings
                    .iter()
                    .filter(|f| !before.findings.contains(f)),
            )
            .map(finding),
    );

    let mode = match (repair, dry_run) {
        (true, _) => "repair",
        (false, true) => "dry-run",
        (false, false) => "check",
    };
    let mut fields = vec![
        ("fs", Json::from("ntfs")),
        ("device", Json::from(name.as_str())),
        ("mode", Json::from(mode)),
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
            Json::object([
                ("mft_records", Json::from(before.records_scanned)),
                ("clusters", Json::from(after.clusters_scanned)),
            ]),
        ),
        ("findings", Json::Arr(findings.clone())),
    ];
    if let Some(plan) = &plan {
        fields.push(("plan", plan_json(plan)));
    }
    let report = Json::object(fields);

    let mut text = vec![match code {
        CLEAN => format!(
            "fsck.ntfs: {name}: clean ({} MFT records, {} clusters; checked {})",
            before.records_scanned,
            after.clusters_scanned,
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
    match &plan {
        Some(Ok(plan)) => text.extend(plan_text(plan)),
        Some(Err(why)) => text.push(format!("  plan refused, nothing would be written: {why}")),
        None => {}
    }
    Ok(Outcome::report(report)
        .with_text(text.join("\n"))
        .with_code(code))
}

/// A dry run's plan as JSON: each unit's writes, by offset, length and the
/// SHA-256 of the bytes before and after; or why the repair is refused.
fn plan_json(plan: &Result<RepairPlan, String>) -> Json {
    let (units, refused) = match plan {
        Ok(plan) => (plan.units.as_slice(), None),
        Err(why) => (&[][..], Some(why.as_str())),
    };
    let writes = units.iter().flat_map(|unit| &unit.writes);
    Json::object([
        ("refused", Json::from(refused)),
        ("writes", Json::from(writes.clone().count() as u64)),
        (
            "bytes",
            Json::from(writes.map(|w| w.after.len() as u64).sum::<u64>()),
        ),
        (
            "units",
            Json::Arr(
                units
                    .iter()
                    .map(|unit| {
                        Json::object([(
                            "writes",
                            Json::Arr(
                                unit.writes
                                    .iter()
                                    .map(|w| {
                                        Json::object([
                                            ("offset", Json::from(w.offset)),
                                            ("length", Json::from(w.after.len() as u64)),
                                            ("before_sha256", Json::from(sha256_hex(&w.before))),
                                            ("after_sha256", Json::from(sha256_hex(&w.after))),
                                        ])
                                    })
                                    .collect(),
                            ),
                        )])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// A dry run's plan for people: a summary, then each unit's writes.
fn plan_text(plan: &RepairPlan) -> Vec<String> {
    let writes: usize = plan.units.iter().map(|unit| unit.writes.len()).sum();
    let mut lines = vec![format!(
        "  plan: {} units, {writes} writes; nothing was written",
        plan.units.len()
    )];
    for (n, unit) in plan.units.iter().enumerate() {
        let at: Vec<String> = unit
            .writes
            .iter()
            .map(|w| format!("{:#x}+{}", w.offset, w.after.len()))
            .collect();
        lines.push(format!("    unit {}: {}", n + 1, at.join(", ")));
    }
    lines
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// How many problems a check left: the dirty flag counts as one, and so
/// does a log holding work.
fn count(report: &CheckReport) -> u64 {
    u64::from(report.dirty)
        + u64::from(report.logfile.needs_replay())
        + report.findings.len() as u64
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
        CheckFinding::BadRunList {
            record,
            type_code,
            reason,
        } => Json::object([
            ("kind", Json::from("bad_run_list")),
            ("record", Json::from(*record)),
            ("attribute_type", Json::from(u64::from(*type_code))),
            ("why", Json::from(reason.as_str())),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::RunOutsideVolume {
            record,
            lcn,
            clusters,
        } => Json::object([
            ("kind", Json::from("run_outside_volume")),
            ("record", Json::from(*record)),
            ("lcn", Json::from(*lcn)),
            ("clusters", Json::from(*clusters)),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::CrossLinkedClusters {
            lcn,
            clusters,
            records: (first, second),
        } => Json::object([
            ("kind", Json::from("cross_linked_clusters")),
            ("lcn", Json::from(*lcn)),
            ("clusters", Json::from(*clusters)),
            (
                "records",
                Json::Arr(vec![Json::from(*first), Json::from(*second)]),
            ),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::LeakedClusters {
            clusters,
            first_lcn,
        } => Json::object([
            ("kind", Json::from("leaked_clusters")),
            ("clusters", Json::from(*clusters)),
            ("first_lcn", Json::from(*first_lcn)),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::ClustersInUseMarkedFree {
            clusters,
            first_lcn,
        } => Json::object([
            ("kind", Json::from("clusters_in_use_marked_free")),
            ("clusters", Json::from(*clusters)),
            ("first_lcn", Json::from(*first_lcn)),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::BitmapUnreadable { reason } => Json::object([
            ("kind", Json::from("bitmap_unreadable")),
            ("why", Json::from(reason.as_str())),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::BadAttributeList { record, reason } => Json::object([
            ("kind", Json::from("bad_attribute_list")),
            ("record", Json::from(*record)),
            ("why", Json::from(reason.as_str())),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::QuotaUsageDrift {
            owner,
            usage,
            charged,
        } => Json::object([
            ("kind", Json::from("quota_usage_drift")),
            ("owner", Json::from(u64::from(*owner))),
            ("usage", usage.map_or(Json::Null, Json::from)),
            ("charged", Json::from(*charged)),
            ("repairable", Json::from(false)),
        ]),
        CheckFinding::QuotaUnreadable { reason } => Json::object([
            ("kind", Json::from("quota_unreadable")),
            ("why", Json::from(reason.as_str())),
            ("repairable", Json::from(false)),
        ]),
    }
}
