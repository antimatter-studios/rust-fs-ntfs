//! An EA name is stored as Windows stores it (#482).
//!
//! `write_ea_io` kept an EA name in the caller's case. Whether NTFS keeps
//! it or folds it was unverified, so a volume this crate wrote could hold a
//! spelling Windows itself never stores. The crate stores what Windows
//! stores.
//!
//! Graded by Windows: `test-disks/ntfs-ea.img`, where Windows set each EA
//! in `test-disks/ntfs-ea.names.tsv` on `/ea.bin` through `NtSetEaFile`.
//! The same names and values are written here through `write_ea_io`, and
//! each must be stored with the bytes Windows stored for it.

mod common;

use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::ea_io::Ea;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::write;
use std::fs::File;
use std::path::Path;

const IMG: &str = "test-disks/ntfs-ea.img";
const NAMES: &str = "test-disks/ntfs-ea.names.tsv";
const VOL_SIZE: u64 = 16 * 1024 * 1024;

/// (name, value) as handed to Windows, in order.
fn handed_to_windows() -> Vec<(String, String)> {
    for path in [IMG, NAMES] {
        assert!(
            Path::new(path).exists(),
            "missing {path}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
        );
    }
    std::fs::read_to_string(NAMES)
        .expect("read the EA names")
        .lines()
        .map(|line| {
            let (name, value) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("{NAMES}: no tab in {line:?}"));
            (name.to_string(), value.to_string())
        })
        .collect()
}

/// The EA among `eas` that `name` names, compared case-insensitively, as
/// both Windows and this crate identify EAs.
fn stored<'e>(eas: &'e [Ea], name: &str, whose: &str) -> &'e Ea {
    eas.iter()
        .find(|ea| ea.name.eq_ignore_ascii_case(name.as_bytes()))
        .unwrap_or_else(|| panic!("{whose} $EA has no EA named {name:?}: {eas:?}"))
}

#[test]
fn each_ea_name_is_stored_as_windows_stores_it() {
    let eas = handed_to_windows();
    assert!(!eas.is_empty(), "{NAMES} names no EA");

    let mut windows_io = PathIo::open_ro(Path::new(IMG)).expect("open the Windows fixture");
    let windows = write::list_eas_io(&mut windows_io, "/ea.bin").expect("list Windows' EAs");

    let img = common::temp_image_path("ea_windows_oracle");
    let f = File::create(&img).expect("create image");
    f.set_len(VOL_SIZE).expect("set_len");
    drop(f);
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open_rw");
    format_filesystem(&mut io, VOL_SIZE, 4096, 1024, None, Some(1)).expect("format_filesystem");
    write::create_file_io(&mut io, "/", "ea.bin").expect("create /ea.bin");
    for (name, value) in &eas {
        write::write_ea_io(&mut io, "/ea.bin", name.as_bytes(), value.as_bytes(), 0)
            .unwrap_or_else(|e| panic!("write EA {name:?}: {e}"));
    }
    <PathIo as BlockIo>::sync(&mut io).expect("sync");
    let ours = write::list_eas_io(&mut io, "/ea.bin").expect("list our EAs");

    let mut wrong = Vec::new();
    for (name, value) in &eas {
        let w = stored(&windows, name, "Windows'");
        let o = stored(&ours, name, "this crate's");
        assert_eq!(w.value, value.as_bytes(), "Windows stored {name:?}'s value");
        if o.name != w.name {
            wrong.push(format!(
                "{name:?}: Windows stored {:?}, this crate {:?}",
                String::from_utf8_lossy(&w.name),
                String::from_utf8_lossy(&o.name)
            ));
        }
        assert_eq!(o.value, w.value, "{name:?}'s value");
    }
    std::fs::remove_file(&img).ok();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
