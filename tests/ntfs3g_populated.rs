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
//! - and after this crate writes to a copy (a stream rewritten in place or
//!   grown into an extension record, a file created with a stream added and
//!   one rewritten), ntfs-3g's own tools read back what was written and still
//!   mount the volume read-write. Deleting a stream or unlinking one of the
//!   sixty names or a name in the 600-name directory is refused, because
//!   this crate does not maintain those `$ATTRIBUTE_LIST` entries (#468,
//!   #480), and ntfs-3g reads the refused changes as untouched;
//! - unlinking the last name of a file whose streams fill extension
//!   records frees those records and the clusters they map (#565).
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

/// A change refused because the record carries an `$ATTRIBUTE_LIST` this
/// crate does not maintain.
fn refused_for_the_list(cluster: u32, what: &str, result: Result<(), fs_ntfs::facade::Error>) {
    match result {
        Err(e) => assert!(
            e.to_string().contains("$ATTRIBUTE_LIST"),
            "{cluster}: {what} was refused, but not for the attribute list: {e}"
        ),
        Ok(()) => panic!(
            "{cluster}: {what} went through on a record with an $ATTRIBUTE_LIST, which this \
             crate does not maintain"
        ),
    }
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
    // Growing this stream outgrows its extension record. The writer must
    // add an extent through the existing $ATTRIBUTE_LIST; ntfs-3g below
    // independently verifies the resulting bytes.
    let grown_s5 = b"stream five, grown by this crate\n".repeat(40);
    fs.write_named_stream("/streams.txt", "s5", &grown_s5)
        .unwrap_or_else(|e| panic!("{cluster}: grow s5: {e}"));
    // Removing an attribute from a record ntfs-3g gave an $ATTRIBUTE_LIST
    // would leave the list naming one that is gone, and this crate does
    // not maintain the list (#468, #480), so the removal is refused.
    refused_for_the_list(
        cluster,
        "delete s6",
        fs.delete_named_stream("/streams.txt", "s6"),
    );
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
    // The sixty names fill an $ATTRIBUTE_LIST too, and one name is one
    // $FILE_NAME attribute, so dropping it is refused for the same reason.
    refused_for_the_list(
        cluster,
        &format!("unlink {gone_link}"),
        fs.unlink(&gone_link),
    );
    let gone_wide = m
        .files
        .keys()
        .find(|p| p.starts_with("/wide/"))
        .unwrap()
        .clone();
    // ntfs-3g gave /wide, with its 600 names, an $ATTRIBUTE_LIST of its own,
    // and the index root a name is removed from is not in the directory's
    // base record, which is the only one this crate edits; the removal is
    // refused for the list, before anything is written.
    refused_for_the_list(
        cluster,
        &format!("unlink {gone_wide}"),
        fs.unlink(&gone_wide),
    );
    drop(fs);

    // ntfs-3g reads back what was written, and not what was removed.
    assert_eq!(
        ntfscat(&img, "/streams.txt", Some("s5")).as_deref(),
        Some(&grown_s5[..]),
        "{cluster}: ntfs-3g's grown s5"
    );
    assert_eq!(
        ntfscat(&img, "/made-here.txt", Some("added")).as_deref(),
        Some(&added[..]),
        "{cluster}: ntfs-3g's /made-here.txt:added"
    );
    for untouched in ["s6", "s7"] {
        assert_eq!(
            ntfscat(&img, "/streams.txt", Some(untouched)).map(|d| sha(&d)),
            m.streams
                .get(&("/streams.txt".to_string(), untouched.to_string()))
                .map(|(_, d)| d.clone()),
            "{cluster}: ntfs-3g's {untouched}, which the refused delete and nothing else touched"
        );
    }
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
    for name in ["/links/target", gone_link.as_str()] {
        assert_eq!(
            ntfscat(&img, name, None).map(|d| sha(&d)),
            m.files.get("/links/target").map(|(_, d)| d.clone()),
            "{cluster}: ntfs-3g's {name}, every name kept by the refused unlink"
        );
    }
    assert_eq!(
        ntfscat(&img, &gone_wide, None).map(|d| sha(&d)),
        m.files.get(&gone_wide).map(|(_, d)| d.clone()),
        "{cluster}: ntfs-3g's {gone_wide}, kept by the refused unlink"
    );

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

/// Every cluster the non-resident attributes of one MFT record map.
fn clusters_mapped(record: &[u8]) -> u64 {
    fs_ntfs::attr_io::iter_attributes(record)
        .filter(|l| !l.is_resident)
        .filter_map(|l| {
            let start = l.attr_offset + l.non_resident_mapping_pairs_offset? as usize;
            Some(
                fs_ntfs::data_runs::decode_runs(&record[start..l.attr_offset + l.attr_length])
                    .unwrap(),
            )
        })
        .flatten()
        .filter(|run| run.lcn.is_some())
        .map(|run| run.length)
        .sum()
}

/// Unlinking the last name of a file whose attributes ntfs-3g spread over
/// extension records frees those records and every cluster their
/// attributes map, as well as the base record's (#565). They used to stay
/// in use, owned by a base record that was not.
fn unlinked_on(cluster: u32) {
    let img = PathBuf::from(common::temp_image_path(format!(
        "ntfs3g_unlinked_{cluster}"
    )));
    std::fs::copy(fixture(&format!("ntfs-populated-{cluster}.img")), &img).unwrap();

    let mut io = PathIo::open_ro(&img).unwrap();
    let base = fs_ntfs::read::resolve_path(&mut io, "/streams.txt").unwrap();
    let list =
        fs_ntfs::read::read_attribute_value(&mut io, base, AttrType::AttributeList, None).unwrap();
    let extensions: BTreeSet<u64> = fs_ntfs::read::parse_attribute_list(&list)
        .unwrap()
        .into_iter()
        .map(|e| e.record_number)
        .filter(|&r| r != base)
        .collect();
    assert!(
        !extensions.is_empty(),
        "{cluster}: ntfs-3g put none of /streams.txt's attributes in an extension record"
    );
    let mut mapped = 0;
    for &r in std::iter::once(&base).chain(&extensions) {
        mapped += clusters_mapped(&fs_ntfs::mft_io::read_mft_record_io(&mut io, r).unwrap().1);
    }
    let bm = fs_ntfs::bitmap::locate_bitmap_io(&mut io).unwrap();
    let mbm = fs_ntfs::mft_bitmap::locate_io(&mut io).unwrap();
    let free_clusters = fs_ntfs::bitmap::count_free_io(&mut io, &bm).unwrap();
    let free_records = fs_ntfs::mft_bitmap::count_free_io(&mut io, &mbm).unwrap();
    drop(io);

    let fs = Filesystem::mount_rw(&img).unwrap_or_else(|e| panic!("mount_rw {cluster}: {e}"));
    fs.unlink("/streams.txt")
        .unwrap_or_else(|e| panic!("{cluster}: unlink /streams.txt: {e}"));
    drop(fs);

    let mut io = PathIo::open_ro(&img).unwrap();
    for &r in std::iter::once(&base).chain(&extensions) {
        let (_, record) = fs_ntfs::mft_io::read_mft_record_io(&mut io, r).unwrap();
        assert_eq!(
            fs_ntfs::mft_io::record_flags(&record) & fs_ntfs::mft_io::MFT_FLAG_IN_USE,
            0,
            "{cluster}: record {r} of /streams.txt (base {base}) is still in use"
        );
        assert!(
            !fs_ntfs::mft_bitmap::is_allocated_io(&mut io, &mbm, r).unwrap(),
            "{cluster}: record {r} of /streams.txt (base {base}) is still allocated in $MFT's bitmap"
        );
    }
    assert_eq!(
        fs_ntfs::mft_bitmap::count_free_io(&mut io, &mbm).unwrap(),
        free_records + 1 + extensions.len() as u64,
        "{cluster}: the records freed are the base and its {} extension records",
        extensions.len()
    );
    assert_eq!(
        fs_ntfs::bitmap::count_free_io(&mut io, &bm).unwrap(),
        free_clusters + mapped,
        "{cluster}: the clusters freed are the {mapped} the file's records mapped"
    );
    let report = fs_ntfs::fsck::check_io(&mut io).unwrap();
    assert!(
        !report.dirty && report.findings.is_empty(),
        "{cluster}: check_io: {report:?}"
    );
    drop(io);

    // ntfs-3g finds no file there, and would still mount the volume
    // read-write.
    assert_eq!(
        ntfscat(&img, "/streams.txt", None),
        None,
        "{cluster}: ntfs-3g still reads /streams.txt"
    );
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
fn unlinking_a_file_whose_streams_fill_extension_records_frees_them_at_512_byte_clusters() {
    unlinked_on(512);
}

#[test]
fn unlinking_a_file_whose_streams_fill_extension_records_frees_them_at_4k_clusters() {
    unlinked_on(4096);
}
