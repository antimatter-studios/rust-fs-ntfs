//! A resident change recorded in `$LogFile` by `logfile_write` is redone by
//! a restart (#581).
//!
//! The volume is `test-disks/windows-interrupted-1`, which Windows left
//! mid-write: its log holds work, its last record is Windows' own, and
//! `windows_interrupted_fixture.rs` shows this crate's replay of it reaching
//! what Windows' restart reached. One change -- a `$STANDARD_INFORMATION`
//! modification time, in a record that replay does not otherwise write --
//! is appended to that log, and the change itself is not made, as a power
//! cut after the log was written and before the record was would leave it.
//!
//! The replay must write that record with the new bytes and the update's
//! LSN, and every other block exactly as it does without the change. This
//! is this crate's reader agreeing with this crate's writer: Windows'
//! restart recovering such a log is the oracle #581 still needs.

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::BlockIo;
use fs_ntfs::logfile_replay::{plan, Plan};
use fs_ntfs::logfile_write::{append_resident_update, ResidentUpdate};
use fs_ntfs::mft_io::{self, BootParams};
use sha2::{Digest, Sha256};

const IMAGE: &str = "test-disks/windows-interrupted-1.img.gz";
const IMAGE_SHA256: &str = "c45659e95104eca5f793c3f21061f42705a01783e62d1c7a46c63e4089db0b9a";

/// The partition, in memory: nothing here writes to it.
struct Image(Vec<u8>);

impl BlockIo for Image {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let at = usize::try_from(offset).map_err(|e| e.to_string())?;
        let src = self
            .0
            .get(at..at + buf.len())
            .ok_or_else(|| format!("read of {} bytes at {offset:#x} past the image", buf.len()))?;
        buf.copy_from_slice(src);
        Ok(())
    }
    fn write_all_at(&mut self, offset: u64, _: &[u8]) -> Result<(), String> {
        Err(format!(
            "this test writes nothing, and was asked to at {offset:#x}"
        ))
    }
    fn size(&self) -> u64 {
        self.0.len() as u64
    }
}

fn image() -> Image {
    let out = std::process::Command::new("gzip")
        .args(["-dc", IMAGE])
        .output()
        .expect("install gzip to read the Windows captures");
    assert!(out.status.success(), "gzip -dc {IMAGE}: {:?}", out.stderr);
    assert_eq!(
        format!("{:x}", Sha256::digest(&out.stdout)),
        IMAGE_SHA256,
        "{IMAGE} is not the captured partition"
    );
    Image(out.stdout)
}

fn replay(img: &mut Image, params: &BootParams, log: &[u8]) -> Plan {
    plan(log, params, &mut |at, buf| {
        img.read_exact_at(at, buf).map_err(Into::into)
    })
    .unwrap_or_else(|e| panic!("replay plan: {e}"))
}

/// A record with fixups undone.
fn fixed(bytes: &[u8], bps: u16) -> Vec<u8> {
    let mut b = bytes.to_vec();
    mft_io::apply_fixup_on_read_magic(&mut b, bps, b"FILE").expect("a FILE record");
    b
}

fn u16_at(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap()) as usize
}
fn u32_at(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

/// The first in-use MFT record the replay does not write, with a resident
/// `$STANDARD_INFORMATION`: its number, byte offset, the record with
/// fixups undone, and the attribute's offset in it.
fn untouched_record(
    img: &mut Image,
    params: &BootParams,
    base: &Plan,
) -> (u64, u64, Vec<u8>, usize) {
    let size = params.file_record_size as usize;
    for n in 0..4096 {
        let at = mft_io::mft_record_offset_io(img, params, n).expect("record offset");
        if base.writes.contains_key(&at) {
            continue;
        }
        let mut raw = vec![0u8; size];
        img.read_exact_at(at, &mut raw).unwrap();
        let mut rec = raw.clone();
        if mft_io::apply_fixup_on_read_magic(&mut rec, params.bytes_per_sector, b"FILE").is_err()
            || u16_at(&rec, 0x16) & 1 == 0
        {
            continue;
        }
        let mut a = u16_at(&rec, 0x14);
        while a + 0x18 <= u32_at(&rec, 0x18) && u32_at(&rec, a) != 0xFFFF_FFFF {
            if u32_at(&rec, a) == 0x10 && rec[a + 8] == 0 {
                return (n, at, rec, a);
            }
            let len = u32_at(&rec, a + 4);
            if len == 0 {
                break;
            }
            a += len;
        }
    }
    panic!("no in-use record with a resident $STANDARD_INFORMATION that replay leaves alone");
}

#[test]
fn a_resident_change_appended_to_the_log_is_redone_by_a_restart() {
    let mut img = image();
    let params = mft_io::read_boot_params_io(&mut img).unwrap();
    // The log's pages are one cluster each here, and a record lies in one.
    assert_eq!(
        (params.cluster_size, params.file_record_size),
        (4096, 1024),
        "the capture's geometry"
    );
    let (log_at, log_len) =
        fs_ntfs::read::nonresident_contiguous_disk_range(&mut img, 2, AttrType::Data, None)
            .unwrap();
    let mut log = vec![0u8; log_len as usize];
    img.read_exact_at(log_at, &mut log).unwrap();
    let base = replay(&mut img, &params, &log);

    let (n, at, rec, a) = untouched_record(&mut img, &params, &base);
    // The modification time: 8 bytes into the value.
    let value = u16_at(&rec, a + 0x14);
    let attribute_offset = value + 8;
    let old = rec[a + attribute_offset..a + attribute_offset + 8].to_vec();
    let new: Vec<u8> = old.iter().map(|b| b ^ 0x5A).collect();
    let in_mft = n * params.file_record_size;
    assert_eq!(
        at % params.cluster_size,
        in_mft % params.cluster_size,
        "record {n} lies where its VCN says"
    );
    let update = ResidentUpdate {
        target_vcn: in_mft / params.cluster_size,
        lcns: vec![at / params.cluster_size],
        cluster_block_offset: ((in_mft % params.cluster_size) / 512) as u16,
        record_size: params.file_record_size as u32,
        record_offset: a as u16,
        attribute_offset: attribute_offset as u16,
        redo: new.clone(),
        undo: old,
    };
    let j = append_resident_update(&mut log, &update).unwrap_or_else(|e| panic!("append: {e}"));
    let ours = replay(&mut img, &params, &log);

    assert_eq!(ours.last_lsn, j.forget);
    assert_eq!(ours.applied, base.applied + 1, "exactly one more redo");
    assert!(ours.mft_records.contains(&(n, at)), "record {n} is written");
    // Every block the replay wrote without the change, it writes the same.
    for (block, bytes) in &base.writes {
        assert!(
            ours.writes.get(block) == Some(bytes),
            "block {block:#x} differs once the change is logged"
        );
    }
    assert_eq!(ours.writes.len(), base.writes.len() + 1);

    // The record: the new bytes, the update's LSN, and nothing else but
    // the update sequence number a write moves.
    let bps = params.bytes_per_sector;
    let got = fixed(&ours.writes[&at], bps);
    let mut want = rec.clone();
    want[0x08..0x10].copy_from_slice(&j.update.to_le_bytes());
    want[a + attribute_offset..a + attribute_offset + 8].copy_from_slice(&new);
    let usa = u16_at(&rec, 0x04);
    want[usa..usa + 2].copy_from_slice(&got[usa..usa + 2]);
    assert!(got == want, "record {n} is not the change alone");
}
