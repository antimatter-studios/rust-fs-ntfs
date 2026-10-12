//! `check_io` accounts for every cluster (#517): `$Bitmap` against the run
//! list of every non-resident attribute in every in-use MFT record. A
//! cluster marked in use that nothing claims is leaked; a claimed cluster
//! marked free is handed out again by the next allocation; a cluster two
//! run lists claim is cross-linked, and a write through either corrupts
//! the other; a run past the volume's last cluster claims storage there is
//! none of.
//!
//! Each corruption is made on a volume our own mkfs made and populated,
//! one field at a time, and `fsck.ntfs -n` must report it. The same check
//! must stay silent on volumes that are right: ours, and every volume
//! Windows itself left in `test-disks/` after its own recovery.

mod common;

use std::path::Path;
use std::process::Command;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::PathIo;
use fs_ntfs::{bitmap, data_runs, fsck, mft_io, mkfs, read, write};

const VOLUME_BYTES: u64 = 64 << 20;

fn fsck_cli(args: &[&str]) -> (i32, serde_json::Value) {
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

/// The one finding of `kind` in a report, failing with the report if there
/// is not exactly one.
fn only_finding<'r>(report: &'r serde_json::Value, kind: &str) -> &'r serde_json::Value {
    let all = report["findings"].as_array().expect("findings is a list");
    let of_kind: Vec<_> = all.iter().filter(|f| f["kind"] == kind).collect();
    assert_eq!(
        of_kind.len(),
        1,
        "expected one {kind} finding, and nothing else: {report:#}"
    );
    assert_eq!(all.len(), 1, "expected only the {kind} finding: {report:#}");
    of_kind[0]
}

/// A volume our mkfs made, with a directory big enough for INDX blocks,
/// files that stay resident, and three non-resident files whose `$DATA` is
/// one run each.
fn populated_volume(name: &str, cluster_size: u32) -> String {
    let img = common::temp_image_path(name);
    std::fs::File::create(&img)
        .and_then(|f| f.set_len(VOLUME_BYTES))
        .expect("create image");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open");
    mkfs::format_filesystem(&mut io, VOLUME_BYTES, cluster_size, 1024, None, Some(1))
        .expect("mkfs");
    write::mkdir_io(&mut io, "/", "many").expect("mkdir");
    for i in 0..120 {
        write::create_file_io(&mut io, "/many", &format!("entry-{i:03}")).expect("create");
    }
    write::create_file_io(&mut io, "/", "empty").expect("create");
    write::create_file_io(&mut io, "/", "small").expect("create");
    write::write_file_contents_io(&mut io, "/small", b"resident").expect("write");
    for (file, len) in [("a", 20_000usize), ("b", 20_000), ("c", 300_000)] {
        write::create_file_io(&mut io, "/", file).expect("create");
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        write::write_file_contents_io(&mut io, &format!("/{file}"), &data).expect("write");
    }
    img
}

/// A file's MFT record and the one run its unnamed `$DATA` is stored in.
fn only_run(img: &str, path: &str) -> (u64, data_runs::DataRun) {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open");
    let record = read::resolve_path(&mut io, path).expect("resolve");
    let (_, rec) = mft_io::read_mft_record_io(&mut io, record).expect("read record");
    let loc = attr_io::find_attribute(&rec, AttrType::Data, None).expect("$DATA");
    let start = loc.attr_offset
        + loc
            .non_resident_mapping_pairs_offset
            .expect("non-resident $DATA") as usize;
    let runs = data_runs::decode_runs(&rec[start..loc.attr_offset + loc.attr_length])
        .expect("decode runs");
    assert_eq!(runs.len(), 1, "{path} is stored in one run: {runs:?}");
    (record, runs[0])
}

/// Point a file's single `$DATA` run at `lcn`, re-encoded in the room the
/// attribute already gives its mapping pairs, and free the clusters it held
/// so the move is the only thing wrong.
fn move_only_run(img: &str, path: &str, lcn: u64) {
    let (record, run) = only_run(img, path);
    let moved = data_runs::encode_runs(&[data_runs::DataRun {
        lcn: Some(lcn),
        ..run
    }])
    .expect("encode");
    let mut io = PathIo::open_rw(Path::new(img)).expect("open");
    mft_io::update_mft_record_io(&mut io, record, |rec| {
        let loc = attr_io::find_attribute(rec, AttrType::Data, None).expect("$DATA");
        let at = loc.attr_offset + loc.non_resident_mapping_pairs_offset.unwrap() as usize;
        let room = &mut rec[at..loc.attr_offset + loc.attr_length];
        assert!(
            moved.len() <= room.len(),
            "{} bytes of mapping pairs do not fit the attribute's {}",
            moved.len(),
            room.len()
        );
        room.fill(0);
        room[..moved.len()].copy_from_slice(&moved);
        Ok(())
    })
    .expect("rewrite the run");
    let bm = bitmap::locate_bitmap_io(&mut io).expect("locate $Bitmap");
    bitmap::free_io(&mut io, &bm, run.lcn.unwrap(), run.length).expect("free the old run");
}

/// `check_io` on a volume, through the library.
fn findings(img: &str) -> Vec<fsck::CheckFinding> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open");
    fsck::check_io(&mut io).expect("check").findings
}

#[test]
fn a_populated_volume_has_nothing_to_report() {
    for cluster_size in [512, 4096] {
        let img = populated_volume(&format!("cluster_acct_clean_{cluster_size}"), cluster_size);
        assert_eq!(findings(&img), vec![], "{cluster_size}-byte clusters");
        let (code, report) = fsck_cli(&["-n", &img]);
        assert_eq!(code, 0, "{report:#}");
        std::fs::remove_file(&img).unwrap();
    }
}

