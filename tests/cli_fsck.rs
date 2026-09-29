//! `fsck.ntfs`'s one repair, end to end through the binary: a dirty volume
//! whose `$LogFile` is empty is corrected (exit 1) and clean after (exit
//! 0). The shell suite cannot reach this state with the installed tools --
//! nothing it installs empties a log -- so it is made here with the
//! library's own `reset_logfile` and `set_dirty`.

mod common;

use std::process::Command;

fn fsck(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
        .arg("fsck")
        .args(args)
        .output()
        .expect("spawn fsck.ntfs");
    let report = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "fsck.ntfs {args:?} printed no JSON report ({e}): stderr {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().expect("exit status"), report)
}

#[test]
fn a_dirty_volume_with_an_empty_log_is_corrected_then_clean() {
    let img = common::temp_image_path("cli_fsck_empty_log");
    {
        let f = std::fs::File::create(&img).expect("create");
        f.set_len(64 << 20).expect("size");
    }
    let mut dev = fs_ntfs::block_io::PathIo::open_rw(std::path::Path::new(&img)).unwrap();
    fs_ntfs::mkfs::format_filesystem(&mut dev, 64 << 20, 4096, 4096, None, Some(1)).unwrap();
    drop(dev);
    fs_ntfs::fsck::reset_logfile(&img).unwrap();
    assert!(fs_ntfs::fsck::set_dirty(&img).unwrap());

    let (code, report) = fsck(&["-n", &img]);
    assert_eq!(code, 4, "{report}");
    assert_eq!(report["findings"][0]["kind"], "dirty");
    assert_eq!(report["findings"][0]["repairable"], true);

    let (code, report) = fsck(&["-y", &img]);
    assert_eq!(code, 1, "corrected: {report}");
    assert_eq!(report["repaired"], 1);
    assert_eq!(report["dirty"], false);
    assert_eq!(report["clean"], true);
    assert!(!fs_ntfs::fsck::is_dirty(&img).unwrap());

    let (code, report) = fsck(&[&img]);
    assert_eq!(code, 0, "clean after: {report}");
}
