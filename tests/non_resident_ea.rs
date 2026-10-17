//! An EA set too large for a resident `$EA` in any MFT record is stored
//! with `$EA` non-resident, as Windows stores it (#661, part of #480).
//!
//! `write_ea_io` refused such a set with "exceeds record capacity", or,
//! through a file's `$ATTRIBUTE_LIST`, with "does not fit in an MFT
//! record", although a file's EAs may total 64 KiB and Windows keeps them
//! in clusters. The layout is Windows': `test-disks/ntfs-ea.img` holds
//! files Windows gave large EA sets through `NtSetEaFile`, and this crate
//! is held to the same `$EA` header sizes, `$EA` bytes and
//! `$EA_INFORMATION` for the same changes, including whether a set that
//! shrinks goes back into its record. The `ntfs` crate, a parser that is
//! not this one, reads the `$EA` each time, `check_io` grades the clusters
//! it owns, and the Windows matrix has chkdsk grade the volume and read
//! every EA back through `NtQueryEaFile`.

mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{bitmap, ea_io, fsck, mft_io, read, write};
use ntfs::{NtfsAttributeType, NtfsReadSeek};
use std::fs::File;
use std::path::Path;

const FIXTURE: &str = "test-disks/ntfs-ea.img";
const VOL_SIZE: u64 = 16 * 1024 * 1024;

/// The changes `test-disks/build-windows-native-read-fixtures.ps1` makes
/// to each file, one `NtSetEaFile` call each: `(i, size)` sets `USER.E<i>`
/// to `size` bytes of `ea_value(i, size)`, and a size of 0 removes it.
const LARGE: (&str, &[(u32, usize)]) = ("/ea-large.bin", &[(1, 2000), (2, 2000), (3, 2000)]);
const FULL: (&str, &[(u32, usize)]) = (
    "/ea-full.bin",
    &[(1, 15000), (2, 15000), (3, 15000), (4, 15000)],
);
const SHRUNK: (&str, &[(u32, usize)]) = ("/ea-shrunk.bin", &[(1, 2000), (2, 10), (1, 0)]);

/// "ea <i> " repeated to `size` bytes: what the fixture builder, the
/// matrix's `mac-write-eas.sh` and `win-verify-eas.ps1` all use.
fn ea_value(i: u32, size: usize) -> Vec<u8> {
    format!("ea {i} ")
        .into_bytes()
        .into_iter()
        .cycle()
        .take(size)
        .collect()
}

/// The EAs `changes` leaves, sorted by name.
fn expected(changes: &[(u32, usize)]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut eas: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for &(i, size) in changes {
        let name = format!("USER.E{i}").into_bytes();
        eas.retain(|(n, _)| *n != name);
        if size > 0 {
            eas.push((name, ea_value(i, size)));
        }
    }
    eas.sort();
    eas
}

