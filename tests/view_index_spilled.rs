//! The `$Extend` view indexes past their root (#571): entries are put into
//! and taken out of `$Extend\$ObjId:$O` and `$Extend\$Reparse:$R` once they
//! have spilled into INDX blocks, wherever each entry belongs.
//!
//! Most volumes here are `windows-interrupted-quota.recovered`, a volume
//! Windows wrote whose `$O` already spills past its root. Its `$R` does not:
//! this test grows `$R` from its root while adding reparse points. One
//! volume is formatted here, so this crate grows both indexes itself. The
//! indexes are read back whole after each change, so a lost, extra or
//! misordered entry shows here. Windows matrix scenarios
//! `win-format-win-write-objids-mac-delete-win-chkdsk` and
//! `win-format-win-write-views-mac-write-views-win-chkdsk` also grade
//! insertions into indexes Windows itself spilled, with chkdsk.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::fsck;
use fs_ntfs::view_index::{self, OBJID_INDEX, OBJID_PATH, REPARSE_INDEX, REPARSE_PATH};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const NAME: &str = "windows-interrupted-quota.recovered";
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

fn unpack() -> String {
    let path = common::temp_image_path(format!("view_index_spilled_{}", NAME.replace('.', "_")));
    let out = File::create(&path).expect("create image copy");
    let status = Command::new("gzip")
        .args(["-dc", &format!("test-disks/{NAME}.img.gz")])
        .stdout(Stdio::from(out))
        .status()
        .expect("run gzip");
    assert!(status.success(), "decompress test-disks/{NAME}.img.gz");
    path
}

/// `$O`'s keys in the index's order, each with whether it sits in an
/// interior node.
fn objid_tree(img: &str) -> Vec<(Vec<u8>, bool)> {
    tree(img, OBJID_PATH, OBJID_INDEX)
}

/// The keys of the view index `index` of the record at `path`, in the
/// index's order, each with whether it sits in an interior node.
fn tree(img: &str, path: &str, index: &str) -> Vec<(Vec<u8>, bool)> {
    let mut io = PathIo::open_ro(Path::new(img)).unwrap();
    let rec = fs_ntfs::read::resolve_path(&mut io, path).unwrap();
    view_index::keys_io(&mut io, rec, index).unwrap()
}

/// How many of a tree's keys sit in interior nodes: one more for each node
/// that split.
fn interior(tree: &[(Vec<u8>, bool)]) -> usize {
    tree.iter().filter(|(_, interior)| *interior).count()
}

fn keys_of(tree: &[(Vec<u8>, bool)]) -> Vec<Vec<u8>> {
    tree.iter().map(|(k, _)| k.clone()).collect()
}

fn ascending(keys: &[Vec<u8>]) -> bool {
    keys.windows(2)
        .all(|w| view_index::ulongs_cmp(&w[0], &w[1]) == std::cmp::Ordering::Less)
}

/// `before` with `added` put in, in `COLLATION_NTOFS_ULONGS` order.
fn merged(before: &[Vec<u8>], added: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut all: Vec<Vec<u8>> = before.iter().chain(added).cloned().collect();
    all.sort_by(|a, b| view_index::ulongs_cmp(a, b));
    all
}

/// The file reference (record and sequence) of `path`.
fn reference(img: &str, path: &str) -> u64 {
    let mut io = PathIo::open_ro(Path::new(img)).unwrap();
    let rec = fs_ntfs::read::resolve_path(&mut io, path).unwrap();
    let (_, record) = fs_ntfs::mft_io::read_mft_record_io(&mut io, rec).unwrap();
    rec | u64::from(fs_ntfs::mft_io::record_sequence(&record)) << 48
}

/// What `check_io` finds wrong with the volume: `$Bitmap` against every
/// run list, so an INDX block written but not allocated, or allocated and
/// left unclaimed, shows here.
fn findings(img: &str) -> Vec<fsck::CheckFinding> {
    let mut io = PathIo::open_ro(Path::new(img)).unwrap();
    fsck::check_io(&mut io).expect("check").findings
}

/// The workload files the manifest lists, each with the object id it
/// carries, if any.
fn workload_files(img: &str) -> Vec<(String, Option<[u8; 16]>)> {
    let manifest =
        std::fs::read_to_string(format!("test-disks/{NAME}.manifest")).expect("read manifest");
    manifest
        .lines()
        .map(|line| {
            let path = format!("/{}", line.split('\t').next().unwrap());
            let id = fs_ntfs::write::read_object_id(Path::new(img), &path)
                .unwrap_or_else(|e| panic!("read the object id of {path}: {e}"));
            (path, id)
        })
        .collect()
}

