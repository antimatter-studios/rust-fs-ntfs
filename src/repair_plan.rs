//! Repair plans: every write a repair would make, worked out without
//! making any of them (#589, #709).
//!
//! A plan is made by running the repair itself, [`crate::fsck::fsck_io_with`],
//! over an overlay of the volume that records each write instead of making
//! it: reads see the writes recorded so far over the volume's own bytes,
//! so the repair decides exactly as it would on the volume. There is one
//! repair, and a plan cannot drift from it.
//!
//! A plan is a list of [`PlanUnit`]s in the order the repair makes them,
//! one per durable boundary: the writes the repair makes between two
//! flushes. A unit's writes may land in any order, so a unit is what is
//! applied, and flushed, as a whole. Each write carries the bytes the
//! volume holds there at its turn -- after the writes before it, including
//! any earlier write to the same bytes -- and the bytes it leaves.
//!
//! Nothing is written to the volume, and nothing is written into its
//! `$LogFile`: an external plan is not a native transaction.

use crate::block_io::BlockIo;
use crate::error::Error;
use crate::fsck::{fsck_io_with, FsckOptions, FsckReport};

/// One write a repair would make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWrite {
    /// Byte offset on the volume.
    pub offset: u64,
    /// What the volume holds there at this write's turn.
    pub before: Vec<u8>,
    /// What this write leaves there: as many bytes as `before`.
    pub after: Vec<u8>,
}

/// The writes a repair makes between two flushes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanUnit {
    pub writes: Vec<PlannedWrite>,
}

/// Every write a repair would make, and what it would report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairPlan {
    /// In the order the repair makes them; empty when it writes nothing.
    pub units: Vec<PlanUnit>,
    /// What the repair would report once every unit is applied.
    pub report: FsckReport,
}

/// Plan what [`fsck_io_with`] with `options` would write to `io`, writing
/// nothing. A repair the library refuses -- a log it cannot replay in full,
/// a hibernated volume, a device that cannot order its writes -- is
/// refused here in the same words, with no plan.
pub fn plan_fsck_io<T: BlockIo + ?Sized>(
    io: &mut T,
    options: FsckOptions,
) -> Result<RepairPlan, Error> {
    let mut planning = Planning {
        io,
        units: Vec::new(),
        open: PlanUnit::default(),
    };
    let report = fsck_io_with(&mut planning, options, None)?;
    planning.close_unit();
    Ok(RepairPlan {
        units: planning.units,
        report,
    })
}

/// The volume as the repair would leave it so far, recording each write
/// instead of making it.
struct Planning<'a, T: ?Sized> {
    io: &'a mut T,
    /// The units flushed so far, then the one still open: every write
    /// recorded, in order, laid over the volume's bytes on read.
    units: Vec<PlanUnit>,
    open: PlanUnit,
}

impl<T: ?Sized> Planning<'_, T> {
    fn close_unit(&mut self) {
        if !self.open.writes.is_empty() {
            self.units.push(std::mem::take(&mut self.open));
        }
    }
}

impl<T: BlockIo + ?Sized> BlockIo for Planning<'_, T> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.io.read_exact_at(offset, buf)?;
        let end = offset + buf.len() as u64;
        // In order, so a later write to the same bytes wins.
        let recorded = self.units.iter().chain([&self.open]);
        for w in recorded.flat_map(|unit| &unit.writes) {
            let (at, bytes) = (w.offset, &w.after);
            let (from, to) = (at.max(offset), (at + bytes.len() as u64).min(end));
            if from < to {
                buf[(from - offset) as usize..(to - offset) as usize]
                    .copy_from_slice(&bytes[(from - at) as usize..(to - at) as usize]);
            }
        }
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let mut before = vec![0u8; buf.len()];
        self.read_exact_at(offset, &mut before)?;
        self.open.writes.push(PlannedWrite {
            offset,
            before,
            after: buf.to_vec(),
        });
        Ok(())
    }

    fn size(&self) -> u64 {
        self.io.size()
    }

    fn sync(&mut self) -> Result<(), String> {
        self.close_unit();
        Ok(())
    }

    /// The repair refuses a device that cannot order its writes (#518);
    /// so does its plan, which would have to be applied to that device.
    fn can_sync(&self) -> bool {
        self.io.can_sync()
    }
}
