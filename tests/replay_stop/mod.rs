//! Stopping a replay right after its first undo write, as a power cut can
//! (#754). Shared by `examples/replay_stopped.rs`, which leaves such a stop
//! on an image file for the Windows scenarios to attach (#755), and by the
//! crate's own test of what that stop leaves.

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::BlockIo;

/// The LSN undo stamps on each block it rolls back: one past the last the
/// volume's `$LogFile` holds (#514).
pub fn undo_stamp<I: BlockIo>(io: &mut I) -> Result<u64, String> {
    let params = fs_ntfs::mft_io::read_boot_params_io(io)?;
    let (at, len) = fs_ntfs::read::nonresident_contiguous_disk_range(io, 2, AttrType::Data, None)?;
    let mut log = vec![0u8; len as usize];
    io.read_exact_at(at, &mut log)?;
    let plan = fs_ntfs::logfile_replay::plan(&log, &params, &mut |at, buf| {
        io.read_exact_at(at, buf).map_err(Into::into)
    })?;
    Ok(plan.last_lsn + 1)
}

/// Runs `fsck`'s replay over `io` and cuts the power right after its first
/// write of a block undo rolled back. Returns how many writes landed.
pub fn stop_after_first_undo<I: BlockIo>(io: &mut I) -> Result<usize, String> {
    let stamp = undo_stamp(io)?;
    let mut cut = StopAfterUndo {
        io,
        stamp,
        writes: 0,
        stopped: false,
    };
    let run = fs_ntfs::fsck::fsck_io(&mut cut, None);
    match (cut.stopped, run) {
        (true, Err(_)) => Ok(cut.writes),
        (true, Ok(_)) => Err("the replay reported success after the power was cut".into()),
        (false, Ok(_)) => Err(format!(
            "the replay finished without writing a block stamped {stamp:#x}"
        )),
        (false, Err(e)) => Err(format!(
            "the replay failed before its first undo write: {e}"
        )),
    }
}

/// A volume whose writes stop, as a power cut stops them, right after the
/// first write holding a `FILE` or `INDX` block that carries `stamp`. That
/// write lands whole; every later one is dropped and fails.
struct StopAfterUndo<'a, I: BlockIo> {
    io: &'a mut I,
    stamp: u64,
    writes: usize,
    stopped: bool,
}

impl<I: BlockIo> BlockIo for StopAfterUndo<'_, I> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.io.read_exact_at(offset, buf)
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        if self.stopped {
            return Err("the power was cut".into());
        }
        self.io.write_all_at(offset, buf)?;
        self.writes += 1;
        // A block starts on a sector, and its LSN sits at offset 8 of its
        // first sector, which fixups never touch.
        self.stopped = buf.chunks_exact(512).any(|s| {
            matches!(&s[..4], b"FILE" | b"INDX")
                && u64::from_le_bytes(s[8..16].try_into().unwrap()) == self.stamp
        });
        Ok(())
    }

    fn size(&self) -> u64 {
        self.io.size()
    }

    fn sync(&mut self) -> Result<(), String> {
        self.io.sync()
    }

    fn can_sync(&self) -> bool {
        self.io.can_sync()
    }
}