/// The `n`th object id of a run spread over the whole key space: its words
/// are `n + 1` times odd constants, so no two in a run agree.
fn spread_id(n: u32, salt: u32) -> [u8; 16] {
    let mut id = [0u8; 16];
    for (w, word) in id.chunks_mut(4).enumerate() {
        let factor = 0x9E37_79B1u32.wrapping_add(0x0246_8ACE * w as u32) | 1;
        let v = (n + 1).wrapping_mul(factor) ^ salt;
        word.copy_from_slice(&v.to_le_bytes());
    }
    id
}

/// The `n`th object id of a run that sorts after every random one: its
/// first word is near the top of the key space, so each routes to the
/// index's last leaf and that leaf has to split.
fn top_id(n: u32) -> [u8; 16] {
    let mut id = spread_id(n, 0x5A5A_5A5A);
    id[0..4].copy_from_slice(&(0xFFFF_F000u32 | n).to_le_bytes());
    id
}

#[test]
fn unlinking_files_whose_object_ids_windows_indexed_past_the_root_takes_their_entries_out() {
    let img = unpack();
    let before = objid_tree(&img);
    assert!(
        before.iter().any(|(_, interior)| *interior) && before.iter().any(|(_, i)| !*i),
        "{NAME}'s $O has not spilled past its root: the test proves nothing"
    );
    let before_keys: Vec<Vec<u8>> = before.iter().map(|(k, _)| k.clone()).collect();
    assert!(
        ascending(&before_keys),
        "Windows' $O is not in order as read"
    );

    // The workload files, and the object id each one carries.
    let manifest =
        std::fs::read_to_string(format!("test-disks/{NAME}.manifest")).expect("read manifest");
    let mut by_id: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    for line in manifest.lines() {
        let path = format!("/{}", line.split('\t').next().unwrap());
        if let Some(id) = fs_ntfs::write::read_object_id(Path::new(&img), &path).unwrap() {
            by_id.insert(id.to_vec(), path);
        }
    }
    let pick = |interior: bool| {
        before
            .iter()
            .find(|(k, i)| *i == interior && by_id.contains_key(k))
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| {
                panic!(
                    "no workload file's object id sits in {} node of $O",
                    if interior { "an interior" } else { "a leaf" }
                )
            })
    };
    let gone = [pick(true), pick(false)];

    for key in &gone {
        let path = &by_id[key];
        fs_ntfs::write::unlink(Path::new(&img), path)
            .unwrap_or_else(|e| panic!("unlink {path}: {e}"));
    }

    let after: Vec<Vec<u8>> = objid_tree(&img).into_iter().map(|(k, _)| k).collect();
    let want: Vec<Vec<u8>> = before_keys
        .iter()
        .filter(|k| !gone.contains(k))
        .cloned()
        .collect();
    assert_eq!(
        after.len(),
        want.len(),
        "$O holds {} entries after the unlinks, not the {} it held less the two",
        after.len(),
        want.len()
    );
    assert!(
        after == want,
        "$O lost or reordered an entry it should have kept"
    );
    assert!(ascending(&after), "$O is out of order after the unlinks");
    for key in &gone {
        assert!(
            fs_ntfs::write::read_object_id(Path::new(&img), &by_id[key]).is_err(),
            "{} still resolves",
            by_id[key]
        );
    }
}

