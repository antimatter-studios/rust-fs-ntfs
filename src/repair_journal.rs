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
        }
    }
}

impl std::error::Error for InPlaceError {}

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
pub fn apply_in_place<D: BlockIo + ?Sized, J: BlockIo + ?Sized>(
    plan: &SavedPlan,
    plan_sha256: [u8; SHA256_LEN],
    device: &mut D,
    journal: &mut J,
) -> Result<(), InPlaceError> {
    let binding = JournalBinding {
        plan_sha256,
        source_size: plan.source_size,
        volume_serial: plan.volume_serial,
    };
    let mut at = 0u64;
    append_durably(journal, &mut at, &header_record(&binding), None)?;
    for u in 0..plan.units.len() {
        apply_unit(plan, u, device, journal, &mut at)?;
    }
    Ok(())
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
/// A journal holding no whole record is started again from its header.
/// The record appended over a torn one is always the record that was torn,
/// as long or longer, so no torn bytes are left after it.
pub fn resume_in_place<D: BlockIo + ?Sized, J: BlockIo + ?Sized>(
    plan: &SavedPlan,
    plan_sha256: [u8; SHA256_LEN],
    device: &mut D,
    journal: &mut J,
) -> Result<(), InPlaceError> {
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
        write_unit(plan, o, device, journal, &mut at)?;
        next += 1;
    }
    for u in next..plan.units.len() {
        apply_unit(plan, u, device, journal, &mut at)?;
    }
    Ok(())
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
