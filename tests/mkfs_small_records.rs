mod common;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::{format_with_plan, plan_format, rec};
use fs_ntfs::{mft_io, read};
use ntfs::{Ntfs, NtfsAttributeType};

const SIZE: u64 = 161 << 20;
const CLUSTERS: [u32; 8] = [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536];

fn format_image(cluster: u32) -> String {
    let path = common::temp_image_path(format!("mkfs-512-{cluster}"));
    File::create(&path).unwrap().set_len(SIZE).unwrap();
    let mut io = PathIo::open_rw(Path::new(&path)).unwrap();
    let plan = plan_format(SIZE, cluster, 512, Some("SMALL"), Some(1295)).unwrap();
    format_with_plan(&mut io, &plan).unwrap();
    io.sync().unwrap();
    path
}

fn secure_streams(path: &str) -> BTreeMap<String, Vec<u8>> {
    let mut reader = BufReader::new(File::open(path).unwrap());
    let mut ntfs = Ntfs::new(&mut reader).unwrap();
    ntfs.read_upcase_table(&mut reader).unwrap();
    assert_eq!(ntfs.file_record_size(), 512);
    assert_eq!(
        common::list_names(&ntfs, &mut reader, "/$Extend"),
        ["$ObjId", "$Quota", "$Reparse"]
    );
    let secure = ntfs.file(&mut reader, u64::from(rec::SECURE)).unwrap();
    let mut attributes = secure.attributes();
    let mut streams = BTreeMap::new();
    while let Some(item) = attributes.next(&mut reader) {
        let item = item.unwrap();
        let attribute = item.to_attribute().unwrap();
        let name = attribute.name().unwrap().to_string().unwrap();
        if name.is_empty() {
            continue;
        }
        assert!(matches!(
            attribute.ty().unwrap(),
            NtfsAttributeType::Data | NtfsAttributeType::IndexRoot
        ));
        let value = attribute.value(&mut reader).unwrap();
        let mut bytes = vec![0; attribute.value_length() as usize];
        value.attach(&mut reader).read_exact(&mut bytes).unwrap();
        assert!(streams.insert(name, bytes).is_none());
    }
    streams
}

#[test]
fn accepted_512_byte_records_format_and_reopen_in_an_independent_reader() {
    for cluster in CLUSTERS {
        let path = format_image(cluster);
        let streams = secure_streams(&path);
        assert_eq!(
            streams.keys().map(String::as_str).collect::<Vec<_>>(),
            ["$SDH", "$SDS", "$SII"]
        );
        let sds = &streams["$SDS"];
        assert_eq!(sds.len(), 0x4007c);
        assert_eq!(&sds[..124], &sds[0x40000..]);
        assert_eq!(u32::from_le_bytes(sds[4..8].try_into().unwrap()), 0x100);
        for (name, key_length) in [("$SDH", 8), ("$SII", 4)] {
            let root = &streams[name];
            let entries_offset = 16 + u32::from_le_bytes(root[16..20].try_into().unwrap()) as usize;
            let entry = &root[entries_offset..];
            assert_eq!(
                u16::from_le_bytes(entry[10..12].try_into().unwrap()),
                key_length
            );
            let value_offset = u16::from_le_bytes(entry[..2].try_into().unwrap()) as usize;
            assert_eq!(&entry[value_offset..value_offset + 20], &sds[..20]);
        }
        let mut io = PathIo::open_ro(Path::new(&path)).unwrap();
        let list = read::read_attribute_value(
            &mut io,
            u64::from(rec::SECURE),
            AttrType::AttributeList,
            None,
        )
        .unwrap();
        assert!(!read::parse_attribute_list(&list).unwrap().is_empty());
        for record in [rec::OBJID, rec::REPARSE, rec::QUOTA] {
            let (_, bytes) = mft_io::read_mft_record_io(&mut io, u64::from(record)).unwrap();
            assert_ne!(
                u16::from_le_bytes(bytes[0x16..0x18].try_into().unwrap()) & 1,
                0
            );
        }
        let plan = plan_format(SIZE, cluster, 512, Some("SMALL"), Some(1295)).unwrap();
        let mut mft = vec![0; (plan.mftmirr_records * 512) as usize];
        let mut mirror = mft.clone();
        io.read_exact_at(plan.mft_lcn * u64::from(cluster), &mut mft)
            .unwrap();
        io.read_exact_at(plan.mftmirr_lcn * u64::from(cluster), &mut mirror)
            .unwrap();
        assert_eq!(mft, mirror, "mirror at {cluster}-byte clusters");
        drop(io);
        std::fs::remove_file(&path).unwrap();
    }
}