#[test]
fn object_ids_and_reparse_points_grow_indexes_on_a_windows_volume() {
    let img = unpack();
    let o_before = objid_tree(&img);
    let r_before = tree(&img, REPARSE_PATH, REPARSE_INDEX);
    assert!(
        interior(&o_before) > 0 && o_before.iter().any(|(_, i)| !*i),
        "{NAME}'s $O has not spilled past its root: the test proves nothing"
    );
    assert_eq!(
        interior(&r_before),
        0,
        "{NAME}'s $R fixture changed; update this root-growth test"
    );
    let found_before = findings(&img);

    // Workload files with no object id of their own. Their junctions are
    // directories beside them, so none of these has a reparse point yet.
    let bare: Vec<String> = workload_files(&img)
        .into_iter()
        .filter_map(|(path, id)| id.is_none().then_some(path))
        .collect();
    let (spread, top, links) = (40usize, 60usize, 160usize);
    assert!(
        bare.len() >= links.max(spread + top),
        "{NAME} has {} workload files without an object id, too few for this test",
        bare.len()
    );

    // Ids spread over the key space land in every leaf Windows made; ids
    // that sort after them all fill the last leaf until it splits.
    let ids: Vec<[u8; 16]> = (0..spread as u32)
        .map(|n| spread_id(n, 0x0571_0571))
        .chain((0..top as u32).map(top_id))
        .collect();
    let mut object_paths = Vec::with_capacity(ids.len());
    for path in &bare {
        let Some(id) = ids.get(object_paths.len()) else {
            break;
        };
        match fs_ntfs::write::write_object_id(Path::new(&img), path, id) {
            Ok(()) => object_paths.push(path.clone()),
            Err(e) if e.to_string().contains("no room for new attribute") => continue,
            Err(e) => panic!("give {path} an object id: {e}"),
        }
    }
    assert_eq!(
        object_paths.len(),
        ids.len(),
        "only {} files had room for the {} object ids",
        object_paths.len(),
        ids.len()
    );
    // New symlink keys go after the fixture's existing $R entries, so they
    // grow its rightmost leaf (and split it as it fills).
    let mut r_added = Vec::new();
    for path in &bare {
        if r_added.len() == links {
            break;
        }
        match fs_ntfs::write::write_reparse_point(
            Path::new(&img),
            path,
            IO_REPARSE_TAG_SYMLINK,
            &[0; 12],
        ) {
            Ok(()) => r_added.push(view_index::reparse_key(
                IO_REPARSE_TAG_SYMLINK,
                reference(&img, path),
            )),
            Err(e) if e.to_string().contains("no room for new attribute") => continue,
            Err(e) => panic!("give {path} a reparse point: {e}"),
        }
    }
    assert_eq!(
        r_added.len(),
        links,
        "too few files had room for reparse points"
    );

    let o_added: Vec<Vec<u8>> = ids.iter().map(|id| id.to_vec()).collect();
    let o_after = objid_tree(&img);
    assert!(
        keys_of(&o_after) == merged(&keys_of(&o_before), &o_added),
        "$O does not hold exactly what it held and the {} ids put into it, in order",
        o_added.len()
    );
    assert!(
        interior(&o_after) > interior(&o_before),
        "no node of $O split, though {top} ids went into its last leaf"
    );
    let r_after = tree(&img, REPARSE_PATH, REPARSE_INDEX);
    assert!(
        keys_of(&r_after) == merged(&keys_of(&r_before), &r_added),
        "$R does not hold exactly what it held and the {links} entries put into it, in order"
    );
    assert!(
        interior(&r_after) > interior(&r_before),
        "no node of $R split, though {links} entries went into its last leaf"
    );
    assert_eq!(
        findings(&img),
        found_before,
        "the INDX blocks the splits took are not accounted for in $Bitmap"
    );
    for (path, id) in object_paths.iter().zip(&ids) {
        assert_eq!(
            fs_ntfs::write::read_object_id(Path::new(&img), path).unwrap(),
            Some(*id),
            "{path} lost its object id"
        );
    }
}

/// An object id names one file, as Windows has it. The id Windows gave a
/// file sits in an INDX block, not the root, and a second file is still
/// refused it, with nothing written.
#[test]
fn a_second_file_is_refused_an_object_id_windows_indexed_past_the_root() {
    let img = unpack();
    let files = workload_files(&img);
    let in_a_leaf: Vec<Vec<u8>> = objid_tree(&img)
        .into_iter()
        .filter(|(_, interior)| !*interior)
        .map(|(k, _)| k)
        .collect();
    let id = files
        .iter()
        .filter_map(|(_, id)| *id)
        .find(|id| in_a_leaf.contains(&id.to_vec()))
        .expect("no workload file's object id sits in a leaf of $O");
    let other = files
        .iter()
        .find(|(_, id)| id.is_none())
        .map(|(path, _)| path.clone())
        .expect("every workload file has an object id");

    let before = std::fs::read(&img).unwrap();
    let err = fs_ntfs::write::write_object_id(Path::new(&img), &other, &id).unwrap_err();
    assert!(
        err.to_string().contains("already has that object id"),
        "{err}"
    );
    assert!(std::fs::read(&img).unwrap() == before, "the refusal wrote");
}

