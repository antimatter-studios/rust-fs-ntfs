//! `fs.ntfs write`, `fs.ntfs write --at` and `fs.ntfs truncate` on a file
//! whose unnamed `$DATA` lives only in an extension record.
//!
//! A file whose base record is full of named streams and an EA gains an
//! `$ATTRIBUTE_LIST`; once its unnamed stream grows non-resident, the whole
//! `$DATA` attribute is moved to an extension record, and the base record
//! holds none. The driver writes and resizes such a stream through the list
//! (#789, #800), but both verbs looked for the attribute's flags in the base
//! record only and refused the file as "not implemented", which stopped the
//! split-extents Windows scenario on 4096-byte records (#1328).
//!
//! The contents are read back through the `ntfs` crate, a parser that is not
//! this one, and `check_io` must find the volume clean.
#![cfg(feature = "cli")]
mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::{fsck, mft_io, read, write};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn cli(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run CLI");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin)
        .expect("write stdin");
    child.wait_with_output().expect("wait for CLI")
}

fn ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `/f` on a fresh 4096-byte-record volume: eight resident bytes, an EA and
/// named streams enough to fill the base record, then grown to `size`
/// bytes, so its unnamed `$DATA` is non-resident and listed.
fn listed_file(tag: &str, size: usize) -> (String, Vec<u8>) {
    let img = common::temp_image_path(format!("cli_listed_data_{tag}"));
    ok(&cli(&["mkfs", "--size", "16M", &img], b""), "mkfs.ntfs");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    let mut model = b"contents".to_vec();
    write::create_file_io(&mut io, "/", "f").expect("create /f");
    write::write_file_contents_io(&mut io, "/f", &model).expect("write /f");
    write::write_ea_io(&mut io, "/f", b"USER.TAG", &[b'e'; 64], 0).expect("write /f's EA");
    for i in 0..36u8 {
        write::write_named_stream_io(&mut io, "/f", &format!("s{i}"), &[b'a' + i % 26; 120])
            .unwrap_or_else(|e| panic!("write /f:s{i}: {e}"));
    }
    // The streams leave the base record with some slack. One-byte streams
    // take it up until one lands in an extension record, so the base
    // record has no room left for a non-resident $DATA header and the
    // growth below has to move the attribute out of it.
    let rec = read::resolve_path(&mut io, "/f").expect("resolve /f");
    let mut filled = false;
    for i in 0..200u32 {
        let name = format!("t{i}");
        write::write_named_stream_io(&mut io, "/f", &name, b"t")
            .unwrap_or_else(|e| panic!("write /f:{name}: {e}"));
        let list =
            read::read_attribute_value_if_present(&mut io, rec, AttrType::AttributeList, None)
                .expect("read /f's list")
                .expect("/f's base record never filled: it has no $ATTRIBUTE_LIST");
        let entries = read::parse_attribute_list(&list).expect("parse /f's list");
        if entries
            .iter()
            .any(|e| e.name.as_deref() == Some(name.as_str()) && e.record_number != rec)
        {
            filled = true;
            break;
        }
    }
    assert!(filled, "200 one-byte streams never filled /f's base record");
    let grow: Vec<u8> = (0..size - model.len())
        .map(|i| (i * 7 % 251) as u8)
        .collect();
    write::write_at_io(&mut io, "/f", model.len() as u64, &grow).expect("grow /f");
    model.extend_from_slice(&grow);
    <PathIo as BlockIo>::sync(&mut io).expect("sync");

    // The shape under test, or the test says it was not built.
    let (_, base) = mft_io::read_mft_record_io(&mut io, rec).expect("read /f's record");
    assert!(
        attr_io::find_attribute(&base, AttrType::AttributeList, None).is_some(),
        "/f's base record never filled: it has no $ATTRIBUTE_LIST"
    );
    assert!(
        attr_io::find_attribute(&base, AttrType::Data, None).is_none(),
        "/f's unnamed $DATA stayed in its base record: the extension-record shape was not built"
    );
    (img, model)
}

/// The unnamed stream, read by the `ntfs` crate, and a clean `check_io`.
fn graded(img: &str, model: &[u8]) {
    let (ntfs, mut reader) = common::open(img);
    let back = common::read_file_all(&ntfs, &mut reader, "f");
    assert!(
        back == model,
        "/f does not read back as written through an independent parser ({} bytes, {} expected)",
        back.len(),
        model.len()
    );
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let report = fsck::check_io(&mut io).expect("check_io");
    assert!(
        report.findings.is_empty(),
        "check_io finds the volume unclean: {:?}",
        report.findings
    );
}

#[test]
fn write_at_patches_a_listed_stream_whose_data_is_in_an_extension_record() {
    let (img, mut model) = listed_file("write_at", 64 * 1024);
    let patch = vec![0x5a; 5000];
    let out = cli(&["fs", &img, "write", "--at", "10000", "/f"], &patch);
    ok(&out, "fs.ntfs write --at 10000 /f");
    model[10000..15000].copy_from_slice(&patch);
    graded(&img, &model);
}

#[test]
fn truncate_resizes_a_listed_stream_whose_data_is_in_an_extension_record() {
    let (img, mut model) = listed_file("truncate", 64 * 1024);
    ok(
        &cli(&["fs", &img, "truncate", "/f", "20000"], b""),
        "fs.ntfs truncate /f 20000",
    );
    model.truncate(20000);
    graded(&img, &model);
    ok(
        &cli(&["fs", &img, "truncate", "/f", "30000"], b""),
        "fs.ntfs truncate /f 30000",
    );
    model.resize(30000, 0);
    graded(&img, &model);
}

/// `fs.ntfs write` replaces the whole stream, shorter and then longer.
/// It looked for the unnamed `$DATA` in the base record alone and refused
/// the file as having none, which stopped the split-extents Windows
/// scenario at its whole rewrite once its patch went through (#1347).
#[test]
fn write_replaces_a_listed_stream_whose_data_is_in_an_extension_record() {
    let (img, _) = listed_file("write", 64 * 1024);
    let shorter: Vec<u8> = (0..40_000u32).map(|i| (i * 13 % 251) as u8).collect();
    ok(
        &cli(&["fs", &img, "write", "/f"], &shorter),
        "fs.ntfs write /f (shorter)",
    );
    graded(&img, &shorter);
    let longer: Vec<u8> = (0..90_000u32).map(|i| (i * 17 % 251) as u8).collect();
    ok(
        &cli(&["fs", &img, "write", "/f"], &longer),
        "fs.ntfs write /f (longer)",
    );
    graded(&img, &longer);
}
