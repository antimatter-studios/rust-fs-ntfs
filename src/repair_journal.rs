//! Repair journals: an append-only external file of checksummed records
//! that says how far an in-place repair got (#713, #918).
//!
//! A journal is bound to one plan file and one source by its header
//! record. Each unit of the plan then gets an intent record, appended and
//! made durable before the unit is written, carrying every write's offset,
//! preimage and redo; and an applied record once the unit is written and
//! flushed. The journal lives outside the volume being repaired, never on
//! it, and it is not a `$LogFile` transaction.
//!
//! Reading a journal back returns its records in order, up to the last
//! whole one. A final record cut short -- a torn append -- is reported and
//! ignored, never applied; anything else wrong with the journal refuses
//! it, naming the record. The format is docs/repair-journal-file.md.
//!
//! Everything here is behind the `plan-file` feature.

use crate::block_io::BlockIo;
use crate::fsck::{check_io, CheckReport};
use crate::repair_plan::{
    check_unit_on_device, load_plan, PlanFileError, PlanSourceError, PlanUnit, PlannedWrite,
    SavedPlan,
};
use sha2::{Digest, Sha256};

/// The journal format this reader writes and reads.
pub const JOURNAL_VERSION: u32 = 1;

/// The first bytes of the header record's payload.
const JOURNAL_MAGIC: &[u8; 8] = b"NTFSJRNL";

/// A record's type and payload length, before its payload.
const RECORD_PREFIX: usize = 4 + 4;

const SHA256_LEN: usize = 32;

const HEADER: u32 = 1;
const INTENT: u32 = 2;
const APPLIED: u32 = 3;

/// The plan file and source a journal is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalBinding {
    /// The SHA-256 of the whole plan file.
    pub plan_sha256: [u8; SHA256_LEN],
    /// The source's size in bytes, as the plan file records it.
    pub source_size: u64,
    /// The source's volume serial number, as the plan file records it.
    pub volume_serial: u64,
}

impl JournalBinding {
    /// The binding for the plan file `plan_file`, which must load.
    pub fn of(plan_file: &[u8]) -> Result<Self, PlanFileError> {
        let plan = load_plan(plan_file)?;
        Ok(Self {
            plan_sha256: Sha256::digest(plan_file).into(),
            source_size: plan.source_size,
            volume_serial: plan.volume_serial,
        })
    }
}

/// One record of a journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalRecord {
    /// The plan file and source the journal is for; always the first.
    Header(JournalBinding),
    /// Unit `unit` is about to be written: each write's offset, its
    /// preimage (`before`) and its redo (`after`).
    Intent {
        unit: u32,
        writes: Vec<PlannedWrite>,
    },
    /// Unit `unit` is written and flushed.
    Applied { unit: u32 },
}

/// A final record cut short: where it starts, ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TornRecord {
    /// Its index: the number of whole records before it.
    pub record: usize,
    /// Its first byte in the journal.
    pub offset: u64,
}

/// A journal read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journal {
    /// Every whole record, in order.
    pub records: Vec<JournalRecord>,
    /// The final record, when a torn append cut it short.
    pub torn: Option<TornRecord>,
}

/// Why a journal was refused: the record, where it starts, and what is
/// wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalError {
    pub record: usize,
    pub offset: u64,
    pub fault: JournalFault,
}

/// What is wrong with a refused record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalFault {
    /// Its checksum does not match its bytes.
    ChecksumMismatch,
    /// A record type this reader does not know.
    UnknownType(u32),
    /// Its payload does not have the shape its type gives it.
    Malformed(&'static str),
    /// The first record is not a journal header.
    NotAJournal,
    /// A journal format version this reader does not know.
    UnsupportedVersion(u32),
    /// The header names a plan file other than the one given.
    OtherPlan,
    /// The header names a source other than the plan's.
    OtherSource,
    /// A header after the first record.
    SecondHeader,
    /// An intent while the unit before it has no applied record.
    IntentWhileOpen { unit: u32, open: u32 },
    /// An intent for a unit that is not the next one.
    UnitOutOfOrder { unit: u32, expected: u32 },
    /// An applied record with no intent before it.
    AppliedWithoutIntent { unit: u32 },
    /// An applied record for a unit other than the one its intent names.
    AppliedForAnotherUnit { unit: u32, open: u32 },
    /// A final record cut short that is not the start of the record the
    /// plan expects next, so it was not left by a torn append (#934).
    TornTailDiffers,
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "journal record {}: ", self.record)?;
        match &self.fault {
            JournalFault::ChecksumMismatch => write!(f, "checksum does not match its contents")?,
            JournalFault::UnknownType(t) => write!(f, "unknown record type {t}")?,
            JournalFault::Malformed(what) => write!(f, "malformed: {what}")?,
            JournalFault::NotAJournal => write!(f, "not a repair journal header")?,
            JournalFault::UnsupportedVersion(v) => write!(
                f,
                "journal format version {v} is not supported (this reader knows version {JOURNAL_VERSION})"
            )?,
            JournalFault::OtherPlan => write!(f, "the header names another plan file")?,
            JournalFault::OtherSource => write!(f, "the header names another source")?,
            JournalFault::SecondHeader => write!(f, "a second header")?,
            JournalFault::IntentWhileOpen { unit, open } => write!(
                f,
                "intent for unit {unit} while unit {open} has no applied record"
            )?,
            JournalFault::UnitOutOfOrder { unit, expected } => {
                write!(f, "intent for unit {unit}, the next unit is {expected}")?
            }
            JournalFault::AppliedWithoutIntent { unit } => {
                write!(f, "applied record for unit {unit} with no intent before it")?
            }
            JournalFault::AppliedForAnotherUnit { unit, open } => write!(
                f,
                "applied record for unit {unit}, the open intent is unit {open}"
            )?,
            JournalFault::TornTailDiffers => write!(
                f,
                "cut short, and not the start of the record the plan expects next"
            )?,
        }
        write!(f, " (at byte {})", self.offset)
    }
}

