//! Prints the LSNs an NTFS image carries, read from the raw bytes alone
//! (#761), so that a Windows scenario can compare the LSNs a replay left on
//! the volume with those Windows logs on it afterwards (#649).
//!
//! The image is scanned at every 512-byte boundary, whatever partition
//! wrapper it carries, and no structure of this crate is used to find
//! anything. Each line is `<kind> <lsn>`:
//!
//! - `block-max`: the highest LSN in any `FILE` or `INDX` block header
//!   (offset 8 of the header, outside the update sequence array).
//! - `restart-current`: the current LSN of each `RSTR` page's restart area
//!   (offset 0 of the area the page header's offset 0x18 names).
//! - `record-page-min` / `record-page-max`: the lowest and highest LSN in
//!   any `RCRD` page header (offset 8: the last LSN the page holds).
//!
//! A kind with nothing to report is not printed.
//!
//! Usage: `cargo run --release --example log_lsns -- IMAGE`

use std::io::Read;

const SECTOR: usize = 512;
const CHUNK: usize = 1 << 20;

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [image] = args.as_slice() else {
        eprintln!("usage: log_lsns IMAGE");
        std::process::exit(2);
    };
    let mut file = match std::fs::File::open(image) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("log_lsns: {image}: {e}");
            std::process::exit(1);
        }
    };
    let (mut block_max, mut restart, mut rcrd_min, mut rcrd_max) =
        (None::<u64>, Vec::new(), None::<u64>, None::<u64>);
    // Each chunk is read with one sector of the next one's bytes before it
    // is scanned, so a restart area that crosses a chunk boundary is still
    // whole: restart areas sit well inside the first sector of their page.
    let mut buf = vec![0u8; CHUNK + SECTOR];
    let mut carry = 0usize;
    loop {
        let mut filled = carry;
        while filled < buf.len() {
            match file.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => {
                    eprintln!("log_lsns: {image}: {e}");
                    std::process::exit(1);
                }
            }
        }
        let scan_to = if filled == buf.len() { CHUNK } else { filled };
        let mut at = 0;
        while at < scan_to && at + SECTOR <= filled {
            let b = &buf[at..filled];
            match &b[..4] {
                b"FILE" | b"INDX" => {
                    if let Some(lsn) = u64_at(b, 8) {
                        block_max = block_max.max(Some(lsn));
                    }
                }
                b"RSTR" => {
                    if let Some(lsn) = u16_at(b, 0x18).and_then(|off| u64_at(b, off)) {
                        restart.push(lsn);
                    }
                }
                b"RCRD" => {
                    if let Some(lsn) = u64_at(b, 8) {
                        rcrd_min = Some(rcrd_min.map_or(lsn, |m| m.min(lsn)));
                        rcrd_max = rcrd_max.max(Some(lsn));
                    }
                }
                _ => {}
            }
            at += SECTOR;
        }
        if filled < buf.len() {
            break;
        }
        buf.copy_within(CHUNK..CHUNK + SECTOR, 0);
        carry = SECTOR;
    }
    if let Some(lsn) = block_max {
        println!("block-max {lsn}");
    }
    for lsn in restart {
        println!("restart-current {lsn}");
    }
    if let (Some(lo), Some(hi)) = (rcrd_min, rcrd_max) {
        println!("record-page-min {lo}");
        println!("record-page-max {hi}");
    }
}
