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
//! WHAT `--dry-run --plan FILE` SAVES: the dry run's plan as a plan file
//! (`fs_ntfs::repair_plan::save_plan`, #906), bound to the volume's size
//! and serial number, for `--apply` to load. FILE is a new file outside the
//! volume: FILE naming the volume itself (by path, or the same device and
//! inode), FILE naming anything that exists, and `--plan` without
//! `--dry-run` are refused before the volume is opened, and nothing is
//! written. A plan the repair refuses saves no file and is an operational
//! error.
//!
//! WHAT `--apply PLAN --to TARGET` DOES: loads a plan file and
//! copy-repairs the volume into a new TARGET through
//! `fs_ntfs::repair_plan::copy_repair` (#910): the volume is opened
//! read-only and checked against the plan, and only then is TARGET created,
//! the volume copied into it and the plan applied there. The volume is the
//! source and is never written. `--to` naming the source, or anything that
//! exists, is refused before anything is created; so is `--apply` beside
//! anything that writes or plans (`-y`, `-p`, `--dry-run`).
//!
//! WHAT `--apply PLAN --in-place --journal FILE` DOES: repairs the volume
//! in place (#922). FILE is checked as a journal location with
//! `check_journal_location`, the volume opened with
//! `PathIo::open_exclusive` and checked against PLAN with
//! `check_plan_source`, and only then is FILE created and PLAN applied
//! through `apply_in_place`, journalling into FILE. In-place is never the
//! default: `--in-place` needs `--journal`, `--journal` needs `--in-place`,
//! and `--in-place` refuses `--to`, `--dry-run` and `-y`. Every refusal comes
//! before anything is created or written.
//!
//! WHAT `--apply PLAN --in-place --resume --journal FILE` DOES: resumes an
//! in-place repair that stopped (#963). FILE is the journal the stopped
//! apply left, and must exist and not be the volume; the volume is opened
//! exclusively and the apply continued through `resume_in_place`, which
//! checks FILE against PLAN and the volume, skips the units it marks
//! applied, redoes the open one and applies the rest.
//!
//! Without `-y` nothing is written, as with `-n`.

use std::ffi::OsString;
use std::path::Path;

use clap::{value_parser, Arg, ArgAction, ArgGroup, ArgMatches, Command as Cmd};

use fs_core::cli::{CliError, Json, Outcome, Tool};
#[cfg(unix)]
use fs_ntfs::block_io::PathIo;
use fs_ntfs::fsck::{
    check_io, repair_dirty_io, replay_logfile_io, CheckFinding, CheckReport, FsckOptions,
    LogfileState,
};
#[cfg(unix)]
use fs_ntfs::repair_plan::check_plan_source;
use fs_ntfs::repair_plan::{
    copy_repair, load_plan, plan_fsck_io, save_plan, CopyRepairError, RepairPlan,
};
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
             --apply PLAN --to TARGET copy-repairs the volume into a new TARGET: the \
             volume is opened read-only and checked against PLAN, and only then is \
             TARGET created, the volume copied into it and PLAN applied there. TARGET \
             must not exist and must not be the volume itself.\n\n\
             --apply PLAN --in-place --journal FILE repairs the volume in place: FILE \
             must be a new file outside the volume, the volume is opened exclusively \
             and checked against PLAN, and only then is FILE created and PLAN applied, \
             each unit journalled into FILE before it is written. With --resume, FILE \
             is the journal a stopped in-place repair left, and the repair continues \
             from where it stopped.\n\n\
             --dry-run works out every write -y would make (--clear-dirty's too, when \
             given) and makes none: TARGET is opened read-only. The report's plan lists \
             them in units, each the writes between two flushes, each by offset, length \
             and the SHA-256 of the bytes before and after it. --plan FILE also saves \
             the plan to FILE, a new file outside the volume, for --apply to load.\n\n\
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
            Arg::new("plan_file")
                .long("plan")
                .value_name("FILE")
                .help(
                    "With --dry-run, save the plan to FILE for --apply; it must not exist \
                     and must not be the volume",
                )
                .value_parser(value_parser!(OsString))
                .conflicts_with("apply"),
        )
        .arg(
            Arg::new("apply")
                .long("apply")
                .value_name("PLAN")
                .help(
                    "Repair by the plan file PLAN: copy-repair into --to's new TARGET, \
                     or with --in-place repair the volume itself",
                )
                .value_parser(value_parser!(OsString))
                .requires("apply_target")
                .conflicts_with_all(["no", "yes", "preen", "dry_run", "clear_dirty", "offset"]),
        )
        .arg(
            Arg::new("to")
                .long("to")
                .value_name("TARGET")
                .help("With --apply, the new file to copy-repair into; it must not exist")
                .value_parser(value_parser!(OsString))
                .requires("apply"),
        )
        .arg(
            Arg::new("in_place")
                .long("in-place")
                .help(
                    "With --apply, repair the volume itself, opened exclusively, journalling \
                     every unit into --journal's FILE first",
                )
                .action(ArgAction::SetTrue)
                .requires_all(["apply", "journal"])
                .conflicts_with_all(["to", "dry_run", "yes", "preen"]),
        )
        .arg(
            Arg::new("journal")
                .long("journal")
                .value_name("FILE")
                .help(
                    "With --in-place, the new journal file; it must not exist and must not \
                     be on the volume",
                )
                .value_parser(value_parser!(OsString))
                .requires("in_place"),
        )
        .arg(
            Arg::new("resume")
                .long("resume")
                .help(
                    "With --in-place, resume a stopped repair from --journal's FILE, the \
                     journal it left, instead of starting a new one",
                )
                .action(ArgAction::SetTrue)
                .requires("in_place"),
        )
        .group(
            ArgGroup::new("apply_target")
                .args(["to", "in_place"])
                .multiple(false),
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
             fsck.ntfs --dry-run --plan plan disk.img   and save them to the file plan\n  \
             fsck.ntfs -y --clear-dirty disk.img   and clear the dirty flag too\n  \
             fsck.ntfs --apply plan --to new.img disk.img   copy-repair into new.img\n  \
             fsck.ntfs --apply plan --in-place --journal /other/j disk.img   in place\n  \
             fsck.ntfs --text disk.img; echo $?",
        )
}

