//! A volume another implementation populated, read back and written (#485).
//!
//! Every other read-path fixture holds a handful of files, made by Windows
//! or by this crate. One volume ntfs-3g filled the way a user's would be
//! found four reader bugs at once (#468, #471, #472, #473), all in shapes
//! the small fixtures never make: an `$INDEX_ROOT` moved to an extension
//! record, streams in extension records, a `$DATA` run list split across
//! records, and an existing `$ATTRIBUTE_LIST`.
//!
//! `test-disks/build-ntfs-feature-images.sh populated` builds that volume at
//! a 512-byte and a 4 KiB cluster with `mkntfs` and an `ntfs-3g` mount, and
//! writes beside each a manifest of what the mount reported: every
//! directory, and every file's and named stream's size and SHA-256. Here:
//!
//! - every directory lists exactly the names the manifest has in it;
//! - every file reads back byte for byte, whole and in windows;
//! - the sixty names of one file resolve to one record;
//! - the named streams list and read back;
//! - `check_io` finds nothing;
//! - and after this crate writes to a copy (a stream rewritten and
//!   removed, a file created with a stream added and one rewritten, a name
//!   unlinked from the sixty and from the wide directory), ntfs-3g's own
//!   tools read back what was written and still mount the volume
//!   read-write. A stream that would grow in the extension record ntfs-3g
//!   filled is refused, because this crate writes no `$ATTRIBUTE_LIST`
//!   (#480).
//!
//! A missing image or manifest fails, naming the builder: nothing skips.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::PathIo;
use fs_ntfs::facade::Filesystem;
use sha2::{Digest, Sha256};

/// What the manifest says of one volume.
#[derive(Default)]
struct Manifest {
    dirs: BTreeSet<String>,
    /// path -> (size, sha256)
    files: BTreeMap<String, (u64, String)>,
    /// (path, stream) -> (size, sha256)
    streams: BTreeMap<(String, String), (u64, String)>,
}

fn fixture(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test-disks")
        .join(name);
    assert!(
        p.exists(),
        "{} is missing: build it with `bash test-disks/build-ntfs-feature-images.sh populated` \
         (Linux, mkntfs and ntfs-3g)",
        p.display()
    );
    p
}

fn manifest(cluster: u32) -> Manifest {
    let text = std::fs::read_to_string(fixture(&format!("ntfs-populated-{cluster}.manifest.tsv")))
        .unwrap();
    let mut m = Manifest::default();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["D", path] => {
                m.dirs.insert((*path).to_string());
            }
            ["F", path, size, sha] => {
                m.files.insert(
                    (*path).to_string(),
                    (size.parse().unwrap(), (*sha).to_string()),
                );
            }
            ["S", path, stream, size, sha] => {
                m.streams.insert(
                    ((*path).to_string(), (*stream).to_string()),
                    (size.parse().unwrap(), (*sha).to_string()),
                );
            }
            _ => panic!("a manifest line this test does not know: {line:?}"),
        }
    }
    // 600 wide entries, 6 compressed files, the target and its 60 other
    // names, the stream file, the sparse and the two fragmented files, the
    // four oddly named ones, the deep file and the empty one.
    assert!(
        m.files.len() == 677 && m.streams.len() == 41,
        "the manifest holds {} files and {} streams, where the builder makes 677 and 41",
        m.files.len(),
        m.streams.len()
    );
    m
}