impl std::error::Error for JournalError {}

/// The bytes of a header record binding a journal to `binding`.
pub fn header_record(binding: &JournalBinding) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + 4 + SHA256_LEN + 8 + 8);
    payload.extend_from_slice(JOURNAL_MAGIC);
    payload.extend_from_slice(&JOURNAL_VERSION.to_le_bytes());
    payload.extend_from_slice(&binding.plan_sha256);
    payload.extend_from_slice(&binding.source_size.to_le_bytes());
    payload.extend_from_slice(&binding.volume_serial.to_le_bytes());
    record(HEADER, &payload)
}

/// The bytes of the intent record for unit `index`, `unit`: every
/// write's offset, before-image as its preimage and after-image as its
/// redo. A plan file already holds each write's length in 32 bits and
/// as many bytes after as before, so a loaded unit always fits.
pub fn intent_record(index: u32, unit: &PlanUnit) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&index.to_le_bytes());
    let writes =
        u32::try_from(unit.writes.len()).expect("a plan unit holds at most u32::MAX writes");
    payload.extend_from_slice(&writes.to_le_bytes());
    for w in &unit.writes {
        assert_eq!(
            w.before.len(),
            w.after.len(),
            "the write at {} has as many bytes after as before",
            w.offset
        );
        let length = u32::try_from(w.before.len()).expect("a planned write is under 4 GiB");
        payload.extend_from_slice(&w.offset.to_le_bytes());
        payload.extend_from_slice(&length.to_le_bytes());
        payload.extend_from_slice(&w.before);
        payload.extend_from_slice(&w.after);
    }
    record(INTENT, &payload)
}

/// The bytes of the applied record for unit `index`.
pub fn applied_record(index: u32) -> Vec<u8> {
    record(APPLIED, &index.to_le_bytes())
}

/// Type, payload length, payload, and the SHA-256 of all three.
fn record(kind: u32, payload: &[u8]) -> Vec<u8> {
    let length = u32::try_from(payload.len()).expect("a journal record is under 4 GiB");
    let mut out = Vec::with_capacity(RECORD_PREFIX + payload.len() + SHA256_LEN);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(payload);
    let sum = Sha256::digest(&out);
    out.extend_from_slice(&sum);
    out
}

/// Read a journal back against the plan it must be for. Returns every
/// whole record in order and, when the final record is cut short, where
/// it starts; refuses the journal, naming the record, for anything else.
pub fn read_journal(bytes: &[u8], binding: &JournalBinding) -> Result<Journal, JournalError> {
    let mut records = Vec::new();
    let mut at = 0usize;
    let mut next_unit = 0u32;
    let mut open: Option<u32> = None;
    while at < bytes.len() {
        let index = records.len();
        let refuse = |fault| JournalError {
            record: index,
            offset: at as u64,
            fault,
        };
        let rest = &bytes[at..];
        let torn = Journal {
            records: Vec::new(),
            torn: Some(TornRecord {
                record: index,
                offset: at as u64,
            }),
        };
        if rest.len() < RECORD_PREFIX {
            return Ok(Journal { records, ..torn });
        }
        let kind = u32::from_le_bytes(rest[0..4].try_into().unwrap());
        let length = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let Some(end) = RECORD_PREFIX
            .checked_add(length)
            .and_then(|n| n.checked_add(SHA256_LEN))
            .filter(|&end| end <= rest.len())
        else {
            return Ok(Journal { records, ..torn });
        };
        let (body, sum) = rest[..end].split_at(end - SHA256_LEN);
        if Sha256::digest(body).as_slice() != sum {
            return Err(refuse(JournalFault::ChecksumMismatch));
        }
        let payload = &body[RECORD_PREFIX..];
        let parsed = match kind {
            HEADER => parse_header(payload),
            INTENT => parse_intent(payload),
            APPLIED => parse_applied(payload),
            _ if index == 0 => Err(JournalFault::NotAJournal),
            other => Err(JournalFault::UnknownType(other)),
        }
        .map_err(refuse)?;
        match (&parsed, index) {
            (JournalRecord::Header(named), 0) => {
                if named.plan_sha256 != binding.plan_sha256 {
                    return Err(refuse(JournalFault::OtherPlan));
                }
                if named.source_size != binding.source_size
                    || named.volume_serial != binding.volume_serial
                {
                    return Err(refuse(JournalFault::OtherSource));
                }
            }
            (_, 0) => return Err(refuse(JournalFault::NotAJournal)),
            (JournalRecord::Header(_), _) => return Err(refuse(JournalFault::SecondHeader)),
            (&JournalRecord::Intent { unit, .. }, _) => {
                if let Some(open) = open {
                    return Err(refuse(JournalFault::IntentWhileOpen { unit, open }));
                }
                if unit != next_unit {
                    return Err(refuse(JournalFault::UnitOutOfOrder {
                        unit,
                        expected: next_unit,
                    }));
                }
                open = Some(unit);
            }
            (&JournalRecord::Applied { unit }, _) => match open {
                None => return Err(refuse(JournalFault::AppliedWithoutIntent { unit })),
                Some(o) if o != unit => {
                    return Err(refuse(JournalFault::AppliedForAnotherUnit {
                        unit,
                        open: o,
                    }))
                }
                Some(_) => {
                    open = None;
                    next_unit += 1;
                }
            },
        }
        records.push(parsed);
        at += end;
    }
    Ok(Journal {
        records,
        torn: None,
    })
}

