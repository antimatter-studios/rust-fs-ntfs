//! `fs.ntfs SOURCE rescue ARCHIVE [--resume]`, end to end through the
//! binary (#1015): the JSON report, the exit status, and that a refusal
//! changes neither the source nor the archive.
//!
//! What the archive holds is graded here against the source's own bytes,
//! read with `std::fs` rather than through the driver: the library's export
//! reassembles the image, and it must be the source byte for byte.

mod common;

use std::path::Path;
use std::process::Command;

const VOL: u64 = 16 << 20;
const PARTITION: u64 = 1 << 20;

fn fs(args: &[&str]) -> (i32, serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
        .arg("fs")
        .args(args)
        .output()
        .expect("spawn fs.ntfs");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let report = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().expect("exit status"), report, stderr)
}

/// A freshly formatted NTFS image, `lead` zero bytes into the file.
fn image(tag: &str, lead: u64) -> String {
    let img = common::temp_image_path(format!("cli_rescue_{tag}"));
    let volume = common::temp_image_path(format!("cli_rescue_{tag}_volume"));
    {
        let f = std::fs::File::create(&volume).expect("create");
        f.set_len(VOL).expect("size");
    }
    let mut dev = fs_ntfs::block_io::PathIo::open_rw(Path::new(&volume)).unwrap();
    fs_ntfs::mkfs::format_filesystem(&mut dev, VOL, 4096, 4096, None, Some(1)).unwrap();
    drop(dev);
    let mut bytes = vec![0u8; lead as usize];
    bytes.extend(std::fs::read(&volume).expect("read volume"));
    std::fs::write(&img, bytes).expect("write image");
    let _ = std::fs::remove_file(&volume);
    img
}

fn exported(archive: &str) -> Vec<u8> {
    let out = format!("{archive}.img");
    let _ = std::fs::remove_file(&out);
    fs_ntfs::rescue::export_to_path(Path::new(archive), Path::new(&out)).expect("export");
    let bytes = std::fs::read(&out).expect("read export");
    let _ = std::fs::remove_file(&out);
    bytes
}

fn fresh_archive(tag: &str) -> String {
    let archive = common::temp_image_path(format!("cli_rescue_{tag}.ntrescue"));
    let _ = std::fs::remove_file(&archive);
    archive
}

#[test]
fn a_rescue_reports_a_complete_archive_of_the_source() {
    let img = image("complete", 0);
    let archive = fresh_archive("complete");

    let (code, report, stderr) = fs(&[&img, "rescue", &archive]);
    assert_eq!(code, 0, "{report} {stderr}");
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(report["resumed"], false, "{report}");
    assert_eq!(report["source_size"], VOL, "{report}");
    assert_eq!(report["readable_bytes"], VOL, "{report}");
    assert_eq!(report["unreadable_bytes"], 0, "{report}");
    let classes = &report["classes"];
    let allocated = classes["allocated"]["archived"].as_u64().unwrap();
    let free = classes["free"]["archived"].as_u64().unwrap();
    assert!(allocated > 0 && free > 0, "{report}");
    assert_eq!(exported(&archive), std::fs::read(&img).unwrap());
}

#[test]
fn a_second_rescue_without_resume_refuses_and_changes_nothing() {
    let img = image("refuse", 0);
    let archive = fresh_archive("refuse");
    assert_eq!(fs(&[&img, "rescue", &archive]).0, 0);
    let source_before = std::fs::read(&img).unwrap();
    let archive_before = std::fs::read(&archive).unwrap();

    let (code, report, stderr) = fs(&[&img, "rescue", &archive]);
    assert_eq!(code, 1, "{report} {stderr}");
    assert!(stderr.contains("--resume"), "{stderr}");
    assert_eq!(std::fs::read(&img).unwrap(), source_before);
    assert_eq!(std::fs::read(&archive).unwrap(), archive_before);

    let (code, report, stderr) = fs(&[&img, "rescue", &archive, "--resume"]);
    assert_eq!(code, 0, "{report} {stderr}");
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(report["resumed"], true, "{report}");
    assert_eq!(exported(&archive), source_before);
}

#[test]
fn every_refusal_exits_non_zero_and_creates_no_archive() {
    let archive = fresh_archive("missing");
    let missing = common::temp_image_path("cli_rescue_no_such_source");
    let _ = std::fs::remove_file(&missing);

    let (code, _, stderr) = fs(&[&missing, "rescue", &archive]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        !Path::new(&archive).exists(),
        "a refused rescue left an archive"
    );

    let img = image("resume_none", 0);
    let source_before = std::fs::read(&img).unwrap();
    let (code, _, stderr) = fs(&[&img, "rescue", &archive, "--resume"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        !Path::new(&archive).exists(),
        "a refused resume made an archive"
    );
    assert_eq!(std::fs::read(&img).unwrap(), source_before);
}

#[test]
fn offset_rescues_the_partition_inside_a_whole_disk() {
    let img = image("offset", PARTITION);
    let archive = fresh_archive("offset");
    let offset = PARTITION.to_string();

    let (code, report, stderr) = fs(&["--offset", &offset, &img, "rescue", &archive]);
    assert_eq!(code, 0, "{report} {stderr}");
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(report["source_size"], VOL, "{report}");
    assert_eq!(report["offset"], PARTITION, "{report}");
    let whole = std::fs::read(&img).unwrap();
    assert_eq!(exported(&archive), whole[PARTITION as usize..]);
}

/// The CLI contract's `fs.<fs>` statuses (diskjockey docs/cli-tooling-design.md):
/// 0 success, 1 failed, 2 usage, 3 unsupported, 4 refused by the volume's state.
const CONTRACT_STATUSES: [u8; 5] = [0, 1, 2, 3, 4];

/// The status an incomplete rescue exits with (#1020): the contract has none
/// for "completed with unrecovered data" yet, so it must be one the contract
/// gives no other meaning, never 4, which a caller reads as "refused,
/// nothing done".
const INCOMPLETE: u8 = 5;

#[test]
fn an_incomplete_rescue_exit_status_is_not_one_the_contract_already_uses() {
    assert!(!CONTRACT_STATUSES.contains(&INCOMPLETE));
    let out = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
        .args(["fs", "source.img", "rescue", "--help"])
        .output()
        .expect("spawn fs.ntfs");
    let help = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        help.contains(&format!("Exit status: 0 the archive holds every byte, {INCOMPLETE} some spans are still unreadable")),
        "{help}"
    );
    for status in CONTRACT_STATUSES.iter().filter(|&&s| s > 1) {
        assert!(
            !help.contains(&format!(", {status} some spans are still unreadable")),
            "{help}"
        );
    }
}
