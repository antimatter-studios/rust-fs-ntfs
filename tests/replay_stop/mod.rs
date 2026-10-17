//! Stopping a replay right after its first undo write, as a power cut can
//! (#754). Shared by `examples/replay_stopped.rs`, which leaves such a stop
//! on an image file for the Windows scenarios to attach (#755), and by the
//! crate's own test of what that stop leaves.

use fs_ntfs::block_io::BlockIo;

/// The LSN undo stamps on each block it rolls back: one past the last the
/// volume's `$LogFile` holds (#514).
pub fn undo_stamp<I: BlockIo>(_io: &mut I) -> Result<u64, String> {
    Err("not written yet (#754)".into())
}

/// Runs `fsck`'s replay over `io` and cuts the power right after its first
/// write of a block undo rolled back. Returns how many writes landed.
pub fn stop_after_first_undo<I: BlockIo>(_io: &mut I) -> Result<usize, String> {
    Err("not written yet (#754)".into())
}