fn parse_header(payload: &[u8]) -> Result<JournalRecord, JournalFault> {
    let mut r = Fields(payload);
    if r.take(8) != Some(&JOURNAL_MAGIC[..]) {
        return Err(JournalFault::NotAJournal);
    }
    let version = r
        .u32()
        .ok_or(JournalFault::Malformed("a header shorter than its fields"))?;
    if version != JOURNAL_VERSION {
        return Err(JournalFault::UnsupportedVersion(version));
    }
    let short = JournalFault::Malformed("a header shorter than its fields");
    let plan_sha256 = r.take(SHA256_LEN).ok_or(short.clone())?.try_into().unwrap();
    let source_size = r.u64().ok_or(short.clone())?;
    let volume_serial = r.u64().ok_or(short)?;
    r.end("a header longer than its fields")?;
    Ok(JournalRecord::Header(JournalBinding {
        plan_sha256,
        source_size,
        volume_serial,
    }))
}

fn parse_intent(payload: &[u8]) -> Result<JournalRecord, JournalFault> {
    let short = JournalFault::Malformed("an intent shorter than its writes");
    let mut r = Fields(payload);
    let unit = r.u32().ok_or(short.clone())?;
    let count = r.u32().ok_or(short.clone())?;
    let mut writes = Vec::new();
    for _ in 0..count {
        let offset = r.u64().ok_or(short.clone())?;
        let length = r.u32().ok_or(short.clone())? as usize;
        let before = r.take(length).ok_or(short.clone())?.to_vec();
        let after = r.take(length).ok_or(short.clone())?.to_vec();
        writes.push(PlannedWrite {
            offset,
            before,
            after,
        });
    }
    r.end("an intent longer than its writes")?;
    Ok(JournalRecord::Intent { unit, writes })
}

fn parse_applied(payload: &[u8]) -> Result<JournalRecord, JournalFault> {
    let mut r = Fields(payload);
    let unit = r.u32().ok_or(JournalFault::Malformed(
        "an applied record without its unit",
    ))?;
    r.end("an applied record longer than its unit")?;
    Ok(JournalRecord::Applied { unit })
}

/// Reads little-endian fields from the front of a payload; `None` past
/// its end.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (taken, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn end(&self, what: &'static str) -> Result<(), JournalFault> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(JournalFault::Malformed(what))
        }
    }
}

/// Where an in-place apply stopped, as its journal says (#926).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Units `0..applied` each have an applied record.
    pub applied: u32,
    /// The unit whose intent record is in the journal with no applied
    /// record after it; always unit `applied` when there is one.
    pub open: Option<u32>,
}

/// Why a journal's records do not fit the plan it is read against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressError {
    /// An intent for a unit the plan, of `units` units, does not hold.
    UnitNotInPlan { unit: u32, units: usize },
    /// An intent with `journal` writes for a plan unit of `plan` writes.
    WriteCount {
        unit: u32,
        journal: usize,
        plan: usize,
    },
    /// An intent whose write `write` has another offset, preimage or redo
    /// than the plan unit's.
    WriteDiffers { unit: u32, write: usize },
}

impl std::fmt::Display for ProgressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnitNotInPlan { unit, units } => write!(
                f,
                "plan unit {unit}: the journal holds an intent for it, and the plan holds {units} units"
            ),
            Self::WriteCount {
                unit,
                journal,
                plan,
            } => write!(
                f,
                "plan unit {unit}: its intent record holds {journal} writes, and the plan unit {plan}"
            ),
            Self::WriteDiffers { unit, write } => write!(
                f,
                "plan unit {unit} write {write}: its intent record holds another offset, preimage or redo than the plan"
            ),
        }
    }
}

impl std::error::Error for ProgressError {}

/// Where the in-place apply that wrote `journal`, already accepted by
/// [`read_journal`], stopped against `plan`: the units it marked applied
/// and the unit it left open. Every intent must carry exactly its plan
/// unit's writes, or the journal is refused naming the unit and write. A
/// journal with no whole record has nothing applied and nothing open.
/// Reads no device.
pub fn progress(journal: &Journal, plan: &SavedPlan) -> Result<Progress, ProgressError> {
    let mut at = Progress {
        applied: 0,
        open: None,
    };
    for record in &journal.records {
        match record {
            JournalRecord::Header(_) => {}
            JournalRecord::Intent { unit, writes } => {
                let planned =
                    plan.units
                        .get(*unit as usize)
                        .ok_or(ProgressError::UnitNotInPlan {
                            unit: *unit,
                            units: plan.units.len(),
                        })?;
                if writes.len() != planned.writes.len() {
                    return Err(ProgressError::WriteCount {
                        unit: *unit,
                        journal: writes.len(),
                        plan: planned.writes.len(),
                    });
                }
                if let Some(write) = writes.iter().zip(&planned.writes).position(|(j, p)| j != p) {
                    return Err(ProgressError::WriteDiffers { unit: *unit, write });
                }
                at.open = Some(*unit);
            }
            JournalRecord::Applied { .. } => {
                at.open = None;
                at.applied += 1;
            }
        }
    }
    Ok(at)
}

/// Why an in-place repair was refused, or stopped. Every unit before the
/// one named is written, read back and marked applied in the journal; the
/// unit named is not marked applied.
#[derive(Debug)]
pub enum InPlaceError {
    /// The device does not hold the unit's before-image; nothing of the
    /// unit was written.
    Device(PlanSourceError),
    /// A record appended to the journal did not read back as written; the
    /// unit it is for was not written. `unit` is `None` for the header.
    JournalReadBack { unit: Option<u32>, offset: u64 },
    /// The device read write `write` of unit `unit` back different from
    /// its redo, in bytes `start..end`. For a unit a resume finds marked
    /// applied, it differs from what the applied units leave there, and
    /// `start..end` runs from the first byte that differs to the last; the
    /// device and the journal are left as the resume found them.
    DeviceReadBack {
        unit: u32,
        write: usize,
        start: u64,
        end: u64,
    },
    /// A resumed unit's write `write` holds neither its preimage nor its
    /// redo in bytes `start..end`, the first such byte to the last (#929);
    /// nothing of the unit was written again, and nothing was appended to
    /// the journal.
    OpenUnitRange {
        unit: u32,
        write: usize,
        start: u64,
        end: u64,
    },
    /// The journal a resume was given was refused.
    Journal(JournalError),
    /// The journal a resume was given does not fit the plan.
    Progress(ProgressError),
    /// Writing, reading or flushing the device or the journal failed.
    Io(String),
    /// The observer asked for a stop (#956). Units `0..applied` are
    /// written, read back and marked applied; no unit is open, and the
    /// journal resumes the apply from unit `applied`.
    Cancelled { applied: u32 },
}