/// Runs one ntfs-3g tool, the independent implementation these tests
/// check against, and returns its output once it has succeeded.
#[cfg(target_os = "linux")]
fn ntfs_3g(tool: &str, arguments: &[&str]) -> std::process::Output {
    // `ntfscp` and `ntfsfix` install into /usr/sbin.
    let path = format!(
        "/usr/sbin:/sbin:{}",
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new(tool)
        .args(arguments)
        .env("PATH", path)
        .output()
        .unwrap_or_else(|error| {
            panic!("{tool}: install ntfs-3g for the small-record oracle: {error}")
        });
    assert!(
        output.status.success(),
        "{tool} {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[cfg(target_os = "linux")]
#[test]
fn ntfs_3g_reads_the_512_byte_record_layout_without_repair() {
    for cluster in CLUSTERS {
        let path = format_image(cluster);
        let mut io = PathIo::open_ro(Path::new(&path)).unwrap();
        let fix = ntfs_3g("ntfsfix", &["-n", &path]);
        assert!(
            fix.stderr.is_empty(),
            "ntfsfix diagnostic at cluster {cluster}: {}",
            String::from_utf8_lossy(&fix.stderr)
        );

        // ntfsls names each of $Extend's three view-index files and cannot
        // classify them, as it cannot for the same files on a volume
        // Windows formatted (test-disks/ntfs-format-64k.img, records 24 to
        // 26). Nothing else may be reported.
        let ls = ntfs_3g("ntfsls", &["-a", "-s", "-p", "/$Extend", &path]);
        let names = String::from_utf8(ls.stdout).unwrap();
        for name in ["$ObjId", "$Quota", "$Reparse"] {
            assert!(names.lines().any(|line| line.trim() == name), "{names}");
        }
        let mut diagnostics: Vec<_> = String::from_utf8(ls.stderr)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        diagnostics.sort();
        assert_eq!(
            diagnostics,
            [rec::OBJID, rec::REPARSE, rec::QUOTA]
                .map(|record| format!("Could not decode the type of inode {record}")),
            "ntfsls diagnostics at cluster {cluster}"
        );

        for (stream, attribute) in [("$SDS", "0x80"), ("$SDH", "0x90"), ("$SII", "0x90")] {
            let ty = if stream == "$SDS" {
                AttrType::Data
            } else {
                AttrType::IndexRoot
            };
            let expected =
                read::read_attribute_value(&mut io, u64::from(rec::SECURE), ty, Some(stream))
                    .unwrap();
            let cat = ntfs_3g(
                "ntfscat",
                &["-i", "9", "-a", attribute, "-n", stream, &path],
            );
            assert!(
                cat.stderr.is_empty(),
                "ntfscat {stream} at cluster {cluster}"
            );
            assert_eq!(cat.stdout, expected, "{stream} at cluster {cluster}");
        }
        drop(io);
        std::fs::remove_file(&path).unwrap();
    }
}

/// The independent reader's 512-byte support, checked on records it did
/// not see this crate write: ntfs-3g creates files on a formatted volume,
/// allocating their 512-byte records and indexing them in the root, and
/// the reader must find every one with the bytes ntfs-3g wrote. Clusters
/// of 8 KiB and more need the reader to follow index blocks smaller than a
/// cluster (#1298).
#[cfg(target_os = "linux")]
#[test]
fn records_ntfs_3g_writes_reopen_in_the_independent_reader() {
    let sources = common::temp_image_path("mkfs-512-sources");
    std::fs::create_dir(&sources).unwrap();
    // Resident and non-resident sizes, and enough names to grow the root's
    // index past one block.
    let files: Vec<(String, Vec<u8>)> = (0..48u32)
        .map(|n| {
            let length = [0, 1, 100, 300, 4096, 70_000][n as usize % 6] + n as usize;
            let bytes = (0..length)
                .map(|at| (at as u32).wrapping_mul(31).wrapping_add(n) as u8)
                .collect();
            (format!("written-by-ntfs-3g-{n:02}.bin"), bytes)
        })
        .collect();
    for (name, bytes) in &files {
        std::fs::write(Path::new(&sources).join(name), bytes).unwrap();
    }
    for cluster in CLUSTERS {
        let path = format_image(cluster);
        for (name, _) in &files {
            let source = Path::new(&sources).join(name);
            ntfs_3g(
                "ntfscp",
                &[&path, source.to_str().unwrap(), &format!("/{name}")],
            );
        }
        ntfs_3g("ntfsfix", &["-n", &path]);

        let (ntfs, mut reader) = common::open(&path);
        assert_eq!(ntfs.file_record_size(), 512);
        let names = common::list_names(&ntfs, &mut reader, "/");
        for (name, bytes) in &files {
            assert!(
                names.contains(name),
                "{name} at cluster {cluster}: {names:?}"
            );
            let read = common::read_file_all(&ntfs, &mut reader, &format!("/{name}"));
            assert!(read == *bytes, "{name} at cluster {cluster}");
            let cat = ntfs_3g("ntfscat", &[&path, &format!("/{name}")]);
            assert!(cat.stdout == *bytes, "ntfscat {name} at cluster {cluster}");
        }
        std::fs::remove_file(&path).unwrap();
    }
    for (name, _) in &files {
        std::fs::remove_file(Path::new(&sources).join(name)).unwrap();
    }
    std::fs::remove_dir(&sources).unwrap();
}

/// `$MFTMirr` covers a whole cluster of records, and past record 16 those
/// are formatted as free records. `$ObjId`, `$Reparse` and `$Quota` are
/// records 16 to 18, so a mirror of more than sixteen records must leave
/// them in use: 1024-byte records at 32 KiB clusters and up, and 512-byte
/// records at 16 KiB clusters and up (#1297).
#[test]
fn extend_children_stay_in_use_when_the_mirror_covers_them() {
    for (record_size, clusters) in [(1024, &CLUSTERS[6..]), (512, &CLUSTERS[5..])] {
        for &cluster in clusters {
            let path = common::temp_image_path(format!("mkfs-mirror-{record_size}-{cluster}"));
            File::create(&path).unwrap().set_len(SIZE).unwrap();
            let mut io = PathIo::open_rw(Path::new(&path)).unwrap();
            let plan = plan_format(SIZE, cluster, record_size, Some("MIRROR"), Some(1295)).unwrap();
            assert!(plan.mftmirr_records > u64::from(rec::QUOTA));
            format_with_plan(&mut io, &plan).unwrap();
            io.sync().unwrap();
            for record in [rec::OBJID, rec::REPARSE, rec::QUOTA] {
                let (_, bytes) = mft_io::read_mft_record_io(&mut io, u64::from(record)).unwrap();
                assert_ne!(
                    u16::from_le_bytes(bytes[0x16..0x18].try_into().unwrap()) & 1,
                    0,
                    "record {record} at {record_size}-byte records, {cluster}-byte clusters"
                );
            }
            let (ntfs, mut reader) = common::open(&path);
            assert_eq!(
                common::list_names(&ntfs, &mut reader, "/$Extend"),
                ["$ObjId", "$Quota", "$Reparse"]
            );
            drop((io, reader));
            std::fs::remove_file(&path).unwrap();
        }
    }
}
