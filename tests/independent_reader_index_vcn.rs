//! The independent test reader on a volume whose clusters are larger than
//! its index blocks (#1298).
//!
//! An index entry names the index block below it by VCN. Windows' formatter
//! gives a 64 KiB-cluster volume 4096-byte index blocks and declares them in
//! the root's `$INDEX_ROOT` as eight 512-byte units, and ntfs-3g, growing that
//! root, writes its second block at byte 4096 of the allocation with VCN 8.
//! The `ntfs` crate the tests read volumes with once multiplied every VCN by
//! the cluster size and sought past the allocation, so a test that reads a
//! large directory on such a volume could not run. The volume here is the one
//! Windows formatted (`test-disks/build-windows-native-read-fixtures.ps1`),
//! grown by ntfs-3g rather than by this crate. Linux-only: ntfs-3g writes it.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;
use std::process::{Command, Output};

const WINDOWS_IMG: &str = "test-disks/ntfs-format-64k.img";

fn ntfs_3g(tool: &str, arguments: &[&str]) -> Output {
    // `ntfscp` and `ntfsfix` install into /usr/sbin.
    let path = format!(
        "/usr/sbin:/sbin:{}",
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(tool)
        .args(arguments)
        .env("PATH", path)
        .output()
        .unwrap_or_else(|error| panic!("{tool}: install ntfs-3g: {error}"));
    assert!(
        output.status.success(),
        "{tool} {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn the_reader_follows_index_blocks_smaller_than_a_cluster() {
    let path = common::temp_image_path("windows-64k-grown-root");
    std::fs::copy(WINDOWS_IMG, &path).unwrap_or_else(|error| {
        panic!(
            "{WINDOWS_IMG}: {error} (the Windows-authored fixtures come from \
             test-disks/build-windows-native-read-fixtures.ps1)"
        )
    });
    let sources = common::temp_image_path("windows-64k-grown-sources");
    std::fs::create_dir(&sources).unwrap();
    // Enough names for the root's index to need three blocks.
    let files: Vec<(String, Vec<u8>)> = (0..48u32)
        .map(|n| {
            let length = [0, 1, 100, 300, 4096, 70_000][n as usize % 6] + n as usize;
            let bytes = (0..length)
                .map(|at| (at as u32).wrapping_mul(37).wrapping_add(n) as u8)
                .collect();
            (format!("grown-by-ntfs-3g-{n:02}.bin"), bytes)
        })
        .collect();
    for (name, bytes) in &files {
        let source = Path::new(&sources).join(name);
        std::fs::write(&source, bytes).unwrap();
        ntfs_3g(
            "ntfscp",
            &[&path, source.to_str().unwrap(), &format!("/{name}")],
        );
    }
    ntfs_3g("ntfsfix", &["-n", &path]);

    let (ntfs, mut reader) = common::open(&path);
    assert_eq!(ntfs.cluster_size(), 65536);
    let names = common::list_names(&ntfs, &mut reader, "/");
    for (name, bytes) in &files {
        assert!(names.contains(name), "{name}: {names:?}");
        let read = common::read_file_all(&ntfs, &mut reader, &format!("/{name}"));
        assert!(read == *bytes, "{name}");
    }

    std::fs::remove_file(&path).unwrap();
    for (name, _) in &files {
        std::fs::remove_file(Path::new(&sources).join(name)).unwrap();
    }
    std::fs::remove_dir(&sources).unwrap();
}