impl std::fmt::Display for InPlaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Device(e) => write!(f, "{e}"),
            Self::JournalReadBack { unit: None, offset } => write!(
                f,
                "the journal header did not read back as written (at byte {offset})"
            ),
            Self::JournalReadBack {
                unit: Some(unit),
                offset,
            } => write!(
                f,
                "plan unit {unit}: its intent record did not read back from the journal as written (at byte {offset})"
            ),
            Self::DeviceReadBack {
                unit,
                write,
                start,
                end,
            } => write!(
                f,
                "plan unit {unit} write {write}: the device read back other bytes than its redo in bytes {start}..{end}"
            ),
            Self::OpenUnitRange {
                unit,
                write,
                start,
                end,
            } => write!(
                f,
                "plan unit {unit} write {write}: the device holds neither its preimage nor its redo in bytes {start}..{end}"
            ),
            Self::Journal(e) => write!(f, "the journal: {e}"),
            Self::Progress(e) => write!(f, "the journal: {e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Cancelled { applied } => write!(
                f,
                "the repair was cancelled after {applied} units were applied; its journal resumes it"
            ),
        }
    }
}

impl std::error::Error for InPlaceError {}

/// How far an in-place apply has gone (#956): the units marked applied
/// and the bytes their writes hold, of the plan's whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyProgress {
    /// Units marked applied in the journal.
    pub units: u32,
    /// Bytes the applied units' writes hold.
    pub bytes: u64,
    /// Units in the plan.
    pub units_total: u32,
    /// Bytes every unit's writes hold.
    pub bytes_total: u64,
}

impl ApplyProgress {
    /// Nothing of `plan` applied.
    fn of(plan: &SavedPlan) -> Self {
        Self {
            units: 0,
            bytes: 0,
            units_total: u32::try_from(plan.units.len())
                .expect("a plan holds at most u32::MAX units"),
            bytes_total: plan.units.iter().map(unit_bytes).sum(),
        }
    }

    /// One more unit, `unit`, marked applied.
    fn past(&mut self, unit: &PlanUnit) {
        self.units += 1;
        self.bytes += unit_bytes(unit);
    }
}

/// The bytes `unit`'s writes hold.
fn unit_bytes(unit: &PlanUnit) -> u64 {
    unit.writes.iter().map(|w| w.after.len() as u64).sum()
}

/// Watches an in-place apply (#956). It is told the progress once before
/// the first unit and again after each unit is marked applied, and asked
/// before each unit begins whether to stop. A stop asked for while a unit
/// is being written is seen at the next boundary, so the unit is finished
/// first: an apply never stops with half a unit.
pub trait ApplyObserver {
    /// The apply has reached `at`.
    fn progress(&mut self, _at: ApplyProgress) {}

    /// Whether to stop before the next unit begins.
    fn cancel_requested(&mut self) -> bool {
        false
    }
}

/// The observer of an apply nobody watches.
struct Unwatched;

impl ApplyObserver for Unwatched {}

/// Apply `plan` in place to `device`, the source it was made from,
/// through `journal`, an empty journal outside that device. The plan
/// file's SHA-256 `plan_sha256` and the plan's source bind the journal's
/// header, which is appended, flushed and read back first. Then, for each
/// unit in order:
///
/// 1. [`check_unit_on_device`] checks the unit's ranges on the device; a
///    mismatch stops the apply with nothing of the unit written.
/// 2. The unit's intent record, every write's preimage and redo, is
///    appended to the journal, the journal flushed, and the record read
///    back and compared with what was written.
/// 3. The unit's writes go to the device, and the device is flushed.
/// 4. Every write's range is read back from the device and compared with
///    what the unit leaves there: its redo, with the unit's later writes
///    laid over it.
/// 5. Only then is the unit's applied record appended to the journal and
///    the journal flushed.
///
/// Nothing reaches the device outside the plan's ranges, and nothing is
/// written into the volume's `$LogFile` but the plan's own units.
///
/// After the last unit, the whole repaired device is audited (#941):
/// [`check_io`] reads it back and its report is returned. The audit writes
/// nothing, so the dirty flag the plan set is left set, and the report
/// shows it so; every finding the report holds is one the repair left.
/// The audit is [`check_io`]'s scope, which is not a structural check of
/// the whole volume, until #583's structural checker takes its place.
pub fn apply_in_place<D: BlockIo + ?Sized, J: BlockIo + ?Sized>(
    plan: &SavedPlan,
    plan_sha256: [u8; SHA256_LEN],
    device: &mut D,
    journal: &mut J,
) -> Result<CheckReport, InPlaceError> {
    apply_in_place_observed(plan, plan_sha256, device, journal, &mut Unwatched)
}

