//! `fs.ntfs <target> inventory`: one bounded, read-only batch of the MFT,
//! as JSON Lines.
//!
//! A batch examines at most `--max-slots` MFT slots from `--cursor`, unused
//! slots included, so the bound limits work rather than results. It is a
//! batch, not a snapshot: nothing makes successive batches of a live
//! volume atomic. The last line always says whether the table is finished
//! and, if not, the cursor that resumes it.

use clap::{value_parser, Arg, ArgMatches, Command as Cmd};

use fs_core::cli::{CliError, Json, Outcome};
use fs_ntfs::block_io::BlockIo;
use fs_ntfs::mft_inventory::{
    inventory_batch_cancellable_io, InventoryBatch, InventoryCursor, InventoryOutcome,
};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by SIGINT or SIGTERM; the batch stops before its next record read.
static CANCELLED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_cancel_signal(_: libc::c_int) {
    CANCELLED.store(true, Ordering::Relaxed);
}

/// Route Ctrl-C and SIGTERM to the cancellation flag, so an interrupted
/// batch still prints its records and a cursor that resumes it.
fn install_cancel_handler() {
    #[cfg(unix)]
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        let handler = on_cancel_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

pub fn command() -> Cmd {
    Cmd::new("inventory")
        .about("List one bounded batch of in-use MFT records as JSON Lines, read-only")
        .long_about(
            "Examine at most --max-slots MFT slots starting at --cursor, used or not, and \
             print one JSON object per line: a \"record\" line for each in-use record, a \
             \"damaged\" line for each record that failed its fixup check, then one \
             \"batch\" line. The batch line's \"finished\" is true once the table is \
             exhausted; otherwise \"next_cursor\" is the first slot not examined, to pass \
             back as --cursor.\n\n\
             File references, base-record references, record numbers and cursors are \
             decimal strings, so a 64-bit value survives a JSON reader that holds numbers \
             as doubles. Nothing is written to the volume. Batches are not a snapshot: a \
             volume that changes between batches gives no atomic view.",
        )
        .arg(
            Arg::new("max-slots")
                .long("max-slots")
                .value_name("N")
                .help("The most MFT slots this batch examines, unused ones included")
                .value_parser(value_parser!(u64).range(1..))
                .required(true),
        )
        .arg(
            Arg::new("cursor")
                .long("cursor")
                .value_name("SLOT")
                .help("The first slot to examine: 0, or a previous batch's next_cursor")
                .value_parser(value_parser!(u64))
                .default_value("0"),
        )
        .after_help(
            "Examples:\n  fs.ntfs disk.img inventory --max-slots 1024\n  \
             fs.ntfs disk.img inventory --max-slots 1024 --cursor 1024",
        )
}

pub fn run<T: BlockIo + ?Sized>(io: &mut T, sub: &ArgMatches) -> Result<Outcome, CliError> {
    let max_slots = *sub.get_one::<u64>("max-slots").expect("clap requires it");
    let cursor = *sub.get_one::<u64>("cursor").expect("clap defaults it");
    install_cancel_handler();
    let cancelled = || CANCELLED.load(Ordering::Relaxed);
    let mut out = String::new();
    for line in lines_cancellable(io, cursor, max_slots, &cancelled)? {
        out.push_str(&line.to_compact());
        out.push('\n');
    }
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| CliError::failed(format!("write: {e}")))?;
    Ok(Outcome::done())
}

/// The batch's lines, in output order: records, damaged records, the batch.
pub fn lines<T: BlockIo + ?Sized>(
    io: &mut T,
    cursor: u64,
    max_slots: u64,
) -> Result<Vec<Json>, CliError> {
    lines_cancellable(io, cursor, max_slots, &|| false)
}

