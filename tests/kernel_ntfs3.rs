//! LINUX'S ntfs3 READS WHAT THIS CRATE WROTE, AND THIS CRATE READS WHAT
//! ntfs3 WROTE (#486).
//!
//! ntfs3 is a third NTFS implementation, independent of this crate, of
//! Windows and of ntfs-3g. A volume our own reader and `check_io` accept can
//! still be one another implementation cannot find a file in: a stale
//! `$FILE_NAME`, or an index entry out of collation order, read back fine
//! here and failed in the kernel. So every volume in this file is mounted
//! by the kernel, in the fs-linux-test-harness VM (`kernel_support`), and
//! compared path by path with what was written.
//!
//! Three directions, as the harness's consumer contract asks: this crate
//! writes and ntfs3 reads; ntfs3 writes and this crate reads; and a byte
//! changed behind the comparison's back must fail it, or the comparison
//! proves nothing.

mod common;
mod kernel_support;

use std::collections::BTreeMap;
use std::path::Path;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::facade::{FileType, Filesystem};
use fs_ntfs::{attr_io, data_runs, mft_io, mkfs, read, write};
use sha2::{Digest, Sha256};

use fs_ntfs::unix_metadata::UnixMetadata;
use kernel_support::{kernel_release, kernel_report, kernel_run, kernel_write};