/// [`apply_in_place`], telling `observer` the progress before the first
/// unit and after each unit is marked applied, and asking it before each
/// unit begins whether to stop (#956). A stop returns
/// [`InPlaceError::Cancelled`] with every unit begun marked applied and
/// none open, and [`resume_in_place`] continues from there.
pub fn apply_in_place_observed<D: BlockIo + ?Sized, J: BlockIo + ?Sized>(
    plan: &SavedPlan,
    plan_sha256: [u8; SHA256_LEN],
    device: &mut D,
    journal: &mut J,
    observer: &mut dyn ApplyObserver,
) -> Result<CheckReport, InPlaceError> {
    let binding = JournalBinding {
        plan_sha256,
        source_size: plan.source_size,
        volume_serial: plan.volume_serial,
    };
    let mut at = 0u64;
    append_durably(journal, &mut at, &header_record(&binding), None)?;
    let mut done = ApplyProgress::of(plan);
    observer.progress(done);
    for u in 0..plan.units.len() {
        if observer.cancel_requested() {
            return Err(InPlaceError::Cancelled {
                applied: done.units,
            });
        }
        apply_unit(plan, u, device, journal, &mut at)?;
        done.past(&plan.units[u]);
        observer.progress(done);
    }
    audit(device)
}

/// Continue the in-place apply of `plan` that stopped, on the same
/// `device` and the same `journal` (#927). The journal is read back with
/// [`read_journal`] and [`progress`]; a torn final record is dropped, and
/// the next record is appended where it starts. Then:
///
/// 1. Every unit the journal marks applied is read back: each write's
///    range must hold what the applied units leave there, its unit's redo
///    with the later applied units' writes laid over it. Those units are
///    not written again. Bytes the open unit also writes are left to its
///    own check.
/// 2. The open unit, an intent with no applied record, is redone when
///    every write's range holds its preimage, its redo, or a write torn
///    between them (#928): every byte equal to the same byte of the
///    preimage or of what the unit leaves there. It is written, flushed
///    and read back, then its applied record appended and flushed, as
///    [`apply_in_place`] does. Its intent is not appended a second time.
/// 3. Every later unit is applied exactly as [`apply_in_place`] applies it.
///
/// A range that fails step 1 or 2, because something changed the device
/// while the apply was stopped, refuses the resume before anything is
/// written to the device or appended to the journal (#929). The refusal
/// names the unit, the write and the bytes, from the first that fails to
/// the last.
///
/// A torn final record must be the start of the record the plan expects
/// next: the header, the open unit's applied record, or the next unit's
/// intent (#934). Any other tail -- a whole record whose length field was
/// raised, a torn record with a byte changed, or a tail after the last
/// unit's applied record -- was not left by a torn append, and refuses the
/// resume with [`JournalFault::TornTailDiffers`] before anything is
/// written.
///
/// A journal holding no whole record is started again from its header.
/// The record appended over a torn one is always the record that was torn,
/// as long or longer, so no torn bytes are left after it.
///
/// Once every unit is applied, the device is audited and the report
/// returned, as [`apply_in_place`] does (#941).
pub fn resume_in_place<D: BlockIo + ?Sized, J: BlockIo + ?Sized>(
    plan: &SavedPlan,
    plan_sha256: [u8; SHA256_LEN],
    device: &mut D,
    journal: &mut J,
) -> Result<CheckReport, InPlaceError> {
    resume_in_place_observed(plan, plan_sha256, device, journal, &mut Unwatched)
}

/// [`resume_in_place`], telling `observer` the progress once the stopped
/// apply's units are read back and after each unit is marked applied, and
/// asking it whether to stop before each later unit begins (#956). The
/// open unit, begun before the stop, is finished without asking. A stop
/// returns [`InPlaceError::Cancelled`] as [`apply_in_place_observed`]
/// does.
pub fn resume_in_place_observed<D: BlockIo + ?Sized, J: BlockIo + ?Sized>(
    plan: &SavedPlan,
    plan_sha256: [u8; SHA256_LEN],
    device: &mut D,
    journal: &mut J,
    observer: &mut dyn ApplyObserver,
) -> Result<CheckReport, InPlaceError> {
    let binding = JournalBinding {
        plan_sha256,
        source_size: plan.source_size,
        volume_serial: plan.volume_serial,
    };
    let length = usize::try_from(journal.size())
        .map_err(|_| InPlaceError::Io("the journal is too large to read back".into()))?;
    let mut bytes = vec![0u8; length];
    journal
        .read_exact_at(0, &mut bytes)
        .map_err(|e| InPlaceError::Io(format!("reading the journal back: {e}")))?;
    let read = read_journal(&bytes, &binding).map_err(InPlaceError::Journal)?;
    let stopped = progress(&read, plan).map_err(InPlaceError::Progress)?;
    if let Some(torn) = read.torn {
        let expected = if read.records.is_empty() {
            Some(header_record(&binding))
        } else if let Some(open) = stopped.open {
            Some(applied_record(open))
        } else {
            plan.units
                .get(stopped.applied as usize)
                .map(|unit| intent_record(stopped.applied, unit))
        };
        let tail = &bytes[torn.offset as usize..];
        if !expected.is_some_and(|record| record.starts_with(tail)) {
            return Err(InPlaceError::Journal(JournalError {
                record: torn.record,
                offset: torn.offset,
                fault: JournalFault::TornTailDiffers,
            }));
        }
    }
    let mut at = read.torn.map_or(length as u64, |t| t.offset);
    if read.records.is_empty() {
        at = 0;
        append_durably(journal, &mut at, &header_record(&binding), None)?;
    }

    let applied = stopped.applied as usize;
    let open = stopped.open.map(|o| o as usize);
    let open_writes: &[PlannedWrite] = open.map_or(&[], |o| &plan.units[o].writes);
    for u in 0..applied {
        for (w, write) in plan.units[u].writes.iter().enumerate() {
            let held = read_range(device, u, w, write)?;
            let left = left_by_units(&plan.units[u..applied], w);
            let changed = changed_span(write.offset, held.len(), |i| {
                let byte = write.offset + i as u64;
                held[i] == left[i]
                    || open_writes
                        .iter()
                        .any(|o| (o.offset..o.offset + o.after.len() as u64).contains(&byte))
            });
            if let Some((start, end)) = changed {
                return Err(InPlaceError::DeviceReadBack {
                    unit: u as u32,
                    write: w,
                    start,
                    end,
                });
            }
        }
    }

    let mut done = ApplyProgress::of(plan);
    for unit in &plan.units[..applied] {
        done.past(unit);
    }
    let mut next = applied;
    if let Some(o) = open {
        let unit = &plan.units[o];
        for (w, write) in unit.writes.iter().enumerate() {
            let held = read_range(device, o, w, write)?;
            let left = left_by_unit(unit, w);
            let changed = changed_span(write.offset, held.len(), |i| {
                held[i] == write.before[i] || held[i] == write.after[i] || held[i] == left[i]
            });
            if let Some((start, end)) = changed {
                return Err(InPlaceError::OpenUnitRange {
                    unit: o as u32,
                    write: w,
                    start,
                    end,
                });
            }
        }
        observer.progress(done);
        write_unit(plan, o, device, journal, &mut at)?;
        done.past(unit);
        next += 1;
    }
    observer.progress(done);
    for u in next..plan.units.len() {
        if observer.cancel_requested() {
            return Err(InPlaceError::Cancelled {
                applied: done.units,
            });
        }
        apply_unit(plan, u, device, journal, &mut at)?;
        done.past(&plan.units[u]);
        observer.progress(done);
    }
    audit(device)
}