fn run(matches: &ArgMatches) -> Result<Outcome, CliError> {
    let target = matches
        .get_one::<OsString>("device")
        .expect("clap requires the device");
    if let Some(plan) = matches.get_one::<OsString>("apply") {
        if matches.get_flag("in_place") {
            let journal = matches
                .get_one::<OsString>("journal")
                .expect("clap requires --journal with --in-place");
            return apply_in_place(
                Path::new(target),
                Path::new(plan),
                Path::new(journal),
                matches.get_flag("resume"),
            );
        }
        let to = matches
            .get_one::<OsString>("to")
            .expect("clap requires --to with --apply");
        return apply(Path::new(target), Path::new(plan), Path::new(to));
    }
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
    let plan_file = matches.get_one::<OsString>("plan_file").map(Path::new);
    if let Some(file) = plan_file {
        check_plan_file(Path::new(target), file, dry_run)?;
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
    // Then saved to its own new file, never onto the volume (#906).
    if let (Some(file), Some(plan)) = (plan_file, &plan) {
        save_plan_file(&name, file, plan, &mut dev)?;
    }

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
    if let Some(file) = plan_file {
        fields.push(("plan_file", Json::from(file.display().to_string().as_str())));
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
    if let Some(file) = plan_file {
        text.push(format!("  plan saved to '{}'", file.display()));
    }
    Ok(Outcome::report(report)
        .with_text(text.join("\n"))
        .with_code(code))
}

/// Refuse `--plan FILE` before the volume is opened (#906): without
/// `--dry-run`, naming `target` itself, or naming anything that exists.
fn check_plan_file(target: &Path, file: &Path, dry_run: bool) -> Result<(), CliError> {
    let shown = file.display();
    if !dry_run {
        return Err(CliError::usage(format!(
            "--plan '{shown}' saves a dry run's plan, so it needs --dry-run: nothing is written \
             without it"
        ))
        .with_code(USAGE));
    }
    if file == target || same_file(file, target) {
        return Err(CliError::failed(format!(
            "--plan '{shown}' is the volume itself: the plan file goes outside the volume it \
             plans for, so nothing is written"
        ))
        .with_code(OPERATIONAL));
    }
    if file.symlink_metadata().is_ok() {
        return Err(CliError::failed(format!(
            "--plan '{shown}' already exists: the plan is saved only to a new file, so nothing \
             is written"
        ))
        .with_code(OPERATIONAL));
    }
    Ok(())
}

/// Save a dry run's `plan`, made from `dev`, to the new file `file`: a plan
/// the repair refuses saves nothing and is an operational error.
fn save_plan_file<T: fs_ntfs::block_io::BlockIo + ?Sized>(
    name: &str,
    file: &Path,
    plan: &Result<RepairPlan, String>,
    dev: &mut T,
) -> Result<(), CliError> {
    use std::io::Write;
    let shown = file.display();
    let failed = |why: String| CliError::failed(why).with_code(OPERATIONAL);
    let plan = plan.as_ref().map_err(|why| {
        failed(format!(
            "{name}: plan refused, no plan file written to '{shown}': {why}"
        ))
    })?;
    let bytes = save_plan(plan, dev)
        .map_err(|e| failed(format!("{name}: save the plan to '{shown}': {e}")))?;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)
        .map_err(|e| failed(format!("--plan '{shown}': {e}")))?;
    out.write_all(&bytes)
        .and_then(|()| out.sync_all())
        .map_err(|e| failed(format!("--plan '{shown}': write: {e}")))
}

/// Copy-repair `source` into a new `target` by the plan file at `plan`
/// (#910). A `target` that is the source, or that exists, is refused before
/// the plan is read; the library then refuses a source the plan does not fit
/// before `target` is created. The source is opened read-only throughout.
fn apply(source: &Path, plan: &Path, target: &Path) -> Result<Outcome, CliError> {
    let name = source.display().to_string();
    let to = target.display().to_string();
    let usage = |m: String| CliError::usage(m).with_code(USAGE);
    let failed = |m: String| CliError::failed(m).with_code(OPERATIONAL);
    if source == target || same_file(source, target) {
        return Err(usage(format!(
            "--to '{to}' is the source: copy-repair writes a new target and never its source"
        )));
    }
    if target.symlink_metadata().is_ok() {
        return Err(usage(format!(
            "--to '{to}' already exists: copy-repair writes only a new target, so nothing is \
             overwritten"
        )));
    }
    let plan_name = plan.display().to_string();
    let bytes = std::fs::read(plan).map_err(|e| failed(format!("read plan '{plan_name}': {e}")))?;
    let saved =
        load_plan(&bytes).map_err(|e| failed(format!("plan '{plan_name}' refused: {e}")))?;
    copy_repair(&saved, source, target).map_err(|e| match e {
        CopyRepairError::TargetExists(_) => usage(format!("{e}: nothing is overwritten")),
        CopyRepairError::Source(_) => failed(format!(
            "{name}: refused for plan '{plan_name}', no target created: {e}"
        )),
        CopyRepairError::Io(_) => failed(format!("{name}: copy-repair into '{to}': {e}")),
    })?;
    let units = saved.units.len() as u64;
    let writes = saved
        .units
        .iter()
        .map(|u| u.writes.len() as u64)
        .sum::<u64>();
    let report = Json::object([
        ("fs", Json::from("ntfs")),
        ("device", Json::from(name.as_str())),
        ("mode", Json::from("apply")),
        ("plan", Json::from(plan_name.as_str())),
        ("target", Json::from(to.as_str())),
        ("units", Json::from(units)),
        ("writes", Json::from(writes)),
        ("exit", Json::from(u64::from(CLEAN))),
    ]);
    let text = format!(
        "fsck.ntfs: {name}: plan '{plan_name}' applied to new target '{to}' ({units} units, \
         {writes} writes); {name} was opened read-only and is unchanged"
    );
    Ok(Outcome::report(report).with_text(text).with_code(CLEAN))
}

/// Set by SIGINT; an in-place apply stops before its next unit (#964).
#[cfg(unix)]
static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Watches an in-place apply from the CLI (#964): each progress report is
/// a line on stderr, and a SIGINT stops the apply before its next unit, so
/// the unit in hand is finished first.
#[cfg(unix)]
struct CliObserver {
    name: String,
    /// The harness build holds the apply once this many units are applied,
    /// until a signal arrives, so a test can send SIGINT at a known point.
    #[cfg(feature = "harness")]
    hold_after: Option<u32>,
}

#[cfg(unix)]
impl CliObserver {
    fn new(name: &str) -> Self {
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe.
        unsafe {
            let handler = on_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::signal(libc::SIGINT, handler);
        }
        Self {
            name: name.to_string(),
            #[cfg(feature = "harness")]
            hold_after: std::env::var("FSCK_NTFS_HOLD_AFTER_UNITS")
                .ok()
                .and_then(|units| units.parse().ok()),
        }
    }
}

#[cfg(unix)]
impl fs_ntfs::repair_journal::ApplyObserver for CliObserver {
    fn progress(&mut self, at: fs_ntfs::repair_journal::ApplyProgress) {
        eprintln!(
            "fsck.ntfs: {}: progress: {} of {} units, {} of {} bytes",
            self.name, at.units, at.units_total, at.bytes, at.bytes_total
        );
        #[cfg(feature = "harness")]
        if self.hold_after == Some(at.units) {
            let held = std::time::Instant::now();
            while !INTERRUPTED.load(std::sync::atomic::Ordering::Relaxed)
                && held.elapsed() < std::time::Duration::from_secs(60)
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    fn cancel_requested(&mut self) -> bool {
        INTERRUPTED.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Repair `device` in place by the plan file at `plan`, journalling into a
/// new file at `journal` (#922). The journal location is checked, the plan
/// loaded, the device opened exclusively and checked against the plan, all
/// before the journal is created; then the plan is applied through
/// `repair_journal::apply_in_place`. With `resume`, `journal` is the one a
/// stopped apply left (#963): it must exist and not be the device, and the
/// apply continues through `repair_journal::resume_in_place`, which checks
/// the journal against the plan and the device before writing.
#[cfg(unix)]
fn apply_in_place(
    device: &Path,
    plan: &Path,
    journal: &Path,
    resume: bool,
) -> Result<Outcome, CliError> {
    use fs_ntfs::repair_journal::{
        apply_in_place_observed, check_journal_location, resume_in_place_observed, InPlaceError,
    };
    let name = device.display().to_string();
    let journal_name = journal.display().to_string();
    let plan_name = plan.display().to_string();
    let usage = |m: String| CliError::usage(m).with_code(USAGE);
    let failed = |m: String| CliError::failed(m).with_code(OPERATIONAL);
    if !resume {
        check_journal_location(journal, device).map_err(|e| {
            usage(format!(
                "--journal '{journal_name}' refused, nothing written: {e}"
            ))
        })?;
    } else if journal.symlink_metadata().is_err() {
        return Err(usage(format!(
            "--resume: --journal '{journal_name}' does not exist, so there is no stopped \
             repair to resume; nothing written"
        )));
    } else if journal == device || same_file(journal, device) {
        return Err(usage(format!(
            "--resume: --journal '{journal_name}' is the volume itself; nothing written"
        )));
    }
    let bytes = std::fs::read(plan).map_err(|e| failed(format!("read plan '{plan_name}': {e}")))?;
    let saved =
        load_plan(&bytes).map_err(|e| failed(format!("plan '{plan_name}' refused: {e}")))?;
    let mut io = PathIo::open_exclusive(device)
        .map_err(|e| failed(format!("{name}: refused, nothing written: {e}")))?;
    if !resume {
        check_plan_source(&saved, &mut io).map_err(|e| {
            failed(format!(
                "{name}: refused for plan '{plan_name}', nothing written: {e}"
            ))
        })?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(journal)
            .map_err(|e| failed(format!("create journal '{journal_name}': {e}")))?;
    }
    let mut journal_io = PathIo::open_rw(journal).map_err(failed)?;
    let sha: [u8; 32] = Sha256::digest(&bytes).into();
    let mut observer = CliObserver::new(&name);
    let done = if resume {
        resume_in_place_observed(&saved, sha, &mut io, &mut journal_io, &mut observer)
    } else {
        apply_in_place_observed(&saved, sha, &mut io, &mut journal_io, &mut observer)
    };
    let units = saved.units.len() as u64;
    done.map_err(|e| match e {
        InPlaceError::Cancelled { applied } => failed(format!(
            "{name}: interrupted: stopped after {applied} of {units} units, each finished; \
             --apply '{plan_name}' --in-place --resume --journal '{journal_name}' finishes it"
        )),
        e => failed(format!(
            "{name}: in-place repair stopped: {e}; the journal '{journal_name}' holds how far \
             it got"
        )),
    })?;
    let writes = saved
        .units
        .iter()
        .map(|u| u.writes.len() as u64)
        .sum::<u64>();
    let report = Json::object([
        ("fs", Json::from("ntfs")),
        ("device", Json::from(name.as_str())),
        ("mode", Json::from("apply-in-place")),
        ("plan", Json::from(plan_name.as_str())),
        ("journal", Json::from(journal_name.as_str())),
        ("resumed", Json::from(resume)),
        ("units", Json::from(units)),
        ("writes", Json::from(writes)),
        ("exit", Json::from(u64::from(CLEAN))),
    ]);
    let how = if resume {
        "resumed and applied"
    } else {
        "applied"
    };
    let text = format!(
        "fsck.ntfs: {name}: plan '{plan_name}' {how} in place ({units} units, {writes} \
         writes), journalled into '{journal_name}'"
    );
    Ok(Outcome::report(report).with_text(text).with_code(CLEAN))
}

/// An in-place repair needs the journal location check, which is Unix-only.
#[cfg(not(unix))]
fn apply_in_place(
    _device: &Path,
    _plan: &Path,
    _journal: &Path,
    _resume: bool,
) -> Result<Outcome, CliError> {
    Err(CliError::usage(String::from("--in-place is supported on Unix only")).with_code(USAGE))
}

/// Whether `a` and `b` name the same existing file: on Unix the same device
/// and inode, so a hard link or a symlink to the source is caught too.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Whether `a` and `b` name the same existing file, by canonical path.
#[cfg(not(unix))]
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
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