fn windows_fixture() -> String {
    assert!(
        Path::new(FIXTURE).is_file(),
        "missing {FIXTURE}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    FIXTURE.to_string()
}

/// A fresh volume with 1 KiB records, as Windows formats one, holding
/// `path`, whose unnamed stream is one byte, as the fixture's files'.
/// Returns the image and the file's record.
fn volume_with_a_file(tag: &str, path: &str) -> (String, u64) {
    let img = common::temp_image_path(format!("nrea_{tag}"));
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    let name = path.trim_start_matches('/');
    let rec = write::create_file_io(&mut io, "/", name).expect("create the file");
    write::write_file_contents_io(&mut io, path, &[0x45]).expect("write the file");
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    (img, rec)
}

/// `volume_with_a_file`, with eight 64-byte named streams: more than the
/// base record holds, so the file has an `$ATTRIBUTE_LIST` before any EA
/// is written.
fn volume_with_a_listed_file(tag: &str, path: &str) -> (String, u64) {
    let (img, rec) = volume_with_a_file(tag, path);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    for i in 1..=8 {
        write::write_named_stream_io(&mut io, path, &format!("s{i}"), &[b'0' + i; 64])
            .unwrap_or_else(|e| panic!("write {path}:s{i}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let (_, record) = mft_io::read_mft_record_io(&mut io, rec).expect("read the file");
    assert!(
        attr_io::find_attribute(&record, AttrType::AttributeList, None).is_some(),
        "eight streams left {path} without a list; the test no longer starts from one"
    );
    (img, rec)
}

/// Make the changes `changes` describes to `path`, each with this crate,
/// as the fixture builder made them with Windows.
fn apply(img: &str, path: &str, changes: &[(u32, usize)]) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    for &(i, size) in changes {
        let name = format!("USER.E{i}");
        let done = if size == 0 {
            write::remove_ea_io(&mut io, path, name.as_bytes())
        } else {
            write::write_ea_io(&mut io, path, name.as_bytes(), &ea_value(i, size), 0)
        };
        done.unwrap_or_else(|e| panic!("set {name} on {path} to {size} bytes: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
}

fn record_of(img: &str, path: &str) -> u64 {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    write::resolve_path_to_record_number_io(&mut io, path)
        .unwrap_or_else(|e| panic!("resolve {path}: {e}"))
}

/// `(name, value)` of each EA `list_eas_io` reports on `path`, sorted.
fn our_eas(img: &str, path: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let mut eas: Vec<(Vec<u8>, Vec<u8>)> = write::list_eas_io(&mut io, path)
        .unwrap_or_else(|e| panic!("list the EAs of {path}: {e}"))
        .into_iter()
        .map(|ea| (ea.name, ea.value))
        .collect();
    eas.sort();
    eas
}

/// What the `ntfs` crate finds of the `$EA` on `path`, following the
/// file's `$ATTRIBUTE_LIST`: whether it is resident, and its value.
fn ntfs_crate_ea(img: &str, path: &str) -> Option<(bool, Vec<u8>)> {
    let (ntfs, mut reader) = common::open(img);
    let file = common::navigate(&ntfs, &mut reader, path);
    let mut attrs = file.attributes();
    while let Some(item) = attrs.next(&mut reader) {
        let item = item.expect("an attribute of the file");
        let attr = item.to_attribute().expect("to_attribute");
        if attr.ty().expect("attribute type") != NtfsAttributeType::EA {
            continue;
        }
        let mut value = attr.value(&mut reader).expect("the $EA's value");
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = value.read(&mut reader, &mut chunk).expect("read the $EA");
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        return Some((attr.is_resident(), bytes));
    }
    None
}

/// The whole attribute of type `ty` the file at `rec` has, from its base
/// record or from the record its `$ATTRIBUTE_LIST` names.
fn attribute(img: &str, rec: u64, ty: AttrType) -> Option<Vec<u8>> {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let (_, base) = mft_io::read_mft_record_io(&mut io, rec).expect("read the base record");
    let mut records = vec![rec];
    if attr_io::find_attribute(&base, AttrType::AttributeList, None).is_some() {
        let list = read::read_attribute_value(&mut io, rec, AttrType::AttributeList, None)
            .expect("read the $ATTRIBUTE_LIST");
        records = read::parse_attribute_list(&list)
            .expect("parse the $ATTRIBUTE_LIST")
            .into_iter()
            .filter(|e| e.type_code == ty as u32)
            .map(|e| e.record_number)
            .collect();
    }
    for n in records {
        let (_, record) = mft_io::read_mft_record_io(&mut io, n).expect("read a record");
        if let Some(loc) = attr_io::find_attribute(&record, ty, None) {
            return Some(record[loc.attr_offset..loc.attr_offset + loc.attr_length].to_vec());
        }
    }
    None
}

/// The fields of a non-resident attribute's header that say how its value
/// is stored: flags, lowest and highest VCN, compression unit, and the
/// allocated, data and initialized sizes. `None` for a resident one.
fn nonresident_sizes(attr: &[u8]) -> Option<[u64; 7]> {
    if attr[0x08] == 0 {
        return None;
    }
    let u16_at = |at: usize| u64::from(u16::from_le_bytes([attr[at], attr[at + 1]]));
    let u64_at = |at: usize| u64::from_le_bytes(attr[at..at + 8].try_into().unwrap());
    Some([
        u16_at(0x0C),
        u64_at(0x10),
        u64_at(0x18),
        u16_at(0x22),
        u64_at(0x28),
        u64_at(0x30),
        u64_at(0x38),
    ])
}

/// The value of the resident `$EA_INFORMATION` of the file at `rec`.
fn ea_information(img: &str, rec: u64) -> Option<Vec<u8>> {
    let attr = attribute(img, rec, AttrType::ExtendedAttributeInformation)?;
    assert_eq!(attr[0x08], 0, "$EA_INFORMATION is non-resident");
    let off = u16::from_le_bytes([attr[0x14], attr[0x15]]) as usize;
    let len = u32::from_le_bytes(attr[0x10..0x14].try_into().unwrap()) as usize;
    Some(attr[off..off + len].to_vec())
}

fn free_clusters(img: &str) -> u64 {
    let mut io = PathIo::open_ro(Path::new(img)).expect("open_ro");
    let bm = bitmap::locate_bitmap_io(&mut io).expect("locate $Bitmap");
    bitmap::count_free_io(&mut io, &bm).expect("count free clusters")
}

fn clean(img: &str) {
    let mut io = PathIo::open_rw(Path::new(img)).expect("open_rw");
    let report = fsck::check_io(&mut io).expect("check the volume");
    assert!(
        report.findings.is_empty(),
        "check_io finds the volume unclean: {:?}",
        report.findings
    );
}

/// Windows stores each large set non-resident, and `list_eas_io` reads
/// back what Windows was handed, from the clusters.
#[test]
fn ea_sets_windows_stored_in_clusters_list_back_as_windows_was_handed_them() {
    let img = windows_fixture();
    for (path, changes) in [LARGE, FULL] {
        let (resident, _) =
            ntfs_crate_ea(&img, path).unwrap_or_else(|| panic!("Windows gave {path} no $EA"));
        assert!(
            !resident,
            "Windows kept the $EA of {path} resident; the fixture no longer measures a \
             non-resident $EA"
        );
        assert_eq!(
            our_eas(&img, path),
            expected(changes),
            "list_eas_io on {path}"
        );
    }
    let (path, changes) = SHRUNK;
    assert_eq!(
        our_eas(&img, path),
        expected(changes),
        "list_eas_io on {path}"
    );
}

/// The same EA changes Windows made give this crate's file the `$EA`
/// Windows' has: non-resident, with the same header sizes and the same
/// bytes, and the same `$EA_INFORMATION`.
#[test]
fn a_large_ea_set_is_stored_as_windows_stores_it() {
    let windows = windows_fixture();
    for (path, changes) in [LARGE, FULL] {
        let (img, rec) = volume_with_a_file(path.trim_start_matches('/'), path);
        apply(&img, path, changes);
        let windows_rec = record_of(&windows, path);
        let theirs = attribute(&windows, windows_rec, AttrType::ExtendedAttribute)
            .unwrap_or_else(|| panic!("Windows gave {path} no $EA"));
        let ours = attribute(&img, rec, AttrType::ExtendedAttribute)
            .unwrap_or_else(|| panic!("this crate gave {path} no $EA"));
        assert_eq!(
            nonresident_sizes(&ours),
            nonresident_sizes(&theirs),
            "the $EA header of {path}: [flags, lowest VCN, highest VCN, compression unit, \
             allocated, data, initialized]"
        );
        assert!(
            nonresident_sizes(&ours).is_some(),
            "{path}'s $EA is resident"
        );
        assert_eq!(
            ntfs_crate_ea(&img, path),
            ntfs_crate_ea(&windows, path),
            "the $EA of {path}, as the ntfs crate reads it"
        );
        assert_eq!(
            ea_information(&img, rec),
            ea_information(&windows, windows_rec),
            "the $EA_INFORMATION of {path}"
        );
        assert_eq!(
            our_eas(&img, path),
            expected(changes),
            "list_eas_io on {path}"
        );
        clean(&img);
    }
}

/// A set that shrinks back to a few bytes is stored where Windows stores
/// it: Windows decides whether `$EA` goes back into the record.
#[test]
fn a_shrunk_ea_set_is_stored_as_windows_stores_it() {
    let windows = windows_fixture();
    let (path, changes) = SHRUNK;
    let (img, rec) = volume_with_a_file("shrunk", path);
    apply(&img, path, changes);
    let windows_rec = record_of(&windows, path);
    let theirs = attribute(&windows, windows_rec, AttrType::ExtendedAttribute)
        .unwrap_or_else(|| panic!("Windows gave {path} no $EA"));
    let ours = attribute(&img, rec, AttrType::ExtendedAttribute)
        .unwrap_or_else(|| panic!("this crate gave {path} no $EA"));
    assert_eq!(
        nonresident_sizes(&ours),
        nonresident_sizes(&theirs),
        "the $EA header of {path} (None: resident): [flags, lowest VCN, highest VCN, \
         compression unit, allocated, data, initialized]"
    );
    assert_eq!(
        ntfs_crate_ea(&img, path),
        ntfs_crate_ea(&windows, path),
        "the $EA of {path}, as the ntfs crate reads it"
    );
    assert_eq!(
        ea_information(&img, rec),
        ea_information(&windows, windows_rec),
        "the $EA_INFORMATION of {path}"
    );
    clean(&img);
}

/// A 2000-byte EA on 1 KiB records: no record holds it resident, so its
/// `$EA` takes a cluster, which a smaller rewrite keeps and the removal
/// gives back.
#[test]
fn a_2000_byte_ea_is_stored_in_a_cluster_and_gives_it_back() {
    let (img, rec) = volume_with_a_file("2000", "/f");
    let before = free_clusters(&img);

    apply(&img, "/f", &[(1, 2000)]);
    assert_eq!(our_eas(&img, "/f"), expected(&[(1, 2000)]), "list_eas_io");
    let (resident, bytes) = ntfs_crate_ea(&img, "/f").expect("the ntfs crate finds no $EA");
    assert!(!resident, "the ntfs crate finds the $EA resident");
    assert_eq!(
        ea_io::decode(&bytes).expect("decode the $EA the ntfs crate read")[0].value,
        ea_value(1, 2000),
        "the ntfs crate's $EA"
    );
    assert_eq!(free_clusters(&img), before - 1, "clusters the $EA took");
    clean(&img);

    apply(&img, "/f", &[(1, 1200)]);
    assert_eq!(
        our_eas(&img, "/f"),
        expected(&[(1, 1200)]),
        "after the rewrite"
    );
    assert_eq!(
        free_clusters(&img),
        before - 1,
        "clusters after the rewrite"
    );
    clean(&img);

    apply(&img, "/f", &[(1, 0)]);
    assert!(our_eas(&img, "/f").is_empty(), "EAs left after the removal");
    assert!(
        ntfs_crate_ea(&img, "/f").is_none(),
        "the ntfs crate finds an $EA after the removal"
    );
    assert!(
        attribute(&img, rec, AttrType::ExtendedAttributeInformation).is_none(),
        "an $EA_INFORMATION is left after the removal"
    );
    assert_eq!(free_clusters(&img), before, "clusters after the removal");
    clean(&img);
}

/// A file with an `$ATTRIBUTE_LIST` takes a large set through it: the list
/// names the non-resident `$EA` where it is, and removing the set takes
/// out both entries and gives the clusters back.
#[test]
fn a_large_ea_set_on_a_file_with_a_list_goes_into_clusters_through_it() {
    let (img, rec) = volume_with_a_listed_file("listed", "/f");
    let before = free_clusters(&img);
    let changes = [(1, 3000), (2, 3000), (3, 3000), (2, 0)];
    apply(&img, "/f", &changes);
    assert_eq!(our_eas(&img, "/f"), expected(&changes), "list_eas_io");
    let (resident, _) = ntfs_crate_ea(&img, "/f").expect("the ntfs crate finds no $EA");
    assert!(!resident, "the ntfs crate finds the $EA resident");
    let ea = attribute(&img, rec, AttrType::ExtendedAttribute).expect("the list names no $EA");
    assert!(
        nonresident_sizes(&ea).is_some(),
        "the listed $EA is resident"
    );
    clean(&img);

    apply(&img, "/f", &[(1, 0), (3, 0)]);
    assert!(our_eas(&img, "/f").is_empty(), "EAs left after the removal");
    assert!(
        attribute(&img, rec, AttrType::ExtendedAttribute).is_none()
            && attribute(&img, rec, AttrType::ExtendedAttributeInformation).is_none(),
        "the list still names an $EA or $EA_INFORMATION"
    );
    assert_eq!(free_clusters(&img), before, "clusters after the removal");
    for i in 1..=8u8 {
        let mut io = PathIo::open_ro(Path::new(&img)).expect("open_ro");
        let got = read::read_attribute_value(&mut io, rec, AttrType::Data, Some(&format!("s{i}")))
            .expect("read a stream");
        assert_eq!(got, vec![b'0' + i; 64], "s{i} changed");
    }
    clean(&img);
}
