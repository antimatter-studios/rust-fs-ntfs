//! Files whose object ids Windows entered in a `$Extend\$ObjId:$O` that has
//! spilled into INDX blocks are unlinked, and their entries leave the index
//! wherever they sit: in an interior node or a leaf (#571).
//!
//! The volume is `windows-interrupted-quota.recovered`, which Windows
//! itself wrote: its workload gave every fourth file an object id with
//! `fsutil objectid create`, and Windows grew `$O` past its root. The index
//! is read back whole after each unlink, so an entry left behind, an entry
//! lost or an order broken shows here; the Windows matrix scenario
//! `win-format-win-write-objids-mac-delete-win-chkdsk` has chkdsk grade the
//! same change on a volume Windows filled.

mod common;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::view_index::{self, OBJID_INDEX, OBJID_PATH};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const NAME: &str = "windows-interrupted-quota.recovered";

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
    let mut io = PathIo::open_ro(Path::new(img)).unwrap();
    let rec = fs_ntfs::read::resolve_path(&mut io, OBJID_PATH).unwrap();
    view_index::keys_io(&mut io, rec, OBJID_INDEX).unwrap()
}

fn ascending(keys: &[Vec<u8>]) -> bool {
    keys.windows(2)
        .all(|w| view_index::ulongs_cmp(&w[0], &w[1]) == std::cmp::Ordering::Less)
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
