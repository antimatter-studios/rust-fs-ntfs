//! Compares every index block a replay's undo wrote with the same block on
//! a volume Windows' own restart finished (#756). CAPTURE is the untouched
//! capture: it is replayed here, uninterrupted, and each `INDX` block a
//! write stamps with undo's LSN is remembered. RESTARTED is the image of a
//! replay stopped after its first undo write (#754) once Windows attached
//! it and ran its restart over the log the stop left (#755). Both sides
//! have their fixups undone, the update sequence number blanked and the LSN
//! ignored before they are compared, so only what an index holds counts.
//!
//! A listing and `chkdsk /scan` both pass a block holding a duplicated
//! entry, which is the shape #514 found on `open-at-checkpoint`, so the
//! blocks are compared directly and every entry of a differing one is
//! printed.
//!
//! Usage: `cargo run --release --example undo_index_compare -- CAPTURE RESTARTED`

#[path = "../tests/replay_stop/mod.rs"]
#[allow(dead_code)]
mod replay_stop;

use fs_ntfs::block_io::{BlockIo, PathIo};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [capture, restarted] = args.as_slice() else {
        eprintln!("usage: undo_index_compare CAPTURE RESTARTED");
        std::process::exit(2);
    };
    match compare(capture, restarted) {
        Ok(0) => println!("undo_index_compare: no index block undo touched differs"),
        Ok(n) => {
            eprintln!("undo_index_compare: {n} index block(s) undo touched differ");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("undo_index_compare: {e}");
            std::process::exit(2);
        }
    }
}

/// Returns how many of the blocks undo touched differ.
fn compare(capture: &str, restarted: &str) -> Result<usize, String> {
    let mut io = PathIo::open_rw(Path::new(capture))?;
    let stamp = replay_stop::undo_stamp(&mut io)?;
    let sector = fs_ntfs::mft_io::read_boot_params_io(&mut io)
        .map_err(|e| e.to_string())?
        .bytes_per_sector;
    let mut rec = RecordUndo {
        io: &mut io,
        stamp,
        blocks: Vec::new(),
    };
    fs_ntfs::fsck::fsck_io(&mut rec, None).map_err(|e| format!("the uninterrupted replay: {e}"))?;
    let mut blocks = rec.blocks;
    blocks.sort_unstable();
    blocks.dedup();
    if blocks.is_empty() {
        return Err(format!(
            "the uninterrupted replay wrote no index block stamped {stamp:#x}"
        ));
    }
    let mut theirs = PathIo::open_rw(Path::new(restarted))?;
    let mut differ = 0;
    for &(at, len) in &blocks {
        let ours = normalised(&mut io, at, len, sector)?;
        let restart = normalised(&mut theirs, at, len, sector)?;
        if ours == restart {
            continue;
        }
        differ += 1;
        println!("index block at {at:#x} ({len} bytes) differs");
        print_entries("  uninterrupted replay", &ours);
        print_entries("  Windows' restart", &restart);
    }
    println!(
        "undo_index_compare: {} index block(s) undo touched, {differ} differ",
        blocks.len()
    );
    Ok(differ)
}

/// The block at `at`, with its fixups undone, its update sequence number
/// blanked and its LSN zeroed.
fn normalised<I: BlockIo>(io: &mut I, at: u64, len: usize, sector: u16) -> Result<Vec<u8>, String> {
    let mut block = vec![0u8; len];
    io.read_exact_at(at, &mut block)?;
    if &block[..4] != b"INDX" {
        return Ok(block);
    }
    fs_ntfs::mft_io::apply_fixup_on_read_magic(&mut block, sector, b"INDX")
        .map_err(|e| format!("block at {at:#x}: {e}"))?;
    let usa = u16::from_le_bytes([block[4], block[5]]) as usize;
    block[usa..usa + 2].fill(0);
    block[8..16].fill(0);
    Ok(block)
}

fn print_entries(side: &str, block: &[u8]) {
    let mut entries = Vec::new();
    match fs_ntfs::index_io::collect_indx_block_entries(block, &mut entries) {
        Ok(()) => {
            println!("{side}: {} entries", entries.len());
            for e in &entries {
                println!(
                    "    record {} seq {}: {}",
                    e.file_record_number, e.sequence, e.name
                );
            }
        }
        Err(e) => println!("{side}: entries unreadable: {e}"),
    }
}

/// Passes every write through and remembers each `INDX` block in it that
/// carries `stamp`: its offset and its size from its own header.
struct RecordUndo<'a, I: BlockIo> {
    io: &'a mut I,
    stamp: u64,
    blocks: Vec<(u64, usize)>,
}

impl<I: BlockIo> BlockIo for RecordUndo<'_, I> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.io.read_exact_at(offset, buf)
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        self.io.write_all_at(offset, buf)?;
        // A block starts on a sector; its LSN sits at offset 8 and its
        // allocated size at offset 0x24 of its first sector, which fixups
        // never touch.
        for (i, s) in buf.chunks_exact(512).enumerate() {
            if &s[..4] == b"INDX" && u64::from_le_bytes(s[8..16].try_into().unwrap()) == self.stamp
            {
                let alloc = u32::from_le_bytes(s[0x24..0x28].try_into().unwrap()) as usize;
                self.blocks.push((offset + (i * 512) as u64, alloc + 0x18));
            }
        }
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