const VOLUME_BYTES: u64 = 64 << 20;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Bytes in which every 8-byte window differs, so a misplaced cluster
/// cannot pass for the right one.
fn payload(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// An empty volume our own mkfs made.
fn fresh_volume(name: &str, cluster_size: u32) -> String {
    let img = common::temp_image_path(name);
    std::fs::File::create(&img)
        .and_then(|f| f.set_len(VOLUME_BYTES))
        .expect("create image");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open");
    mkfs::format_filesystem(&mut io, VOLUME_BYTES, cluster_size, 1024, None, Some(1))
        .expect("mkfs");
    img
}

/// What a report must say, built from what was written.
#[derive(Default)]
struct Expected(BTreeMap<(String, String), String>);

impl Expected {
    fn dir(&mut self, path: &str) {
        self.0
            .insert(("type".into(), path.into()), "directory".into());
    }

    fn file(&mut self, path: &str, data: &[u8]) {
        let kind = if data.is_empty() {
            "regular-empty-file"
        } else {
            "regular-file"
        };
        self.0.insert(("type".into(), path.into()), kind.into());
        self.0
            .insert(("size".into(), path.into()), data.len().to_string());
        self.0
            .insert(("sha256".into(), path.into()), sha256_hex(data));
    }

    fn symlink(&mut self, path: &str, target: &str) {
        self.0
            .insert(("type".into(), path.into()), "symbolic-link".into());
        self.0.insert(("target".into(), path.into()), target.into());
    }
}

/// Every difference between what was expected and what ntfs3 reported, one
/// per line, so a failure names each path rather than two whole maps.
fn differences(
    expected: &BTreeMap<(String, String), String>,
    got: &BTreeMap<(String, String), String>,
) -> String {
    let mut out = String::new();
    for (key, want) in expected {
        match got.get(key) {
            None => out += &format!("  missing  {} {}: want {want}\n", key.0, key.1),
            Some(have) if have != want => {
                out += &format!(
                    "  differs  {} {}: want {want}, ntfs3 has {have}\n",
                    key.0, key.1
                )
            }
            Some(_) => {}
        }
    }
    for (key, have) in got {
        if !expected.contains_key(key) {
            out += &format!("  extra    {} {}: ntfs3 has {have}\n", key.0, key.1);
        }
    }
    out
}

/// Populate a volume through this crate's write API, recording what each
/// path must read back as.
fn populate(img: &str) -> Expected {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open");
    let mut want = Expected::default();

    write::mkdir_io(&mut io, "/", "docs").expect("mkdir docs");
    write::mkdir_io(&mut io, "/docs", "deep").expect("mkdir deep");
    write::mkdir_io(&mut io, "/docs/deep", "er").expect("mkdir er");
    want.dir("docs");
    want.dir("docs/deep");
    want.dir("docs/deep/er");

    // Enough entries for the index to need INDX blocks.
    write::mkdir_io(&mut io, "/", "many").expect("mkdir many");
    want.dir("many");
    for i in 0..150 {
        let name = format!("entry-{i:03}.txt");
        let data = format!("entry {i}\n").into_bytes();
        write::create_file_io(&mut io, "/many", &name).expect("create");
        write::write_file_contents_io(&mut io, &format!("/many/{name}"), &data).expect("write");
        want.file(&format!("many/{name}"), &data);
    }

    let files: [(&str, usize); 5] = [
        ("/empty", 0),
        ("/small.txt", 100),
        ("/docs/odd.bin", 12_345),
        ("/docs/deep/er/cluster.bin", 65_536),
        ("/docs/big.bin", 3 << 20),
    ];
    for (n, (path, len)) in files.into_iter().enumerate() {
        let (parent, name) = path.rsplit_once('/').unwrap();
        let parent = if parent.is_empty() { "/" } else { parent };
        let data = payload(len, n as u64);
        write::create_file_io(&mut io, parent, name).expect("create");
        if !data.is_empty() {
            write::write_file_contents_io(&mut io, path, &data).expect("write");
        }
        want.file(&path[1..], &data);
    }

    // Sparse: a hole between two runs of data.
    let mut sparse = vec![0u8; 1 << 20];
    sparse[..4096].copy_from_slice(&payload(4096, 90));
    sparse[(1 << 20) - 4096..].copy_from_slice(&payload(4096, 91));
    write::create_file_io(&mut io, "/", "sparse.bin").expect("create");
    write::write_sparse_file_io(&mut io, "/sparse.bin", &sparse).expect("write sparse");
    want.file("sparse.bin", &sparse);

    // A rename, a hard link, an unlink, and a relative symbolic link.
    let renamed = payload(5000, 92);
    write::create_file_io(&mut io, "/docs", "draft.txt").expect("create");
    write::write_file_contents_io(&mut io, "/docs/draft.txt", &renamed).expect("write");
    write::rename_io(&mut io, "/docs/draft.txt", "final-version.txt").expect("rename");
    want.file("docs/final-version.txt", &renamed);

    write::link_io(&mut io, "/docs/odd.bin", "/docs/deep", "odd-link.bin").expect("link");
    want.file("docs/deep/odd-link.bin", &payload(12_345, 2));

    write::create_file_io(&mut io, "/", "gone.txt").expect("create");
    write::unlink_io(&mut io, "/gone.txt").expect("unlink");

    write::create_symlink_io(&mut io, "/", "to-small", "small.txt", true).expect("symlink");
    want.symlink("to-small", "small.txt");

    want
}

#[test]
fn ntfs3_reads_back_what_this_crate_wrote() {
    for cluster_size in [512, 4096] {
        let img = fresh_volume(&format!("kernel_reads_{cluster_size}"), cluster_size);
        let want = populate(&img);
        let (got, dmesg) = kernel_report(&img);
        let diff = differences(&want.0, &got);
        assert!(
            diff.is_empty(),
            "ntfs3 ({}) reads {cluster_size}-byte-cluster {img} differently from what was \
             written:\n{diff}",
            kernel_release()
        );
        assert_eq!(
            dmesg,
            Vec::<String>::new(),
            "ntfs3 complained about {img} ({cluster_size}-byte clusters)"
        );
        std::fs::remove_file(&img).unwrap();
    }
}

/// What this crate reads at every path ntfs3 reported, in the report's own
/// terms, so the two maps compare directly.
fn read_back(
    img: &str,
    reported: &BTreeMap<(String, String), String>,
) -> BTreeMap<(String, String), String> {
    let fs = Filesystem::mount(img).unwrap_or_else(|e| panic!("mount {img}: {e:?}"));
    let mut io = PathIo::open_ro(Path::new(img)).expect("open");
    let mut out = BTreeMap::new();
    for (kind, path) in reported.keys() {
        if kind != "type" {
            continue;
        }
        let at = format!("/{path}");
        let attr = fs.stat(&at).unwrap_or_else(|e| panic!("stat {at}: {e:?}"));
        match attr.file_type {
            FileType::Directory => {
                out.insert(("type".into(), path.clone()), "directory".into());
            }
            FileType::Symlink => {
                let point = write::read_reparse_point_io(&mut io, &at)
                    .unwrap_or_else(|e| panic!("reparse point of {at}: {e:?}"))
                    .unwrap_or_else(|| panic!("{at} is a symlink with no reparse point"));
                let mut value = point.reparse_tag.to_le_bytes().to_vec();
                value.extend_from_slice(&(point.data.len() as u16).to_le_bytes());
                value.extend_from_slice(&[0, 0]);
                value.extend_from_slice(&point.data);
                let target = fs_ntfs::reparse_link_target(&value)
                    .unwrap_or_else(|| panic!("{at} names no link target"));
                out.insert(("type".into(), path.clone()), "symbolic-link".into());
                out.insert(("target".into(), path.clone()), target);
            }
            _ => {
                let mut data = vec![0u8; attr.size as usize + 1];
                let n = fs
                    .read_file(&at, 0, &mut data)
                    .unwrap_or_else(|e| panic!("read {at}: {e:?}"));
                data.truncate(n);
                let kind = if data.is_empty() {
                    "regular-empty-file"
                } else {
                    "regular-file"
                };
                out.insert(("type".into(), path.clone()), kind.into());
                out.insert(("size".into(), path.clone()), data.len().to_string());
                out.insert(("sha256".into(), path.clone()), sha256_hex(&data));
            }
        }
    }
    out
}

#[test]
fn this_crate_reads_back_what_ntfs3_wrote() {
    let img = fresh_volume("kernel_writes", 4096);
    let (reported, dmesg) = kernel_write(
        &img,
        r#"mkdir -p a/b many
seq 1 200000 > a/b/numbers.txt
for i in $(seq 0 399); do printf 'file %d\n' "$i" > "many/f$i"; done
head -c 3000000 /dev/urandom > big.bin
truncate -s 10M sparse.bin
printf 'end' | dd of=sparse.bin bs=1 seek=$((10 * 1024 * 1024 - 3)) conv=notrunc status=none
ln big.bin a/big-link.bin
mv a/b/numbers.txt a/renamed-numbers.txt
touch gone && rm gone
ln -s big.bin to-big
: > empty"#,
    );
    assert_eq!(
        dmesg,
        Vec::<String>::new(),
        "ntfs3 complained writing {img}"
    );
    assert!(
        reported.len() > 400 * 3,
        "the kernel's report is too short to be the tree it wrote: {} entries",
        reported.len()
    );
    let ours = read_back(&img, &reported);
    let diff = differences(&reported, &ours);
    assert!(
        diff.is_empty(),
        "this crate reads what ntfs3 ({}) wrote differently (\"want\" is ntfs3, \
         \"ntfs3 has\" is this crate):\n{diff}",
        kernel_release()
    );
    std::fs::remove_file(&img).unwrap();
}

#[test]
fn a_byte_changed_behind_the_comparison_fails_it() {
    let img = fresh_volume("kernel_negative", 4096);
    let data = payload(64 * 1024, 7);
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open");
        write::create_file_io(&mut io, "/", "f.bin").expect("create");
        write::write_file_contents_io(&mut io, "/f.bin", &data).expect("write");
    }
    // Change one byte of the file's data on disk, where this crate's write
    // put it, without telling anything that would update a checksum or a
    // size: only a reader that compares content can see it.
    let at = {
        let mut io = PathIo::open_ro(Path::new(&img)).expect("open");
        let record = read::resolve_path(&mut io, "/f.bin").expect("resolve");
        let (params, rec) = mft_io::read_mft_record_io(&mut io, record).expect("record");
        let loc = attr_io::find_attribute(&rec, attr_io::AttrType::Data, None).expect("$DATA");
        let start =
            loc.attr_offset + loc.non_resident_mapping_pairs_offset.expect("non-resident") as usize;
        let runs =
            data_runs::decode_runs(&rec[start..loc.attr_offset + loc.attr_length]).expect("runs");
        runs[0].lcn.expect("allocated") * params.cluster_size + 1000
    };
    let mut bytes = std::fs::read(&img).unwrap();
    bytes[at as usize] ^= 0xFF;
    std::fs::write(&img, &bytes).unwrap();

    let mut want = Expected::default();
    want.file("f.bin", &data);
    let (got, _) = kernel_report(&img);
    let diff = differences(&want.0, &got);
    assert!(
        diff.contains("differs  sha256 f.bin"),
        "a changed byte must fail the content comparison, and it found:\n{diff}"
    );
    assert!(
        !diff.contains("size f.bin"),
        "only the content changed, not the size:\n{diff}"
    );
    std::fs::remove_file(&img).unwrap();
}

