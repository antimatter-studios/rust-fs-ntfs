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

use crate::repair_plan::{load_plan, PlanFileError, PlanUnit, PlannedWrite};
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
