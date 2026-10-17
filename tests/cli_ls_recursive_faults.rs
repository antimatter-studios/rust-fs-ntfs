//! Fault injection only; Windows grades the real enumeration in cli-oracle.
#![cfg(feature = "cli")]
mod common;
use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::{mft_io, read};
use std::path::Path;
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
        .args(args)
        .output()
        .expect("run CLI")
}

#[test]
fn cycles_duplicates_and_unreadable_directories_preserve_the_report() {
    for fault in ["self", "duplicate", "unreadable"] {
        let img = common::temp_image_path(format!("ls_recursive_{fault}"));
        assert!(cli(&["mkfs", "--size", "16M", &img]).status.success());
        for dir in ["/a", "/a/loop", "/b", "/b/alias"] {
            assert!(cli(&["fs", &img, "mkdir", dir]).status.success());
        }
        let mut io = PathIo::open_rw(Path::new(&img)).unwrap();
        let a = read::resolve_path(&mut io, "/a").unwrap();
        let parent =
            read::resolve_path(&mut io, if fault == "duplicate" { "/b" } else { "/a" }).unwrap();
        let (_, target) = mft_io::read_mft_record_io(&mut io, a).unwrap();
        let reference = a | (u64::from(mft_io::record_sequence(&target)) << 48);
        mft_io::update_mft_record_io(&mut io, parent, |raw| {
            let loc = attr_io::find_attribute(raw, AttrType::IndexRoot, Some("$I30")).unwrap();
            let header = loc.attr_offset + usize::from(loc.resident_value_offset.unwrap()) + 16;
            if fault == "unreadable" {
                raw[header..header + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            } else {
                let first = header
                    + u32::from_le_bytes(raw[header..header + 4].try_into().unwrap()) as usize;
                raw[first..first + 8].copy_from_slice(&reference.to_le_bytes());
            }
            Ok(())
        })
        .unwrap();
        io.sync().unwrap();
        drop(io);
        let before = std::fs::read(&img).unwrap();
        let out = cli(&["fs", &img, "ls", "-R", "/"]);
        let rows: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("whole JSON report");
        if fault == "unreadable" {
            assert_eq!(out.status.code(), Some(1), "{out:?}");
            assert_eq!(rows.as_array().unwrap().len(), 1);
            assert_eq!(rows[0]["path"], "/a");
            let error: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
            assert_eq!(error["code"], 1);
            assert!(error["error"].as_str().unwrap().contains("/a"));
        } else {
            assert!(out.status.success(), "{fault}: {out:?}");
            assert_eq!(rows.as_array().unwrap().len(), 4);
            let repeated = if fault == "self" {
                "/a/loop"
            } else {
                "/b/alias"
            };
            let revisited: Vec<_> = rows
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["revisited"] == true)
                .collect();
            assert_eq!(revisited.len(), 1);
            assert_eq!(revisited[0]["path"], repeated);
            assert_eq!(revisited[0]["record"], a);
            let text = cli(&["fs", &img, "ls", "-R", "--text", "/"]);
            assert!(text.status.success());
            assert!(String::from_utf8(text.stdout)
                .unwrap()
                .contains(&format!("{repeated} [revisited]")));
        }
        assert_eq!(std::fs::read(&img).unwrap(), before, "{fault}: read-only");
    }
}