#[test]
fn what_ntfs3_logs_reaches_the_report() {
    // Every other test asserts that ntfs3 logged nothing, which only means
    // something if what it logs would arrive. A volume whose dirty flag is
    // set is the one case found to make ntfs3 log on a read-only mount
    // (`ntfs3(loop0): It is recommened to use chkdsk.` on 6.12): it
    // mounted, read through and logged NOTHING for a file record whose
    // update-sequence check is broken, a torn $MFT record 0, and a $LogFile
    // overwritten with garbage. ntfs3 grades whether a volume reads back,
    // not whether every record is intact; check_io and chkdsk grade that.
    let img = fresh_volume("kernel_logs", 4096);
    let data = payload(20_000, 11);
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open");
        write::create_file_io(&mut io, "/", "f.bin").expect("create");
        write::write_file_contents_io(&mut io, "/f.bin", &data).expect("write");
    }
    assert!(fs_ntfs::fsck::set_dirty(&img).expect("set dirty"));

    let mut want = Expected::default();
    want.file("f.bin", &data);
    let (got, dmesg) = kernel_report(&img);
    assert!(
        !dmesg.is_empty(),
        "ntfs3 ({}) mounted a dirty volume and nothing it logged reached the report, \
         so the other tests' empty-log assertions would prove nothing",
        kernel_release()
    );
    let diff = differences(&want.0, &got);
    assert!(
        diff.is_empty(),
        "the dirty volume still reads back:\n{diff}"
    );
    std::fs::remove_file(&img).unwrap();
}