#[test]
fn a_cluster_marked_in_use_that_nothing_claims_is_leaked() {
    for cluster_size in [512, 4096] {
        let img = populated_volume(&format!("cluster_acct_leak_{cluster_size}"), cluster_size);
        let leaked = {
            let mut io = PathIo::open_rw(Path::new(&img)).unwrap();
            let bm = bitmap::locate_bitmap_io(&mut io).unwrap();
            let lcn = bitmap::find_free_run_io(&mut io, &bm, 3, 1000)
                .unwrap()
                .expect("three free clusters");
            bitmap::allocate_io(&mut io, &bm, lcn, 3).unwrap();
            lcn
        };
        let (code, report) = fsck_cli(&["-n", &img]);
        assert_eq!(code, 4, "{report:#}");
        let found = only_finding(&report, "leaked_clusters");
        assert_eq!(found["clusters"], 3, "{report:#}");
        assert_eq!(found["first_lcn"], leaked, "{report:#}");
        std::fs::remove_file(&img).unwrap();
    }
}

#[test]
fn a_claimed_cluster_marked_free_is_found() {
    for cluster_size in [512, 4096] {
        let img = populated_volume(&format!("cluster_acct_free_{cluster_size}"), cluster_size);
        let (_, run) = only_run(&img, "/c");
        let lcn = run.lcn.unwrap() + 1;
        {
            let mut io = PathIo::open_rw(Path::new(&img)).unwrap();
            let bm = bitmap::locate_bitmap_io(&mut io).unwrap();
            bitmap::free_io(&mut io, &bm, lcn, 2).unwrap();
        }
        let (code, report) = fsck_cli(&["-n", &img]);
        assert_eq!(code, 4, "{report:#}");
        let found = only_finding(&report, "clusters_in_use_marked_free");
        assert_eq!(found["clusters"], 2, "{report:#}");
        assert_eq!(found["first_lcn"], lcn, "{report:#}");
        std::fs::remove_file(&img).unwrap();
    }
}

#[test]
fn two_files_claiming_one_cluster_are_cross_linked() {
    for cluster_size in [512, 4096] {
        let img = populated_volume(&format!("cluster_acct_xlink_{cluster_size}"), cluster_size);
        let (a, run_a) = only_run(&img, "/a");
        let (b, _) = only_run(&img, "/b");
        move_only_run(&img, "/b", run_a.lcn.unwrap());
        let (code, report) = fsck_cli(&["-n", &img]);
        assert_eq!(code, 4, "{report:#}");
        let found = only_finding(&report, "cross_linked_clusters");
        assert_eq!(found["lcn"], run_a.lcn.unwrap(), "{report:#}");
        assert_eq!(found["clusters"], run_a.length, "{report:#}");
        let mut records: Vec<u64> = found["records"]
            .as_array()
            .expect("records")
            .iter()
            .map(|r| r.as_u64().unwrap())
            .collect();
        records.sort_unstable();
        assert_eq!(records, vec![a.min(b), a.max(b)], "{report:#}");
        std::fs::remove_file(&img).unwrap();
    }
}

#[test]
fn a_run_past_the_last_cluster_is_found() {
    for cluster_size in [512, 4096] {
        let img = populated_volume(
            &format!("cluster_acct_outside_{cluster_size}"),
            cluster_size,
        );
        let (b, run_b) = only_run(&img, "/b");
        // Straddling the end: it starts inside the volume and runs past it.
        let clusters = VOLUME_BYTES / u64::from(cluster_size);
        let lcn = clusters - 2;
        move_only_run(&img, "/b", lcn);
        let (code, report) = fsck_cli(&["-n", &img]);
        assert_eq!(code, 4, "{report:#}");
        let found = only_finding(&report, "run_outside_volume");
        assert_eq!(found["record"], b, "{report:#}");
        assert_eq!(found["lcn"], lcn, "{report:#}");
        assert_eq!(found["clusters"], run_b.length, "{report:#}");
        std::fs::remove_file(&img).unwrap();
    }
}

/// Unpack a `test-disks/` image to a scratch path.
fn unpack(name: &str) -> String {
    let img = common::temp_image_path(format!("cluster_acct_{}", name.replace(['.', '-'], "_")));
    let out = std::fs::File::create(&img).expect("create");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{name}.img.gz")])
        .stdout(out)
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{name}.img.gz");
    img
}

#[test]
fn every_volume_windows_recovered_accounts_for_its_clusters() {
    let mut names: Vec<String> = std::fs::read_dir("test-disks")
        .expect("test-disks")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".img.gz").map(str::to_owned))
        .filter(|n| n.ends_with(".recovered") || n == "ntfs-fragmented-indx")
        .collect();
    names.sort();
    assert!(
        names.len() >= 12,
        "the recovered captures are present: {names:?}"
    );
    for name in names {
        let img = unpack(&name);
        assert_eq!(findings(&img), vec![], "{name}");
        std::fs::remove_file(&img).unwrap();
    }
}

#[test]
fn a_log_holding_work_is_replayed_before_its_clusters_are_counted() {
    // Mid-transaction, on-disk metadata disagrees with itself until the log
    // is replayed: this capture's $Bitmap and run lists differ by hundreds
    // of clusters. The pending log is the finding; counting clusters over it
    // would report what the replay is about to repair.
    let img = unpack("windows-interrupted-6");
    let mut io = PathIo::open_ro(Path::new(&img)).unwrap();
    let report = fsck::check_io(&mut io).expect("check");
    assert!(report.logfile.needs_replay(), "{:?}", report.logfile);
    assert_eq!(report.findings, vec![], "nothing but the log is reported");
    std::fs::remove_file(&img).unwrap();
}
