//! Reparse points and object ids are entered in the `$Extend` view
//! indexes that follow them, `$Reparse:$R` and `$ObjId:$O`, and leave them
//! with the attribute or the file (#478).
//!
//! The volume is the mkntfs-made `ntfs-basic.img`, so the indexes are laid
//! out as another implementation made them. The entries' layout is held to
//! what Windows writes in `view_index`'s unit tests, and chkdsk grades a
//! volume this crate wrote them on in the Windows matrix
//! (`mac-format-mac-symlink-win-chkdsk`, `mac-format-mac-objid-win-chkdsk`).

mod common;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::view_index::{self, OBJID_INDEX, OBJID_PATH, REPARSE_INDEX, REPARSE_PATH};
use fs_ntfs::write;
use std::path::Path;

const BASIC_IMG: &str = "test-disks/ntfs-basic.img";
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

fn working_copy(tag: &str) -> String {
    let dst = common::temp_image_path(format!("extend_view_{tag}"));
    std::fs::copy(BASIC_IMG, &dst).expect("copy");
    dst
}

/// The file reference (record and sequence) of `path`.
fn reference(img: &str, path: &str) -> u64 {
    let mut io = PathIo::open_ro(Path::new(img)).unwrap();
    let rec = fs_ntfs::read::resolve_path(&mut io, path).unwrap();
    let (_, record) = fs_ntfs::mft_io::read_mft_record_io(&mut io, rec).unwrap();
    rec | u64::from(fs_ntfs::mft_io::record_sequence(&record)) << 48
}

/// The keys of the view index `index` in the record at `path`.
fn keys(img: &str, path: &str, index: &str) -> Vec<Vec<u8>> {
    let mut io = PathIo::open_ro(Path::new(img)).unwrap();
    let rec = fs_ntfs::read::resolve_path(&mut io, path).unwrap();
    let (_, record) = fs_ntfs::mft_io::read_mft_record_io(&mut io, rec).unwrap();
    view_index::keys(&record, index).unwrap()
}

#[test]
fn a_symlink_is_entered_in_reparse_and_leaves_it_when_unlinked() {
    let img = working_copy("symlink");
    let before = keys(&img, REPARSE_PATH, REPARSE_INDEX);
    write::create_symlink(Path::new(&img), "/", "link.txt", "hello.txt", true).unwrap();
    let key = view_index::reparse_key(IO_REPARSE_TAG_SYMLINK, reference(&img, "/link.txt"));
    let after = keys(&img, REPARSE_PATH, REPARSE_INDEX);
    assert!(
        after.contains(&key),
        "$R does not hold the symlink's entry: {after:?}"
    );
    assert_eq!(
        after.len(),
        before.len() + 1,
        "$R gained more than the one entry"
    );

    write::unlink(Path::new(&img), "/link.txt").unwrap();
    assert_eq!(
        keys(&img, REPARSE_PATH, REPARSE_INDEX),
        before,
        "the unlinked symlink's entry is still in $R"
    );
}

#[test]
fn a_reparse_point_retagged_moves_its_entry_and_one_removed_takes_it_away() {
    let img = working_copy("retag");
    let file = reference(&img, "/hello.txt");
    write::write_reparse_point(
        Path::new(&img),
        "/hello.txt",
        IO_REPARSE_TAG_SYMLINK,
        &[0; 12],
    )
    .unwrap();
    write::write_reparse_point(
        Path::new(&img),
        "/hello.txt",
        IO_REPARSE_TAG_MOUNT_POINT,
        &[0; 8],
    )
    .unwrap();
    let after = keys(&img, REPARSE_PATH, REPARSE_INDEX);
    assert!(after.contains(&view_index::reparse_key(IO_REPARSE_TAG_MOUNT_POINT, file)));
    assert!(
        !after.contains(&view_index::reparse_key(IO_REPARSE_TAG_SYMLINK, file)),
        "the old tag's entry stayed in $R: {after:?}"
    );

    write::remove_reparse_point(Path::new(&img), "/hello.txt").unwrap();
    assert!(
        keys(&img, REPARSE_PATH, REPARSE_INDEX)
            .iter()
            .all(|k| k[4..] != file.to_le_bytes()),
        "a removed reparse point's entry stayed in $R"
    );
}

#[test]
fn a_junction_directory_leaves_reparse_when_removed() {
    let img = working_copy("junction");
    write::mkdir(Path::new(&img), "/", "j").unwrap();
    write::write_reparse_point(Path::new(&img), "/j", IO_REPARSE_TAG_MOUNT_POINT, &[0; 8]).unwrap();
    let key = view_index::reparse_key(IO_REPARSE_TAG_MOUNT_POINT, reference(&img, "/j"));
    assert!(keys(&img, REPARSE_PATH, REPARSE_INDEX).contains(&key));
    write::rmdir(Path::new(&img), "/j").unwrap();
    assert!(
        !keys(&img, REPARSE_PATH, REPARSE_INDEX).contains(&key),
        "the removed junction's entry stayed in $R"
    );
}

#[test]
fn an_object_id_is_entered_in_objid_moves_with_it_and_leaves_with_the_file() {
    let img = working_copy("objid");
    let (first, second) = ([0x11u8; 16], [0x22u8; 16]);
    write::write_object_id(Path::new(&img), "/hello.txt", &first).unwrap();
    assert!(keys(&img, OBJID_PATH, OBJID_INDEX).contains(&first.to_vec()));

    write::write_object_id(Path::new(&img), "/hello.txt", &second).unwrap();
    let after = keys(&img, OBJID_PATH, OBJID_INDEX);
    assert!(after.contains(&second.to_vec()));
    assert!(
        !after.contains(&first.to_vec()),
        "the old id's entry stayed in $O"
    );

    assert!(write::remove_object_id(Path::new(&img), "/hello.txt").unwrap());
    assert!(!keys(&img, OBJID_PATH, OBJID_INDEX).contains(&second.to_vec()));

    write::write_object_id(Path::new(&img), "/hello.txt", &first).unwrap();
    write::unlink(Path::new(&img), "/hello.txt").unwrap();
    assert!(
        !keys(&img, OBJID_PATH, OBJID_INDEX).contains(&first.to_vec()),
        "the unlinked file's id stayed in $O"
    );
}

/// An object id names one file, as Windows has it: a second file is
/// refused the id, and nothing is written.
#[test]
fn a_second_file_is_refused_an_object_id_another_has() {
    let img = working_copy("objid_dup");
    let id = [0x33u8; 16];
    write::write_object_id(Path::new(&img), "/hello.txt", &id).unwrap();
    write::create_file(Path::new(&img), "/", "other.txt").unwrap();
    let before = std::fs::read(&img).unwrap();
    let err = write::write_object_id(Path::new(&img), "/other.txt", &id).unwrap_err();
    assert!(
        err.to_string().contains("already has that object id"),
        "{err}"
    );
    assert!(std::fs::read(&img).unwrap() == before, "the refusal wrote");
    // The same id again on the same file is no change.
    write::write_object_id(Path::new(&img), "/hello.txt", &id).unwrap();
}