/// Audit `device` once every unit of a plan is applied and read back
/// (#941): [`check_io`] over the whole device as it now reads, writing
/// nothing.
fn audit<D: BlockIo + ?Sized>(device: &mut D) -> Result<CheckReport, InPlaceError> {
    let mut device = device;
    check_io(&mut device).map_err(|e| {
        InPlaceError::Io(format!(
            "every unit applied; auditing the repaired device: {e}"
        ))
    })
}

/// The device bytes, first to last, of the `len` bytes from `offset` for
/// which `held(i)` is false; `None` when it holds for every byte.
fn changed_span(offset: u64, len: usize, held: impl Fn(usize) -> bool) -> Option<(u64, u64)> {
    let first = (0..len).find(|&i| !held(i))?;
    let last = (first..len).rev().find(|&i| !held(i))?;
    Some((offset + first as u64, offset + last as u64 + 1))
}

/// Read write `w` of unit `u`'s range back from `device`.
fn read_range<D: BlockIo + ?Sized>(
    device: &mut D,
    u: usize,
    w: usize,
    write: &PlannedWrite,
) -> Result<Vec<u8>, InPlaceError> {
    let mut held = vec![0u8; write.after.len()];
    device
        .read_exact_at(write.offset, &mut held)
        .map_err(|e| InPlaceError::Io(format!("plan unit {u} write {w}: reading back: {e}")))?;
    Ok(held)
}

/// Apply unit `u` of `plan` as [`apply_in_place`]'s steps 1 to 5 do,
/// appending to `journal` at `at`.
fn apply_unit<D: BlockIo + ?Sized, J: BlockIo + ?Sized>(
    plan: &SavedPlan,
    u: usize,
    device: &mut D,
    journal: &mut J,
    at: &mut u64,
) -> Result<(), InPlaceError> {
    let index = u32::try_from(u).expect("a plan holds at most u32::MAX units");
    check_unit_on_device(plan, u, device).map_err(InPlaceError::Device)?;
    append_durably(
        journal,
        at,
        &intent_record(index, &plan.units[u]),
        Some(index),
    )?;
    write_unit(plan, u, device, journal, at)
}

/// [`apply_in_place`]'s steps 3 to 5 for unit `u` of `plan`, whose intent
/// is already durable in `journal`: write, flush, read back, then append
/// and flush the applied record at `at`.
fn write_unit<D: BlockIo + ?Sized, J: BlockIo + ?Sized>(
    plan: &SavedPlan,
    u: usize,
    device: &mut D,
    journal: &mut J,
    at: &mut u64,
) -> Result<(), InPlaceError> {
    let index = u32::try_from(u).expect("a plan holds at most u32::MAX units");
    let unit = &plan.units[u];
    for (w, write) in unit.writes.iter().enumerate() {
        device
            .write_all_at(write.offset, &write.after)
            .map_err(|e| InPlaceError::Io(format!("plan unit {u} write {w}: {e}")))?;
    }
    device
        .sync()
        .map_err(|e| InPlaceError::Io(format!("plan unit {u}: flushing the device: {e}")))?;
    for (w, write) in unit.writes.iter().enumerate() {
        let held = read_range(device, u, w, write)?;
        if held != left_by_unit(unit, w) {
            return Err(InPlaceError::DeviceReadBack {
                unit: index,
                write: w,
                start: write.offset,
                end: write.offset + write.after.len() as u64,
            });
        }
    }
    let applied = applied_record(index);
    journal
        .write_all_at(*at, &applied)
        .map_err(|e| InPlaceError::Io(format!("plan unit {u}: journal applied record: {e}")))?;
    journal
        .sync()
        .map_err(|e| InPlaceError::Io(format!("plan unit {u}: flushing the journal: {e}")))?;
    *at += applied.len() as u64;
    Ok(())
}