/// `stat` of each path in `image` under ntfs3, read-only, as
/// `path -> (uid, gid, mode)` with the mode in octal, and what ntfs3 logged.
fn kernel_owners(image: &str, paths: &[&str]) -> (BTreeMap<String, String>, Vec<String>) {
    let script = paths
        .iter()
        .map(|path| format!("stat -c '%n\t%u %g %a %F' '{path}'"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut owners = BTreeMap::new();
    let mut dmesg = Vec::new();
    for line in kernel_run(image, false, &script).lines() {
        if let Some(log) = line.strip_prefix("dmesg\t\t") {
            dmesg.push(log.to_string());
        } else if let Some((path, rest)) = line.split_once('\t') {
            owners.insert(path.to_string(), rest.to_string());
        }
    }
    (owners, dmesg)
}

#[test]
fn ntfs3_reports_the_owner_group_and_mode_this_crate_wrote() {
    // WSL's `$LXUID`, `$LXGID` and `$LXMOD` (#1195): ntfs3 is the reader
    // that takes a file's owner, group and mode from them, so it grades the
    // bytes. A file with none of them is the control: ntfs3 must not report
    // another file's owner for it, or the comparison proves nothing.
    let img = fresh_volume("kernel_unix_metadata", 4096);
    {
        let mut io = PathIo::open_rw(Path::new(&img)).expect("open");
        write::create_file_io(&mut io, "/", "owned").expect("create");
        write::write_file_contents_io(&mut io, "/owned", b"owned\n").expect("write");
        write::mkdir_io(&mut io, "/", "owned-dir").expect("mkdir");
        write::create_file_io(&mut io, "/", "plain").expect("create");
        write::set_unix_metadata_io(
            &mut io,
            "/owned",
            &UnixMetadata {
                uid: Some(1234),
                gid: Some(5678),
                mode: Some(0o640),
            },
        )
        .expect("owner of /owned");
        write::set_unix_metadata_io(
            &mut io,
            "/owned-dir",
            &UnixMetadata {
                uid: Some(42),
                gid: Some(43),
                mode: Some(0o2750),
            },
        )
        .expect("owner of /owned-dir");
    }
    let (owners, dmesg) = kernel_owners(&img, &["owned", "owned-dir", "plain"]);
    let release = kernel_release();
    assert_eq!(
        owners.get("owned").map(String::as_str),
        Some("1234 5678 640 regular file"),
        "ntfs3 ({release}) reports another owner, group or mode for /owned: {owners:?}"
    );
    assert_eq!(
        owners.get("owned-dir").map(String::as_str),
        Some("42 43 2750 directory"),
        "ntfs3 ({release}) reports another owner, group or mode for /owned-dir: {owners:?}"
    );
    let plain = owners.get("plain").expect("ntfs3 stat of /plain");
    assert!(
        !plain.starts_with("1234 ") && !plain.contains(" 640 "),
        "ntfs3 ({release}) reports /owned's owner or mode for /plain, which has none: {plain}"
    );
    assert_eq!(dmesg, Vec::<String>::new(), "ntfs3 complained about {img}");
    std::fs::remove_file(&img).unwrap();
}

#[test]
fn this_crate_reads_the_owner_group_and_mode_ntfs3_wrote() {
    let img = fresh_volume("kernel_unix_metadata_back", 4096);
    let (_, dmesg) = kernel_write(
        &img,
        "cd \"$MNT\"\n\
         printf made > made\n\
         chown 2001:2002 made\n\
         chmod 0604 made\n\
         mkdir made-dir\n\
         chown 3001:3002 made-dir\n\
         chmod 0715 made-dir",
    );
    assert_eq!(dmesg, Vec::<String>::new(), "ntfs3 complained about {img}");
    let release = kernel_release();
    for (path, want) in [
        ("/made", (2001, 2002, 0o100_604)),
        ("/made-dir", (3001, 3002, 0o040_715)),
    ] {
        let got = write::read_unix_metadata(Path::new(&img), path).unwrap_or_else(|e| {
            panic!("read the WSL attributes ntfs3 ({release}) wrote on {path}: {e:?}")
        });
        assert_eq!(
            got,
            UnixMetadata {
                uid: Some(want.0),
                gid: Some(want.1),
                mode: Some(want.2),
            },
            "{path} as ntfs3 ({release}) left it"
        );
    }
    std::fs::remove_file(&img).unwrap();
}
