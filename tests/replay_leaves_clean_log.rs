//! What a replay leaves in `$LogFile` (#762, part of #649).
//!
//! fsck used to fill a replayed log with `0xFF`. Windows, mounting such a
//! volume and writing to it, started its own log at LSNs at or below those
//! the replay had left in MFT records and index blocks: the Windows
//! scenario `cli-windows-interrupted-fsck-replay-win-write-lsn-after-windows`
//! measured restart areas naming 1067480 over blocks up to 1169203
//! (merge-queue run 39852914842). Redo trusts a block's LSN, so Windows'
//! next restart could skip its own records for those blocks.
//!
//! A volume Windows dismounts cleanly keeps its log
//! (`test-disks/windows-clean-logfile.bin.gz`): two restart pages flagged
//! clean (`0x0002`), both naming one current LSN, which is also the
//! client's restart LSN, and at the byte that LSN resolves to, a client
//! restart record (type 2) whose tables are empty. The replay must leave
//! that, with a current LSN past the last one it replayed.
//!
//! The log is located with the `ntfs` crate and decoded here, not through
//! this crate's `$LogFile` reader.
mod common;

use fs_ntfs::fsck;
use ntfs::{KnownNtfsFileRecordNumber, Ntfs, NtfsAttributeType};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, Stdio};

const RESTART_VOLUME_IS_CLEAN: u16 = 0x0002;
const CLIENT_RESTART: u32 = 2;

fn unpack(name: &str) -> String {
    let path = common::temp_image_path(format!("replay_clean_log_{}", name.replace('-', "_")));
    let out = File::create(&path).expect("create image copy");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    path
}

/// `$LogFile`'s unnamed `$DATA`, read where the `ntfs` crate puts it.
fn logfile(path: &str) -> Vec<u8> {
    let mut reader = std::io::BufReader::new(File::open(path).expect("open"));
    let ntfs = Ntfs::new(&mut reader).expect("parse ntfs");
    let log = ntfs
        .file(&mut reader, KnownNtfsFileRecordNumber::LogFile as u64)
        .expect("open $LogFile");
    let mut attrs = log.attributes();
    while let Some(item) = attrs.next(&mut reader) {
        let item = item.expect("attribute");
        let attribute = item.to_attribute().expect("attribute");
        if attribute.ty().ok() != Some(NtfsAttributeType::Data)
            || !attribute.name().map(|n| n.is_empty()).unwrap_or(true)
        {
            continue;
        }
        let at = attribute
            .value(&mut reader)
            .expect("value")
            .data_position()
            .value()
            .expect("$LogFile's data is on the disk")
            .get();
        let mut bytes = vec![0u8; attribute.value_length() as usize];
        let mut f = File::open(path).expect("open");
        f.seek(SeekFrom::Start(at)).expect("seek");
        f.read_exact(&mut bytes).expect("read $LogFile");
        return bytes;
    }
    panic!("no unnamed $DATA on $LogFile");
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// A multi-sector page as written, with each sector's last two bytes put
/// back from its update sequence array, or `None` when its magic is not
/// `magic` or a sector does not carry the update sequence number.
fn page(log: &[u8], at: usize, len: usize, magic: &[u8; 4]) -> Option<Vec<u8>> {
    let mut p = log.get(at..at + len)?.to_vec();
    if &p[..4] != magic {
        return None;
    }
    let (usa, count) = (u16_at(&p, 4) as usize, u16_at(&p, 6) as usize);
    let usn = [p[usa], p[usa + 1]];
    for i in 1..count {
        let end = i * 512;
        if p[end - 2..end] != usn {
            return None;
        }
        p[end - 2] = p[usa + 2 * i];
        p[end - 1] = p[usa + 2 * i + 1];
    }
    Some(p)
}

struct Restart {
    current_lsn: u64,
    flags: u16,
    seq_number_bits: u32,
    client_restart_lsn: u64,
}

fn restart(log: &[u8], at: usize, sps: usize) -> Option<Restart> {
    let p = page(log, at, sps, b"RSTR")?;
    let ra = u16_at(&p, 0x18) as usize;
    let client = ra + u16_at(&p, ra + 0x16) as usize;
    Some(Restart {
        current_lsn: u64_at(&p, ra),
        flags: u16_at(&p, ra + 0x0E),
        seq_number_bits: u32_at(&p, ra + 0x10),
        client_restart_lsn: u64_at(&p, client + 0x08),
    })
}

#[test]
fn a_replay_leaves_a_clean_log_naming_a_current_lsn_past_its_last() {
    for k in ["1", "6", "small-cluster"] {
        let img = unpack(&format!("windows-interrupted-{k}"));
        let report = fsck::replay_logfile(&img).unwrap_or_else(|e| panic!("{k}: replay: {e}"));
        let log = logfile(&img);
        let sps = u32_at(&log, 0x10) as usize;
        let lps = u32_at(&log, 0x14) as usize;

        let mut current = None;
        for at in [0, sps] {
            let ra = restart(&log, at, sps)
                .unwrap_or_else(|| panic!("{k}: no restart page at {at:#x} after the replay"));
            assert_ne!(
                ra.flags & RESTART_VOLUME_IS_CLEAN,
                0,
                "{k}: the restart page at {at:#x} is not flagged clean"
            );
            assert!(
                ra.current_lsn > report.last_lsn,
                "{k}: the restart page at {at:#x} names current LSN {:#x}, not past the \
                 replay's last, {:#x}",
                ra.current_lsn,
                report.last_lsn
            );
            assert_eq!(
                ra.client_restart_lsn, ra.current_lsn,
                "{k}: the client's restart LSN is not the current LSN"
            );
            assert_eq!(
                *current.get_or_insert(ra.current_lsn),
                ra.current_lsn,
                "{k}: the two restart pages name different current LSNs"
            );
        }

        // The checkpoint the current LSN names, where the LSN says it is.
        let ra = restart(&log, 0, sps).unwrap();
        let bits = ra.seq_number_bits;
        let at = ((ra.current_lsn << bits) >> (bits - 3)) as usize;
        let home = at - at % lps;
        let page = page(&log, home, lps, b"RCRD")
            .unwrap_or_else(|| panic!("{k}: no record page at {home:#x}, the current LSN's home"));
        let record = &page[at - home..];
        assert_eq!(
            u64_at(record, 0x00),
            ra.current_lsn,
            "{k}: the record at the current LSN's offset carries another LSN"
        );
        assert_eq!(
            u32_at(record, 0x20),
            CLIENT_RESTART,
            "{k}: the record the current LSN names is not a checkpoint"
        );
        let body = &record[0x30..];
        assert_eq!(
            [u32_at(body, 0x38), u32_at(body, 0x3C)],
            [0, 0],
            "{k}: the checkpoint's dirty page table and transaction table are not empty"
        );
    }
}