/// Append `record` to `journal` at `at`, flush the journal, read the
/// record back and compare it with what was written; `at` then moves past
/// it. `unit` names the unit an intent is for, `None` for the header.
fn append_durably<J: BlockIo + ?Sized>(
    journal: &mut J,
    at: &mut u64,
    record: &[u8],
    unit: Option<u32>,
) -> Result<(), InPlaceError> {
    let what = match unit {
        Some(u) => format!("plan unit {u}: journal intent record"),
        None => "journal header".to_string(),
    };
    journal
        .write_all_at(*at, record)
        .map_err(|e| InPlaceError::Io(format!("{what}: {e}")))?;
    journal
        .sync()
        .map_err(|e| InPlaceError::Io(format!("{what}: flushing the journal: {e}")))?;
    let mut back = vec![0u8; record.len()];
    journal
        .read_exact_at(*at, &mut back)
        .map_err(|e| InPlaceError::Io(format!("{what}: reading back: {e}")))?;
    if back != record {
        return Err(InPlaceError::JournalReadBack { unit, offset: *at });
    }
    *at += record.len() as u64;
    Ok(())
}

/// What `unit` leaves in write `w`'s range: its redo, with the unit's
/// later writes laid over it.
fn left_by_unit(unit: &PlanUnit, w: usize) -> Vec<u8> {
    let write = &unit.writes[w];
    lay_over(write, write.after.clone(), &unit.writes[w + 1..])
}

/// What `units` leave in write `w`'s range of their first unit: that
/// unit's redo, with every later write of it and of the later units laid
/// over it.
fn left_by_units(units: &[PlanUnit], w: usize) -> Vec<u8> {
    let write = &units[0].writes[w];
    let later = units[0].writes[w + 1..]
        .iter()
        .chain(units[1..].iter().flat_map(|u| &u.writes));
    lay_over(write, write.after.clone(), later)
}

/// `left`, the bytes of `write`'s range, with each of `later` laid over
/// it in order where it overlaps.
fn lay_over<'a>(
    write: &PlannedWrite,
    mut left: Vec<u8>,
    later: impl IntoIterator<Item = &'a PlannedWrite>,
) -> Vec<u8> {
    let end = write.offset + left.len() as u64;
    for later in later {
        let from = later.offset.max(write.offset);
        let to = (later.offset + later.after.len() as u64).min(end);
        if from < to {
            left[(from - write.offset) as usize..(to - write.offset) as usize].copy_from_slice(
                &later.after[(from - later.offset) as usize..(to - later.offset) as usize],
            );
        }
    }
    left
}

/// Why a journal location was refused (#921). A refused location has had
/// nothing created at it and nothing written to the target.
#[derive(Debug, PartialEq, Eq)]
pub enum JournalLocationError {
    /// The directory the journal would be created in does not exist, or
    /// is not a directory: the external storage meant for it is missing.
    MissingDirectory(std::path::PathBuf),
    /// The journal is the target: the same path, or another name for the
    /// same file (a hard link, or a symlink to it).
    IsTarget,
    /// The target is a device, and the journal's directory is on a
    /// filesystem that device holds.
    OnTargetDevice,
    /// The target is a whole disk, and the journal's directory is on a
    /// filesystem one of its partitions holds (#948).
    OnTargetPartition,
    /// The target is a partition, and the journal's directory is on a
    /// filesystem the whole disk holding it holds (#948).
    OnTargetWholeDisk,
    /// Something already exists at the journal path. A new journal is
    /// never written over an existing file.
    Exists,
    /// The target or the journal's directory could not be looked at.
    Io(String),
}

impl std::fmt::Display for JournalLocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDirectory(dir) => write!(
                f,
                "the journal's directory '{}' does not exist",
                dir.display()
            ),
            Self::IsTarget => write!(f, "the journal is the device being repaired"),
            Self::OnTargetDevice => write!(
                f,
                "the journal's directory is on a filesystem the device being repaired holds"
            ),
            Self::OnTargetPartition => write!(
                f,
                "the journal's directory is on a partition of the disk being repaired"
            ),
            Self::OnTargetWholeDisk => write!(
                f,
                "the journal's directory is on the disk that holds the partition being repaired"
            ),
            Self::Exists => write!(f, "a file already exists at the journal path"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for JournalLocationError {}

/// The parts of a file's `stat` a journal location is checked by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationStat {
    /// `st_dev`: the device of the filesystem the file is on.
    pub dev: u64,
    /// `st_ino`: the file on that filesystem.
    pub ino: u64,
    /// `st_rdev`: the device the file is, read only when `is_device`.
    pub rdev: u64,
    /// The file is a block or character device.
    pub is_device: bool,
}

#[cfg(unix)]
impl LocationStat {
    /// The location fields of `meta`.
    pub fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let kind = meta.file_type();
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            rdev: meta.rdev(),
            is_device: kind.is_block_device() || kind.is_char_device(),
        }
    }

    /// The same file as `other`: the same inode on the same filesystem.
    pub fn same_file(&self, other: &Self) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
}

/// Whether `directory` is on a filesystem `target` holds: `target` is a
/// device, and it is the device `directory`'s filesystem is on.
pub fn directory_on_target(directory: &LocationStat, target: &LocationStat) -> bool {
    target.is_device && directory.dev == target.rdev
}

/// A block device a journal location is compared by: its name, and the
/// name of the whole disk that holds it, its own name when it is a whole
/// disk (#948).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    /// The device: `disk2s1` on macOS, `sda1` on Linux.
    pub name: String,
    /// The whole disk holding the device: `disk2`, `sda`.
    pub whole_disk: String,
}

/// How the device a journal's directory is on relates to the device
/// being repaired (#948).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRelation {
    /// The same device.
    Same,
    /// The journal's device is a partition of the whole disk being
    /// repaired.
    PartitionOfTarget,
    /// The device being repaired is a partition of the whole disk the
    /// journal's directory is on.
    TargetOnJournalDisk,
    /// Neither holds the other: different disks, or two partitions of
    /// one disk.
    Unrelated,
}