/// A volume formatted here has empty roots. Entries put in fill each root,
/// which hands its entries down to a first INDX block and gains its
/// `$INDEX_ALLOCATION` and `$Bitmap`; more split the leaves. Taking half
/// out again leaves the rest, in order.
#[test]
fn indexes_this_crate_formatted_grow_past_their_roots_and_shrink_again() {
    const VOL_SIZE: u64 = 64 * 1024 * 1024;
    const COUNT: u32 = 200;
    let img = common::temp_image_path("view_index_grows");
    File::create(&img).unwrap().set_len(VOL_SIZE).unwrap();
    {
        let mut io = PathIo::open_rw(Path::new(&img)).unwrap();
        // 1 KiB records, as Windows formats: each root holds only a few
        // entries before it fills.
        fs_ntfs::mkfs::format_filesystem(&mut io, VOL_SIZE, 4096, 1024, Some("VIEWS"), None)
            .unwrap();
        <PathIo as BlockIo>::sync(&mut io).unwrap();
    }
    for (index, path) in [(OBJID_INDEX, OBJID_PATH), (REPARSE_INDEX, REPARSE_PATH)] {
        assert!(
            tree(&img, path, index).is_empty(),
            "a fresh {index} is not empty"
        );
    }

    fs_ntfs::write::mkdir(Path::new(&img), "/", "views").unwrap();
    let mut o_added = Vec::new();
    let mut r_added = Vec::new();
    let mut paths = Vec::new();
    for n in 0..COUNT {
        let name = format!("l{n:03}");
        fs_ntfs::write::create_symlink(Path::new(&img), "/views", &name, "target", true)
            .unwrap_or_else(|e| panic!("symlink /views/{name}: {e}"));
        let path = format!("/views/{name}");
        let id = spread_id(n, 0x0047_0571);
        fs_ntfs::write::write_object_id(Path::new(&img), &path, &id)
            .unwrap_or_else(|e| panic!("give {path} an object id: {e}"));
        o_added.push(id.to_vec());
        r_added.push(view_index::reparse_key(
            IO_REPARSE_TAG_SYMLINK,
            reference(&img, &path),
        ));
        paths.push(path);
    }

    let o_tree = objid_tree(&img);
    let r_tree = tree(&img, REPARSE_PATH, REPARSE_INDEX);
    assert!(
        keys_of(&o_tree) == merged(&[], &o_added),
        "$O does not hold exactly the {COUNT} ids put into it, in order"
    );
    assert!(
        keys_of(&r_tree) == merged(&[], &r_added),
        "$R does not hold exactly the {COUNT} entries put into it, in order"
    );
    // 88-byte $O entries fill a 4 KiB block with fewer than fifty, and
    // 32-byte $R entries with fewer than 130: both had to split.
    assert!(interior(&o_tree) > 0, "$O never split past its first block");
    assert!(interior(&r_tree) > 0, "$R never split past its first block");
    assert_eq!(findings(&img), vec![], "after the indexes grew");

    // A junction is a mount point on a directory: it goes in beside them.
    fs_ntfs::write::mkdir(Path::new(&img), "/", "j").unwrap();
    fs_ntfs::write::write_reparse_point(Path::new(&img), "/j", IO_REPARSE_TAG_MOUNT_POINT, &[0; 8])
        .unwrap();
    let junction = view_index::reparse_key(IO_REPARSE_TAG_MOUNT_POINT, reference(&img, "/j"));
    r_added.push(junction.clone());
    assert!(
        keys_of(&tree(&img, REPARSE_PATH, REPARSE_INDEX)) == merged(&[], &r_added),
        "$R does not hold the junction in its place"
    );

    let (gone, kept): (Vec<_>, Vec<_>) = paths.iter().enumerate().partition(|(n, _)| n % 2 == 0);
    for (_, path) in &gone {
        fs_ntfs::write::unlink(Path::new(&img), path)
            .unwrap_or_else(|e| panic!("unlink {path}: {e}"));
    }
    let o_kept: Vec<Vec<u8>> = kept.iter().map(|(n, _)| o_added[*n].clone()).collect();
    let mut r_kept: Vec<Vec<u8>> = kept.iter().map(|(n, _)| r_added[*n].clone()).collect();
    r_kept.push(junction);
    assert!(
        keys_of(&objid_tree(&img)) == merged(&[], &o_kept),
        "$O does not hold exactly the ids of the files left, in order"
    );
    assert!(
        keys_of(&tree(&img, REPARSE_PATH, REPARSE_INDEX)) == merged(&[], &r_kept),
        "$R does not hold exactly the entries of the files left, in order"
    );
    assert_eq!(findings(&img), vec![], "after half the files went");
    std::fs::remove_file(&img).unwrap();
}
