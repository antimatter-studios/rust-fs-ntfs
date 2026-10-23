//! The shared readers at a file's initialized size (#1162).
//!
//! A non-resident `$DATA` attribute carries three sizes: allocated, data and
//! initialized. Windows reads every byte past the initialized size as zero
//! whatever its clusters hold, and so does ntfs-3g's `ntfscat`. The low-level
//! `ntfs` crate behind `common::read_file_all` returns the clusters' bytes
//! there instead. So `read_file_all` is a raw attribute read, and
//! `common::read_file_logical` is the reader to compare with Windows.
//!
//! The volume and its file are written by this crate. The file's initialized
//! size is then set in its MFT record, by the published attribute layout and
//! update sequence rules, to 0, inside a cluster, at a cluster boundary and
//! at the data size. Only `ntfscat`, run at arm's length, says what the
//! logical bytes are.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::facade::Filesystem;
use fs_ntfs::mkfs::format_filesystem;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const VOL_SIZE: u64 = 32 * 1024 * 1024;
const PATH: &str = "/init.bin";
/// Three whole clusters and part of a fourth.
const LEN: usize = 3 * 4096 + 100;

fn le16(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes([b[at], b[at + 1]]) as usize
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The volume, holding `PATH` with `data` written through this crate.
fn volume(data: &[u8]) -> String {
    let img = common::temp_image_path("common_initialized_size");
    let f = std::fs::File::create(&img).expect("create");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 4096, Some("INIT"), Some(0x1162)).expect("format");
    io.sync().expect("sync");
    drop(io);
    let fs = Filesystem::mount(&img).expect("mount");
    fs.create_file("/", "init.bin").expect("create init.bin");
    fs.write_file_contents(PATH, data).expect("write init.bin");
    drop(fs);
    img
}

/// Where `PATH`'s MFT record starts, its length, and the offset in it of
/// its unnamed `$DATA` attribute, as the `ntfs` crate locates them.
fn data_attribute(img: &str) -> (u64, usize, usize) {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, PATH);
    let record = file
        .position()
        .value()
        .expect("the record's position")
        .get();
    let length = file.allocated_size() as usize;
    let item = file
        .data(&mut reader, "")
        .expect("no $DATA attribute")
        .expect("data attribute error");
    let attribute = item.to_attribute().expect("to_attribute");
    assert!(!attribute.is_resident(), "{PATH}'s $DATA stayed resident");
    let at = attribute
        .position()
        .value()
        .expect("the attribute's position")
        .get();
    assert!(
        at > record && at < record + length as u64,
        "{PATH}'s $DATA is not in its base record"
    );
    (record, length, (at - record) as usize)
}

/// Set the initialized size of the non-resident attribute at `attribute` in
/// the record at `record`. The update sequence is undone, the field (at 0x38
/// of the attribute, after the allocated size at 0x28 and the data size at
/// 0x30) written, and the sequence applied again.
fn set_initialized_size(img: &str, record: u64, length: usize, attribute: usize, size: u64) {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(img)
        .expect("open the image");
    let mut rec = vec![0u8; length];
    f.seek(SeekFrom::Start(record)).unwrap();
    f.read_exact(&mut rec).expect("read the record");
    assert_eq!(&rec[0..4], b"FILE", "not an MFT record");
    let (usa, count) = (le16(&rec, 4), le16(&rec, 6));
    assert!(
        count >= 2 && length % (count - 1) == 0,
        "update sequence count {count}"
    );
    let stride = length / (count - 1);
    let usn = [rec[usa], rec[usa + 1]];
    for i in 1..count {
        let end = i * stride - 2;
        assert_eq!(
            rec[end..end + 2],
            usn,
            "stride {i} does not end in the sequence number"
        );
        let saved = [rec[usa + 2 * i], rec[usa + 2 * i + 1]];
        rec[end..end + 2].copy_from_slice(&saved);
    }
    assert_eq!(rec[attribute + 8], 1, "the attribute is not non-resident");
    assert_eq!(
        le64(&rec, attribute + 0x30),
        LEN as u64,
        "the data size field"
    );
    assert!(
        le64(&rec, attribute + 0x28) >= LEN as u64,
        "the allocated size field"
    );
    rec[attribute + 0x38..attribute + 0x40].copy_from_slice(&size.to_le_bytes());
    for i in 1..count {
        let end = i * stride - 2;
        let kept = [rec[end], rec[end + 1]];
        rec[usa + 2 * i..usa + 2 * i + 2].copy_from_slice(&kept);
        rec[end..end + 2].copy_from_slice(&usn);
    }
    f.seek(SeekFrom::Start(record)).unwrap();
    f.write_all(&rec).expect("write the record");
    f.sync_all().expect("sync the image");
}

#[test]
fn a_logical_read_is_zero_past_the_initialized_size_and_a_raw_read_is_not() {
    // No zero byte, so every byte past the initialized size tells the two
    // readers apart.
    let data: Vec<u8> = (0..LEN as u32).map(|i| (i % 251 + 1) as u8).collect();
    let img = volume(&data);
    let (record, length, attribute) = data_attribute(&img);
    assert_eq!(
        common::read_file_logical(&img, PATH),
        data,
        "fully initialized"
    );

    for size in [0, 4096 + 7, 2 * 4096, LEN as u64] {
        set_initialized_size(&img, record, length, attribute, size);
        let at = size as usize;
        let logical = common::read_file_logical(&img, PATH);
        assert_eq!(
            logical.len(),
            LEN,
            "ntfscat's length at initialized size {size}"
        );
        assert_eq!(
            logical[..at],
            data[..at],
            "ntfscat below initialized size {size}"
        );
        assert!(
            logical[at..].iter().all(|&b| b == 0),
            "ntfscat reads past initialized size {size} as something other than zeros"
        );

        let (ntfs, mut reader) = common::open(&img);
        let raw = common::read_file_all(&ntfs, &mut reader, PATH);
        assert_eq!(
            raw, data,
            "read_file_all no longer returns the clusters' bytes past initialized \
             size {size}: update its documentation"
        );
    }
    let _ = std::fs::remove_file(&img);
}