impl DeviceRelation {
    /// The refusal a journal on a device so related to the target gets,
    /// or `None` when it is accepted.
    pub fn refusal(self) -> Option<JournalLocationError> {
        match self {
            Self::Same => Some(JournalLocationError::OnTargetDevice),
            Self::PartitionOfTarget => Some(JournalLocationError::OnTargetPartition),
            Self::TargetOnJournalDisk => Some(JournalLocationError::OnTargetWholeDisk),
            Self::Unrelated => None,
        }
    }
}

/// How `journal`, the device the journal's directory is on, relates to
/// `target`, the device being repaired.
pub fn device_relation(journal: &DeviceRecord, target: &DeviceRecord) -> DeviceRelation {
    if journal.name == target.name {
        DeviceRelation::Same
    } else if journal.whole_disk == target.name {
        DeviceRelation::PartitionOfTarget
    } else if target.whole_disk == journal.name {
        DeviceRelation::TargetOnJournalDisk
    } else {
        DeviceRelation::Unrelated
    }
}

/// The record of a macOS BSD device name: `disk2s1` is on `disk2`, and
/// the raw `rdisk2s1` is the same device as `disk2s1`. A name that is not
/// a disk is its own whole disk.
pub fn bsd_device_record(name: &str) -> DeviceRecord {
    let name = match name.strip_prefix('r') {
        Some(block) if block.starts_with("disk") => block,
        _ => name,
    };
    let whole_disk = match name.strip_prefix("disk") {
        Some(rest) => {
            let unit = rest.bytes().take_while(u8::is_ascii_digit).count();
            if unit == 0 {
                name
            } else {
                &name[.."disk".len() + unit]
            }
        }
        None => name,
    };
    DeviceRecord {
        name: name.into(),
        whole_disk: whole_disk.into(),
    }
}

/// The record of block device `major:minor` in the sysfs tree at `sys`
/// (`/sys` on Linux): `dev/block/<major>:<minor>` links to the device's
/// directory, and a partition's directory holds a `partition` file and
/// sits inside its disk's. `None` when sysfs lists no such block device,
/// as for tmpfs or NFS.
pub fn sysfs_device_record(sys: &std::path::Path, major: u32, minor: u32) -> Option<DeviceRecord> {
    let dir = std::fs::canonicalize(sys.join(format!("dev/block/{major}:{minor}"))).ok()?;
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let whole_disk = if dir.join("partition").exists() {
        dir.parent()?.file_name()?.to_string_lossy().into_owned()
    } else {
        name.clone()
    };
    Some(DeviceRecord { name, whole_disk })
}

/// The record of device number `dev`, a character device when `raw`, or
/// `None` when it names no disk this platform can describe.
#[cfg(target_os = "macos")]
fn device_record(dev: u64, raw: bool) -> Option<DeviceRecord> {
    let mode = if raw { libc::S_IFCHR } else { libc::S_IFBLK };
    // SAFETY: devname returns NULL or a pointer to a NUL-terminated name
    // in static storage, read before any other devname call.
    let name = unsafe { libc::devname(dev as libc::dev_t, mode) };
    if name.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy();
    if name.starts_with('?') {
        return None;
    }
    Some(bsd_device_record(&name))
}

/// The record of device number `dev`, or `None` when it names no block
/// device sysfs lists.
#[cfg(target_os = "linux")]
fn device_record(dev: u64, raw: bool) -> Option<DeviceRecord> {
    if raw {
        return None;
    }
    // glibc's dev_t encoding: 12 + 20 bits of major, 8 + 24 bits of minor.
    let major = ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0x0fff);
    let minor = ((dev >> 12) & 0xffff_ff00) | (dev & 0xff);
    sysfs_device_record(std::path::Path::new("/sys"), major as u32, minor as u32)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn device_record(_dev: u64, _raw: bool) -> Option<DeviceRecord> {
    None
}

/// Check `journal` as the place an in-place repair of `target` keeps its
/// journal, before anything is created or written (#921). It is refused,
/// naming the reason, when:
///
/// - the directory it would be created in does not exist;
/// - it is the target, by path or as another name for the same file (a
///   hard link, or a symlink to it);
/// - the target is a device and the journal's directory is on a
///   filesystem that device holds, a partition of it holds, or the whole
///   disk holding it holds (#948), the disks named through their BSD
///   names on macOS and through sysfs on Linux;
/// - something already exists at it, a dangling symlink included.
///
/// Nothing is created at `journal` and nothing is written to `target`,
/// whether the location is refused or not.
#[cfg(unix)]
pub fn check_journal_location(
    journal: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), JournalLocationError> {
    let directory = match journal.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => std::path::Path::new("."),
    };
    let directory_meta = match std::fs::metadata(directory) {
        Ok(meta) if meta.is_dir() => meta,
        _ => return Err(JournalLocationError::MissingDirectory(directory.into())),
    };
    let target_meta = std::fs::metadata(target)
        .map_err(|e| JournalLocationError::Io(format!("the target '{}': {e}", target.display())))?;
    let target_stat = LocationStat::of(&target_meta);

    if std::fs::symlink_metadata(journal).is_ok() {
        return match std::fs::metadata(journal) {
            Ok(meta) if LocationStat::of(&meta).same_file(&target_stat) => {
                Err(JournalLocationError::IsTarget)
            }
            _ => Err(JournalLocationError::Exists),
        };
    }
    let directory_stat = LocationStat::of(&directory_meta);
    if directory_on_target(&directory_stat, &target_stat) {
        return Err(JournalLocationError::OnTargetDevice);
    }
    if target_stat.is_device {
        use std::os::unix::fs::FileTypeExt;
        let raw = target_meta.file_type().is_char_device();
        if let (Some(journal_device), Some(target_device)) = (
            device_record(directory_stat.dev, false),
            device_record(target_stat.rdev, raw),
        ) {
            if let Some(refusal) = device_relation(&journal_device, &target_device).refusal() {
                return Err(refusal);
            }
        }
    }
    Ok(())
}