/// As [`lines`], but `cancelled` is polled before every record read. A
/// cancelled batch prints what it read and a batch line with
/// `"interrupted": true`, `"finished": false` and the first unread slot as
/// `next_cursor`, so resuming from it loses and repeats nothing.
pub fn lines_cancellable<T: BlockIo + ?Sized>(
    io: &mut T,
    cursor: u64,
    max_slots: u64,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<Json>, CliError> {
    let start = InventoryCursor {
        next_record: cursor,
    };
    let outcome = inventory_batch_cancellable_io(io, start, max_slots, cancelled)
        .map_err(|e| CliError::failed(format!("inventory from slot {cursor}: {e}")))?;
    Ok(match outcome {
        InventoryOutcome::Complete(batch) => render(&batch),
        InventoryOutcome::Interrupted { partial, resume } => {
            // Cancelled before any read: the table's size is not known.
            let total_known = partial.is_some();
            let batch = partial.unwrap_or(InventoryBatch {
                entries: Vec::new(),
                slots_visited: 0,
                unused_slots: 0,
                damaged_records: Vec::new(),
                next: None,
                total_slots: 0,
            });
            render_with(
                &InventoryBatch {
                    next: Some(resume),
                    ..batch
                },
                true,
                total_known,
            )
        }
    })
}

fn render(batch: &InventoryBatch) -> Vec<Json> {
    render_with(batch, false, true)
}

fn render_with(batch: &InventoryBatch, interrupted: bool, total_known: bool) -> Vec<Json> {
    let text = |n: u64| Json::from(n.to_string());
    let mut out: Vec<Json> = batch
        .entries
        .iter()
        .map(|e| {
            Json::object([
                ("type", Json::from("record")),
                ("record", text(e.record_number)),
                ("sequence", Json::from(u64::from(e.sequence))),
                ("file_reference", text(e.file_reference)),
                ("base_reference", text(e.base_reference)),
                ("directory", Json::from(e.is_directory)),
            ])
        })
        .collect();
    out.extend(
        batch
            .damaged_records
            .iter()
            .map(|&r| Json::object([("type", Json::from("damaged")), ("record", text(r))])),
    );
    out.push(Json::object([
        ("type", Json::from("batch")),
        ("slots_visited", Json::from(batch.slots_visited)),
        ("unused_slots", Json::from(batch.unused_slots)),
        (
            "total_slots",
            if total_known {
                text(batch.total_slots)
            } else {
                Json::Null
            },
        ),
        ("finished", Json::from(batch.next.is_none())),
        ("interrupted", Json::from(interrupted)),
        (
            "next_cursor",
            batch.next.map_or(Json::Null, |c| text(c.next_record)),
        ),
    ]));
    out
}

#[cfg(test)]
mod tests {
    //! Against the Windows-made inventory fixture and the references
    //! `fsutil file queryfileid` reported for it (#609), so the CLI is
    //! graded by Windows, not by the library it wraps.

    use super::*;
    use fs_ntfs::block_io::PathIo;
    use std::path::Path;

    const WINDOWS_IMG: &str = "test-disks/ntfs-inventory.img";
    const WINDOWS_IDS: &str = "test-disks/ntfs-inventory.fileids.txt";

    fn fixture() -> PathIo {
        for path in [WINDOWS_IMG, WINDOWS_IDS] {
            assert!(
                Path::new(path).exists(),
                "missing {path}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
            );
        }
        PathIo::open_ro(Path::new(WINDOWS_IMG)).expect("open_ro")
    }

    fn windows_references() -> Vec<u64> {
        std::fs::read_to_string(WINDOWS_IDS)
            .expect("read the fsutil answers")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                let hex = line.rsplit("0x").next().expect("a file ID").trim();
                let id = u128::from_str_radix(hex, 16).expect("hexadecimal file ID");
                assert_eq!(id >> 64, 0, "{line}: NTFS file IDs have a zero high half");
                id as u64
            })
            .collect()
    }

    fn str_field<'a>(line: &'a Json, key: &str) -> &'a str {
        match line.get(key) {
            Some(Json::Str(s)) => s,
            other => panic!("{key} is {other:?}, not a string, in {}", line.to_compact()),
        }
    }

    fn kind(line: &Json) -> &str {
        str_field(line, "type")
    }

    fn parse(args: &[&str]) -> Result<ArgMatches, clap::Error> {
        command().try_get_matches_from(std::iter::once("inventory").chain(args.iter().copied()))
    }

    #[test]
    fn max_slots_is_required_and_zero_is_refused() {
        assert!(parse(&[]).is_err(), "no --max-slots");
        assert!(parse(&["--max-slots", "0"]).is_err(), "a zero bound");
        assert!(parse(&["--max-slots", "-1"]).is_err(), "a negative bound");
        assert!(parse(&["--max-slots", "4", "--cursor", "x"]).is_err());
        let m = parse(&["--max-slots", "4"]).expect("a bound alone");
        assert_eq!(
            m.get_one::<u64>("cursor"),
            Some(&0),
            "the cursor starts at 0"
        );
        let m = parse(&["--max-slots", "4", "--cursor", "18446744073709551615"]).expect("u64");
        assert_eq!(m.get_one::<u64>("cursor"), Some(&u64::MAX));
    }

    #[test]
    fn every_batch_ends_with_one_batch_line_that_bounds_examined_slots() {
        let mut io = fixture();
        let out = lines(&mut io, 0, 5).expect("batch");
        let last = out.last().expect("a batch line");
        assert_eq!(kind(last), "batch");
        assert_eq!(out.iter().filter(|l| kind(l) == "batch").count(), 1);
        assert_eq!(last.get("slots_visited"), Some(&Json::UInt(5)));
        assert_eq!(last.get("finished"), Some(&Json::Bool(false)));
        assert_eq!(str_field(last, "next_cursor"), "5");
        let records = out.iter().filter(|l| kind(l) == "record").count() as u64;
        let Some(Json::UInt(unused)) = last.get("unused_slots") else {
            panic!("unused_slots is a number");
        };
        assert!(
            records + unused <= 5,
            "used plus unused slots exceed the bound"
        );
    }

    /// Resumed batches, cursor to cursor, list each record exactly once,
    /// end with `finished` and no cursor, and between them carry every
    /// reference Windows reports, as the decimal string of its full 64
    /// bits. The image is not written.
    #[test]
    fn resumed_batches_carry_every_windows_reference_losslessly_and_write_nothing() {
        let before = std::fs::read(WINDOWS_IMG).expect("read image");
        let mut io = fixture();
        let mut cursor = 0u64;
        let mut references = Vec::new();
        let mut records = Vec::new();
        let mut batches = 0;
        loop {
            batches += 1;
            assert!(batches < 1 << 20, "the cursor never finished");
            let out = lines(&mut io, cursor, 3).expect("batch");
            for line in &out {
                if kind(line) != "record" {
                    continue;
                }
                let record: u64 = str_field(line, "record").parse().expect("decimal");
                let reference: u64 = str_field(line, "file_reference").parse().expect("u64");
                let base: u64 = str_field(line, "base_reference").parse().expect("u64");
                let Some(Json::UInt(sequence)) = line.get("sequence") else {
                    panic!("sequence is a number");
                };
                assert_eq!(reference, record | (sequence << 48));
                assert!(base == 0 || base & ((1 << 48) - 1) != record);
                assert!(record >= cursor && record < cursor + 3, "outside the batch");
                records.push(record);
                references.push(reference);
            }
            let last = out.last().expect("a batch line");
            match last.get("finished") {
                Some(Json::Bool(true)) => {
                    assert_eq!(last.get("next_cursor"), Some(&Json::Null));
                    break;
                }
                Some(Json::Bool(false)) => {
                    let next: u64 = str_field(last, "next_cursor").parse().expect("decimal");
                    assert!(next > cursor, "the cursor did not advance");
                    cursor = next;
                }
                other => panic!("finished is {other:?}"),
            }
        }
        let mut unique = records.clone();
        unique.dedup();
        assert_eq!(unique, records, "a record was listed twice or out of order");
        let windows = windows_references();
        assert!(windows.len() >= 9, "eight reused files and a directory");
        for reference in &windows {
            assert!(
                references.contains(reference),
                "Windows reports {reference} ({reference:#018x}); the CLI does not list it"
            );
        }
        assert!(
            windows.iter().any(|r| r >> 48 > 1),
            "slot reuse left at least one sequence above one"
        );
        drop(io);
        assert_eq!(before, std::fs::read(WINDOWS_IMG).expect("read image"));
    }

    #[test]
    fn a_cursor_past_the_table_is_refused_not_reported_finished() {
        let mut io = fixture();
        let err = lines(&mut io, MAX_PAST, 4).expect_err("a cursor past the table");
        assert!(err.to_json().to_compact().contains("inventory from slot"));
    }

    const MAX_PAST: u64 = 1 << 40;

    /// Records plus the batch line, from a run that may be cancelled.
    fn records_of(out: &[Json]) -> Vec<String> {
        out.iter()
            .filter(|l| kind(l) == "record")
            .map(|l| str_field(l, "file_reference").to_string())
            .collect()
    }

    /// A cancellation that is already pending when the batch starts reads
    /// nothing, reports it was interrupted, and resumes at the same cursor.
    #[test]
    fn a_batch_cancelled_before_it_starts_reads_nothing_and_resumes_in_place() {
        let mut io = fixture();
        let polls = std::cell::Cell::new(0u32);
        let out = lines_cancellable(&mut io, 4, 8, &|| {
            polls.set(polls.get() + 1);
            true
        })
        .expect("an interrupted batch is not an error");
        assert_eq!(polls.get(), 1, "polled once, before any read");
        assert_eq!(out.len(), 1, "only the batch line: {out:?}");
        let last = &out[0];
        assert_eq!(kind(last), "batch");
        assert_eq!(last.get("interrupted"), Some(&Json::Bool(true)));
        assert_eq!(last.get("finished"), Some(&Json::Bool(false)));
        assert_eq!(last.get("slots_visited"), Some(&Json::UInt(0)));
        assert_eq!(last.get("total_slots"), Some(&Json::Null));
        assert_eq!(str_field(last, "next_cursor"), "4");
    }

    /// A batch cancelled part-way, then resumed from its `next_cursor`,
    /// lists exactly the records one uncancelled batch lists: none lost,
    /// none repeated. The image is not written.
    #[test]
    fn a_batch_cancelled_mid_way_resumes_without_loss_or_repeat() {
        let before = std::fs::read(WINDOWS_IMG).expect("read image");
        let mut io = fixture();
        let whole = lines(&mut io, 0, 40).expect("uncancelled batch");
        assert_eq!(
            whole.last().and_then(|l| l.get("interrupted")),
            Some(&Json::Bool(false))
        );
        let polls = std::cell::Cell::new(0u32);
        let first = lines_cancellable(&mut io, 0, 40, &|| {
            polls.set(polls.get() + 1);
            polls.get() > 12
        })
        .expect("interrupted batch");
        let stop = first.last().expect("a batch line");
        assert_eq!(stop.get("interrupted"), Some(&Json::Bool(true)));
        assert_eq!(stop.get("finished"), Some(&Json::Bool(false)));
        let resume: u64 = str_field(stop, "next_cursor").parse().expect("decimal");
        assert!(resume > 0 && resume < 40, "stopped mid-batch at {resume}");
        let Some(Json::UInt(visited)) = stop.get("slots_visited") else {
            panic!("slots_visited is a number");
        };
        assert_eq!(
            *visited, resume,
            "every slot before the cursor was examined"
        );
        let rest = lines(&mut io, resume, 40 - resume).expect("resumed batch");
        let mut joined = records_of(&first);
        joined.extend(records_of(&rest));
        assert_eq!(joined, records_of(&whole), "lost or repeated records");
        drop(io);
        assert_eq!(before, std::fs::read(WINDOWS_IMG).expect("read image"));
    }

    #[test]
    fn damaged_records_are_reported_as_lines() {
        let batch = InventoryBatch {
            entries: Vec::new(),
            slots_visited: 2,
            unused_slots: 0,
            damaged_records: vec![7, u64::from(u32::MAX) + 1],
            next: None,
            total_slots: 9,
        };
        let out = render(&batch);
        let damaged: Vec<_> = out
            .iter()
            .filter(|l| kind(l) == "damaged")
            .map(|l| str_field(l, "record"))
            .collect();
        assert_eq!(damaged, ["7", "4294967296"]);
        assert_eq!(
            out.last().and_then(|l| l.get("finished")),
            Some(&Json::Bool(true))
        );
    }

    #[test]
    fn a_reference_above_two_to_the_53_survives_as_text() {
        let reference = (0xFFFFu64 << 48) | 5;
        let batch = InventoryBatch {
            entries: vec![fs_ntfs::mft_inventory::InventoryEntry {
                record_number: 5,
                sequence: 0xFFFF,
                file_reference: reference,
                is_directory: false,
                base_reference: (3u64 << 48) | 4,
            }],
            slots_visited: 1,
            unused_slots: 0,
            damaged_records: Vec::new(),
            next: Some(InventoryCursor { next_record: 6 }),
            total_slots: 9,
        };
        let out = render(&batch);
        assert_eq!(str_field(&out[0], "file_reference"), reference.to_string());
        assert_eq!(
            str_field(&out[0], "base_reference"),
            ((3u64 << 48) | 4).to_string()
        );
        assert_eq!(out[0].get("sequence"), Some(&Json::UInt(0xFFFF)));
        assert_eq!(str_field(&out[1], "next_cursor"), "6");
    }
}
