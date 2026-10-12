//! Listing a file's named streams follows its `$ATTRIBUTE_LIST` (#472).
//!
//! `list_named_streams_io` walked the base record only. A file whose
//! streams spill into extension records got a short list with no error,
//! while reading each stream by name worked -- so a backup, a copy or a
//! host's xattr listing silently lost the rest.
//!
//! Graded by Windows: `test-disks/ntfs-attrlist.img` is a volume Windows
//! wrote, where `/many.bin` carries the 96 streams `stream-000` to
//! `stream-095` that `build-windows-native-read-fixtures.ps1` gave it, more
//! than its base record holds.

use fs_ntfs::block_io::PathIo;
use fs_ntfs::write;
use std::path::Path;

const ATTRLIST_IMG: &str = "test-disks/ntfs-attrlist.img";

#[test]
fn every_stream_windows_wrote_is_listed() {
    assert!(
        Path::new(ATTRLIST_IMG).exists(),
        "missing {ATTRLIST_IMG}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let mut io = PathIo::open_ro(Path::new(ATTRLIST_IMG)).expect("open the fixture");
    let mut listed = write::list_named_streams_io(&mut io, "/many.bin").expect("list streams");
    let count = listed.len();
    listed.sort();
    listed.dedup();
    assert_eq!(listed.len(), count, "a stream is listed more than once");
    let want: Vec<String> = (0..96).map(|i| format!("stream-{i:03}")).collect();
    assert_eq!(
        listed, want,
        "the listing is not the 96 streams Windows wrote"
    );
}