fn sha(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// The parent directory of a manifest path, and its last name.
fn split(path: &str) -> (&str, &str) {
    let at = path.rfind('/').unwrap();
    (if at == 0 { "/" } else { &path[..at] }, &path[at + 1..])
}

/// A file's whole contents, read through this crate in 1 MiB windows.
fn read_all(fs: &Filesystem, path: &str, size: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(size as usize);
    let mut buf = vec![0u8; 1 << 20];
    while (out.len() as u64) < size {
        let n = fs
            .read_file(path, out.len() as u64, &mut buf)
            .unwrap_or_else(|e| panic!("read {path} at {}: {e}", out.len()));
        assert!(n > 0, "{path}: a read at {} returned nothing", out.len());
        out.extend_from_slice(&buf[..n]);
    }
    out
}

fn reads_back(cluster: u32) {
    let m = manifest(cluster);
    let img = fixture(&format!("ntfs-populated-{cluster}.img"));
    let fs = Filesystem::mount(&img).unwrap_or_else(|e| panic!("mount {cluster}: {e}"));

    // Every directory lists exactly the names the manifest puts in it.
    let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    children.insert("/".into(), BTreeSet::new());
    for d in &m.dirs {
        children.entry(d.clone()).or_default();
    }
    for path in m.dirs.iter().chain(m.files.keys()) {
        let (parent, name) = split(path);
        children
            .entry(parent.to_string())
            .or_default()
            .insert(name.to_string());
    }
    for (dir, want) in &children {
        let listed: BTreeSet<String> = fs
            .read_dir(dir)
            .unwrap_or_else(|e| panic!("{cluster}: list {dir}: {e}"))
            .into_iter()
            .map(|e| e.name)
            .filter(|n| n != "." && n != "..")
            .filter(|n| !(dir == "/" && n.starts_with('$')))
            .collect();
        let want: BTreeSet<String> = if dir == "/" {
            want.iter()
                .filter(|n| !n.starts_with('$'))
                .cloned()
                .collect()
        } else {
            want.clone()
        };
        assert_eq!(&listed, &want, "{cluster}: the listing of {dir}");
    }

    // Every file, whole and in windows.
    for (path, (size, digest)) in &m.files {
        let st = fs
            .stat(path)
            .unwrap_or_else(|e| panic!("{cluster}: stat {path}: {e}"));
        assert_eq!(st.size, *size, "{cluster}: the size of {path}");
        let data = read_all(&fs, path, *size);
        assert_eq!(&sha(&data), digest, "{cluster}: the contents of {path}");
        for at in [0, size / 2, size.saturating_sub(4096)] {
            let mut buf = vec![0u8; 4096];
            let n = fs.read_file(path, at, &mut buf).unwrap();
            let end = (at as usize + n).min(data.len());
            assert_eq!(
                &buf[..n],
                &data[at as usize..end],
                "{cluster}: {path}, the window at {at}"
            );
        }
    }

    // The sixty names of one file are one record.
    let target = fs.stat("/links/target").unwrap().file_record_number;
    let names: Vec<&String> = m
        .files
        .keys()
        .filter(|p| p.starts_with("/links/"))
        .collect();
    assert_eq!(names.len(), 61, "{cluster}: the names in /links");
    for p in names {
        assert_eq!(
            fs.stat(p).unwrap().file_record_number,
            target,
            "{cluster}: {p} is another record"
        );
    }

    // The named streams list and read back.
    let listed: BTreeSet<String> = fs_ntfs::write::list_named_streams(&img, "/streams.txt")
        .unwrap()
        .into_iter()
        .collect();
    let want: BTreeSet<String> = m.streams.keys().map(|(_, s)| s.clone()).collect();
    assert_eq!(listed, want, "{cluster}: the streams of /streams.txt");
    let mut io = PathIo::open_ro(&img).unwrap();
    let record = fs_ntfs::read::resolve_path(&mut io, "/streams.txt").unwrap();
    for ((path, stream), (size, digest)) in &m.streams {
        let data = fs_ntfs::read::read_attribute_range(
            &mut io,
            record,
            AttrType::Data,
            Some(stream),
            0,
            *size as usize,
        )
        .unwrap_or_else(|e| panic!("{cluster}: read {path}:{stream}: {e}"));
        assert_eq!(&sha(&data), digest, "{cluster}: {path}:{stream}");
    }

    // And the checker finds nothing.
    let report = fs_ntfs::fsck::check_io(&mut io).unwrap();
    assert!(
        !report.dirty && report.findings.is_empty(),
        "{cluster}: check_io: {report:?}"
    );
}

#[test]
fn a_volume_ntfs3g_populated_reads_back_at_512_byte_clusters() {
    reads_back(512);
}

#[test]
fn a_volume_ntfs3g_populated_reads_back_at_4k_clusters() {
    reads_back(4096);
}

/// `ntfscat [-n stream] img path`: what ntfs-3g reads there, or `None`
/// when it finds nothing to read.
fn ntfscat(img: &Path, path: &str, stream: Option<&str>) -> Option<Vec<u8>> {
    let mut cmd = Command::new("ntfscat");
    if let Some(s) = stream {
        cmd.args(["-n", s]);
    }
    let out = cmd.arg(img).arg(path).output().unwrap_or_else(|e| {
        panic!("ntfscat could not be run ({e}): install ntfs-3g (`apt-get install ntfs-3g`)")
    });
    out.status.success().then_some(out.stdout)
}

fn written_on(cluster: u32) {
    let m = manifest(cluster);
    let img = PathBuf::from(common::temp_image_path(format!("ntfs3g_written_{cluster}")));
    std::fs::copy(fixture(&format!("ntfs-populated-{cluster}.img")), &img).unwrap();

    let fs = Filesystem::mount_rw(&img).unwrap_or_else(|e| panic!("mount_rw {cluster}: {e}"));
    // ntfs-3g spread streams.txt's forty streams over extension records
    // it named in an $ATTRIBUTE_LIST, and filled them. A stream rewritten
    // at its own length fits where it is.
    let rewritten = b"STREAM 5 OF 40\n".to_vec();
    fs.write_named_stream("/streams.txt", "s5", &rewritten)
        .unwrap_or_else(|e| panic!("{cluster}: rewrite s5: {e}"));
    // One that grows needs room its record does not have, and this crate
    // writes no $ATTRIBUTE_LIST to move it with (#480), so the write is
    // refused, and refused before anything is written.
    let grown_s5 = b"stream five, grown by this crate\n".repeat(40);
    match fs.write_named_stream("/streams.txt", "s5", &grown_s5) {
        Err(e) => assert!(
            e.to_string().contains("exceeds record capacity"),
            "{cluster}: growing s5 was refused, but not for its record's room: {e}"
        ),
        Ok(()) => panic!(
            "{cluster}: s5 grew in a full extension record; #480 is done, so assert the bytes \
             ntfs-3g reads instead"
        ),
    }
    fs.delete_named_stream("/streams.txt", "s6")
        .unwrap_or_else(|e| panic!("{cluster}: delete s6: {e}"));
    fs.create_file("/", "made-here.txt")
        .unwrap_or_else(|e| panic!("{cluster}: create: {e}"));
    let made = b"written by this crate onto a volume ntfs-3g filled\n".repeat(300);
    fs.write_file_contents("/made-here.txt", &made)
        .unwrap_or_else(|e| panic!("{cluster}: write the new file: {e}"));
    // A new stream goes on the new file, whose record has the room.
    let added = b"a stream added by this crate\n".to_vec();
    fs.write_named_stream("/made-here.txt", "added", &added)
        .unwrap_or_else(|e| panic!("{cluster}: add a stream: {e}"));
    let grown = b"umlaut, and more after it\n".repeat(100);
    fs.write_file_contents("/grüße.txt", &grown)
        .unwrap_or_else(|e| panic!("{cluster}: rewrite grüße.txt: {e}"));
    let gone_link = m
        .files
        .keys()
        .find(|p| p.starts_with("/links/l"))
        .unwrap()
        .clone();
    fs.unlink(&gone_link)
        .unwrap_or_else(|e| panic!("{cluster}: unlink {gone_link}: {e}"));
    let gone_wide = m
        .files
        .keys()
        .find(|p| p.starts_with("/wide/"))
        .unwrap()
        .clone();
    fs.unlink(&gone_wide)
        .unwrap_or_else(|e| panic!("{cluster}: unlink {gone_wide}: {e}"));
    drop(fs);

    // ntfs-3g reads back what was written, and not what was removed.
    assert_eq!(
        ntfscat(&img, "/streams.txt", Some("s5")).as_deref(),
        Some(&rewritten[..]),
        "{cluster}: ntfs-3g's s5"
    );
    assert_eq!(
        ntfscat(&img, "/made-here.txt", Some("added")).as_deref(),
        Some(&added[..]),
        "{cluster}: ntfs-3g's /made-here.txt:added"
    );
    assert_eq!(
        ntfscat(&img, "/streams.txt", Some("s6")),
        None,
        "{cluster}: ntfs-3g still reads the deleted s6"
    );
    assert_eq!(
        ntfscat(&img, "/streams.txt", Some("s7")).map(|d| sha(&d)),
        m.streams
            .get(&("/streams.txt".to_string(), "s7".to_string()))
            .map(|(_, d)| d.clone()),
        "{cluster}: ntfs-3g's s7, which nothing touched"
    );
    assert_eq!(
        ntfscat(&img, "/made-here.txt", None).as_deref(),
        Some(&made[..]),
        "{cluster}: ntfs-3g's /made-here.txt"
    );
    assert_eq!(
        ntfscat(&img, "/grüße.txt", None).as_deref(),
        Some(&grown[..]),
        "{cluster}: ntfs-3g's /grüße.txt"
    );
    assert_eq!(
        ntfscat(&img, "/links/target", None).map(|d| sha(&d)),
        m.files.get("/links/target").map(|(_, d)| d.clone()),
        "{cluster}: ntfs-3g's /links/target, one name fewer"
    );
    for gone in [&gone_link, &gone_wide] {
        assert_eq!(
            ntfscat(&img, gone, None),
            None,
            "{cluster}: ntfs-3g still finds {gone}"
        );
    }

    // And it would still mount the volume read-write.
    let probe = Command::new("ntfs-3g.probe")
        .arg("--readwrite")
        .arg(&img)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "ntfs-3g.probe could not be run ({e}): install ntfs-3g (`apt-get install ntfs-3g`)"
            )
        });
    assert!(
        probe.status.success(),
        "{cluster}: ntfs-3g.probe --readwrite: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
}

#[test]
fn ntfs3g_reads_what_this_crate_wrote_on_its_volume_at_512_byte_clusters() {
    written_on(512);
}

#[test]
fn ntfs3g_reads_what_this_crate_wrote_on_its_volume_at_4k_clusters() {
    written_on(4096);
}
